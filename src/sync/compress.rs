//! Adaptive compression for the write-bound USB path (design §9bis.3).
//!
//! ## Why compressing toward a USB key is a win
//!
//! The bottleneck on a USB backup is the **write** path (~50 MB/s). The CPU
//! sits idle during that time. LZ4 runs at ~500 MB/s/core with a ~2–3× ratio
//! on text/code/documents. So on 200 MB of documents:
//!
//! |              | write            | time  |
//! |--------------|------------------|-------|
//! | no compress  | 200 MB @ 50 MB/s | 4.0 s |
//! | LZ4          | 80 MB @ 50 MB/s + 0.4 s CPU | ~2.0 s |
//!
//! That is ~2× faster **and** ~2.5× more data on the key — by moving work from
//! the saturated resource (the key) onto the idle one (the CPU).
//!
//! ## But: never compress the already-compressed
//!
//! On jpg / mp4 / zip / zstd data, LZ4 burns CPU for essentially **no** gain
//! (ratio ≈ 1.0×) and can even *inflate* the output. So compression is
//! **adaptive**: sample the entropy of the first [`SAMPLE_BYTES`]; if the data
//! looks incompressible, write it raw and mark it uncompressed. Zero overhead
//! on media, full gain on documents.
//!
//! ## Manifest marker
//!
//! A compressed file is written with a small [`COMPRESS_MAGIC`] header so the
//! restore path knows to decompress. The header carries the uncompressed
//! length, so a restore can pre-size its buffer. Files written raw have no
//! header — restore detects this via the magic and passes bytes through.

use std::io;

/// Magic bytes prefixing a compressed blob. Absent ⇒ stored raw.
pub const COMPRESS_MAGIC: &[u8; 4] = b"UCZ1";

/// Number of leading bytes sampled to decide compressibility.
pub const SAMPLE_BYTES: usize = 64 * 1024; // 64 KiB

/// If compressing the sample yields less than this ratio, treat the data as
/// incompressible and store it raw (design §9bis.3).
pub const MIN_USEFUL_RATIO: f64 = 1.05;

/// Which compression to apply.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Compression {
    /// Store bytes as-is.
    None,
    /// LZ4, optionally adaptive (the default and recommended setting).
    Lz4 { adaptive: bool },
}

impl Default for Compression {
    fn default() -> Self {
        // Recommended default: adaptive LZ4.
        Compression::lz4_adaptive()
    }
}

impl Compression {
    /// Recommended default: adaptive LZ4.
    pub fn lz4_adaptive() -> Self {
        Compression::Lz4 { adaptive: true }
    }
}

/// Outcome of a compression attempt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Compressed {
    /// The data was compressed; the buffer carries the magic header and the
    /// payload. Write this to disk.
    Yes(Vec<u8>),
    /// The data was incompressible (or compression was off); write `raw`
    /// unchanged. No header is added.
    No(Vec<u8>),
}

impl Compressed {
    /// The bytes to write to disk, whatever the decision.
    pub fn into_bytes(self) -> Vec<u8> {
        match self {
            Compressed::Yes(buf) => buf,
            Compressed::No(raw) => raw,
        }
    }

    /// True when the data was compressed (i.e. a manifest marker should say so).
    pub fn was_compressed(&self) -> bool {
        matches!(self, Compressed::Yes(_))
    }
}

/// Estimate the compressibility of `data` by measuring how much LZ4 shrinks
/// the leading [`SAMPLE_BYTES`].
///
/// Returns the observed ratio (raw_len / compressed_len). A ratio near 1.0
/// means the data is already dense (media, encrypted, compressed).
pub fn sample_ratio(data: &[u8]) -> f64 {
    let sample = &data[..data.len().min(SAMPLE_BYTES)];
    if sample.is_empty() {
        return 0.0;
    }
    let compressed = lz4_flex::compress(sample);
    if compressed.is_empty() {
        // Shouldn't happen for non-empty input, but guard against div-by-zero.
        return f64::INFINITY;
    }
    sample.len() as f64 / compressed.len() as f64
}

/// Cheap entropy proxy: distinct byte values in the sample, scaled to 0..=1.
///
/// A low byte-diversity sample (e.g. mostly zeros) compresses extremely well;
/// a sample using all 256 symbols densely is usually already compressed.
/// This is a fast pre-filter; the authoritative check is [`sample_ratio`].
pub fn byte_entropy_estimate(data: &[u8]) -> f64 {
    let sample = &data[..data.len().min(SAMPLE_BYTES)];
    if sample.is_empty() {
        return 0.0;
    }
    let mut seen = [false; 256];
    let mut distinct = 0usize;
    for &b in sample {
        if !seen[b as usize] {
            seen[b as usize] = true;
            distinct += 1;
        }
    }
    distinct as f64 / 256.0
}

/// Compress `data` according to `mode`, returning whether it was worth it.
///
/// Under [`Compression::Lz4`] with `adaptive`, the leading sample is measured
/// first; incompressible data is returned raw (`Compressed::No`) so no CPU is
/// wasted and no space is lost. Non-adaptive LZ4 always compresses.
pub fn compress(data: &[u8], mode: Compression) -> Compressed {
    match mode {
        Compression::None => Compressed::No(data.to_vec()),
        Compression::Lz4 { adaptive } => {
            if adaptive && sample_ratio(data) < MIN_USEFUL_RATIO {
                // Incompressible: store raw, no header.
                return Compressed::No(data.to_vec());
            }
            Compressed::Yes(encode(data))
        }
    }
}

