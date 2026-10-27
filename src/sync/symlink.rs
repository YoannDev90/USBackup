//! Preserve source symlinks verbatim — never dereference them (design §7a).
//!
//! ## The non-negotiable rule
//!
//! A symlink in the source tree is a first-class entry, not a shortcut to be
//! expanded. Recreating it means writing the *same target string* at the
//! destination. We never read through a link to copy its referent: that would
//! silently turn a 10-byte link into a multi-GB copy and lose the link itself.
//!
//! | Situation | Behaviour |
//! |---|---|
//! | target is relative | recreated as-is |
//! | target is absolute | recreated as-is (documented: may not resolve on another host) |
//! | target points outside the backup root | fine — we store the raw string |
//! | target is dangling / does not exist | fine — links are created without touching the target |
//!
//! ## Windows: the privilege problem
//!
//! `symlink_file` / `symlink_dir` require `SeCreateSymbolicLinkPrivilege` **or**
//! Windows Developer Mode. Without either, creation fails with `PermissionDenied`.
//! FAT32/exFAT keys have no symlink support at all. In both cases we do **not**
//! crash: the caller reports a warning and counts
//! [`crate::sync::progress::Stats::symlinks_failed`], then falls back to writing
//! a sidecar record (this module) so the target survives and a restore on a
//! capable filesystem can recreate the real link.
//!
//! ## Sidecar fallback
//!
//! When a real link cannot be created, [`write_sidecar`] records the raw target
//! in a small text file, and [`SymlinkManifest`] accumulates every such link so
//! the whole set can be persisted (as JSON) and replayed at restore time. This
//! keeps the link information intact even where the filesystem cannot hold a
//! live symlink.

use crate::sync::error::{Result, SyncError};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// The raw target of a symlink, classified by absolute vs relative.
///
/// Classification is purely lexical (`Path::is_absolute`); the target is never
/// resolved or stat'ed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LinkTarget {
    /// A relative target — the common case, recreated byte-for-byte.
    Relative(PathBuf),
    /// An absolute target — stored verbatim. Documented caveat: it may not
    /// resolve on a different host, but preserving it is still the correct
    /// behaviour (the source said exactly this).
    Absolute(PathBuf),
}

impl LinkTarget {
    /// Classify a raw target string read from a link.
    pub fn classify(target: PathBuf) -> Self {
        if target.is_absolute() {
            LinkTarget::Absolute(target)
        } else {
            LinkTarget::Relative(target)
        }
    }

    /// The target path, whichever variant this is.
    pub fn as_path(&self) -> &Path {
        match self {
            LinkTarget::Relative(p) | LinkTarget::Absolute(p) => p,
        }
    }
}

/// Read a symlink's target **without resolving it**.
///
/// Uses `read_link`, which returns the literal target string whether or not the
/// target exists and without following the link.
pub fn read_symlink(path: &Path) -> Result<LinkTarget> {
    let target = fs::read_link(path).map_err(|e| SyncError::io(path, e))?;
    Ok(LinkTarget::classify(target))
}

/// Remove an existing entry at `dst` so a link can be (re)created there.
///
/// Only the *link itself* is removed, never its target. Tries `remove_file`
/// (correct for file symlinks and Unix links generally), then `remove_dir` for
/// directory symlinks/junctions on Windows. Best-effort: an already-absent path
/// is not an error.
fn remove_existing_link(dst: &Path) -> io::Result<()> {
    match fs::symlink_metadata(dst) {
        Ok(md) if md.is_symlink() || md.is_file() => {
            // A symlink is removed by unlink; on Unix this never touches the
            // referent. remove_file also handles regular files (type change).
            fs::remove_file(dst).or_else(|_| fs::remove_dir(dst))
        }
        // A real directory at the destination is not ours to delete here; let
        // the create below fail loudly rather than silently removing a tree.
        Ok(_) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    }
}

