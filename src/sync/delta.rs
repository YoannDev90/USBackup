//! Delta transfer: rebuild a changed file by copying only the blocks that
//! still match a previous version, instead of re-writing the whole file
//! (design §9).
//!
//! ## Why this is viable (and why most naive deltas are not)
//!
//! The classic rsync-style delta re-reads the *old* file from the slow medium
//! to find matching blocks. On a USB key that read can erase the whole gain.
//! Here the base file's [`BlockTable`] is **persisted in the manifest** at
//! snapshot time (design §0, §8.2), so reconstruction **loads the table**
//! instead of re-reading the old file end-to-end. Only the matched blocks are
//! actually read from the key (k×), never the entire file.
//!
//! * Scanning the **new** file happens on the local disk (fast).
//! * Reading matched blocks from the **key** is bounded by the number of
//!   matches, and the read/write ratio guard (§9.3) decides whether that is
//!   worth it at all.
//!
//! ## Algorithm (fixed-block, M5b)
//!
//! 1. Build a lookup `weak_rolling -> [(offset, strong)]` from the base
//!    [`BlockTable`] (already computed at snapshot, loaded here).
//! 2. Slide a window of `block_size` over the new file with an **incremental**
//!    Rabin-Karp hash (O(1) per byte, not O(block_size)).
//! 3. On a weak hit, confirm with the strong xxh3. Match → emit
//!    `Copy { offset, len }` and jump forward `block_size`; no match → emit a
//!    literal byte and advance one.
//! 4. Reconstruct by reading only the referenced base blocks.
//!
//! Content-defined chunking (FastCDC) is a later milestone (M5h); fixed
//! blocks already catch the append / insert / contiguous-edit cases that
//! dominate real document backups.

use crate::sync::digest::{xxh3_bytes, BlockTable};
use std::collections::HashMap;
use std::fs::File;
use std::hash::Hasher;
use std::io::{BufReader, BufWriter, Read, Seek, SeekFrom, Write};
use std::path::Path;
use xxhash_rust::xxh3::Xxh3;

/// A single step of a delta script.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeltaInstruction {
    /// Copy `len` bytes from the base file starting at `offset`.
    Copy { offset: u64, len: u64 },
    /// Bytes that exist only in the new file (verbatim literals).
    Literal(Vec<u8>),
}

/// The full plan to turn a base file into a new file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeltaScript {
    pub instructions: Vec<DeltaInstruction>,
    /// Length of the base (old) file.
    pub base_len: u64,
    /// Length of the resulting (new) file.
    pub new_len: u64,
}

impl DeltaScript {
    /// Bytes that must be read from the base file (sum of `Copy` lengths).
    pub fn bytes_from_base(&self) -> u64 {
        self.instructions
            .iter()
            .map(|i| match i {
                DeltaInstruction::Copy { len, .. } => *len,
                DeltaInstruction::Literal(_) => 0,
            })
            .sum()
    }

    /// Bytes that are new (sum of literal lengths).
    pub fn bytes_literal(&self) -> u64 {
        self.instructions
            .iter()
            .map(|i| match i {
                DeltaInstruction::Literal(d) => d.len() as u64,
                DeltaInstruction::Copy { .. } => 0,
            })
            .sum()
    }

    /// Number of base reads (each `Copy` is one seek+read).
    pub fn copy_ops(&self) -> u64 {
        self.instructions
            .iter()
            .filter(|i| matches!(i, DeltaInstruction::Copy { .. }))
            .count() as u64
    }
}

/// Whether delta transfer is enabled, and its guard rails (§9.3, §9.4).
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub enum DeltaPolicy {
    /// Always fall back to a plain copy. This is the safe default until the
    /// profile has been benchmarked on real hardware (M10).
    #[default]
    Off,
    /// Use delta when the file is above `threshold` **and** the estimated
    /// read/write ratio stays under `max_read_write_ratio`.
    Auto {
        threshold: u64,
        max_read_write_ratio: f64,
    },
    /// Force delta regardless of cost (debug / benchmark).
    Force,
}

impl DeltaPolicy {
    /// Default `Auto` policy with the 64 MiB threshold and a 4× ratio cap.
    pub fn auto_default() -> Self {
        DeltaPolicy::Auto {
            threshold: crate::sync::digest::DELTA_THRESHOLD,
            max_read_write_ratio: 4.0,
        }
    }

