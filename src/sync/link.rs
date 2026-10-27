//! Hard-link placement with a copy fallback (design §8.4).
//!
//! ## Why this exists
//!
//! A hard link makes an unchanged file cost ~microseconds instead of a full
//! copy: `hard_link(base/rel, new/rel)` points the new name at the *same*
//! inode. That is the mechanism behind incremental snapshots (§8) — re-linking
//! every unchanged file is what makes "snapshot #2 in under a second" possible,
//! where rsync would re-transfer or re-copy the data.
//!
//! ## The fallback is not optional
//!
//! `std::fs::hard_link` fails when:
//!
//! * source and destination are on **different filesystems** (`EXDEV`) — should
//!   not happen for an in-tree snapshot, but never assume it;
//! * the filesystem has **no hard links at all**: **FAT32 and exFAT**, which is
//!   exactly what many USB keys are formatted as.
//!
//! On those keys every link attempt must degrade to a real copy — the current
//! rsync behaviour — so the backup still *works*, just without the ×1000 boost.
//! On NTFS / APFS / ext4 (the majority of modern keys) the hard links give the
//! full speed-up.
//!
//! [`link_or_copy`] performs that decision and reports which path it took so the
//! caller can record it in [`crate::sync::progress::Stats`].

use crate::sync::copy::copy_file_atomic;
use crate::sync::error::{Result, SyncError};
use std::io;
use std::path::Path;

/// What actually happened when placing a file that already exists in a base
/// snapshot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinkOutcome {
    /// A hard link was created — the file was not copied (the fast path).
    Linked,
    /// The filesystem rejected hard links; the file was copied instead.
    Copied,
}

impl LinkOutcome {
    /// `true` when a hard link was made (no bytes moved).
    pub fn was_linked(&self) -> bool {
        matches!(self, LinkOutcome::Linked)
    }
}

/// Map a `hard_link` error onto the engine's error type, preserving the path.
fn link_error(at: &Path, e: io::Error) -> SyncError {
    SyncError::io(at, e)
}

/// Test whether a single error kind means "this filesystem cannot hard-link".
///
/// `Unsupported` is the explicit "operation not supported" signal (FAT32/exFAT).
/// `CrossesDevices` (or the raw `EXDEV`/`EOPNOTSUPP` codes) is the
/// different-filesystem case. Anything else (permissions, disk full) is a real
/// failure and must not be swallowed into a copy.
fn is_hard_link_unsupported(e: &io::Error) -> bool {
    if matches!(e.kind(), io::ErrorKind::Unsupported) {
        return true;
    }
    // `CrossesDevices` is not yet in every MSRV's `ErrorKind`, so also check the
    // raw OS codes for the classic "cannot link across filesystems" cases.
    #[cfg(unix)]
    {
        const EXDEV: i32 = 18; // cross-device link
        const EOPNOTSUPP: i32 = 95;
        if let Some(code) = e.raw_os_error() {
            return matches!(code, EXDEV | EOPNOTSUPP)
                || (code == 1 && e.kind() == io::ErrorKind::PermissionDenied); // EPERM
        }
    }
    false
}

/// Link `from` to `to`, falling back to a full copy when the filesystem cannot
/// hard-link.
///
/// `from` must already exist (it is an unchanged file in a base snapshot). On
/// success the caller learns whether it paid for a link (nanoseconds) or a copy
/// (bytes).
///
/// A pre-existing `to` is replaced, so re-running a snapshot is idempotent.
pub fn link_or_copy(from: &Path, to: &Path, buf: &mut [u8]) -> Result<LinkOutcome> {
    // Ensure the destination parent exists before linking/copying.
    if let Some(parent) = to.parent() {
        std::fs::create_dir_all(parent).map_err(|e| SyncError::io(parent, e))?;
    }

    // Replace any previous entry so a re-run converges. A stale regular file or
    // symlink here would make hard_link fail with EEXIST.
    remove_if_exists(to)?;

    match std::fs::hard_link(from, to) {
        Ok(()) => Ok(LinkOutcome::Linked),
        Err(e) if is_hard_link_unsupported(&e) => {
            // Filesystem cannot link (FAT32/exFAT) or the paths straddle two
            // devices — degrade to a real copy so the backup still succeeds.
            copy_file_atomic(from, to, buf)?;
            Ok(LinkOutcome::Copied)
        }
        Err(e) => Err(link_error(to, e)),
    }
}

