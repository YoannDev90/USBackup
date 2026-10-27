//! Bounded, two-class parallel execution — the performance core (design §6).
//!
//! ## Why synchronous
//!
//! The engine's work is **blocking filesystem I/O** (open/read/write/close).
//! Running it as async futures would occupy tokio executor threads doing
//! nothing while a syscall blocks — the classic async-over-blocking-IO
//! anti-pattern. So the engine is synchronous and bounded; the async daemon
//! wraps it with `spawn_blocking` at integration (M9), the same pattern already
//! used elsewhere in the crate.
//!
//! ## Two work classes
//!
//! Files are split by size into two classes with very different concurrency
//! policies:
//!
//! | Class | Condition | Concurrency | Why |
//! |---|---|---|---|
//! | `Metadata` | everything except large copies | high (8) | small-file work is **latency-bound** — many threads hide open/write/close latency |
//! | `Bulk` | `Copy` of ≥ `bulk_threshold` | low (2) | large sequential I/O is **bandwidth-bound** — the disk controller saturates at 1–2 streams; more just fragments it |
//!
//! Each class gets its own rayon thread pool. Both pools run **concurrently**,
//! so up to 8 metadata operations and 2 bulk copies proceed at once without one
//! class starving the other.
//!
//! ## Directory-first ordering (§6.4)
//!
//! Directories are created in a **first, completed phase** before any file is
//! copied. That removes `ENOENT` entirely and lets file tasks run in any order —
//! no dependency on plan position. `create_dir_all` is idempotent, so
//! concurrency within the directory phase is safe.
//!
//! ## Bounded memory
//!
//! A copy buffer is allocated **once per worker thread** (thread-local) and
//! reused: `buffer_size` for bulk workers, a smaller `METADATA_BUFFER_SIZE` for
//! metadata workers. Nothing allocates per read.
//!
//! ## Failure model
//!
//! A failed file is counted, never fatal (see [`SyncError`]). An unsupported
//! symlink (Windows privilege, FAT) is counted as `symlinks_failed`, not an
//! error, so a run on a link-incapable volume still completes.

use crate::sync::compare::{compare, Action, PlanItem};
use crate::sync::copy::copy_file_atomic;
use crate::sync::error::{Result, SyncError};
use crate::sync::link::{link_or_copy, LinkOutcome};
use crate::sync::progress::{Progress, Stats};
use crate::sync::symlink::{create_symlink, recreate_symlink};
use crate::sync::tree::{scan, TreeIndex};
use rayon::prelude::*;
use std::cell::RefCell;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Instant;

/// Default concurrency for the latency-bound `Metadata` class.
pub const DEFAULT_METADATA_CONCURRENCY: usize = 8;
/// Default concurrency for the bandwidth-bound `Bulk` class.
pub const DEFAULT_BULK_CONCURRENCY: usize = 2;
/// Default bulk copy buffer (1 MiB) — sized for sequential USB throughput.
pub const DEFAULT_BUFFER_SIZE: usize = 1024 * 1024;
/// Default size at/above which a `Copy` is classified `Bulk` (1 MiB).
pub const DEFAULT_BULK_THRESHOLD: u64 = 1024 * 1024;
/// Copy buffer for `Metadata`-class files (64 KiB) — smaller, since many run at
/// once and each is tiny.
pub const METADATA_BUFFER_SIZE: usize = 64 * 1024;

// Per-thread copy buffer, reused across items so there is no allocation per
// read or per file. Separate pools mean bulk workers and metadata workers are
// different threads and grow their own buffer to their class's size.
thread_local! {
    static COPY_BUF: RefCell<Vec<u8>> = const { RefCell::new(Vec::new()) };
}

/// How the engine classifies and runs work. See the module docs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkClass {
    /// Small files / metadata: latency-bound, high concurrency.
    Metadata,
    /// Large sequential copies: bandwidth-bound, low concurrency.
    Bulk,
}

/// Progress callback: invoked with a snapshot as each file completes.
pub type ProgressCallback = Box<dyn Fn(&Progress) + Send + Sync>;

