//! Content-defined chunking (FastCDC) — a delta that survives insertions
//! (design §9bis.4).
//!
//! ## Why fixed blocks fail
//!
//! The fixed-block delta (§9) has a fatal flaw for append-only workloads: if
//! data is **inserted** in the middle (a log that grows, a database that
//! expands), every block after the insertion point shifts. None of them line up
//! with the base's boundaries any more, so the weak-hash lookups all miss and
//! the delta degenerates into a full copy — the exact case where delta matters
//! most.
//!
//! ```text
//! fixed blocks:  [A][B][C][D][E]  -> insert before B -> [A][B'][C'][D'][E']  (4 shift)
//! FastCDC:       [A|B|C|D|E]      -> insert          -> [A|B'|C|D|E]         (1 shift)
//! ```
//!
//! ## Content-defined boundaries
//!
//! FastCDC (Xia et al. 2016) picks each chunk's **end** from its **content**,
//! not from a fixed offset. A gear rolling hash is advanced per byte; a cut is
//! made where the hash matches a mask (`hash & mask == 0`). Because the cut
//! depends only on the bytes near it, an insertion perturbs just the chunk it
//! lands in; every later chunk re-synchronises on the same content and keeps its
//! old boundary. This is what borg / restic / rclone use.
//!
//! ## Normalization
//!
//! A single mask gives an exponential chunk-size distribution (lots of tiny
//! chunks, rare huge ones). FastCDC **normalizes** that with two masks:
//!
//! * from `min` to `avg`: a *sparser* mask (more bits set → lower cut chance),
//!   so chunks grow toward the target;
//! * from `avg` to `max`: a *denser* mask (fewer bits set → higher cut chance),
//!   so a cut is forced before `max`.
//!
//! Result: sizes cluster tightly around `avg` while `min`/`max` stay hard
//! bounds. Defaults (§9.1): min 8 KiB, avg 16 KiB, max 64 KiB.
//!
//! ## Cost
//!
//! One extra rolling-hash pass over the source. That is CPU on the local disk,
//! never USB bandwidth — the CPU is otherwise idle during the writes — so it is
//! effectively free. It is only worth enabling for files that grow by
//! insertion, hence it is opt-in per rule.

use crate::sync::delta::{DeltaInstruction, DeltaScript};
use crate::sync::digest::{rolling_hash, xxh3_bytes};
use std::collections::HashMap;
use std::fs;
use std::path::Path;
use std::sync::OnceLock;

/// How to cut a stream into chunks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Chunking {
    /// Every chunk is exactly this many bytes (except a short final one). Fast
    /// and fine for in-place edits, but shifts on insertion.
    Fixed(u32),
    /// Content-defined chunks bounded to `[min, max]` with a target average of
    /// `avg`. Robust to insertion.
    FastCdc { min: u32, avg: u32, max: u32 },
}

impl Chunking {
    /// Recommended FastCDC geometry (design §9.1).
    pub fn fastcdc_default() -> Self {
        Chunking::FastCdc {
            min: 8 * 1024,
            avg: 16 * 1024,
            max: 64 * 1024,
        }
    }

    /// `(min, avg, max)` sizes. For `Fixed`, all three equal the block size.
    pub fn sizes(&self) -> (u32, u32, u32) {
        match *self {
            Chunking::Fixed(bs) => (bs, bs, bs),
            Chunking::FastCdc { min, avg, max } => (min, avg, max),
        }
    }
}

/// One chunk of a file: where it lives and the hashes used to match it against
/// a previous version.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Chunk {
    /// Byte offset of this chunk in its file.
    pub offset: u64,
    /// Length of this chunk in bytes.
    pub len: u32,
    /// Weak rolling hash — the fast `HashMap` probe.
    pub weak: u64,
    /// Strong xxh3 — confirms a candidate so a weak collision can never corrupt
    /// output.
    pub strong: u64,
}

