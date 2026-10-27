//! Portable file-sync engine.
//!
//! A local disk→USB sync engine that replaces the external `rsync` dependency.
//! It is pure Rust (no C, no unsafe) so it cross-compiles to Linux, macOS,
//! Windows and Android/Termux. See `design_sync_engine.md` for the full design.
//!
//! Layering (lower layers never depend on upper ones):
//!
//! * `error`, `progress` — transverse types
//! * `tree` — recursive scan + indexing
//! * `compare` — change detection (added in M3)
//! * `copy`, `link`, `symlink`, `meta` — single-entry operations
//! * `engine` — planning + bounded parallel execution
//! * `snapshot` — hard-link incremental snapshots

// The engine is being built milestone-by-milestone; most of this public API is
// not yet called from `backup.rs` (integration lands in M9). Silence the
// dead_code / unused_imports noise at the module root instead of sprinkling
// allows everywhere.
#![allow(dead_code, unused_imports)]

pub mod error;
pub mod progress;
pub mod tree;

// Re-exports for the public surface of the engine.
pub use error::{Result, SyncError};
pub use progress::{Progress, Stats};
pub use tree::{scan, EntryMeta, TreeIndex};
