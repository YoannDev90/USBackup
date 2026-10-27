//! Change detection: build a sync plan by diffing two [`TreeIndex`] values.
//!
//! Detection is O(1) per file and reads **no file contents**. A file is
//! considered changed when its size differs, or when its mtime differs beyond
//! a small tolerance. The tolerance exists because FAT/exFAT stores mtime with
//! 2-second resolution, so a copy can round a timestamp and produce a spurious
//! difference on every run.
//!
//! Symlinks are compared by their *target string*, never by following them.

use crate::sync::tree::{EntryMeta, TreeIndex};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

/// mtime tolerance. FAT/exFAT has 2s resolution; add a little slack for clock
/// rounding during the copy itself.
pub const MTIME_TOLERANCE: Duration = Duration::from_secs(2);

/// What to do for a single entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// Create the destination directory.
    CreateDir,
    /// Recreate a symlink verbatim (never dereference it).
    Symlink,
    /// Copy the file from the source.
    Copy,
    /// Nothing to do: already up to date.
    Skip,
    /// Delete an orphan present only in the destination (prune mode).
    Remove,
}

/// One planned operation.
#[derive(Debug, Clone)]
pub struct PlanItem {
    pub rel_path: PathBuf,
    pub meta: EntryMeta,
    pub action: Action,
}

/// How mtimes compare, applying the FAT tolerance.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TimeCmp {
    /// Within tolerance — treat as unchanged.
    Close,
    /// Beyond tolerance — treat as changed.
    Different,
}

/// Compare two mtimes with the FAT/exFAT tolerance applied.
pub fn compare_mtime(a: SystemTime, b: SystemTime) -> TimeCmp {
    let delta = a.duration_since(b).or_else(|_| b.duration_since(a));
    match delta {
        Ok(d) if d <= MTIME_TOLERANCE => TimeCmp::Close,
        _ => TimeCmp::Different,
    }
}

/// The desired outcome for a single source entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// Present and identical — skip.
    Unchanged,
    /// Needs creating or overwriting at the destination.
    Changed,
}

/// Decide whether a single source entry matches its destination counterpart.
///
/// Pass `dst = None` when the entry does not exist at the destination.
pub fn classify(src: &EntryMeta, dst: Option<&EntryMeta>) -> Verdict {
    let dst = match dst {
        Some(d) => d,
        // Absent at the destination and not a symlink -> must be created.
        // (Symlink creation is handled by the caller via `needs_symlink`.)
        None => return Verdict::Changed,
    };

    // Type mismatch (file <-> dir <-> symlink) always means "changed": the
    // destination has the wrong kind of thing at this path.
    if src.is_dir != dst.is_dir || src.is_symlink != dst.is_symlink {
        return Verdict::Changed;
    }

    // Symlinks: identity is the target string, not size or mtime.
    if src.is_symlink {
        return if src.link_target == dst.link_target {
            Verdict::Unchanged
        } else {
            Verdict::Changed
        };
    }

    // Directories carry no content; existence is enough.
    if src.is_dir {
        return Verdict::Unchanged;
    }

    // Regular files: size must match exactly, then mtime within tolerance.
    if src.len != dst.len {
        return Verdict::Changed;
    }

    match compare_mtime(src.mtime, dst.mtime) {
        TimeCmp::Close => Verdict::Unchanged,
        TimeCmp::Different => Verdict::Changed,
    }
}

/// Plan a sync from `src` into `dst`.
///
/// * `src` — the source tree index.
/// * `dst` — the destination tree index (empty for a first copy).
/// * `prune` — when true, destination-only entries become [`Action::Remove`].
///
/// Symlinks in the source are always planned as [`Action::Symlink`]; they are
/// never dereferenced. Directories produce [`Action::CreateDir`].
pub fn compare(src: &TreeIndex, dst: &TreeIndex, prune: bool) -> Vec<PlanItem> {
    let mut plan = Vec::new();

    for meta in src.iter() {
        let counterpart = dst.get(&meta.rel_path);

        let action = if meta.is_symlink {
            // A symlink is (re)created when absent or when the target changed.
            match counterpart {
                Some(d) if d.link_target == meta.link_target && d.is_symlink => Action::Skip,
                _ => Action::Symlink,
            }
        } else if meta.is_dir {
            match counterpart {
                Some(d) if d.is_dir => Action::Skip,
                _ => Action::CreateDir,
            }
        } else {
            match classify(meta, counterpart) {
                Verdict::Unchanged => Action::Skip,
                Verdict::Changed => Action::Copy,
            }
        };

        plan.push(PlanItem {
            rel_path: meta.rel_path.clone(),
            meta: meta.clone(),
            action,
        });
    }

    if prune {
        for meta in dst.iter() {
            if !src.contains(&meta.rel_path) {
                plan.push(PlanItem {
                    rel_path: meta.rel_path.clone(),
                    meta: meta.clone(),
                    action: Action::Remove,
                });
            }
        }
    }

    plan
}

/// Convenience: total bytes that will actually be copied (excludes skip/link).
pub fn bytes_to_copy(plan: &[PlanItem]) -> u64 {
    plan.iter()
        .filter(|p| p.action == Action::Copy)
        .map(|p| p.meta.len)
        .sum()
}

/// Rel path of a plan item, for logging.
pub fn rel_path_str(item: &PlanItem) -> String {
    item.rel_path.to_string_lossy().into_owned()
}