/// Lookup for a base file: weak hash → candidate `(offset, strong, len)` triples.
pub type CdcIndex = HashMap<u64, Vec<(u64, u64, u32)>>;

/// Deterministic gear table for the FastCDC boundary hash.
///
/// Must be identical on every run and for every file, or the base and the new
/// file would never agree on chunk boundaries. A fixed-seed xorshift keeps it
/// reproducible without a 256-line constant.
fn gear_table() -> &'static [u64; 256] {
    static GEAR: OnceLock<[u64; 256]> = OnceLock::new();
    GEAR.get_or_init(|| {
        let mut table = [0u64; 256];
        let mut state: u64 = 0x9E37_79B9_7F4A_7C15;
        for entry in table.iter_mut() {
            // xorshift64*
            state ^= state >> 12;
            state ^= state << 25;
            state ^= state >> 27;
            *entry = state.wrapping_mul(0x2545_F491_4F6C_DD1D);
        }
        table
    })
}

/// Bit positions used to build the normalized masks. Spread across the hash so
/// the cut decision is not dominated by adjacent (correlated) bits.
const MASK_POSITIONS: [u32; 22] = [
    1, 2, 4, 5, 7, 8, 9, 11, 12, 14, 15, 16, 18, 19, 21, 22, 23, 25, 26, 28, 30, 31,
];

/// Derive the two normalized masks for a target average of `2^avg_bits`.
///
/// Returns `(mask_s, mask_l)`:
/// * `mask_s` has `avg_bits + 1` bits (sparser cut) for the `[min, avg)` range;
/// * `mask_l` has `avg_bits` bits (denser cut) for the `[avg, max)` range.
fn make_masks(avg_bits: usize) -> (u64, u64) {
    let bits = avg_bits.clamp(1, MASK_POSITIONS.len() - 1);
    let mut mask_l = 0u64;
    let mut mask_s = 0u64;
    for &p in &MASK_POSITIONS[..bits] {
        mask_l |= 1u64 << p;
    }
    for &p in &MASK_POSITIONS[..bits + 1] {
        mask_s |= 1u64 << p;
    }
    (mask_s, mask_l)
}

/// `floor(log2(avg))`, clamped to the mask-position table.
fn avg_bits(avg: u32) -> usize {
    let bits = if avg <= 1 {
        1
    } else {
        (31 - avg.leading_zeros()) as usize
    };
    bits.clamp(1, MASK_POSITIONS.len() - 1)
}

/// Find the end offset of the next chunk starting at `start`.
///
/// Uses normalized FastCDC: skip-checking the first `min` bytes, a sparse mask
/// up to `avg`, a denser mask up to `max`, then a forced cut at `max`.
fn find_cut(
    data: &[u8],
    start: usize,
    min: usize,
    avg: usize,
    max: usize,
    mask_s: u64,
    mask_l: u64,
) -> usize {
    let gear = gear_table();
    let len = data.len();
    let mut h: u64 = 0;
    let mut i = start;

    // Hash [start, start+min) but never cut there (guarantees a floor size).
    let min_end = (start + min).min(len);
    while i < min_end {
        h = (h << 1).wrapping_add(gear[data[i] as usize]);
        i += 1;
    }
    if i >= len {
        return len;
    }

    // [min, avg): sparse mask — cuts are rare, chunks grow toward avg.
    let avg_end = (start + avg).min(len);
    while i < avg_end {
        h = (h << 1).wrapping_add(gear[data[i] as usize]);
        if h & mask_s == 0 {
            return i + 1;
        }
        i += 1;
    }
    if i >= len {
        return len;
    }

    // [avg, max): denser mask — cuts are likely, forcing a cut before max.
    let max_end = (start + max).min(len);
    while i < max_end {
        h = (h << 1).wrapping_add(gear[data[i] as usize]);
        if h & mask_l == 0 {
            return i + 1;
        }
        i += 1;
    }

    // Forced cut at max (or the end of data).
    max_end
}