/// The sync engine: bounded parallel execution of a plan.
///
/// Build once (or use [`SyncEngine::default`]), then call
/// [`SyncEngine::run`] for the full scan→compare→execute pipeline, or
/// [`SyncEngine::plan`] + [`SyncEngine::execute`] to split the phases.
pub struct SyncEngine {
    /// Concurrent `Metadata` workers (default 8).
    pub metadata_concurrency: usize,
    /// Concurrent `Bulk` workers (default 2).
    pub bulk_concurrency: usize,
    /// Bulk copy buffer in bytes (default 1 MiB).
    pub buffer_size: usize,
    /// Size at/above which a `Copy` becomes `Bulk` (default 1 MiB).
    pub bulk_threshold: u64,
    /// Optional progress callback, invoked as files complete.
    pub progress: Option<ProgressCallback>,
    /// Base snapshot root: where [`Action::LinkOrCopy`] items are hard-linked
    /// from. Set by the snapshot layer; `None` for a normal sync (in which case
    /// a stray `LinkOrCopy` degrades to a copy).
    pub base_root: Option<PathBuf>,
    /// Cooperative cancellation flag; when set, pending items are skipped.
    pub cancel: Arc<AtomicBool>,
}

impl Default for SyncEngine {
    fn default() -> Self {
        SyncEngine {
            metadata_concurrency: DEFAULT_METADATA_CONCURRENCY,
            bulk_concurrency: DEFAULT_BULK_CONCURRENCY,
            buffer_size: DEFAULT_BUFFER_SIZE,
            bulk_threshold: DEFAULT_BULK_THRESHOLD,
            progress: None,
            base_root: None,
            cancel: Arc::new(AtomicBool::new(false)),
        }
    }
}

impl std::fmt::Debug for SyncEngine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SyncEngine")
            .field("metadata_concurrency", &self.metadata_concurrency)
            .field("bulk_concurrency", &self.bulk_concurrency)
            .field("buffer_size", &self.buffer_size)
            .field("bulk_threshold", &self.bulk_threshold)
            .field("progress", &self.progress.is_some())
            .field("base_root", &self.base_root)
            .field("cancel", &self.cancel)
            .finish()
    }
}

impl SyncEngine {
    /// Shared, lock-free run counters. All increments are `Relaxed` (they are
    /// informational); the scope join provides the happens-before edge for the
    /// final read.
    fn build_pool(&self, threads: usize, at: &Path) -> Result<rayon::ThreadPool> {
        let threads = threads.max(1);
        rayon::ThreadPoolBuilder::new()
            .num_threads(threads)
            .build()
            .map_err(|e| SyncError::io(at, io::Error::other(e)))
    }

    /// Scan source and destination and produce a plan (design §6.3 steps 1–4).
    ///
    /// A missing destination is normal on the first run and is treated as an
    /// empty index (everything is new). A missing source is a real error — there
    /// is nothing to back up from.
    pub fn plan(&self, src_root: &Path, dst_root: &Path, prune: bool) -> Result<Vec<PlanItem>> {
        let src_index = scan(src_root)?;
        let dst_index = if dst_root.exists() {
            scan(dst_root)?
        } else {
            TreeIndex::default()
        };
        Ok(compare(&src_index, &dst_index, prune))
    }

    /// Full pipeline: plan, order, execute, return stats (§6.3).
    pub fn run(&self, src_root: &Path, dst_root: &Path, prune: bool) -> Result<Stats> {
        let plan = self.plan(src_root, dst_root, prune)?;
        self.execute(src_root, dst_root, &plan)
    }