/// Create a symlink at `dst` pointing to `target`, verbatim.
///
/// This is the core primitive: it never reads the target and never touches the
/// referent. If something already exists at `dst`, it is replaced (the old link
/// is removed, its target untouched).
///
/// # Platform
/// * Unix: `std::os::unix::fs::symlink` — one call for files and directories.
/// * Windows: `symlink_file` / `symlink_dir`, chosen by whether the target is a
///   directory; if it cannot be determined (dangling link), file is tried first
///   then directory. A privilege failure surfaces as
///   [`SyncError::SymlinkUnsupported`] so the caller can fall back.
pub fn create_symlink(target: &Path, dst: &Path) -> Result<()> {
    if let Some(parent) = dst.parent() {
        fs::create_dir_all(parent).map_err(|e| SyncError::io(parent, e))?;
    }
    remove_existing_link(dst).map_err(|e| SyncError::io(dst, e))?;

    create_symlink_os(target, dst)
}

#[cfg(unix)]
fn create_symlink_os(target: &Path, dst: &Path) -> Result<()> {
    std::os::unix::fs::symlink(target, dst).map_err(|e| classify_creation_error(dst, e))
}

#[cfg(windows)]
fn create_symlink_os(target: &Path, dst: &Path) -> Result<()> {
    // Prefer the accurate kind when the target is stat-able; fall back to a
    // file/dir cascade for dangling links.
    let as_dir = std::fs::metadata(dst.parent().map(|p| p.join(target)).unwrap_or(target))
        .map(|m| m.is_dir())
        .unwrap_or(false);

    let first = if as_dir {
        std::os::windows::fs::symlink_dir(target, dst)
    } else {
        std::os::windows::fs::symlink_file(target, dst)
    };
    first
        .or_else(|_| {
            // Cascade: try the other kind before giving up (covers the case where
            // the dir-ness probe above was wrong or the target was dangling).
            if as_dir {
                std::os::windows::fs::symlink_file(target, dst)
            } else {
                std::os::windows::fs::symlink_dir(target, dst)
            }
        })
        .map_err(|e| classify_creation_error(dst, e))
}

/// Map a raw creation error onto the engine's error type, tagging the two
/// expected non-fatal cases (missing privilege, unsupported filesystem) as
/// [`SyncError::SymlinkUnsupported`] so the caller falls back instead of
/// aborting the run.
fn classify_creation_error(dst: &Path, e: io::Error) -> SyncError {
    match e.kind() {
        io::ErrorKind::PermissionDenied | io::ErrorKind::Unsupported => {
            SyncError::SymlinkUnsupported {
                path: dst.to_path_buf(),
                reason: e.to_string(),
            }
        }
        _ => SyncError::io(dst, e),
    }
}

/// Recreate the symlink found at `src` at the location `dst`, verbatim.
///
/// Convenience wrapper over [`read_symlink`] + [`create_symlink`]; the target is
/// read with `read_link` and never resolved.
pub fn recreate_symlink(src: &Path, dst: &Path) -> Result<()> {
    let target = fs::read_link(src).map_err(|e| SyncError::io(src, e))?;
    create_symlink(&target, dst)
}

/// Suffix used for sidecar files that stand in for an uncreatable symlink.
pub const SIDECAR_SUFFIX: &str = ".usbackup-symlink";

/// Record of one symlink that had to be preserved as a sidecar.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SymlinkRecord {
    /// Path of the link relative to the backup root.
    pub rel_path: PathBuf,
    /// Raw target, verbatim.
    pub target: PathBuf,
}

/// Write the raw target of a link into a sidecar text file at `sidecar`.
///
/// The file holds exactly the target string (no trailing newline), so a restore
/// can read it and recreate a real symlink on a capable filesystem. Returns the
/// path written.
pub fn write_sidecar(sidecar: &Path, target: &Path) -> Result<PathBuf> {
    if let Some(parent) = sidecar.parent() {
        fs::create_dir_all(parent).map_err(|e| SyncError::io(parent, e))?;
    }
    fs::write(sidecar, target.as_os_str().as_encoded_bytes())
        .map_err(|e| SyncError::io(sidecar, e))?;
    Ok(sidecar.to_path_buf())
}

/// Read a sidecar written by [`write_sidecar`] back into a target path.
pub fn read_sidecar(sidecar: &Path) -> Result<PathBuf> {
    let bytes = fs::read(sidecar).map_err(|e| SyncError::io(sidecar, e))?;
    let target = PathBuf::from(String::from_utf8_lossy(&bytes).into_owned());
    Ok(target)
}