/// Split `data` into chunks according to `chunking`.
///
/// The result covers `data` exactly: chunks are contiguous, non-overlapping, in
/// order, and their lengths sum to `data.len()`.
pub fn chunk_data(data: &[u8], chunking: Chunking) -> Vec<Chunk> {
    let (min, avg, max) = chunking.sizes();
    let (mask_s, mask_l) = match chunking {
        Chunking::FastCdc { avg, .. } => make_masks(avg_bits(avg)),
        Chunking::Fixed(_) => (0, 0),
    };

    let mut out = Vec::new();
    let mut pos = 0usize;
    while pos < data.len() {
        let cut = match chunking {
            Chunking::Fixed(bs) => (pos + bs as usize).min(data.len()),
            Chunking::FastCdc { .. } => find_cut(
                data,
                pos,
                min as usize,
                avg as usize,
                max as usize,
                mask_s,
                mask_l,
            ),
        };
        let slice = &data[pos..cut];
        out.push(Chunk {
            offset: pos as u64,
            len: (cut - pos) as u32,
            weak: rolling_hash(slice),
            strong: xxh3_bytes(slice),
        });
        pos = cut;
    }
    out
}

/// Read a file and split it into chunks.
pub fn chunk_file(path: &Path, chunking: Chunking) -> std::io::Result<Vec<Chunk>> {
    let data = fs::read(path)?;
    Ok(chunk_data(&data, chunking))
}

/// Build the weak→strong lookup from a base file's chunks.
pub fn cdc_index(chunks: &[Chunk]) -> CdcIndex {
    let mut map: CdcIndex = HashMap::with_capacity(chunks.len());
    for c in chunks {
        map.entry(c.weak)
            .or_default()
            .push((c.offset, c.strong, c.len));
    }
    map
}

/// A base file prepared for content-defined delta: its chunk index, its length,
/// and the chunking geometry used (so diffs use the same settings).
#[derive(Debug, Clone)]
pub struct CdcBase {
    index: CdcIndex,
    len: u64,
    chunking: Chunking,
}

impl CdcBase {
    /// Chunk `base_data` and index it.
    pub fn build(base_data: &[u8], chunking: Chunking) -> Self {
        let chunks = chunk_data(base_data, chunking);
        CdcBase {
            index: cdc_index(&chunks),
            len: base_data.len() as u64,
            chunking,
        }
    }

    /// Read and index a base file.
    pub fn build_from_file(path: &Path, chunking: Chunking) -> std::io::Result<Self> {
        let data = fs::read(path)?;
        Ok(Self::build(&data, chunking))
    }

