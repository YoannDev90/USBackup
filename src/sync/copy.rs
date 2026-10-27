//! Atomic file copy.
//!
//! The single most important property here is that a destination path is
//! **never left partially written**. We write into a sibling temp file and
//! `rename()` it into place, which is atomic on every filesystem we care about
//! (ext4, APFS, NTFS, and the FAT family as long as temp and destination are
//! on the same volume — which they always are, since we create the temp next
//! to the destination).
//!
//! Consequences:
//!
//! * A crash or a yanked USB key mid-copy leaves a `*.usbackup.tmp-*` orphan,
//!   never a truncated destination. The next scan ignores/cleans these.
//! * Readers of the destination always see either the old file or the complete
//!   new one.
//!
//! The caller supplies the scratch buffer so a `Bulk` worker allocates it once
//! and reuses it across every file it copies — no per-file allocation.

use crate::sync::error::{Result, SyncError};
use crate::sync::meta;
use std::fs::File;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

/// Default scratch buffer for `Bulk` copies (design §7.3).
pub const BULK_BUFFER: usize = 1024 * 1024; // 1 MiB

/// Reduced scratch buffer for many-small-files (`Metadata`) copies.
pub const META_BUFFER: usize = 64 * 1024; // 64 KiB

/// Distinguishes temp files created by this process from unrelated litter.
pub const TEMP_PREFIX: &str = ".usbackup.tmp-";

/// Monotonic counter so two concurrent copies in one process never collide.
static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Build the temp path for `dst`: a sibling in the same directory so the final
/// `rename` stays on one filesystem (a requirement for atomicity).
pub(crate) fn temp_path(dst: &Path) -> PathBuf {
    let n = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
    let file_name = dst
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "file".to_string());

    let temp_name = format!("{file_name}{TEMP_PREFIX}{}-{n}", std::process::id());
    match dst.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent.join(temp_name),
        _ => PathBuf::from(temp_name),
    }
}

/// True when `path` looks like one of our temp files (used by the scanner to
/// skip or clean orphans).
pub fn is_temp_file(path: &Path) -> bool {
    path.file_name()
        .map(|n| n.to_string_lossy().contains(TEMP_PREFIX))
        .unwrap_or(false)
}

