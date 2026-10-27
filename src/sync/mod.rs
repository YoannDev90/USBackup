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

pub mod chunk;
pub mod compare;
pub mod compress;
pub mod copy;
pub mod delta;
pub mod digest;
pub mod durability;
pub mod error;
pub mod filter;
pub mod link;
pub mod meta;
pub mod pipeline;
pub mod progress;
pub mod sparse;
pub mod symlink;
pub mod tree;

// Re-exports for the public surface of the engine.
pub use chunk::{chunk_data, CdcBase, Chunking};
pub use compare::{compare, Action, PlanItem};
pub use compress::{compress, decompress, Compression};
pub use copy::{copy_file_atomic, is_temp_file};
pub use delta::{DeltaPolicy, DeltaScript};
pub use digest::{file_digest, FileDigest};
pub use durability::{Durability, FlushJournal};
pub use error::{Result, SyncError};
pub use link::{link_or_copy, LinkOutcome};
pub use meta::{copy_metadata, set_mtime, PreservedMeta};
pub use pipeline::copy_file_pipelined;
pub use progress::{Progress, Stats};
pub use sparse::{copy_sparse, is_sparse};
pub use symlink::{recreate_symlink, LinkTarget, SymlinkManifest};
pub use tree::{scan, EntryMeta, TreeIndex};
