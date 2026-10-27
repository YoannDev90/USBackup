//! Recursive tree scanning and in-memory indexing.
//!
//! The scan produces a [`TreeIndex`]: a map from relative path to [`EntryMeta`].
//! It is the input to change detection ([`crate::sync::compare`]) and to the
//! snapshot layer. Two rules matter here:
//!
//! 1. **Symlinks are never dereferenced.** We record them as first-class
//!    entries (`is_symlink`) so the copier can recreate them verbatim. Reading
//!    through a link would silently expand it into a full copy.
//! 2. **No file contents are read.** Metadata comes from a single `stat`, so a
//!    scan of 100k files costs 100k `stat` calls and nothing more.

use crate::sync::error::{Result, SyncError};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::SystemTime;
use walkdir::WalkDir;

/// Metadata for a single entry, captured from one `stat` during the scan.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EntryMeta {
    /// Path relative to the scanned root. Never starts with a separator.
    pub rel_path: PathBuf,
    /// Size in bytes. For a symlink this is the length of the target string,
    /// not the size of the referent.
    pub len: u64,
    /// Modification time, used for cheap change detection.
    pub mtime: SystemTime,
    pub is_dir: bool,
    pub is_symlink: bool,
}

/// An indexed tree: relative path → metadata.
#[derive(Debug, Default)]
pub struct TreeIndex {
    entries: HashMap<PathBuf, EntryMeta>,
}

impl TreeIndex {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn get(&self, rel_path: &Path) -> Option<&EntryMeta> {
        self.entries.get(rel_path)
    }

    pub fn contains(&self, rel_path: &Path) -> bool {
        self.entries.contains_key(rel_path)
    }

    pub fn iter(&self) -> impl Iterator<Item = &EntryMeta> {
        self.entries.values()
    }

    fn insert(&mut self, meta: EntryMeta) {
        self.entries.insert(meta.rel_path.clone(), meta);
    }
}

/// Scan `root` and build a [`TreeIndex`].
///
/// Symlinks are followed *into* the index as entries but their targets are not
/// traversed. Hidden files are included; filtering by exclude patterns happens
/// later in [`crate::sync::filter`].
pub fn scan(root: &Path) -> Result<TreeIndex> {
    let mut index = TreeIndex::new();

    // `follow_links(false)` is the default but we set it explicitly: it is the
    // load-bearing setting that keeps symlinks as entries rather than expanding
    // them into their targets.
    let walker = WalkDir::new(root).follow_links(false).into_iter();

    for entry in walker {
        let entry = match entry {
            Ok(e) => e,
            // A single unreadable entry must not abort the scan; surface it as
            // a hard error only if the root itself is inaccessible.
            Err(e) if e.depth() == 0 => {
                // The root itself is inaccessible — a real failure.
                let io = match e.into_io_error() {
                    Some(io) => io,
                    None => std::io::Error::other("unknown walkdir error"),
                };
                return Err(SyncError::io(root, io));
            }
            Err(_) => continue,
        };

        let rel_path = match relative_path(root, entry.path()) {
            Some(p) => p,
            None => continue,
        };

        let file_type = entry.file_type();
        let is_symlink = file_type.is_symlink();
        let is_dir = if is_symlink {
            false
        } else {
            file_type.is_dir()
        };

        // For symlinks `metadata()` (from walkdir with follow_links(false))
        // returns the *link's own* metadata, not the target's.
        let md = entry
            .metadata()
            .map_err(|e| SyncError::io(entry.path(), e.into()))?;
        let len = md.len();
        let mtime = md.modified().unwrap_or(SystemTime::UNIX_EPOCH);

        index.insert(EntryMeta {
            rel_path,
            len,
            mtime,
            is_dir,
            is_symlink,
        });
    }

    Ok(index)
}

/// Compute `path` relative to `root`, returning `None` when `path == root` or
/// when `path` escapes `root` (which should not happen with `WalkDir`).
fn relative_path(root: &Path, path: &Path) -> Option<PathBuf> {
    if path == root {
        return None;
    }
    path.strip_prefix(root).ok().map(|p| p.to_path_buf())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn scan_indexes_nested_files() {
        let dir = tempdir().unwrap();
        fs::create_dir_all(dir.path().join("a/b")).unwrap();
        fs::write(dir.path().join("a/b/c.txt"), b"hello").unwrap();

        let idx = scan(dir.path()).unwrap();

        let file = idx.get(Path::new("a/b/c.txt")).expect("file indexed");
        assert_eq!(file.len, 5);
        assert!(!file.is_dir);
        assert!(!file.is_symlink);

        // Directory entries are indexed too.
        assert!(idx.get(Path::new("a")).unwrap().is_dir);
        assert!(idx.get(Path::new("a/b")).unwrap().is_dir);
    }

    #[test]
    fn scan_records_symlink_without_dereferencing() {
        let dir = tempdir().unwrap();
        let target = dir.path().join("real.txt");
        fs::write(&target, b"0123456789").unwrap();
        let link = dir.path().join("link.txt");
        std::os::unix::fs::symlink("real.txt", &link).unwrap();

        let idx = scan(dir.path()).unwrap();
        let entry = idx.get(Path::new("link.txt")).expect("link indexed");

        assert!(entry.is_symlink);
        // len is the target string length ("real.txt" = 9), NOT 10.
        assert_eq!(entry.len, "real.txt".len() as u64);
        assert!(!entry.is_dir);
    }

    #[test]
    fn root_itself_is_not_indexed() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("x"), b"").unwrap();
        let idx = scan(dir.path()).unwrap();
        assert!(idx.get(Path::new("")).is_none());
    }

    #[test]
    fn empty_dir_yields_empty_index() {
        let dir = tempdir().unwrap();
        let idx = scan(dir.path()).unwrap();
        assert!(idx.is_empty());
    }
}