/// Encode a blob with the magic header + uncompressed length.
fn encode(data: &[u8]) -> Vec<u8> {
    let compressed = lz4_flex::compress(data);
    let mut out = Vec::with_capacity(4 + 8 + compressed.len());
    out.extend_from_slice(COMPRESS_MAGIC);
    out.extend_from_slice(&(data.len() as u64).to_le_bytes());
    out.extend_from_slice(&compressed);
    out
}

/// True when `buf` starts with our compression magic.
pub fn is_compressed(buf: &[u8]) -> bool {
    buf.len() >= COMPRESS_MAGIC.len() && &buf[..COMPRESS_MAGIC.len()] == COMPRESS_MAGIC
}

/// Decompress a buffer produced by [`compress`].
///
/// If the buffer has no magic header it is returned verbatim (it was stored
/// raw), so the restore path can call this unconditionally.
pub fn decompress(buf: &[u8]) -> io::Result<Vec<u8>> {
    if !is_compressed(buf) {
        return Ok(buf.to_vec());
    }
    if buf.len() < COMPRESS_MAGIC.len() + 8 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "compressed blob truncated (missing length header)",
        ));
    }
    let mut len_bytes = [0u8; 8];
    len_bytes.copy_from_slice(&buf[COMPRESS_MAGIC.len()..COMPRESS_MAGIC.len() + 8]);
    let expected = u64::from_le_bytes(len_bytes) as usize;

    let payload = &buf[COMPRESS_MAGIC.len() + 8..];
    // `decompress` (not the size-prepended variant) because our own header
    // already carries the uncompressed length, which doubles as the hint.
    let out = lz4_flex::decompress(payload, expected).map_err(|e| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("lz4 decompress failed: {e}"),
        )
    })?;

    if out.len() != expected {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("length mismatch: header says {expected}, got {}", out.len()),
        ));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Generate high-entropy bytes that LZ4 genuinely cannot shrink, using a
    /// xorshift PRNG (a linear ramp would still compress, defeating the test).
    fn incompressible_bytes(len: usize) -> Vec<u8> {
        let mut state: u64 = 0x9E3779B97F4A7C15;
        let mut out = Vec::with_capacity(len);
        for _ in 0..len {
            // xorshift64*
            state ^= state >> 12;
            state ^= state << 25;
            state ^= state >> 27;
            let v = state.wrapping_mul(0x2545F4914F6CDD1D);
            out.push((v >> 56) as u8);
        }
        out
    }

    #[test]
    fn compressible_text_roundtrips() {
        // Highly repetitive text compresses well.
        let data: Vec<u8> = "the quick brown fox jumps over the lazy dog. "
            .repeat(2000)
            .into_bytes();

        let out = compress(&data, Compression::lz4_adaptive());
        assert!(out.was_compressed(), "repetitive text should compress");

        let bytes = out.into_bytes();
        assert!(is_compressed(&bytes));
        assert!(bytes.len() < data.len(), "compressed should be smaller");
        assert_eq!(decompress(&bytes).unwrap(), data);
    }

    #[test]
    fn incompressible_data_stays_raw() {
        // Simulate high-entropy (already-compressed) data.
        let data = incompressible_bytes(200_000);

        let out = compress(&data, Compression::lz4_adaptive());
        // Adaptive mode must detect this and store raw.
        assert!(!out.was_compressed(), "high-entropy data should stay raw");
        let bytes = out.into_bytes();
        assert!(!is_compressed(&bytes));
        assert_eq!(bytes, data);
        // And decompress passes raw through unchanged.
        assert_eq!(decompress(&bytes).unwrap(), data);
    }

    #[test]
    fn non_adaptive_always_compresses() {
        let data = incompressible_bytes(100_000);
        let out = compress(&data, Compression::Lz4 { adaptive: false });
        assert!(out.was_compressed());
    }

    #[test]
    fn compression_none_stores_raw() {
        let data = b"hello world".to_vec();
        let out = compress(&data, Compression::None);
        assert!(!out.was_compressed());
        assert_eq!(out.into_bytes(), data);
    }

    #[test]
    fn sample_ratio_high_for_repetition_low_for_entropy() {
        let repetitive = vec![b'a'; 100_000];
        assert!(sample_ratio(&repetitive) > 10.0);

        let dense = incompressible_bytes(100_000);
        assert!(sample_ratio(&dense) < MIN_USEFUL_RATIO);
    }

    #[test]
    fn empty_data_roundtrips() {
        let data: Vec<u8> = Vec::new();
        let out = compress(&data, Compression::lz4_adaptive());
        // Empty is trivially "incompressible" by ratio; stays raw.
        let bytes = out.into_bytes();
        assert_eq!(decompress(&bytes).unwrap(), data);
    }

    #[test]
    fn truncated_compressed_blob_errors() {
        let data: Vec<u8> = "repeat me ".repeat(500).into_bytes();
        let out = compress(&data, Compression::Lz4 { adaptive: false });
        let bytes = out.into_bytes();
        // Chop off the tail to corrupt it.
        let truncated = &bytes[..bytes.len() - 5];
        assert!(decompress(truncated).is_err());
    }

    #[test]
    fn raw_passthrough_needs_no_header() {
        // A buffer without magic decompresses to itself.
        let raw = b"not compressed at all".to_vec();
        assert!(!is_compressed(&raw));
        assert_eq!(decompress(&raw).unwrap(), raw);
    }
}
