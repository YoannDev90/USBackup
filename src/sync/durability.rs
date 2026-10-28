//! Batched durability: defer every fsync to a single end-of-run pass
//! (design §9bis.1).
//!
//! ## Why this is the single biggest speedup on USB
//!
//! An fsync on a USB key costs ~1–20 ms, and 100 ms+ on cheap media (the FTL
//! has to flush its own cache). Doing one per file turns a 100k-file backup
//! into `100k × 5 ms = 500 s` spent purely on fsync. That is the number-one
//! killer of naive backup tools.
//!
//! The fix: write every file with buffered I/O and **zero** fsync per file —
//! the data lives in the kernel page cache. Only at the very end, after the
//! last byte is written, run **one** batched flush pass. Do not report
//! "backup complete, safe to unplug" until that pass finishes.
//!
//! `BatchedFinal` is ~100–1000× faster than `PerFile` with a single real risk
//! window: yanking the key *during* the transfer, not after. This is what
//! serious tools (rsync, restic) do — never fsync per file.
//!
//! ## What gets flushed
//!
//! Two things must be durable for a file to survive a yank:
//!
//! * the file's **data** — `fsync` on the file's fd.
//! * the file's **directory entry** — `fsync` on the parent directory's fd.
//!   (fsync on a dir fd flushes name→inode links, *not* file data, so both
//!   are needed.)
//!
//! So the journal records both files and their parent directories, and the
//! final pass syncs files first, then directories.
//!
//! ## Portability
//!
//! `File::sync_all` maps to `fsync` on Unix and `FlushFileBuffers` on Windows,
//! so the batched pass is portable with no platform code. macOS's stricter
//! `fcntl(F_FULLFSYNC)` is a later refinement behind `cfg`; `sync_all` (plain
//! fsync) is correct there, just less forceful about the drive cache.

use std::collections::HashSet;
use std::fs::File;
use std::io;
use std::path::{Path, PathBuf};

/// When (and whether) to fsync.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Durability {
    /// No fsync at all. Fastest. Risk: yanking the key mid-run loses data.
    None,
    /// Collect every write and fsync once at the very end. The recommended
    /// default — near `None`'s speed with `PerFile`'s end-state guarantee.
    #[default]
    BatchedFinal,
    /// fsync each file as it is written. Slow. Only for fragile media.
    PerFile,
}

/// Outcome of a flush pass.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct FlushReport {
    /// Files whose data was synced.
    pub files_synced: u64,
    /// Directories whose entries were synced.
    pub dirs_synced: u64,
    /// Files that failed to sync (non-fatal; reported, not thrown).
    pub errors: u64,
}

impl FlushReport {
    pub fn is_partial_failure(&self) -> bool {
        self.errors > 0
    }
}

/// A journal of everything written during a run, flushed in one batch at the
/// end under [`Durability::BatchedFinal`].
///
/// Under [`Durability::PerFile`] a recorded file is synced immediately and the
/// journal still tracks directories for the final pass. Under
/// [`Durability::None`] nothing is ever synced.
#[derive(Debug, Default)]
pub struct FlushJournal {
    mode: Durability,
    files: Vec<PathBuf>,
    dirs: Vec<PathBuf>,
    seen_files: HashSet<PathBuf>,
    seen_dirs: HashSet<PathBuf>,
}

impl FlushJournal {
    pub fn new(mode: Durability) -> Self {
        Self {
            mode,
            ..Default::default()
        }
    }

    pub fn mode(&self) -> Durability {
        self.mode
    }

    /// Number of distinct files queued for the final flush.
    pub fn pending_files(&self) -> usize {
        self.files.len()
    }

    /// Number of distinct directories queued for the final flush.
    pub fn pending_dirs(&self) -> usize {
        self.dirs.len()
    }

    /// Record a file that was just written.
    ///
    /// Under `PerFile` the file's data is fsynced right now; otherwise it is
    /// queued for the final pass. The parent directory is always queued so the
    /// directory entry becomes durable too.
    pub fn record_file(&mut self, path: &Path) -> io::Result<()> {
        // Always remember the parent directory (entry durability).
        if let Some(parent) = path.parent() {
            self.record_dir(parent);
        }

        match self.mode {
            Durability::None => Ok(()),
            Durability::PerFile => {
                sync_file_at(path)?;
                if self.seen_files.insert(path.to_path_buf()) {
                    self.files.push(path.to_path_buf());
                }
                Ok(())
            }
            Durability::BatchedFinal => {
                if self.seen_files.insert(path.to_path_buf()) {
                    self.files.push(path.to_path_buf());
                }
                Ok(())
            }
        }
    }

    /// Queue a directory for entry flush. No-op under `None`.
    pub fn record_dir(&mut self, dir: &Path) {
        if self.mode == Durability::None {
            return;
        }
        if self.seen_dirs.insert(dir.to_path_buf()) {
            self.dirs.push(dir.to_path_buf());
        }
    }

    /// Run the single batched flush pass: sync every queued file's data, then
    /// every queued directory's entries.
    ///
    /// Returns a [`FlushReport`]. Individual failures are counted, never
    /// thrown — one unreadable fd must not abort a finished backup.
    pub fn flush(&mut self) -> FlushReport {
        if self.mode == Durability::None {
            return FlushReport::default();
        }

        let mut report = FlushReport::default();

        // Data first: file contents must be durable before we make their
        // directory entries durable.
        for path in &self.files {
            // PerFile already synced these at record time; skip re-opening.
            if self.mode == Durability::PerFile {
                report.files_synced += 1;
                continue;
            }
            match sync_file_at(path) {
                Ok(()) => report.files_synced += 1,
                Err(_) => report.errors += 1,
            }
        }

        // Then directory entries.
        for dir in &self.dirs {
            match sync_dir_at(dir) {
                Ok(()) => report.dirs_synced += 1,
                Err(_) => report.errors += 1,
            }
        }

        // Journal is consumed; clear so a second flush is a no-op.
        self.files.clear();
        self.dirs.clear();
        self.seen_files.clear();
        self.seen_dirs.clear();

        report
    }
}

