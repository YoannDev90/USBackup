//! Content hashing: whole-file digest for change detection, and per-block
//! tables that gate delta transfer (design §5.2, §8.2, §9).
//!
//! ## Why hashing is cheap here
//!
//! Every hash in this module is computed on the **source** (the local disk),
//! which is fast. We never re-read the USB key to decide whether a file
//! changed — the comparison digest comes from the persisted manifest. This is
//! the same principle as delta transfer: never read the key just to *decide*.
//!
//! ## Two granularities
//!
//! * [`FileDigest`] — one xxh3 over the whole file. Stored per file in the
//!   manifest; a mismatch means "changed, consider delta or copy".
//! * [`BlockTable`] — per-block `(offset, weak_rolling, xxh3_strong)`. Only
//!   built for files above `DELTA_THRESHOLD`, because small files never
//!   benefit from delta and would only bloat the manifest.
//!
//! xxh3 is used because it is extremely fast (near memcpy bandwidth) and has
//! excellent distribution, which matters for both change detection and the
//! strong-hash confirmation step of delta reconstruction.

use std::fs::File;
use std::hash::Hasher;
use std::io::{BufReader, Read};
use std::path::Path;
use std::time::SystemTime;
use xxhash_rust::xxh3::{xxh3_64, Xxh3};

/// Minimum file size (bytes) for which a [`BlockTable`] is worth building.
/// Smaller files are always copied whole — the table would cost more than the
/// copy it saves.
pub const DELTA_THRESHOLD: u64 = 64 * 1024 * 1024; // 64 MiB

/// Default block size for delta tables.
pub const DEFAULT_BLOCK_SIZE: u32 = 8 * 1024; // 8 KiB

/// How many bytes are read per iteration when hashing.
const HASH_BUF: usize = 1024 * 1024; // 1 MiB

/// Per-file digest persisted in the manifest for change detection (§5.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FileDigest {
    /// xxh3 of the whole file contents.
    pub xxh3: u64,
    pub len: u64,
    pub mtime: SystemTime,
}

impl FileDigest {
    /// Build a digest from a known hash and the file's stat metadata.
    pub fn new(xxh3: u64, len: u64, mtime: SystemTime) -> Self {
        Self { xxh3, len, mtime }
    }
}

/// One entry in a [`BlockTable`].
///
/// `weak_rolling` is a Rabin-Karp rolling hash used to find candidate block
/// boundaries cheaply; `xxh3_strong` confirms a match so a weak-hash collision
/// can never corrupt the reconstruction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BlockEntry {
    /// Byte offset of this block from the start of the file.
    pub offset: u64,
    /// Weak rolling hash (Rabin-Karp) over the block bytes.
    pub weak_rolling: u64,
    /// Strong xxh3 over the block bytes — the authoritative match check.
    pub xxh3_strong: u64,
}

/// Per-block checksum table for one large file (§8.2, §9).
///
/// Persisted in the manifest so delta reconstruction can rebuild a file by
/// matching blocks **without re-reading the old file from the USB key** — the
/// matched bytes are already known to exist on the source side, and only the
/// non-matching spans need to be read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlockTable {
    /// xxh3 of the whole file (same value as [`FileDigest::xxh3`]).
    pub xxh3_whole: u64,
    /// Block size used to segment the file.
    pub block_size: u32,
    /// One entry per block, in ascending offset order.
    pub blocks: Vec<BlockEntry>,
}

impl BlockTable {
    /// True when a file this small should get a delta table at all.
    pub fn worth_building(len: u64) -> bool {
        len >= DELTA_THRESHOLD
    }

    /// Total byte length implied by the table (offset of last block + its
    /// size is not stored, so this is the offset span the blocks cover).
    pub fn covered_len(&self) -> u64 {
        self.blocks.last().map(|b| b.offset).unwrap_or(0)
    }
}

/// xxh3 of an in-memory byte slice.
pub fn xxh3_bytes(data: &[u8]) -> u64 {
    xxh3_64(data)
}

/// Hash a file's whole contents, returning the xxh3 and the byte count.
///
/// This reads the file once with a 1 MiB buffer. It never touches the
/// destination — hashing is always a source-side (local disk) operation.
pub fn hash_file(path: &Path) -> std::io::Result<(u64, u64)> {
    let file = File::open(path)?;
    let mut reader = BufReader::with_capacity(HASH_BUF, file);
    let mut hasher = Xxh3::new();
    let mut buf = vec![0u8; HASH_BUF];
    let mut total: u64 = 0;

    loop {
        let n = reader.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
        total += n as u64;
    }

    Ok((hasher.finish(), total))
}

/// Compute the [`FileDigest`] for a file: whole-file hash plus stat metadata.
///
/// Returns `None` when the file cannot be read (the caller treats that as a
/// per-file error, never a fatal one).
pub fn file_digest(path: &Path) -> std::io::Result<FileDigest> {
    let (xxh3, len) = hash_file(path)?;
    let mtime = std::fs::metadata(path)?
        .modified()
        .unwrap_or(SystemTime::UNIX_EPOCH);
    Ok(FileDigest::new(xxh3, len, mtime))
}