/// True when `path` is a direct child of `dir` (used to reason about pruning).
pub fn is_direct_child(dir: &Path, path: &Path) -> bool {
    match path.parent() {
        Some(parent) => parent == dir,
        None => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sync::tree::EntryMeta;
    use std::time::Duration;

    fn meta(path: &str, len: u64, secs_ago: u64) -> EntryMeta {
        EntryMeta {
            rel_path: PathBuf::from(path),
            len,
            mtime: SystemTime::now() - Duration::from_secs(secs_ago),
            is_dir: false,
            is_symlink: false,
            link_target: None,
        }
    }

    fn dir_meta(path: &str) -> EntryMeta {
        EntryMeta {
            rel_path: PathBuf::from(path),
            len: 0,
            mtime: SystemTime::now(),
            is_dir: true,
            is_symlink: false,
            link_target: None,
        }
    }

    fn link_meta(path: &str, target: &str) -> EntryMeta {
        EntryMeta {
            rel_path: PathBuf::from(path),
            len: target.len() as u64,
            mtime: SystemTime::now(),
            is_dir: false,
            is_symlink: true,
            link_target: Some(PathBuf::from(target)),
        }
    }

    #[test]
    fn mtime_within_tolerance_is_close() {
        let a = SystemTime::now();
        let b = a - Duration::from_millis(1500);
        assert_eq!(compare_mtime(a, b), TimeCmp::Close);
    }

    #[test]
    fn mtime_beyond_tolerance_is_different() {
        let a = SystemTime::now();
        let b = a - Duration::from_secs(10);
        assert_eq!(compare_mtime(a, b), TimeCmp::Different);
    }

    #[test]
    fn classify_absent_destination_is_changed() {
        let s = meta("a.txt", 10, 5);
        assert_eq!(classify(&s, None), Verdict::Changed);
    }

    #[test]
    fn classify_same_size_and_close_mtime_is_unchanged() {
        let s = meta("a.txt", 10, 5);
        let d = meta("a.txt", 10, 6); // 1s apart, within tolerance
        assert_eq!(classify(&s, Some(&d)), Verdict::Unchanged);
    }

    #[test]
    fn classify_size_mismatch_is_changed() {
        let s = meta("a.txt", 10, 5);
        let d = meta("a.txt", 99, 5);
        assert_eq!(classify(&s, Some(&d)), Verdict::Changed);
    }

    #[test]
    fn classify_fat_two_second_rounding_is_unchanged() {
        // The exact FAT failure mode: 2s-resolution rounding.
        let s = meta("a.txt", 10, 5);
        let d = meta("a.txt", 10, 7); // 2s apart, at the boundary
        assert_eq!(classify(&s, Some(&d)), Verdict::Unchanged);
    }

    #[test]
    fn classify_type_mismatch_is_changed() {
        let s = meta("a", 10, 5);
        let d = dir_meta("a");
        assert_eq!(classify(&s, Some(&d)), Verdict::Changed);
    }

    #[test]
    fn classify_symlink_same_target_is_unchanged() {
        let s = link_meta("l", "x");
        let d = link_meta("l", "x");
        assert_eq!(classify(&s, Some(&d)), Verdict::Unchanged);
    }

    #[test]
    fn classify_symlink_changed_target_is_changed() {
        let s = link_meta("l", "new-target");
        let d = link_meta("l", "old-target");
        assert_eq!(classify(&s, Some(&d)), Verdict::Changed);
    }

    #[test]
    fn compare_plans_copy_for_new_file() {
        let mut src = TreeIndex::new();
        src.insert(meta("a.txt", 10, 5));
        let dst = TreeIndex::new();

        let plan = compare(&src, &dst, false);
        assert_eq!(plan.len(), 1);
        assert_eq!(plan[0].action, Action::Copy);
    }

    #[test]
    fn compare_skips_unchanged_and_copies_changed() {
        let mut src = TreeIndex::new();
        src.insert(meta("same.txt", 10, 5));
        src.insert(meta("new.txt", 20, 5));

        let mut dst = TreeIndex::new();
        dst.insert(meta("same.txt", 10, 6)); // unchanged (within tolerance)

        let plan = compare(&src, &dst, false);
        let by = |p: &str| plan.iter().find(|i| i.rel_path == Path::new(p)).unwrap();
        assert_eq!(by("same.txt").action, Action::Skip);
        assert_eq!(by("new.txt").action, Action::Copy);
    }

    #[test]
    fn compare_prune_removes_orphans() {
        let src = TreeIndex::new();
        let mut dst = TreeIndex::new();
        dst.insert(meta("orphan.txt", 10, 5));

        let plan = compare(&src, &dst, true);
        assert_eq!(plan.len(), 1);
        assert_eq!(plan[0].action, Action::Remove);
    }

    #[test]
    fn compare_without_prune_keeps_orphans() {
        let src = TreeIndex::new();
        let mut dst = TreeIndex::new();
        dst.insert(meta("orphan.txt", 10, 5));

        let plan = compare(&src, &dst, false);
        assert!(plan.is_empty());
    }

    #[test]
    fn compare_creates_missing_dir_and_symlink() {
        let mut src = TreeIndex::new();
        src.insert(dir_meta("d"));
        src.insert(link_meta("d/l", "target"));
        let dst = TreeIndex::new();

        let plan = compare(&src, &dst, false);
        let by = |p: &str| plan.iter().find(|i| i.rel_path == Path::new(p)).unwrap();
        assert_eq!(by("d").action, Action::CreateDir);
        assert_eq!(by("d/l").action, Action::Symlink);
    }

    #[test]
    fn bytes_to_copy_counts_only_copies() {
        let mut src = TreeIndex::new();
        src.insert(meta("a.txt", 100, 5)); // copy
        src.insert(meta("b.txt", 200, 5)); // copy
        let mut dst = TreeIndex::new();
        dst.insert(meta("a.txt", 999, 999)); // changed -> copy

        let plan = compare(&src, &dst, false);
        // Both a.txt (size mismatch) and b.txt (absent) are copies.
        assert_eq!(bytes_to_copy(&plan), 300);
    }
}