/// Open `path` and fsync its data. Portable (`fsync` / `FlushFileBuffers`).
///
/// `FlushFileBuffers` needs a handle with write access on Windows (fsync
/// works on any fd on Unix), so open write-first and fall back to a
/// read-only open: plain files stay flushable on both platforms, and
/// read-only destinations keep working on Unix.
pub fn sync_file_at(path: &Path) -> io::Result<()> {
    let file = File::options()
        .write(true)
        .open(path)
        .or_else(|_| File::open(path))?;
    file.sync_all()
}

/// Open the directory at `dir` and fsync its entries (name→inode links).
///
/// On Unix this opens the directory itself. On Windows opening a directory as a
/// file is not supported the same way, so this degrades to a no-op success —
/// directory-entry durability there relies on `FlushFileBuffers` of the volume,
/// which the OS does more eagerly anyway.
#[cfg(unix)]
fn sync_dir_at(dir: &Path) -> io::Result<()> {
    let file = File::open(dir)?;
    file.sync_all()
}

#[cfg(not(unix))]
fn sync_dir_at(_dir: &Path) -> io::Result<()> {
    Ok(())
}

/// Convenience: fsync a single file's data in place (used by `PerFile`).
pub fn sync_file(file: &File) -> io::Result<()> {
    file.sync_all()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn default_mode_is_batched_final() {
        assert_eq!(Durability::default(), Durability::BatchedFinal);
    }

    #[test]
    fn none_records_nothing_and_flushes_nothing() {
        let dir = tempdir().unwrap();
        let f = dir.path().join("a.txt");
        fs::write(&f, b"data").unwrap();

        let mut j = FlushJournal::new(Durability::None);
        j.record_file(&f).unwrap();
        assert_eq!(j.pending_files(), 0);
        assert_eq!(j.pending_dirs(), 0);

        let report = j.flush();
        assert_eq!(report.files_synced, 0);
        assert_eq!(report.dirs_synced, 0);
        assert!(!report.is_partial_failure());
    }

    #[test]
    fn batched_final_queues_file_and_parent_dir() {
        let dir = tempdir().unwrap();
        let f = dir.path().join("a.txt");
        fs::write(&f, b"data").unwrap();

        let mut j = FlushJournal::new(Durability::BatchedFinal);
        j.record_file(&f).unwrap();

        // One file, one parent dir, not yet flushed.
        assert_eq!(j.pending_files(), 1);
        assert_eq!(j.pending_dirs(), 1);

        let report = j.flush();
        assert_eq!(report.files_synced, 1);
        assert_eq!(report.dirs_synced, 1);
        assert!(!report.is_partial_failure());
    }

    #[test]
    fn batched_final_dedups_repeated_paths() {
        let dir = tempdir().unwrap();
        let f = dir.path().join("a.txt");
        fs::write(&f, b"data").unwrap();

        let mut j = FlushJournal::new(Durability::BatchedFinal);
        for _ in 0..5 {
            j.record_file(&f).unwrap();
        }
        assert_eq!(j.pending_files(), 1);
        assert_eq!(j.pending_dirs(), 1);
    }

    #[test]
    fn flush_clears_journal() {
        let dir = tempdir().unwrap();
        let f = dir.path().join("a.txt");
        fs::write(&f, b"data").unwrap();

        let mut j = FlushJournal::new(Durability::BatchedFinal);
        j.record_file(&f).unwrap();
        j.flush();
        // Second flush is a no-op.
        let report = j.flush();
        assert_eq!(report.files_synced, 0);
    }

    #[test]
    fn per_file_syncs_immediately_and_counts() {
        let dir = tempdir().unwrap();
        let f = dir.path().join("a.txt");
        fs::write(&f, b"data").unwrap();

        let mut j = FlushJournal::new(Durability::PerFile);
        j.record_file(&f).unwrap();
        // Still tracked so the final pass reports it, but data already synced.
        assert_eq!(j.pending_files(), 1);

        let report = j.flush();
        assert_eq!(report.files_synced, 1);
        assert_eq!(report.dirs_synced, 1);
        assert!(!report.is_partial_failure());
    }

    #[test]
    fn missing_file_counts_error_not_panic() {
        let dir = tempdir().unwrap();
        let ghost = dir.path().join("ghost.txt"); // never written

        let mut j = FlushJournal::new(Durability::BatchedFinal);
        j.record_file(&ghost).unwrap();

        let report = j.flush();
        assert_eq!(report.files_synced, 0);
        assert!(report.is_partial_failure());
    }

    #[test]
    fn many_files_all_flushed() {
        let dir = tempdir().unwrap();
        let mut j = FlushJournal::new(Durability::BatchedFinal);
        for i in 0..50 {
            let f = dir.path().join(format!("f{i}.txt"));
            fs::write(&f, b"x").unwrap();
            j.record_file(&f).unwrap();
        }
        assert_eq!(j.pending_files(), 50);

        let report = j.flush();
        assert_eq!(report.files_synced, 50);
        assert_eq!(report.dirs_synced, 1); // all share the temp parent
        assert!(!report.is_partial_failure());
    }
}