/// Copy `src` to `dst` atomically, using the caller-supplied `buf` as scratch.
///
/// Returns the number of bytes written. Metadata (mtime + permissions) is
/// preserved via [`crate::sync::meta`]. On any error the temp file is removed,
/// so a failed copy leaves no litter.
pub fn copy_file_atomic(src: &Path, dst: &Path, buf: &mut [u8]) -> Result<u64> {
    let mut reader = File::open(src).map_err(|e| SyncError::io(src, e))?;
    let temp = temp_path(dst);

    let written = (|| -> Result<u64> {
        let mut writer = File::create(&temp).map_err(|e| SyncError::io(&temp, e))?;
        let mut total: u64 = 0;

        loop {
            let n = reader.read(buf).map_err(|e| SyncError::io(src, e))?;
            if n == 0 {
                break;
            }
            writer
                .write_all(&buf[..n])
                .map_err(|e| SyncError::io(&temp, e))?;
            total += n as u64;
        }

        // Flush file data before the rename so a yanked key cannot expose a
        // renamed-but-empty file. Errors here are non-fatal (some filesystems
        // do not support sync); the batched end-of-run flush is the backstop.
        let _ = writer.sync_data();

        // Atomically move the temp into place.
        std::fs::rename(&temp, dst).map_err(|e| SyncError::io(dst, e))?;

        // Preserve mtime/permissions. A failure here does not undo the copy.
        if let Err(e) = meta::copy_metadata(src, dst) {
            log::debug!(
                "copied {} but could not preserve metadata: {}",
                dst.display(),
                e
            );
        }

        Ok(total)
    })();

    if written.is_err() {
        // Best-effort cleanup of the orphan temp file.
        let _ = std::fs::remove_file(&temp);
    }

    written
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    fn bulk_buf() -> Vec<u8> {
        vec![0u8; BULK_BUFFER]
    }

    #[test]
    fn copies_content_and_returns_size() {
        let dir = tempdir().unwrap();
        let src = dir.path().join("a.bin");
        let dst = dir.path().join("b.bin");
        let payload = vec![7u8; 3 * 1024 * 1024]; // > 1 MiB, forces loop
        fs::write(&src, &payload).unwrap();

        let mut buf = bulk_buf();
        let n = copy_file_atomic(&src, &dst, &mut buf).unwrap();

        assert_eq!(n, payload.len() as u64);
        assert_eq!(fs::read(&dst).unwrap(), payload);
    }

    #[test]
    fn copies_empty_file() {
        let dir = tempdir().unwrap();
        let src = dir.path().join("empty");
        let dst = dir.path().join("out");
        fs::write(&src, b"").unwrap();

        let mut buf = bulk_buf();
        let n = copy_file_atomic(&src, &dst, &mut buf).unwrap();

        assert_eq!(n, 0);
        assert_eq!(fs::read(&dst).unwrap(), b"");
    }

    #[test]
    fn preserves_source_mtime() {
        let dir = tempdir().unwrap();
        let src = dir.path().join("a");
        let dst = dir.path().join("b");
        fs::write(&src, b"data").unwrap();

        let want = std::time::SystemTime::now() - std::time::Duration::from_secs(900);
        meta::set_mtime(&src, want).unwrap();

        let mut buf = bulk_buf();
        copy_file_atomic(&src, &dst, &mut buf).unwrap();

        let got = fs::metadata(&dst).unwrap().modified().unwrap();
        let delta = got
            .duration_since(want)
            .or_else(|_| want.duration_since(got))
            .unwrap();
        assert!(delta < std::time::Duration::from_secs(2));
    }

    #[test]
    fn no_temp_file_remains_after_success() {
        let dir = tempdir().unwrap();
        let src = dir.path().join("a");
        let dst = dir.path().join("b");
        fs::write(&src, b"x").unwrap();

        let mut buf = bulk_buf();
        copy_file_atomic(&src, &dst, &mut buf).unwrap();

        let leftovers: Vec<_> = fs::read_dir(dir.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| is_temp_file(&e.path()))
            .collect();
        assert!(
            leftovers.is_empty(),
            "temp file left behind: {:?}",
            leftovers
        );
    }

    #[test]
    fn failed_copy_leaves_no_temp_and_no_destination() {
        let dir = tempdir().unwrap();
        let missing = dir.path().join("does-not-exist");
        let dst = dir.path().join("b");

        let mut buf = bulk_buf();
        let res = copy_file_atomic(&missing, &dst, &mut buf);
        assert!(res.is_err());

        assert!(!dst.exists());
        let leftovers: Vec<_> = fs::read_dir(dir.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| is_temp_file(&e.path()))
            .collect();
        assert!(
            leftovers.is_empty(),
            "temp litter on failure: {:?}",
            leftovers
        );
    }

    #[test]
    fn temp_files_are_distinguishable() {
        let p = Path::new("/x/y/file.usbackup.tmp-1234-0");
        assert!(is_temp_file(p));
        assert!(!is_temp_file(Path::new("/x/y/regular.txt")));
        assert!(!is_temp_file(Path::new("/x/y/.usbackup.toml")));
    }

    #[test]
    fn temp_path_is_sibling_of_destination() {
        let dst = Path::new("/mnt/usb/backups/file.txt");
        let t = temp_path(dst);
        assert_eq!(t.parent().unwrap(), Path::new("/mnt/usb/backups"));
        assert!(is_temp_file(&t));
    }

    #[test]
    fn temp_paths_are_unique_per_call() {
        let dst = Path::new("/x/f");
        let a = temp_path(dst);
        let b = temp_path(dst);
        assert_ne!(a, b);
    }
}