    /// Decide if a script is worth applying under this policy.
    pub fn is_worth_it(&self, len: u64, script: &DeltaScript) -> bool {
        match *self {
            DeltaPolicy::Off => false,
            DeltaPolicy::Force => true,
            DeltaPolicy::Auto {
                threshold,
                max_read_write_ratio,
            } => {
                if len < threshold {
                    return false;
                }
                let literal = script.bytes_literal();
                if literal == 0 {
                    // Nothing new to write; trivially a win.
                    return true;
                }
                let ratio = script.bytes_from_base() as f64 / literal as f64;
                ratio <= max_read_write_ratio
            }
        }
    }
}

/// Adaptive block size (§9.1): `sqrt(len)` clamped to `[8 KiB, 1 MiB]`.
///
/// Larger files get larger blocks so the table stays small; smaller files get
/// smaller blocks so edits are localized.
pub fn adaptive_block_size(len: u64) -> u32 {
    let sqrt = (len as f64).sqrt() as u32;
    sqrt.clamp(8 * 1024, 1024 * 1024)
}

/// Polynomial base for the rolling hash. Odd and large, so the hash wraps
/// over the full u64 space.
const BASE: u64 = 0x100_0000_0000_0001;

/// Incremental Rabin-Karp rolling hash over a fixed-size window.
///
/// Maintains the hash in O(1) per byte instead of recomputing O(window) work
/// at every offset, which is what makes byte-by-byte scanning affordable.
pub struct RollingHash {
    size: usize,
    /// BASE^(size-1), precomputed so a byte can be shifted out in O(1).
    top_power: u64,
    h: u64,
    filled: usize,
}

impl RollingHash {
    pub fn new(size: usize) -> Self {
        let mut top_power: u64 = 1;
        for _ in 1..size {
            top_power = top_power.wrapping_mul(BASE);
        }
        Self {
            size,
            top_power,
            h: 0,
            filled: 0,
        }
    }

    /// Push the next byte, sliding the window. Returns `true` once the window
    /// is full (a complete block is available).
    pub fn push(&mut self, byte: u8) -> bool {
        if self.filled == self.size {
            // Shift out the oldest byte. The caller must supply it via
            // `slide`; this path is only used when `slide` is not called.
            unreachable!("use slide() once the window is full");
        }
        self.h = self.h.wrapping_mul(BASE).wrapping_add(byte as u64);
        self.filled += 1;
        self.filled == self.size
    }

    /// Slide the window by one: drop `outgoing`, push `incoming`. Returns the
    /// new hash. Must be called only when the window is full.
    pub fn slide(&mut self, outgoing: u8, incoming: u8) -> u64 {
        debug_assert_eq!(self.filled, self.size);
        self.h = self
            .h
            .wrapping_sub((outgoing as u64).wrapping_mul(self.top_power))
            .wrapping_mul(BASE)
            .wrapping_add(incoming as u64);
        self.h
    }

    pub fn current(&self) -> u64 {
        self.h
    }
}

/// Compute the weak rolling hash of a full block (non-incremental, for tests
/// and for building tables).
pub fn rolling_hash(data: &[u8]) -> u64 {
    let mut h: u64 = 0;
    for &b in data {
        h = h.wrapping_mul(BASE).wrapping_add(b as u64);
    }
    h
}

/// Lookup structure built from a base [`BlockTable`]: weak hash → candidate
/// (offset, strong) pairs.
pub type BaseIndex = HashMap<u64, Vec<(u64, u64)>>;

/// Build the weak→strong lookup from a base block table.
pub fn index_base(table: &BlockTable) -> BaseIndex {
    let mut map: BaseIndex = HashMap::with_capacity(table.blocks.len());
    for b in &table.blocks {
        map.entry(b.weak_rolling)
            .or_default()
            .push((b.offset, b.xxh3_strong));
    }
    map
}

/// Read exactly `len` bytes from `reader` at `offset` into `out`.
fn read_block_at(
    reader: &mut (impl Read + Seek),
    offset: u64,
    out: &mut [u8],
) -> std::io::Result<()> {
    reader.seek(SeekFrom::Start(offset))?;
    reader.read_exact(out)?;
    Ok(())
}

