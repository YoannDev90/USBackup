//! Unified error type for the sync engine.
//!
//! Philosophy: a single failed file must never abort the whole run. Callers
//! catch per-file errors, count them in [`crate::sync::progress::Stats`] and
//! keep going. `SyncError` therefore carries the offending path so a partial
//! failure is always attributable.

use std::io;
use std::path::PathBuf;

/// Errors produced by the sync engine.
#[derive(Debug, thiserror::Error)]
pub enum SyncError {
    /// An I/O error on a specific path.
    #[error("i/o error on {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },

    /// A symlink target could not be preserved (Windows privilege, FAT, ...).
    #[error("cannot preserve symlink {path}: {reason}")]
    SymlinkUnsupported { path: PathBuf, reason: String },

    /// The run finished but `count` files failed. Never fatal by itself.
    #[error("{count} file(s) failed to sync")]
    PartialFailure { count: usize },

    /// The run was cancelled (drop of the cancel token).
    #[error("sync cancelled")]
    Cancelled,
}

impl SyncError {
    /// Attach a path to a raw [`io::Error`].
    pub fn io(path: impl Into<PathBuf>, source: io::Error) -> Self {
        SyncError::Io {
            path: path.into(),
            source,
        }
    }

    /// The path involved in this error, when known.
    pub fn path(&self) -> Option<&std::path::Path> {
        match self {
            SyncError::Io { path, .. } => Some(path),
            SyncError::SymlinkUnsupported { path, .. } => Some(path),
            _ => None,
        }
    }
}

impl From<SyncError> for io::Error {
    fn from(e: SyncError) -> io::Error {
        match e {
            SyncError::Io { source, .. } => source,
            other => io::Error::other(other.to_string()),
        }
    }
}

/// Convenience alias used throughout the engine.
pub type Result<T> = std::result::Result<T, SyncError>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn io_error_carries_path() {
        let e = SyncError::io("/tmp/x", io::Error::from(io::ErrorKind::NotFound));
        assert_eq!(e.path().unwrap(), std::path::Path::new("/tmp/x"));
    }

    #[test]
    fn partial_failure_is_not_fatal_by_construction() {
        let e = SyncError::PartialFailure { count: 3 };
        assert!(e.path().is_none());
        assert_eq!(e.to_string(), "3 file(s) failed to sync");
    }
}