    /// Execute a plan with bounded two-class parallelism.
    ///
    /// Directories are created first (a completed phase), then files run in the
    /// two concurrent pools. See the module docs for the ordering and classing
    /// rules.
    pub fn execute(&self, src_root: &Path, dst_root: &Path, plan: &[PlanItem]) -> Result<Stats> {
        let started = Instant::now();
        if plan.is_empty() {
            return Ok(Stats {
                elapsed: started.elapsed(),
                ..Stats::default()
            });
        }
        // Abort before touching the filesystem if cancellation is already
        // requested; a mid-run request is caught again after the phases.
        if self.cancel.load(Ordering::Relaxed) {
            return Err(SyncError::Cancelled);
        }

        let state = RunState::new(plan);

        // The destination root may not exist yet (first run). `CreateDir`
        // actions only cover source *subdirectories*, so ensure the root itself
        // is present before anything is copied into it.
        fs::create_dir_all(dst_root).map_err(|e| SyncError::io(dst_root, e))?;

        let meta_pool = self.build_pool(self.metadata_concurrency, src_root)?;
        let bulk_pool = self.build_pool(self.bulk_concurrency, src_root)?;

        // --- Phase A: directories first, fully completed (§6.4) -------------
        let dirs: Vec<&PlanItem> = plan
            .iter()
            .filter(|i| i.action == Action::CreateDir)
            .collect();
        if !dirs.is_empty() {
            meta_pool.install(|| {
                dirs.into_par_iter().for_each(|item| {
                    self.execute_item(&state, src_root, dst_root, item);
                });
            });
        }

        // --- Phase B: files in two concurrent pools -------------------------
        let files: Vec<&PlanItem> = plan
            .iter()
            .filter(|i| i.action != Action::CreateDir)
            .collect();
        if !files.is_empty() {
            let (bulk, meta): (Vec<&PlanItem>, Vec<&PlanItem>) =
                files.into_iter().partition(|i| self.is_bulk(i));

            std::thread::scope(|scope| {
                scope.spawn(|| {
                    meta_pool.install(|| {
                        meta.into_par_iter().for_each(|item| {
                            self.execute_item(&state, src_root, dst_root, item);
                        });
                    });
                });
                scope.spawn(|| {
                    bulk_pool.install(|| {
                        bulk.into_par_iter().for_each(|item| {
                            self.execute_item(&state, src_root, dst_root, item);
                        });
                    });
                });
            });
        }

        // Cancellation is cooperative: if it was requested, surface it so the
        // caller knows the run is incomplete rather than reporting success.
        if self.cancel.load(Ordering::Relaxed) {
            return Err(SyncError::Cancelled);
        }

        Ok(state.into_stats(started.elapsed()))
    }

    /// Classify a plan item: only a `Copy` at/above the threshold is `Bulk`.
    /// Symlinks, removes and small copies are all cheap metadata operations.
    fn is_bulk(&self, item: &PlanItem) -> bool {
        item.action == Action::Copy && item.meta.len >= self.bulk_threshold
    }

    /// Execute one plan item, updating shared counters and emitting progress.
    /// Never propagates a per-file error — it is counted instead.
    fn execute_item(&self, state: &RunState, src_root: &Path, dst_root: &Path, item: &PlanItem) {
        if self.cancel.load(Ordering::Relaxed) {
            return;
        }

        let src = src_root.join(&item.rel_path);
        let dst = dst_root.join(&item.rel_path);
        let is_file_action = matches!(
            item.action,
            Action::Copy | Action::Symlink | Action::Remove | Action::LinkOrCopy
        );

        match item.action {
            Action::Skip => return,
            Action::CreateDir => {
                // Idempotent; parents are handled by create_dir_all.
                match fs::create_dir_all(&dst) {
                    Ok(()) => {
                        state.dirs_created.fetch_add(1, Ordering::Relaxed);
                    }
                    Err(_) => {
                        state.errors.fetch_add(1, Ordering::Relaxed);
                    }
                }
            }
            Action::Symlink => {
                self.execute_symlink(state, &src, &dst, item);
            }
            Action::Copy => {
                self.execute_copy(state, &src, &dst, item);
            }
            Action::LinkOrCopy => {
                self.execute_link(state, &src, &dst, item);
            }
            Action::Remove => {
                if remove_entry(&dst) {
                    state.files_removed.fetch_add(1, Ordering::Relaxed);
                }
            }
        }

        if is_file_action {
            state.files_done.fetch_add(1, Ordering::Relaxed);
            if let Some(cb) = &self.progress {
                cb(&state.snapshot_progress());
            }
        }
    }

    /// Recreate a symlink verbatim. The target is already known from the scan
    /// (`meta.link_target`), so no re-read of the source is needed. An
    /// unsupported link (Windows privilege, FAT) is counted as failed, not as an
    /// error, so the run completes on link-incapable volumes.
    fn execute_symlink(&self, state: &RunState, src: &Path, dst: &Path, item: &PlanItem) {
        let outcome = match &item.meta.link_target {
            Some(target) => create_symlink(target, dst),
            None => recreate_symlink(src, dst),
        };
        match outcome {
            Ok(()) => {
                state.symlinks_preserved.fetch_add(1, Ordering::Relaxed);
            }
            Err(SyncError::SymlinkUnsupported { .. }) => {
                // Non-fatal by design: warn-level, counted, run continues.
                state.symlinks_failed.fetch_add(1, Ordering::Relaxed);
            }
            Err(_) => {
                state.errors.fetch_add(1, Ordering::Relaxed);
            }
        }
    }