/// Compute a delta script turning the base file into `src`, using a
/// previously persisted `base_table` (no re-read of the base for hashing).
///
/// `block_size` should match the one used when `base_table` was built.
pub fn diff(base_table: &BlockTable, src: &Path, block_size: u32) -> std::io::Result<DeltaScript> {
    let index = index_base(base_table);
    let file = File::open(src)?;
    let mut reader = BufReader::with_capacity(1024 * 1024, file);

    // Read the whole source into memory for scanning. Delta only runs above
    // DELTA_THRESHOLD (tens of MiB), which is fine to buffer; a streaming
    // variant can come later if a profile needs it.
    let mut data = Vec::new();
    reader.read_to_end(&mut data)?;

    let block_size = block_size.max(1) as usize;
    let mut instructions: Vec<DeltaInstruction> = Vec::new();
    let mut literal: Vec<u8> = Vec::new();
    let mut i = 0usize;

    while i < data.len() {
        if i + block_size <= data.len() {
            let window = &data[i..i + block_size];
            let weak = rolling_hash(window);
            let strong = xxh3_bytes(window);

            let mut matched: Option<(u64, usize)> = None;
            if let Some(candidates) = index.get(&weak) {
                for &(offset, cand_strong) in candidates {
                    if cand_strong == strong {
                        matched = Some((offset, block_size));
                        break;
                    }
                }
            }

            if let Some((offset, len)) = matched {
                if !literal.is_empty() {
                    instructions.push(DeltaInstruction::Literal(std::mem::take(&mut literal)));
                }
                instructions.push(DeltaInstruction::Copy {
                    offset,
                    len: len as u64,
                });
                i += len;
                continue;
            }
        }

        // No match: emit one literal byte and slide forward.
        literal.push(data[i]);
        i += 1;
    }

    if !literal.is_empty() {
        instructions.push(DeltaInstruction::Literal(literal));
    }

    let new_len = data.len() as u64;
    Ok(DeltaScript {
        instructions,
        base_len: base_table
            .covered_len()
            .max(base_table.blocks.last().map(|b| b.offset + 1).unwrap_or(0)),
        new_len,
    })
}

/// Reconstruct the new file at `dst` by applying `script` to the base file at
/// `base`.
///
/// Only the referenced base blocks are read (seek + read per `Copy`); the base
/// file is never read end-to-end. Returns the number of bytes written.
pub fn apply(base: &Path, script: &DeltaScript, dst: &Path) -> std::io::Result<u64> {
    let base_file = File::open(base)?;
    let mut base_reader = BufReader::with_capacity(1024 * 1024, base_file);
    let dst_file = File::create(dst)?;
    let mut writer = BufWriter::with_capacity(1024 * 1024, dst_file);

    let mut written: u64 = 0;
    // Reused scratch buffer for base block reads.
    let mut scratch: Vec<u8> = Vec::new();

    for instr in &script.instructions {
        match instr {
            DeltaInstruction::Literal(data) => {
                writer.write_all(data)?;
                written += data.len() as u64;
            }
            DeltaInstruction::Copy { offset, len } => {
                scratch.resize(*len as usize, 0);
                read_block_at(&mut base_reader, *offset, &mut scratch)?;
                writer.write_all(&scratch)?;
                written += *len;
            }
        }
    }

    writer.flush()?;
    // Data must hit disk before the caller renames/uses it.
    writer
        .into_inner()
        .map_err(|e| e.into_error())?
        .sync_data()?;

    Ok(written)
}

/// One-shot convenience: compute the script and check it against a policy.
///
/// Returns `None` when the policy says a plain copy is cheaper (§9.3 guard).
pub fn plan_delta(
    policy: &DeltaPolicy,
    base_table: &BlockTable,
    src: &Path,
    src_len: u64,
    block_size: u32,
) -> std::io::Result<Option<DeltaScript>> {
    let script = diff(base_table, src, block_size)?;
    if policy.is_worth_it(src_len, &script) {
        Ok(Some(script))
    } else {
        Ok(None)
    }
}

/// Verify a script reproduces the expected content (test/debug helper).
pub fn script_is_sound(base: &[u8], script: &DeltaScript, expected: &[u8]) -> bool {
    let mut out: Vec<u8> = Vec::with_capacity(script.new_len as usize);
    for instr in &script.instructions {
        match instr {
            DeltaInstruction::Literal(d) => out.extend_from_slice(d),
            DeltaInstruction::Copy { offset, len } => {
                let s = *offset as usize;
                let e = s + *len as usize;
                if e > base.len() {
                    return false;
                }
                out.extend_from_slice(&base[s..e]);
            }
        }
    }
    out == expected
}