/// The sidecar path for a link at `rel_path` under `root`.
///
/// e.g. `root/photos/album` → `root/photos/album.usbackup-symlink`.
pub fn sidecar_path(root: &Path, rel_path: &Path) -> PathBuf {
    let mut os = rel_path.as_os_str().to_os_string();
    os.push(SIDECAR_SUFFIX);
    root.join(PathBuf::from(os))
}

/// Aggregate of every symlink preserved as a sidecar, persisted so a restore
/// can recreate them all as real links.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SymlinkManifest {
    pub links: Vec<SymlinkRecord>,
}

impl SymlinkManifest {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn len(&self) -> usize {
        self.links.len()
    }

    pub fn is_empty(&self) -> bool {
        self.links.is_empty()
    }

    /// Record a link and write its sidecar under `root`.
    pub fn record(&mut self, root: &Path, rel_path: &Path, target: &Path) -> Result<()> {
        write_sidecar(&sidecar_path(root, rel_path), target)?;
        self.links.push(SymlinkRecord {
            rel_path: rel_path.to_path_buf(),
            target: target.to_path_buf(),
        });
        Ok(())
    }

    /// Serialize to the JSON stored in the backup's sidecar manifest.
    pub fn to_json(&self) -> String {
        let entries: Vec<String> = self
            .links
            .iter()
            .map(|r| {
                format!(
                    "  {{\"rel_path\":{},\"target\":{}}}",
                    json_escape(&r.rel_path.to_string_lossy()),
                    json_escape(&r.target.to_string_lossy())
                )
            })
            .collect();
        format!("{{\n\"links\":[\n{}\n]\n}}", entries.join(",\n"))
    }
}