    pub fn len(&self) -> u64 {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Compute a delta script turning this base into `new_data`.
    ///
    /// Both sides are chunked with the same geometry, so a chunk whose content
    /// is unchanged keeps its boundary and matches — even after an insertion
    /// elsewhere. Matching chunks become `Copy`; everything else becomes
    /// coalesced `Literal` bytes. The result reconstructs via
    /// [`crate::sync::delta::apply`].
    pub fn diff(&self, new_data: &[u8]) -> DeltaScript {
        let new_chunks = chunk_data(new_data, self.chunking);
        let mut instructions: Vec<DeltaInstruction> = Vec::new();
        let mut literal: Vec<u8> = Vec::new();

        for c in &new_chunks {
            let start = c.offset as usize;
            let end = start + c.len as usize;
            let slice = &new_data[start..end];

            let mut matched: Option<(u64, u64)> = None;
            if let Some(candidates) = self.index.get(&c.weak) {
                for &(off, strong, len) in candidates {
                    if strong == c.strong && len == c.len {
                        matched = Some((off, len as u64));
                        break;
                    }
                }
            }

            match matched {
                Some((off, len)) => {
                    if !literal.is_empty() {
                        instructions.push(DeltaInstruction::Literal(std::mem::take(&mut literal)));
                    }
                    instructions.push(DeltaInstruction::Copy { offset: off, len });
                }
                None => literal.extend_from_slice(slice),
            }
        }

        if !literal.is_empty() {
            instructions.push(DeltaInstruction::Literal(literal));
        }

        DeltaScript {
            instructions,
            base_len: self.len,
            new_len: new_data.len() as u64,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Deterministic, low-redundancy payload so FastCDC finds natural boundaries
    /// (a run of one byte would produce a single giant chunk).
    fn payload(len: usize, seed: u64) -> Vec<u8> {
        let mut state = seed | 1;
        let mut out = Vec::with_capacity(len);
        for _ in 0..len {
            state ^= state >> 12;
            state ^= state << 25;
            state ^= state >> 27;
            out.push((state.wrapping_mul(0x2545_F491_4F6C_DD1D) >> 56) as u8);
        }
        out
    }

    #[test]
    fn chunking_sizes_helper() {
        assert_eq!(Chunking::Fixed(4096).sizes(), (4096, 4096, 4096));
        let c = Chunking::fastcdc_default();
        assert_eq!(c.sizes(), (8 * 1024, 16 * 1024, 64 * 1024));
    }

    #[test]
    fn chunks_cover_input_exactly() {
        let data = payload(300_000, 7);
        let chunks = chunk_data(&data, Chunking::fastcdc_default());

        assert!(!chunks.is_empty());
        let mut expect_offset = 0u64;
        let mut total = 0u32;
        for c in &chunks {
            assert_eq!(c.offset, expect_offset, "chunks must be contiguous");
            expect_offset += c.len as u64;
            total += c.len;
        }
        assert_eq!(total as usize, data.len(), "chunks must cover the input");
    }

    #[test]
    fn fastcdc_respects_min_max_bounds() {
        let data = payload(2_000_000, 42);
        let chunks = chunk_data(&data, Chunking::fastcdc_default());
        let (min, _avg, max) = Chunking::fastcdc_default().sizes();

        for (i, c) in chunks.iter().enumerate() {
            let is_last = i == chunks.len() - 1;
            assert!(c.len >= min, "chunk {i} below min: {}", c.len);
            // Only the final chunk may be below min (short tail); none may exceed max.
            assert!(c.len <= max, "chunk {i} above max: {}", c.len);
            if !is_last {
                assert!(c.len >= min);
            }
        }
    }

    #[test]
    fn fastcdc_average_is_near_target() {
        // Normalization should keep the mean reasonably close to avg.
        let data = payload(4_000_000, 99);
        let chunks = chunk_data(&data, Chunking::fastcdc_default());
        let avg = Chunking::fastcdc_default().sizes().1;

        let mean = chunks.iter().map(|c| c.len as u64).sum::<u64>() / chunks.len() as u64;
        // Loose sanity band: within [avg/4, avg*2]. Normalization keeps this
        // far tighter in practice; the band guards against a broken mask.
        assert!(
            mean >= avg as u64 / 4 && mean <= avg as u64 * 2,
            "mean {mean} far from avg {avg}"
        );
    }

    #[test]
    fn fixed_chunking_is_deterministic() {
        let data = payload(50_000, 3);
        let a = chunk_data(&data, Chunking::Fixed(1024));
        let b = chunk_data(&data, Chunking::Fixed(1024));
        assert_eq!(a, b);
        assert_eq!(a.len(), 50_000 / 1024 + 1);
        assert!(a
            .iter()
            .all(|c| c.len == 1024 || c.offset + c.len as u64 == 50_000));
    }

    /// Count how many of `new`'s chunk strong hashes appear in `base`.
    fn matching_chunks(base: &[Chunk], new: &[Chunk]) -> usize {
        let base_set: std::collections::HashSet<u64> = base.iter().map(|c| c.strong).collect();
        new.iter().filter(|c| base_set.contains(&c.strong)).count()
    }

    #[test]
    fn fastcdc_survives_insertion_but_fixed_does_not() {
        let base = payload(1_000_000, 17);

        // Insert 1000 bytes near the START — the design's motivating case
        // ("insertion avant B"), where every following fixed block shifts off
        // the grid. A midpoint insert would leave the untouched first half
        // still matching, understating the difference.
        let mut new = base.clone();
        new.splice(100..100, payload(1000, 1234));

        // FastCDC: only the chunk containing the insert changes; every later
        // chunk re-syncs on the same content.
        let cdc_base = chunk_data(&base, Chunking::fastcdc_default());
        let cdc_new = chunk_data(&new, Chunking::fastcdc_default());
        let cdc_match = matching_chunks(&cdc_base, &cdc_new);
        let cdc_total = cdc_new.len();
        assert!(
            cdc_match * 10 >= cdc_total * 9,
            "FastCDC should keep almost all chunks: {cdc_match}/{cdc_total}"
        );

        // Fixed: the insert lands inside block 0, and every block after it is
        // shifted by 1000 bytes, so essentially nothing matches.
        let fix_base = chunk_data(&base, Chunking::Fixed(8 * 1024));
        let fix_new = chunk_data(&new, Chunking::Fixed(8 * 1024));
        let fix_match = matching_chunks(&fix_base, &fix_new);
        let fix_total = fix_new.len();
        assert!(
            fix_match * 10 <= fix_total,
            "fixed blocks should nearly all miss after a near-start insert: {fix_match}/{fix_total}"
        );

        // The whole point: CDC recovers nearly everything fixed throws away.
        assert!(cdc_match > fix_match * 5);
    }

    #[test]
    fn cdc_diff_and_apply_reconstructs_inserted_file() {
        let base = payload(800_000, 55);
        let mut new = base.clone();
        let mid = base.len() / 2;
        new.splice(mid..mid, payload(5000, 777));

        let cdc = CdcBase::build(&base, Chunking::fastcdc_default());
        let script = cdc.diff(&new);

        // It must actually use copies (not degenerate to a full literal copy).
        assert!(
            script.bytes_from_base() > 0,
            "insertion should still reuse most of the base"
        );

        // Reconstruct and compare byte-for-byte using the shared delta apply.
        let dir = tempfile::tempdir().unwrap();
        let base_path = dir.path().join("base");
        let dst_path = dir.path().join("dst");
        std::fs::write(&base_path, &base).unwrap();

        crate::sync::delta::apply(&base_path, &script, &dst_path).unwrap();
        assert_eq!(std::fs::read(&dst_path).unwrap(), new);
    }

    #[test]
    fn cdc_diff_identical_file_is_all_copy() {
        let base = payload(500_000, 88);
        let cdc = CdcBase::build(&base, Chunking::fastcdc_default());
        let script = cdc.diff(&base);

        assert_eq!(script.bytes_literal(), 0);
        assert_eq!(script.bytes_from_base(), base.len() as u64);
    }

    #[test]
    fn gear_and_masks_are_stable() {
        // The gear table must be deterministic across calls.
        assert_eq!(gear_table(), gear_table());
        // Masks: sparser mask has exactly one more bit than the denser one.
        let (mask_s, mask_l) = make_masks(14);
        assert_eq!(mask_s.count_ones(), mask_l.count_ones() + 1);
        // avg_bits of a power of two is its exponent.
        assert_eq!(avg_bits(16 * 1024), 14);
    }

    #[test]
    fn empty_and_tiny_inputs() {
        assert!(chunk_data(b"", Chunking::fastcdc_default()).is_empty());

        // A file shorter than min is a single chunk.
        let tiny = b"hello";
        let chunks = chunk_data(tiny, Chunking::fastcdc_default());
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].len, 5);
        assert_eq!(chunks[0].offset, 0);
    }
}