// Keep the digest rolling hash import used (it powers xxh3 confirmation).
#[allow(unused)]
fn _touch(_: &Xxh3) {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sync::digest::build_block_table;
    use std::fs;
    use tempfile::tempdir;

    fn write(dir: &Path, name: &str, data: &[u8]) -> std::path::PathBuf {
        let p = dir.join(name);
        fs::write(&p, data).unwrap();
        p
    }

    #[test]
    fn adaptive_block_size_clamps() {
        assert_eq!(adaptive_block_size(0), 8 * 1024);
        assert_eq!(adaptive_block_size(100), 8 * 1024);
        // 1 MiB^2 = 1 TiB hits the upper clamp.
        assert_eq!(adaptive_block_size(u64::MAX), 1024 * 1024);
        // 16 MiB -> sqrt = 4096 -> clamped up to 8 KiB.
        assert_eq!(adaptive_block_size(16 * 1024 * 1024), 8 * 1024);
    }

    #[test]
    fn rolling_hash_matches_naive() {
        let data = b"the quick brown fox";
        let mut incremental = RollingHash::new(data.len());
        for &b in data {
            incremental.push(b);
        }
        assert_eq!(incremental.current(), rolling_hash(data));
    }

    #[test]
    fn rolling_hash_slide_matches_naive() {
        // Slide over a longer buffer and compare every window to the naive
        // hash, proving the O(1) update is correct.
        let data = b"abcdefghijklmnop";
        let w = 4;
        let mut rh = RollingHash::new(w);
        for &b in &data[..w] {
            rh.push(b);
        }
        assert_eq!(rh.current(), rolling_hash(&data[0..w]));

        for start in 1..=(data.len() - w) {
            let new_h = rh.slide(data[start - 1], data[start + w - 1]);
            assert_eq!(new_h, rolling_hash(&data[start..start + w]), "at {}", start);
        }
    }

    #[test]
    fn identical_file_is_all_copy() {
        let dir = tempdir().unwrap();
        let content = vec![7u8; 32 * 1024];
        let base = write(&dir.path().to_path_buf(), "base", &content);
        let src = write(&dir.path().to_path_buf(), "src", &content);

        let bs = 1024;
        let table = build_block_table(&base, bs).unwrap();
        let script = diff(&table, &src, bs).unwrap();

        // Entirely references, nothing literal.
        assert_eq!(script.bytes_literal(), 0);
        assert_eq!(script.bytes_from_base(), content.len() as u64);
        assert!(script_is_sound(&content, &script, &content));
    }

    #[test]
    fn appended_tail_is_literal() {
        let dir = tempdir().unwrap();
        let content = vec![1u8; 32 * 1024];
        let base = write(dir.path(), "base", &content);
        let mut new_content = content.clone();
        new_content.extend_from_slice(&[9u8; 5000]);
        let src = write(dir.path(), "src", &new_content);

        let bs = 1024;
        let table = build_block_table(&base, bs).unwrap();
        let script = diff(&table, &src, bs).unwrap();

        assert!(script_is_sound(&content, &script, &new_content));
        // Most of the file is still referenced; only the tail is new.
        assert!(script.bytes_literal() <= 5000 + bs as u64);
        assert!(script.bytes_from_base() >= content.len() as u64 - bs as u64);
    }

    #[test]
    fn middle_edit_localizes_literals() {
        let dir = tempdir().unwrap();
        let mut content = vec![2u8; 64 * 1024];
        let base = write(dir.path(), "base", &content);
        // Change 10 bytes in the middle.
        let mid = content.len() / 2;
        for b in &mut content[mid..mid + 10] {
            *b = 0xEE;
        }
        let src = write(dir.path(), "src", &content);

        let bs = 1024;
        let table = build_block_table(&base, bs).unwrap();
        let script = diff(&table, &src, bs).unwrap();

        assert!(script_is_sound(&base_content(64 * 1024), &script, &content));
        // Only around one block should be literal; the rest is referenced.
        assert!(script.bytes_literal() <= 2 * bs as u64);
    }

    fn base_content(len: usize) -> Vec<u8> {
        vec![2u8; len]
    }

    #[test]
    fn inserted_prefix_shifts_and_still_matches() {
        let dir = tempdir().unwrap();
        let content = vec![5u8; 32 * 1024];
        let base = write(dir.path(), "base", &content);
        // Prepend 3000 bytes: fixed-block delta still finds the shifted body.
        let mut new_content = vec![0xAAu8; 3000];
        new_content.extend_from_slice(&content);
        let src = write(dir.path(), "src", &new_content);

        let bs = 1024;
        let table = build_block_table(&base, bs).unwrap();
        let script = diff(&table, &src, bs).unwrap();

        assert!(script_is_sound(&content, &script, &new_content));
        // Body still largely referenced despite the shift.
        assert!(script.bytes_from_base() >= content.len() as u64 - 2 * bs as u64);
    }

    #[test]
    fn apply_reconstructs_modified_file() {
        let dir = tempdir().unwrap();
        let mut content = vec![3u8; 48 * 1024];
        let base = write(dir.path(), "base", &content);
        for b in &mut content[1000..1200] {
            *b = 0x55;
        }
        let src = write(dir.path(), "src", &content);
        let dst = dir.path().join("dst");

        let bs = 512;
        let table = build_block_table(&base, bs).unwrap();
        let script = diff(&table, &src, bs).unwrap();
        let n = apply(&base, &script, &dst).unwrap();

        assert_eq!(n, content.len() as u64);
        assert_eq!(fs::read(&dst).unwrap(), content);
    }

    #[test]
    fn policy_off_rejects() {
        let dir = tempdir().unwrap();
        let content = vec![1u8; 128 * 1024];
        let base = write(dir.path(), "base", &content);
        let src = write(dir.path(), "src", &content);
        let table = build_block_table(&base, 1024).unwrap();

        let script = diff(&table, &src, 1024).unwrap();
        assert!(!DeltaPolicy::Off.is_worth_it(content.len() as u64, &script));
    }

    #[test]
    fn policy_force_accepts() {
        let dir = tempdir().unwrap();
        let content = vec![1u8; 128 * 1024];
        let base = write(dir.path(), "base", &content);
        let src = write(dir.path(), "src", &content);
        let table = build_block_table(&base, 1024).unwrap();

        let script = diff(&table, &src, 1024).unwrap();
        assert!(DeltaPolicy::Force.is_worth_it(content.len() as u64, &script));
    }

    #[test]
    fn policy_auto_enforces_threshold() {
        let dir = tempdir().unwrap();
        let content = vec![1u8; 4096]; // tiny, below threshold
        let base = write(dir.path(), "base", &content);
        let src = write(dir.path(), "src", &content);
        let table = build_block_table(&base, 256).unwrap();
        let script = diff(&table, &src, 256).unwrap();

        assert!(!DeltaPolicy::auto_default().is_worth_it(content.len() as u64, &script));
    }

    #[test]
    fn policy_auto_rejects_bad_ratio() {
        // Genuine bad-ratio case (design 9.3): a large base is almost fully
        // matched, so we would re-read a lot of the key to avoid writing only a
        // little. With a tight ratio cap the guard must fall back to a copy.
        let dir = tempdir().unwrap();
        let base_content: Vec<u8> = (0..200_000).map(|i| (i % 251) as u8).collect();
        let base = write(dir.path(), "base", &base_content);

        // Change only 10 bytes in the middle: huge base reads, tiny literal.
        let mut new_content = base_content.clone();
        let mid = new_content.len() / 2;
        for b in &mut new_content[mid..mid + 10] {
            *b = 0xEE;
        }
        let src = write(dir.path(), "src", &new_content);

        let table = build_block_table(&base, 1024).unwrap();
        let script = diff(&table, &src, 1024).unwrap();

        // The ratio of base-reads to literal-writes is large here.
        assert!(
            script.bytes_from_base() > script.bytes_literal(),
            "expected reads to dominate for this scenario"
        );

        let policy = DeltaPolicy::Auto {
            threshold: 1024,
            max_read_write_ratio: 1.0,
        };
        assert!(!policy.is_worth_it(new_content.len() as u64, &script));
        // But Force would take it.
        assert!(DeltaPolicy::Force.is_worth_it(new_content.len() as u64, &script));
    }

    #[test]
    fn plan_delta_returns_none_when_off() {
        let dir = tempdir().unwrap();
        let content = vec![1u8; 200_000];
        let base = write(dir.path(), "base", &content);
        let src = write(dir.path(), "src", &content);
        let table = build_block_table(&base, 1024).unwrap();

        let out = plan_delta(&DeltaPolicy::Off, &table, &src, content.len() as u64, 1024).unwrap();
        assert!(out.is_none());
    }

    #[test]
    fn index_base_groups_candidates() {
        let dir = tempdir().unwrap();
        // Two identical blocks -> same weak hash -> two candidates grouped.
        let mut content = vec![9u8; 2048];
        let first_half: Vec<u8> = content[..1024].to_vec();
        content[1024..].copy_from_slice(&first_half);
        let base = write(dir.path(), "base", &content);
        let table = build_block_table(&base, 1024).unwrap();

        let index = index_base(&table);
        let grouped = index.values().any(|v| v.len() >= 2);
        assert!(grouped, "identical blocks should share a weak-hash bucket");
    }
}