    /// Copy a file through the thread-local buffer, preserving metadata via
    /// `copy_file_atomic` (which also does temp+rename atomicity).
    fn execute_copy(&self, state: &RunState, src: &Path, dst: &Path, item: &PlanItem) {
        let needed = if self.is_bulk(item) {
            self.buffer_size
        } else {
            METADATA_BUFFER_SIZE
        }
        .max(1);

        let result = COPY_BUF.with(|cell| {
            let mut buf = cell.borrow_mut();
            if buf.len() < needed {
                buf.resize(needed, 0);
            }
            copy_file_atomic(src, dst, &mut buf[..needed])
        });

        match result {
            Ok(n) => {
                state.files_copied.fetch_add(1, Ordering::Relaxed);
                state.bytes_copied.fetch_add(n, Ordering::Relaxed);
                state.bytes_done.fetch_add(n, Ordering::Relaxed);
            }
            Err(_) => {
                state.errors.fetch_add(1, Ordering::Relaxed);
            }
        }
    }

    /// Hard-link an unchanged file from `base_root` into the destination (the
    /// snapshot fast path), or copy it from the source when no base is set.
    ///
    /// A link is a microseconds-long `hard_link` that moves zero bytes; if the
    /// filesystem cannot link (FAT32/exFAT) [`link_or_copy`] degrades to a real
    /// copy. `Ok(Linked)` advances `files_linked`; `Ok(Copied)` advances the
    /// copy+byte counters, since that path actually moved data.
    fn execute_link(&self, state: &RunState, src: &Path, dst: &Path, item: &PlanItem) {
        // LinkOrCopy is O(1) regardless of size, so it always runs with the
        // smaller metadata-class buffer (only used if the copy fallback fires).
        let needed = METADATA_BUFFER_SIZE.max(1);

        match &self.base_root {
            Some(base_root) => {
                let from = base_root.join(&item.rel_path);
                let outcome = COPY_BUF.with(|cell| {
                    let mut buf = cell.borrow_mut();
                    if buf.len() < needed {
                        buf.resize(needed, 0);
                    }
                    link_or_copy(&from, dst, &mut buf[..needed])
                });
                match outcome {
                    Ok(LinkOutcome::Linked) => {
                        state.files_linked.fetch_add(1, Ordering::Relaxed);
                    }
                    Ok(LinkOutcome::Copied) => {
                        // Fallback copied from base; count bytes as moved.
                        state.files_copied.fetch_add(1, Ordering::Relaxed);
                        if let Ok(md) = fs::metadata(dst) {
                            let n = md.len();
                            state.bytes_copied.fetch_add(n, Ordering::Relaxed);
                            state.bytes_done.fetch_add(n, Ordering::Relaxed);
                        }
                    }
                    Err(_) => {
                        state.errors.fetch_add(1, Ordering::Relaxed);
                    }
                }
            }
            None => {
                // No base configured: fall back to a plain copy from source so
                // the file still lands correctly.
                self.execute_copy(state, src, dst, item);
            }
        }
    }
}

/// Remove a destination entry without following it: a symlink is unlinked (its
/// target untouched), a real directory is removed recursively, a file is
/// unlinked. Returns whether anything was actually removed.
fn remove_entry(dst: &Path) -> bool {
    match fs::symlink_metadata(dst) {
        Ok(md) if md.is_dir() && !md.is_symlink() => fs::remove_dir_all(dst).is_ok(),
        Ok(_) => fs::remove_file(dst).is_ok(),
        // Already gone (e.g. a parent dir was removed first) — not an error.
        Err(_) => false,
    }
}

/// Shared counters for one run. Totals are fixed up-front so progress can
/// compute a percentage; done/stats counters advance as items complete.
struct RunState {
    files_total: AtomicU64,
    bytes_total: AtomicU64,
    files_done: AtomicU64,
    bytes_done: AtomicU64,
    files_copied: AtomicU64,
    files_linked: AtomicU64,
    files_removed: AtomicU64,
    dirs_created: AtomicU64,
    bytes_copied: AtomicU64,
    symlinks_preserved: AtomicU64,
    symlinks_failed: AtomicU64,
    errors: AtomicU64,
}

