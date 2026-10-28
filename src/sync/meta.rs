//! Cross-platform metadata preservation: mtime/atime and permissions.
//!
//! Scope is deliberately narrow (see design §1): preserve **timestamps** and
//! **basic permissions**, nothing more. No ACLs, no ownership, no xattrs —
//! those are large, platform-specific and low-value for a USB document backup.
//!
//! Timestamps use `std::fs::FileTimes` / `File::set_times` (stable since the
//! crate's MSRV 1.75), so this needs no third-party crate and no `unsafe`.
//!
//! ## Platform behaviour
//!
//! | OS     | mtime | permissions              |
//! |--------|-------|--------------------------|
//! | Linux  | yes   | full mode bits           |
//! | macOS  | yes   | full mode bits           |
//! | Windows| yes   | read-only bit only       |
//! | Termux | yes   | mode bits (scoped)       |
//!
//! Every fallible operation here is a *best effort* at the call site: a
//! permission that cannot be applied is a warning, never a fatal error, so a
//! slightly odd filesystem cannot abort a backup.

use std::fs::{self, File, FileTimes};
use std::io;
use std::path::Path;
use std::time::SystemTime;

/// The metadata we know how to preserve.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PreservedMeta {
    pub mtime: Option<SystemTime>,
    pub atime: Option<SystemTime>,
    /// Read-only flag. Honoured on every platform (it is the only permission
    /// Windows exposes without ACLs).
    pub readonly: bool,
    /// Unix permission bits. Ignored on Windows.
    #[cfg(unix)]
    pub mode: u32,
}

/// Read the preservable metadata from `path`.
///
/// Timestamps are `None` when the filesystem does not report them. This is not
/// an error: some FUSE/virtual filesystems omit atime, for instance.
pub fn read_meta(path: &Path) -> io::Result<PreservedMeta> {
    let md = fs::metadata(path)?;
    let readonly = md.permissions().readonly();

    #[cfg(unix)]
    let mode = {
        use std::os::unix::fs::PermissionsExt;
        md.permissions().mode()
    };

    Ok(PreservedMeta {
        mtime: md.modified().ok(),
        atime: md.accessed().ok(),
        readonly,
        #[cfg(unix)]
        mode,
    })
}

/// Set only the last-modification time of `path`.
///
/// Used by change detection round-trips. Atime is left untouched.
#[cfg(not(windows))]
pub fn set_mtime(path: &Path, t: SystemTime) -> io::Result<()> {
    let file = open_for_times(path)?;
    file.set_times(FileTimes::new().set_modified(t))
}

/// Windows variant: `std` can only open directories for reading there and
/// `set_times` needs write access, so the std path above can never touch a
/// directory on Windows. `filetime` opens with `FILE_WRITE_ATTRIBUTES` +
/// `FILE_FLAG_BACKUP_SEMANTICS` and handles files and directories alike.
#[cfg(windows)]
pub fn set_mtime(path: &Path, t: SystemTime) -> io::Result<()> {
    filetime::set_file_mtime(path, filetime::FileTime::from_system_time(t))
        .map_err(io::Error::other)
}

/// Apply `meta` to `path`: timestamps first, then permissions.
///
/// Order matters. Setting permissions first could mark a file read-only and
/// then block the timestamp write on Windows, so we always set times before
/// tightening permissions.
pub fn apply_meta(path: &Path, meta: &PreservedMeta) -> io::Result<()> {
    if meta.mtime.is_some() || meta.atime.is_some() {
        let mut times = FileTimes::new();
        if let Some(m) = meta.mtime {
            times = times.set_modified(m);
        }
        if let Some(a) = meta.atime {
            times = times.set_accessed(a);
        }
        // A timestamp we cannot set is not fatal; permissions still apply.
        let _ = open_for_times(path)?.set_times(times);
    }

    apply_permissions(path, meta)?;
    Ok(())
}

/// Copy preservable metadata (timestamps + permissions) from `src` to `dst`.
///
/// This is the convenience entry point used by the copier after a data copy.
pub fn copy_metadata(src: &Path, dst: &Path) -> io::Result<()> {
    let meta = read_meta(src)?;
    apply_meta(dst, &meta)
}

/// Apply only the permission bits of `meta` to `path`.
fn apply_permissions(path: &Path, meta: &PreservedMeta) -> io::Result<()> {
    // Read-only flag, on every platform.
    let mut perms = fs::metadata(path)?.permissions();
    perms.set_readonly(meta.readonly);

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        // Preserve the full mode bits (type bits are masked off by set_mode).
        perms.set_mode(meta.mode);
    }

    fs::set_permissions(path, perms)
}