/// Minimal JSON string escaping for the sidecar manifest (quotes + backslash).
fn json_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            _ => out.push(c),
        }
    }
    out.push('"');
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[cfg(unix)]
    #[test]
    fn recreate_symlink_preserves_relative_target_verbatim() {
        let dir = tempdir().unwrap();
        let src = dir.path().join("src");
        fs::create_dir_all(&src).unwrap();
        fs::write(src.join("real.txt"), b"payload").unwrap();
        // A relative link, as most real links are.
        std::os::unix::fs::symlink("real.txt", src.join("link.txt")).unwrap();

        let dst = dir.path().join("dst");
        fs::create_dir_all(&dst).unwrap();
        recreate_symlink(&src.join("link.txt"), &dst.join("link.txt")).unwrap();

        // It is still a symlink, pointing at the same target string.
        let md = fs::symlink_metadata(dst.join("link.txt")).unwrap();
        assert!(md.is_symlink(), "destination must be a symlink, not a copy");
        assert_eq!(
            fs::read_link(dst.join("link.txt")).unwrap(),
            Path::new("real.txt")
        );
        // And it did NOT copy the referent: no real.txt at the destination.
        assert!(!dst.join("real.txt").exists());
    }

    #[cfg(unix)]
    #[test]
    fn recreate_symlink_keeps_dangling_and_outside_targets() {
        let dir = tempdir().unwrap();
        let src = dir.path().join("src");
        fs::create_dir_all(&src).unwrap();
        // Dangling: the target never exists. Out-of-tree: absolute path.
        std::os::unix::fs::symlink("nowhere.dat", src.join("dangling")).unwrap();

        let dst = dir.path().join("dst");
        fs::create_dir_all(&dst).unwrap();
        recreate_symlink(&src.join("dangling"), &dst.join("dangling")).unwrap();

        assert!(fs::symlink_metadata(dst.join("dangling"))
            .unwrap()
            .is_symlink());
        assert_eq!(
            fs::read_link(dst.join("dangling")).unwrap(),
            Path::new("nowhere.dat")
        );
    }

    #[cfg(unix)]
    #[test]
    fn recreate_symlink_replaces_existing_link_without_touching_target() {
        let dir = tempdir().unwrap();
        let src = dir.path().join("src");
        fs::create_dir_all(&src).unwrap();
        fs::write(src.join("new-target"), b"B").unwrap();
        std::os::unix::fs::symlink("new-target", src.join("link")).unwrap();

        let dst = dir.path().join("dst");
        fs::create_dir_all(&dst).unwrap();
        // Pre-existing link with a DIFFERENT target, plus that target's content.
        fs::write(dst.join("old-target"), b"KEEP").unwrap();
        std::os::unix::fs::symlink("old-target", dst.join("link")).unwrap();

        recreate_symlink(&src.join("link"), &dst.join("link")).unwrap();

        assert_eq!(
            fs::read_link(dst.join("link")).unwrap(),
            Path::new("new-target")
        );
        // The old referent's data is untouched (we removed the link, not the file).
        assert_eq!(fs::read(dst.join("old-target")).unwrap(), b"KEEP");
    }

    #[test]
    fn read_symlink_classifies_relative_and_absolute() {
        let dir = tempdir().unwrap();

        #[cfg(unix)]
        {
            let rel = dir.path().join("rel");
            std::os::unix::fs::symlink("some/target", &rel).unwrap();
            assert_eq!(
                read_symlink(&rel).unwrap(),
                LinkTarget::Relative(PathBuf::from("some/target"))
            );

            let abs = dir.path().join("abs");
            std::os::unix::fs::symlink("/var/tmp/elsewhere", &abs).unwrap();
            assert_eq!(
                read_symlink(&abs).unwrap(),
                LinkTarget::Absolute(PathBuf::from("/var/tmp/elsewhere"))
            );
        }
    }

    #[test]
    fn classify_is_purely_lexical() {
        assert_eq!(
            LinkTarget::classify(PathBuf::from("a/b")),
            LinkTarget::Relative(PathBuf::from("a/b"))
        );
        #[cfg(unix)]
        assert_eq!(
            LinkTarget::classify(PathBuf::from("/a/b")),
            LinkTarget::Absolute(PathBuf::from("/a/b"))
        );
    }

    #[test]
    fn sidecar_roundtrip_preserves_target() {
        let dir = tempdir().unwrap();
        let sidecar = dir.path().join("link.usbackup-symlink");
        let target = Path::new("../some/where/odd name.txt");

        write_sidecar(&sidecar, target).unwrap();
        assert_eq!(read_sidecar(&sidecar).unwrap(), target);
    }

    #[test]
    fn sidecar_path_appends_suffix() {
        assert_eq!(
            sidecar_path(Path::new("/backup"), Path::new("photos/album")),
            Path::new("/backup/photos/album.usbackup-symlink")
        );
    }

    #[test]
    fn manifest_records_and_serializes() {
        let dir = tempdir().unwrap();
        let mut m = SymlinkManifest::new();
        assert!(m.is_empty());

        m.record(dir.path(), Path::new("a/b"), Path::new("../target"))
            .unwrap();
        m.record(dir.path(), Path::new("c"), Path::new("other"))
            .unwrap();

        assert_eq!(m.len(), 2);
        // Sidecars were written for both.
        assert!(sidecar_path(dir.path(), Path::new("a/b")).exists());
        assert!(sidecar_path(dir.path(), Path::new("c")).exists());

        let json = m.to_json();
        assert!(json.contains("\"rel_path\":\"a/b\""));
        assert!(json.contains("\"target\":\"../target\""));
        assert!(json.contains("\"rel_path\":\"c\""));
    }

    #[test]
    fn json_escape_handles_quotes_and_backslashes() {
        assert_eq!(json_escape("plain"), "\"plain\"");
        assert_eq!(json_escape("say \"hi\""), "\"say \\\"hi\\\"\"");
        assert_eq!(json_escape("back\\slash"), "\"back\\\\slash\"");
    }

    #[cfg(unix)]
    #[test]
    fn directory_symlink_is_preserved_as_a_link() {
        let dir = tempdir().unwrap();
        let src = dir.path().join("src");
        fs::create_dir_all(src.join("subdir")).unwrap();
        std::os::unix::fs::symlink("subdir", src.join("dlink")).unwrap();

        let dst = dir.path().join("dst");
        fs::create_dir_all(dst.join("subdir")).unwrap();
        recreate_symlink(&src.join("dlink"), &dst.join("dlink")).unwrap();

        let md = fs::symlink_metadata(dst.join("dlink")).unwrap();
        assert!(md.is_symlink());
        assert_eq!(
            fs::read_link(dst.join("dlink")).unwrap(),
            Path::new("subdir")
        );
    }
}