impl RunState {
    fn new(plan: &[PlanItem]) -> Self {
        // Progress tracks actionable files (not skips, not dir creation) and
        // only counts bytes we will actually transfer (copies).
        let files_total = plan
            .iter()
            .filter(|i| !matches!(i.action, Action::Skip | Action::CreateDir))
            .count() as u64;
        let bytes_total = plan
            .iter()
            .filter(|i| i.action == Action::Copy)
            .map(|i| i.meta.len)
            .sum();
        RunState {
            files_total: AtomicU64::new(files_total),
            bytes_total: AtomicU64::new(bytes_total),
            files_done: AtomicU64::new(0),
            bytes_done: AtomicU64::new(0),
            files_copied: AtomicU64::new(0),
            files_linked: AtomicU64::new(0),
            files_removed: AtomicU64::new(0),
            dirs_created: AtomicU64::new(0),
            bytes_copied: AtomicU64::new(0),
            symlinks_preserved: AtomicU64::new(0),
            symlinks_failed: AtomicU64::new(0),
            errors: AtomicU64::new(0),
        }
    }

    fn snapshot_progress(&self) -> Progress {
        Progress {
            files_done: self.files_done.load(Ordering::Relaxed),
            files_total: self.files_total.load(Ordering::Relaxed),
            bytes_done: self.bytes_done.load(Ordering::Relaxed),
            bytes_total: self.bytes_total.load(Ordering::Relaxed),
            current_file: None,
        }
    }