/// Open a handle suitable for `set_times`.
///
/// `File::set_times` maps to `futimens` on Unix, which needs ownership but not
/// write access — so a read-only open is a valid fallback. On Windows setting
/// times needs write access, so we try write first. Directories cannot be
/// opened for writing (`EISDIR`), so they fall through to the read-only open.
fn open_for_times(path: &Path) -> io::Result<File> {
    File::options()
        .write(true)
        .open(path)
        .or_else(|_| File::options().read(true).open(path))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::OpenOptions;
    use std::io::Write;
    use tempfile::tempdir;

    // FAT-ish tolerance for comparing timestamps read back from disk.
    fn assert_mtime_close(got: SystemTime, want: SystemTime) {
        let delta = got
            .duration_since(want)
            .or_else(|_| want.duration_since(got))
            .unwrap();
        assert!(
            delta < std::time::Duration::from_secs(2),
            "mtime drifted by {:?}",
            delta
        );
    }

    #[test]
    fn set_mtime_roundtrips() {
        let dir = tempdir().unwrap();
        let f = dir.path().join("a.txt");
        fs::write(&f, b"hi").unwrap();

        let want = SystemTime::now() - std::time::Duration::from_secs(3600);
        set_mtime(&f, want).unwrap();

        let got = fs::metadata(&f).unwrap().modified().unwrap();
        assert_mtime_close(got, want);
    }

    #[test]
    fn set_mtime_works_on_directory() {
        let dir = tempdir().unwrap();
        let sub = dir.path().join("sub");
        fs::create_dir(&sub).unwrap();

        let want = SystemTime::now() - std::time::Duration::from_secs(7200);
        set_mtime(&sub, want).unwrap();

        let got = fs::metadata(&sub).unwrap().modified().unwrap();
        assert_mtime_close(got, want);
    }

    #[test]
    fn copy_metadata_preserves_mtime() {
        let dir = tempdir().unwrap();
        let src = dir.path().join("src.txt");
        let dst = dir.path().join("dst.txt");
        fs::write(&src, b"payload").unwrap();
        fs::write(&dst, b"payload").unwrap();

        let want = SystemTime::now() - std::time::Duration::from_secs(500);
        set_mtime(&src, want).unwrap();

        copy_metadata(&src, &dst).unwrap();

        let got = fs::metadata(&dst).unwrap().modified().unwrap();
        assert_mtime_close(got, want);
    }

    #[cfg(unix)]
    #[test]
    fn copy_metadata_preserves_mode_bits() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempdir().unwrap();
        let src = dir.path().join("exec.sh");
        let dst = dir.path().join("dst.sh");
        fs::write(&src, b"#!/bin/sh\n").unwrap();
        fs::write(&dst, b"#!/bin/sh\n").unwrap();

        fs::set_permissions(&src, fs::Permissions::from_mode(0o755)).unwrap();
        copy_metadata(&src, &dst).unwrap();

        let mode = fs::metadata(&dst).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o755);
    }

    #[cfg(unix)]
    #[test]
    fn readonly_flag_is_preserved() {
        let dir = tempdir().unwrap();
        let src = dir.path().join("ro.txt");
        let dst = dir.path().join("dst.txt");
        fs::write(&src, b"x").unwrap();
        fs::write(&dst, b"x").unwrap();

        let mut p = fs::metadata(&src).unwrap().permissions();
        p.set_readonly(true);
        fs::set_permissions(&src, p).unwrap();

        copy_metadata(&src, &dst).unwrap();
        assert!(fs::metadata(&dst).unwrap().permissions().readonly());
    }

    #[test]
    fn apply_meta_handles_open_file() {
        // Setting metadata on a file that is still open for writing must not
        // panic or fail catastrophically.
        let dir = tempdir().unwrap();
        let f = dir.path().join("open.txt");
        let mut h = OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .open(&f)
            .unwrap();
        h.write_all(b"data").unwrap();

        let want = SystemTime::now() - std::time::Duration::from_secs(60);
        let meta = PreservedMeta {
            mtime: Some(want),
            atime: None,
            readonly: false,
            #[cfg(unix)]
            mode: 0o644,
        };
        apply_meta(&f, &meta).unwrap();

        let got = fs::metadata(&f).unwrap().modified().unwrap();
        assert_mtime_close(got, want);
    }
}