/// Remove `path` if present, without ever following it (a symlink is unlinked,
/// its target untouched). Not an error when it is already absent.
fn remove_if_exists(path: &Path) -> Result<()> {
    match std::fs::symlink_metadata(path) {
        Ok(md) if md.is_dir() && !md.is_symlink() => {
            std::fs::remove_dir_all(path).map_err(|e| SyncError::io(path, e))
        }
        Ok(_) => std::fs::remove_file(path).map_err(|e| SyncError::io(path, e)),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(SyncError::io(path, e)),
    }
}

/// Verify that `path` and `base` share an inode — i.e. `path` really is a hard
/// link to `base`, not a copy. Returns `false` on filesystems without stable
/// inodes or when they differ.
///
/// Used by the snapshot tests to assert the fast path was taken.
#[cfg(unix)]
pub fn same_file(path: &Path, base: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    match (std::fs::metadata(path), std::fs::metadata(base)) {
        (Ok(a), Ok(b)) => a.ino() == b.ino() && a.dev() == b.dev(),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn link_creates_hard_link_when_supported() {
        let dir = tempdir().unwrap();
        let base = dir.path().join("base.txt");
        fs::write(&base, b"shared content").unwrap();

        let out = dir.path().join("snap.txt");
        let mut buf = [0u8; 4096];
        let outcome = link_or_copy(&base, &out, &mut buf).unwrap();

        // On ext4/APFS/NTFS-with-links we expect a hard link; on FAT we expect
        // a copy. Both are correct, but assert the data is right either way.
        assert_eq!(fs::read(&out).unwrap(), b"shared content");
        if outcome == LinkOutcome::Linked {
            #[cfg(unix)]
            {
                use std::os::unix::fs::MetadataExt;
                assert!(same_file(&out, &base), "Linked must share an inode");
                assert_eq!(
                    fs::metadata(&out).unwrap().nlink(),
                    2,
                    "a hard link means nlink == 2"
                );
            }
        }
    }

    #[test]
    fn link_replaces_existing_destination() {
        let dir = tempdir().unwrap();
        let base = dir.path().join("base");
        fs::write(&base, b"new").unwrap();

        let out = dir.path().join("out");
        fs::write(&out, b"stale old value").unwrap();

        let mut buf = [0u8; 4096];
        link_or_copy(&base, &out, &mut buf).unwrap();
        assert_eq!(fs::read(&out).unwrap(), b"new");
    }

    #[test]
    fn link_creates_missing_parent_dirs() {
        let dir = tempdir().unwrap();
        let base = dir.path().join("base");
        fs::write(&base, b"deep").unwrap();

        let out = dir.path().join("a/b/c/out");
        let mut buf = [0u8; 4096];
        link_or_copy(&base, &out, &mut buf).unwrap();
        assert_eq!(fs::read(&out).unwrap(), b"deep");
    }

    #[test]
    fn missing_source_is_an_error() {
        let dir = tempdir().unwrap();
        let out = dir.path().join("out");
        let mut buf = [0u8; 4096];
        let err = link_or_copy(&dir.path().join("nope"), &out, &mut buf);
        assert!(err.is_err());
    }

    #[test]
    fn is_hard_link_unsupported_is_false_for_generic_errors() {
        // A plain NotFound must NOT be treated as "filesystem can't link".
        let e = io::Error::from(io::ErrorKind::NotFound);
        assert!(!is_hard_link_unsupported(&e));
        let e = io::Error::from(io::ErrorKind::Unsupported);
        assert!(is_hard_link_unsupported(&e));
    }

    #[cfg(unix)]
    #[test]
    fn remove_if_exists_unlinks_symlink_but_keeps_target() {
        let dir = tempdir().unwrap();
        let target = dir.path().join("target");
        fs::write(&target, b"KEEP").unwrap();
        let link = dir.path().join("link");
        std::os::unix::fs::symlink(&target, &link).unwrap();

        remove_if_exists(&link).unwrap();
        assert!(!link.exists());
        assert_eq!(fs::read(&target).unwrap(), b"KEEP", "target must survive");
    }
}