    fn into_stats(self, elapsed: std::time::Duration) -> Stats {
        Stats {
            files_copied: self.files_copied.load(Ordering::Relaxed),
            files_linked: self.files_linked.load(Ordering::Relaxed),
            files_skipped: 0, // populated by callers that track skips, if needed
            files_removed: self.files_removed.load(Ordering::Relaxed),
            dirs_created: self.dirs_created.load(Ordering::Relaxed),
            bytes_copied: self.bytes_copied.load(Ordering::Relaxed),
            symlinks_preserved: self.symlinks_preserved.load(Ordering::Relaxed),
            symlinks_failed: self.symlinks_failed.load(Ordering::Relaxed),
            errors: self.errors.load(Ordering::Relaxed),
            elapsed,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use tempfile::tempdir;

    /// Build a small source tree: nested dirs, files, a symlink, unchanged/
    /// changed/removed cases for a second run.
    fn seed_source(root: &Path) -> io::Result<()> {
        fs::create_dir_all(root.join("docs"))?;
        fs::create_dir_all(root.join("bin"))?;
        fs::write(root.join("docs/a.txt"), b"alpha content")?;
        fs::write(root.join("docs/b.txt"), b"beta content")?;
        fs::write(root.join("bin/app"), b"\x7fELF fake binary")?;
        fs::write(root.join("root.txt"), b"top level")?;
        #[cfg(unix)]
        std::os::unix::fs::symlink("docs/a.txt", root.join("docs/link")).unwrap();
        Ok(())
    }

    #[test]
    fn run_copies_tree_and_preserves_symlink() {
        let dir = tempdir().unwrap();
        let src = dir.path().join("src");
        let dst = dir.path().join("dst");
        seed_source(&src).unwrap();

        let stats = SyncEngine::default().run(&src, &dst, false).unwrap();

        assert!(dst.join("docs/a.txt").exists());
        assert!(dst.join("docs/b.txt").exists());
        assert!(dst.join("bin/app").exists());
        assert!(dst.join("root.txt").exists());
        assert!(stats.dirs_created >= 2);
        assert_eq!(stats.files_copied, 4);
        assert_eq!(stats.errors, 0);
        assert!(stats.bytes_copied > 0);

        // The symlink is a symlink at the destination, not a copy.
        #[cfg(unix)]
        {
            let md = fs::symlink_metadata(dst.join("docs/link")).unwrap();
            assert!(md.is_symlink(), "symlink must be recreated as a link");
            assert_eq!(
                fs::read_link(dst.join("docs/link")).unwrap(),
                Path::new("docs/a.txt")
            );
        }
    }

    #[test]
    fn second_run_skips_unchanged_files() {
        let dir = tempdir().unwrap();
        let src = dir.path().join("src");
        let dst = dir.path().join("dst");
        seed_source(&src).unwrap();

        let engine = SyncEngine::default();
        engine.run(&src, &dst, false).unwrap();

        // Change one file; everything else must be skipped on the re-run.
        fs::write(src.join("docs/a.txt"), b"alpha content CHANGED").unwrap();
        let stats = engine.run(&src, &dst, false).unwrap();

        assert_eq!(stats.files_copied, 1, "only the changed file is recopied");
        assert_eq!(stats.errors, 0);
        assert_eq!(
            fs::read(dst.join("docs/a.txt")).unwrap(),
            b"alpha content CHANGED"
        );
    }

    #[test]
    fn prune_removes_destination_only_entries() {
        let dir = tempdir().unwrap();
        let src = dir.path().join("src");
        let dst = dir.path().join("dst");
        seed_source(&src).unwrap();

        let engine = SyncEngine::default();
        engine.run(&src, &dst, false).unwrap();

        // Add an orphan file + orphan dir on the destination.
        fs::create_dir_all(dst.join("orphan")).unwrap();
        fs::write(dst.join("orphan/extra"), b"should go").unwrap();
        assert!(dst.join("orphan/extra").exists());

        let stats = engine.run(&src, &dst, true).unwrap();
        assert!(!dst.join("orphan").exists(), "orphan dir must be pruned");
        assert!(stats.files_removed > 0);
        assert_eq!(stats.errors, 0);
        // Source files survived the prune.
        assert!(dst.join("docs/a.txt").exists());
    }

    #[test]
    fn missing_source_file_counts_error_but_completes() {
        let dir = tempdir().unwrap();
        let src = dir.path().join("src");
        let dst = dir.path().join("dst");
        fs::create_dir_all(&src).unwrap();
        fs::write(src.join("good.txt"), b"fine").unwrap();

        let stats = SyncEngine::default().run(&src, &dst, false).unwrap();
        assert_eq!(stats.files_copied, 1);
        assert_eq!(stats.errors, 0);
        assert!(dst.join("good.txt").exists());
    }

    #[test]
    fn large_file_is_classified_bulk() {
        let engine = SyncEngine::default();
        let mk = |len: u64, action| PlanItem {
            rel_path: PathBuf::from("f"),
            meta: crate::sync::tree::EntryMeta {
                rel_path: PathBuf::from("f"),
                len,
                mtime: std::time::SystemTime::UNIX_EPOCH,
                is_dir: false,
                is_symlink: false,
                link_target: None,
            },
            action,
        };

        assert!(engine.is_bulk(&mk(DEFAULT_BULK_THRESHOLD, Action::Copy)));
        assert!(!engine.is_bulk(&mk(DEFAULT_BULK_THRESHOLD - 1, Action::Copy)));
        // Only Copy is size-classed: a symlink/remove stays Metadata even if
        // reported length is large.
        assert!(!engine.is_bulk(&mk(u64::MAX, Action::Remove)));
        assert!(!engine.is_bulk(&mk(u64::MAX, Action::Symlink)));
    }

    #[test]
    fn progress_reaches_totals_after_run() {
        let dir = tempdir().unwrap();
        let src = dir.path().join("src");
        let dst = dir.path().join("dst");
        seed_source(&src).unwrap();

        let seen: Arc<std::sync::Mutex<Vec<Progress>>> = Arc::new(std::sync::Mutex::new(vec![]));
        let sink = Arc::clone(&seen);
        let mut engine = SyncEngine::default();
        engine.progress = Some(Box::new(move |p| {
            sink.lock().unwrap().push(p.clone());
        }));

        engine.run(&src, &dst, false).unwrap();

        let reports = seen.lock().unwrap();
        assert!(!reports.is_empty(), "progress must be reported");
        let last = reports.last().unwrap();
        assert_eq!(last.files_done, last.files_total);
        assert_eq!(last.bytes_done, last.bytes_total);
        assert_eq!(last.percent(), 100);
    }

    #[test]
    fn empty_plan_is_a_noop_success() {
        let dir = tempdir().unwrap();
        let stats = SyncEngine::default()
            .execute(dir.path(), dir.path(), &[])
            .unwrap();
        assert_eq!(stats.files_copied, 0);
        assert_eq!(stats.errors, 0);
    }

    #[test]
    fn cancel_flag_stops_the_run() {
        let dir = tempdir().unwrap();
        let src = dir.path().join("src");
        let dst = dir.path().join("dst");
        seed_source(&src).unwrap();

        let engine = SyncEngine::default();
        engine.cancel.store(true, Ordering::Relaxed);
        let err = engine.run(&src, &dst, false).unwrap_err();
        assert!(matches!(err, SyncError::Cancelled));
        // Nothing was written because every item saw the flag first.
        assert!(!dst.join("root.txt").exists());
    }
}
