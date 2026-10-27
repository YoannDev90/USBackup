//! Progress reporting and final run statistics.
//!
//! The engine emits [`Progress`] through a callback while running and returns
//! a [`Stats`] summary when the run finishes. Both are cheap `Copy` structs so
//! they can be read from an `indicatif` bar without allocation.

use std::path::PathBuf;
use std::time::Duration;

/// A snapshot of run progress, emitted periodically and per completed file.
#[derive(Clone, Debug, Default)]
pub struct Progress {
    pub files_done: u64,
    pub files_total: u64,
    pub bytes_done: u64,
    pub bytes_total: u64,
    /// The file currently being copied, if any.
    pub current_file: Option<PathBuf>,
}

impl Progress {
    /// Completion in the range `0..=100`, guarding against division by zero.
    pub fn percent(&self) -> u64 {
        self.bytes_done
            .saturating_mul(100)
            .checked_div(self.bytes_total)
            .unwrap_or(100)
            .min(100)
    }
}

/// Outcome counters for a completed run.
#[derive(Clone, Debug, Default)]
pub struct Stats {
    pub files_copied: u64,
    /// Files represented by a hard link to a previous snapshot (cheap).
    pub files_linked: u64,
    /// Files already up to date and skipped.
    pub files_skipped: u64,
    pub files_removed: u64,
    pub dirs_created: u64,
    pub bytes_copied: u64,
    /// Symlinks recreated verbatim (never dereferenced).
    pub symlinks_preserved: u64,
    /// Symlinks that could not be recreated (privilege / FAT).
    pub symlinks_failed: u64,
    /// Files that failed to copy for any reason. Drives `PartialFailure`.
    pub errors: u64,
    pub elapsed: Duration,
}

impl Stats {
    /// True when at least one file failed.
    pub fn is_partial_failure(&self) -> bool {
        self.errors > 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percent_is_zero_safe() {
        let p = Progress::default();
        assert_eq!(p.percent(), 100);
    }

    #[test]
    fn percent_reports_progress() {
        let p = Progress {
            bytes_done: 50,
            bytes_total: 200,
            ..Default::default()
        };
        assert_eq!(p.percent(), 25);
    }

    #[test]
    fn percent_is_capped_at_100() {
        let p = Progress {
            bytes_done: 300,
            bytes_total: 200,
            ..Default::default()
        };
        assert_eq!(p.percent(), 100);
    }

    #[test]
    fn partial_failure_flag() {
        let mut s = Stats::default();
        assert!(!s.is_partial_failure());
        s.errors = 1;
        assert!(s.is_partial_failure());
    }
}