/// Build a [`BlockTable`] for a file by reading it once and segmenting it
/// into `block_size` blocks.
///
/// The weak rolling hash is a Rabin-Karp polynomial hash so that delta
/// reconstruction (M5b) can slide a window over a modified file and detect
/// shifted content. The strong xxh3 guards every candidate against collision.
pub fn build_block_table(path: &Path, block_size: u32) -> std::io::Result<BlockTable> {
    let file = File::open(path)?;
    let mut reader = BufReader::with_capacity(HASH_BUF, file);
    let block_size = block_size.max(1) as usize;

    let mut whole = Xxh3::new();
    let mut blocks = Vec::new();
    let mut block = vec![0u8; block_size];
    let mut offset: u64 = 0;

    loop {
        // Fill one whole block (a short read at EOF is a valid final block).
        let mut filled = 0;
        while filled < block_size {
            let n = reader.read(&mut block[filled..])?;
            if n == 0 {
                break;
            }
            filled += n;
        }
        if filled == 0 {
            break;
        }

        let slice = &block[..filled];
        whole.update(slice);
        blocks.push(BlockEntry {
            offset,
            weak_rolling: rolling_hash(slice),
            xxh3_strong: xxh3_bytes(slice),
        });
        offset += filled as u64;

        if filled < block_size {
            break; // EOF
        }
    }

    Ok(BlockTable {
        xxh3_whole: whole.finish(),
        block_size: block_size as u32,
        blocks,
    })
}

/// Rabin-Karp rolling hash over `data`.
///
/// Uses a fixed base and modulus-free wrapping arithmetic (u64 overflow is the
/// modulus), which is the standard cheap form for content-defined chunking.
pub fn rolling_hash(data: &[u8]) -> u64 {
    const BASE: u64 = 0x100_0000_0000_0001; // a large prime-ish odd constant
    let mut h: u64 = 0;
    for &b in data {
        h = h.wrapping_mul(BASE).wrapping_add(b as u64);
    }
    h
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn xxh3_bytes_is_deterministic() {
        assert_eq!(xxh3_bytes(b"hello"), xxh3_bytes(b"hello"));
        assert_ne!(xxh3_bytes(b"hello"), xxh3_bytes(b"hellp"));
    }

    #[test]
    fn hash_file_matches_bytes_hash_and_length() {
        let dir = tempdir().unwrap();
        let f = dir.path().join("a.bin");
        let payload = vec![42u8; 3 * 1024 * 1024 + 17]; // crosses 1 MiB reads
        fs::write(&f, &payload).unwrap();

        let (h, len) = hash_file(&f).unwrap();
        assert_eq!(len, payload.len() as u64);
        assert_eq!(h, xxh3_bytes(&payload));
    }

    #[test]
    fn hash_file_empty() {
        let dir = tempdir().unwrap();
        let f = dir.path().join("empty");
        fs::write(&f, b"").unwrap();
        let (h, len) = hash_file(&f).unwrap();
        assert_eq!(len, 0);
        assert_eq!(h, xxh3_bytes(b""));
    }

    #[test]
    fn file_digest_captures_len_and_content() {
        let dir = tempdir().unwrap();
        let f = dir.path().join("a");
        fs::write(&f, b"payload").unwrap();

        let d = file_digest(&f).unwrap();
        assert_eq!(d.len, 7);
        assert_eq!(d.xxh3, xxh3_bytes(b"payload"));
    }

    #[test]
    fn block_table_segments_evenly() {
        let dir = tempdir().unwrap();
        let f = dir.path().join("big");
        // 4 blocks of 1000 bytes exactly.
        let payload = vec![9u8; 4000];
        fs::write(&f, &payload).unwrap();

        let t = build_block_table(&f, 1000).unwrap();
        assert_eq!(t.block_size, 1000);
        assert_eq!(t.blocks.len(), 4);
        assert_eq!(t.blocks[0].offset, 0);
        assert_eq!(t.blocks[1].offset, 1000);
        assert_eq!(t.blocks[3].offset, 3000);
        // Whole hash equals the whole-file hash.
        assert_eq!(t.xxh3_whole, xxh3_bytes(&payload));
    }

    #[test]
    fn block_table_handles_short_final_block() {
        let dir = tempdir().unwrap();
        let f = dir.path().join("ragged");
        let payload = vec![1u8; 2500]; // 2 full 1000 + one 500
        fs::write(&f, &payload).unwrap();

        let t = build_block_table(&f, 1000).unwrap();
        assert_eq!(t.blocks.len(), 3);
        assert_eq!(t.blocks[2].offset, 2000);
        assert_eq!(t.xxh3_whole, xxh3_bytes(&payload));
    }

    #[test]
    fn block_table_empty_file() {
        let dir = tempdir().unwrap();
        let f = dir.path().join("empty");
        fs::write(&f, b"").unwrap();

        let t = build_block_table(&f, 1024).unwrap();
        assert!(t.blocks.is_empty());
        assert_eq!(t.xxh3_whole, xxh3_bytes(b""));
    }

    #[test]
    fn block_strong_hash_matches_slice() {
        let dir = tempdir().unwrap();
        let f = dir.path().join("b");
        let payload = b"ABCDEFGHIJ".to_vec();
        fs::write(&f, &payload).unwrap();

        let t = build_block_table(&f, 4).unwrap();
        // Blocks: ABCD, EFGH, IJ
        assert_eq!(t.blocks.len(), 3);
        assert_eq!(t.blocks[0].xxh3_strong, xxh3_bytes(b"ABCD"));
        assert_eq!(t.blocks[2].xxh3_strong, xxh3_bytes(b"IJ"));
    }

    #[test]
    fn rolling_hash_detects_shifted_content() {
        // The property delta relies on: a shifted window yields a different
        // rolling hash, so a moved block can be re-found by sliding.
        let a = rolling_hash(b"abcdefgh");
        let b = rolling_hash(b"bcdefghi");
        assert_ne!(a, b);
    }

    #[test]
    fn worth_building_threshold() {
        assert!(!BlockTable::worth_building(DELTA_THRESHOLD - 1));
        assert!(BlockTable::worth_building(DELTA_THRESHOLD));
    }

    #[test]
    fn delta_threshold_is_64mib() {
        assert_eq!(DELTA_THRESHOLD, 64 * 1024 * 1024);
    }
}
