//! Sparse-file-aware copying (design §9bis.7).
//!
//! ## The problem
//!
//! A sparse file (VM disk qcow2/vmdk, a SQLite DB with holes, a mostly-empty
//! preallocated image) has a large *apparent* size but little *real* data. A
//! naive copy reads the holes as zeros and **writes those zeros to the key** —
//! turning a 10 GB file with 1 GB of real data into 10 GB of USB writes. For VM
//! and database workloads this is the single largest waste in a backup.
//!
//! ## The fix
//!
//! Don't write the holes. We scan the source for runs of zeros and, instead of
//! writing them, we `seek` over them on the destination so the gap becomes a
//! hole there too. The 10 GB / 1 GB example drops from 10 GB written to ~1 GB.
//!
//! This is portable and needs no `unsafe`: we detect holes by reading (cheap on
//! the local source) and reproduce them by seeking (no data written). On Unix
//! the `SEEK_DATA`/`SEEK_HOLE` lseek verbs could skip reading the holes
//! entirely; that is a future refinement behind `cfg`, not required for
//! correctness. Detection of *whether a file is sparse* uses allocated-block
//! accounting, which is Unix-only for now; elsewhere we conservatively fall
//! back to a dense (but still correct) copy.
//!
//! Cost when a file is not sparse: one pass of zero-scanning over the source.
//! That is CPU on the local disk, never USB bandwidth, so it is close to free.

use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::Path;

/// Slack (bytes) when comparing allocated vs apparent size, to absorb
/// filesystem block-rounding so a fully-dense file is not misread as sparse.
const SPARSE_SLACK: u64 = 8 * 1024;

/// Decide whether a file is worth treating as sparse.
///
/// On Unix this compares allocated 512-byte blocks against apparent size: a
/// file whose allocated bytes are well under its length has holes. Elsewhere it
/// returns `false`, so callers fall back to a normal (dense) copy — correct,
/// just without the space saving.
#[cfg(unix)]
pub fn is_sparse(path: &Path) -> io::Result<bool> {
    use std::os::unix::fs::MetadataExt;
    let md = std::fs::metadata(path)?;
    let apparent = md.len();
    let allocated = md.blocks().saturating_mul(512);
    // Must be a regular file; directories/symlinks are not copied this way.
    if !md.is_file() {
        return Ok(false);
    }
    Ok(allocated + SPARSE_SLACK < apparent)
}

#[cfg(not(unix))]
pub fn is_sparse(_path: &Path) -> io::Result<bool> {
    // No portable allocated-size probe without platform code; treat as dense.
    Ok(false)
}

/// Minimum length of a zero run before it is worth leaving as a hole.
///
/// A filesystem allocates in blocks (~4 KiB), so a shorter zero run saved
/// nothing but costs a seek. Skipping only long runs keeps dense files with
/// scattered zero bytes written contiguously (no syscall explosion) while still
/// capturing the multi-megabyte holes that make VM/database images sparse.
const HOLE_MIN: usize = 4096;

/// Copy `src` to `dst`, preserving sparseness: long runs of zeros are not
/// written, leaving holes in the destination instead.
///
/// Zero-runs are evaluated at **sub-chunk** granularity, and only runs of at
/// least [`HOLE_MIN`] bytes become holes — a data byte at offset 0 followed by
/// a 16 MiB hole writes only that byte, while a stray zero byte inside an
/// otherwise dense file is still written literally (no per-byte seeks). For a
/// fully dense file there is exactly one contiguous run per chunk, so the cost
/// equals a normal sequential copy.
///
/// Returns the number of **real bytes actually written** (holes excluded), the
/// interesting figure for a USB-bound backup. The destination's apparent length
/// always matches the source's (trailing holes preserved via `set_len`).
///
/// This writes directly to `dst` (not atomically); callers that need atomicity
/// should use [`copy_sparse_atomic`].
pub fn copy_sparse(src: &Path, dst: &Path, buf: &mut [u8]) -> io::Result<u64> {
    let mut reader = File::open(src)?;
    let src_len = reader.metadata()?.len();
    let mut writer = File::create(dst)?;

    // Absolute offset of buf[0] in the source.
    let mut base: u64 = 0;
    // Cursor position of the writer, so we only seek when the next write is
    // not contiguous with the last one (dense regions cost no extra seek).
    let mut write_pos: u64 = 0;
    let mut written: u64 = 0;

    loop {
        let n = reader.read(buf)?;
        if n == 0 {
            break;
        }
        let chunk = &buf[..n];

        // Walk the chunk as alternating [data run][zero run][data run]...
        let mut i = 0;
        while i < n {
            // Data run: extend over consecutive non-zero bytes.
            let data_start = i;
            while i < n && chunk[i] != 0 {
                i += 1;
            }
            if i > data_start {
                write_run(
                    &mut writer,
                    &chunk[data_start..i],
                    base + data_start as u64,
                    &mut write_pos,
                )?;
                written += (i - data_start) as u64;
            }

            // Zero run: extend over consecutive zero bytes.
            let zero_start = i;
            while i < n && chunk[i] == 0 {
                i += 1;
            }
            let zero_len = i - zero_start;
            if zero_len >= HOLE_MIN {
                // Long enough to be worth a hole: write nothing, leave a gap.
                // The next data run's seek creates the hole implicitly.
                // Not counted in `written`: these bytes never reach the key.
            } else {
                // Too short to save anything; write the zeros literally so we
                // do not pay a seek for a sub-block gap. They DO hit the key,
                // so they count toward `written`.
                write_run(
                    &mut writer,
                    &chunk[zero_start..i],
                    base + zero_start as u64,
                    &mut write_pos,
                )?;
                written += zero_len as u64;
            }
        }

        base += n as u64;
    }

    // Establish the apparent length so trailing holes are preserved. On a file
    // that ends in data this is a no-op; on one that ends in a hole it creates
    // the final gap without writing zeros.
    writer.set_len(src_len)?;
    writer.flush()?;

    Ok(written)
}

/// Write `run` at absolute `abs_start`, seeking only if not already in place.
fn write_run(writer: &mut File, run: &[u8], abs_start: u64, write_pos: &mut u64) -> io::Result<()> {
    if *write_pos != abs_start {
        writer.seek(SeekFrom::Start(abs_start))?;
    }
    writer.write_all(run)?;
    *write_pos = abs_start + run.len() as u64;
    Ok(())
}

/// Like [`copy_sparse`] but writes to a temp sibling and renames into place, so
/// the destination is never left partially written (matches the atomicity
/// guarantee of `copy_file_atomic`). Returns real bytes written.
pub fn copy_sparse_atomic(src: &Path, dst: &Path, buf: &mut [u8]) -> io::Result<u64> {
    let temp = crate::sync::copy::temp_path(dst);
    let result = copy_sparse(src, &temp, buf);
    match result {
        Ok(written) => {
            std::fs::rename(&temp, dst)?;
            // Best-effort metadata preservation; a failure does not undo the copy.
            if let Err(e) = crate::sync::meta::copy_metadata(src, dst) {
                log::debug!(
                    "sparse-copied {} but could not preserve metadata: {}",
                    dst.display(),
                    e
                );
            }
            Ok(written)
        }
        Err(e) => {
            let _ = std::fs::remove_file(&temp);
            Err(e)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    /// Create a sparse-ish file: a small blob near the start, a large apparent
    /// hole, then a small blob at the end. Returns its path.
    ///
    /// The hole is carved with `set_len` (ftruncate), the portable way to get a
    /// physically unallocated region: on macOS an lseek-past-EOF + write can be
    /// preallocated by the filesystem, which would make the fixture dense.
    fn make_sparse(dir: &Path, name: &str, hole: u64) -> std::path::PathBuf {
        let p = dir.join(name);
        let mut f = File::create(&p).unwrap();
        f.write_all(b"HEAD").unwrap();
        f.set_len(hole + 4).unwrap();
        f.seek(SeekFrom::Start(hole)).unwrap();
        f.write_all(b"TAIL").unwrap();
        drop(f);
        p
    }

    #[cfg(unix)]
    #[test]
    fn sparse_file_is_detected() {
        use std::os::unix::fs::MetadataExt;
        let dir = tempdir().unwrap();
        // 32 MiB, matching the copy test below: the macOS CI runner's
        // filesystem preallocates smaller files entirely (a 16 MiB fixture
        // came back fully allocated even via ftruncate), while 32 MiB holes
        // are kept physical on every leg.
        let p = make_sparse(dir.path(), "sparse.bin", 32 * 1024 * 1024);
        let md = fs::metadata(&p).unwrap();
        assert!(
            is_sparse(&p).unwrap(),
            "32 MiB hole should read as sparse: allocated {} bytes, len {}",
            md.blocks().saturating_mul(512),
            md.len()
        );
    }

    #[cfg(unix)]
    #[test]
    fn dense_file_is_not_sparse() {
        let dir = tempdir().unwrap();
        let p = dir.path().join("dense.bin");
        fs::write(&p, vec![7u8; 4 * 1024 * 1024]).unwrap();
        assert!(!is_sparse(&p).unwrap());
    }

    #[test]
    fn copy_sparse_preserves_length_and_content() {
        let dir = tempdir().unwrap();
        let hole = 8 * 1024 * 1024;
        let src = make_sparse(dir.path(), "src.bin", hole);
        let dst = dir.path().join("dst.bin");

        let mut buf = vec![0u8; 256 * 1024];
        copy_sparse(&src, &dst, &mut buf).unwrap();

        // Apparent length matches (head + hole + tail).
        assert_eq!(
            fs::metadata(&dst).unwrap().len(),
            fs::metadata(&src).unwrap().len()
        );

        // Content round-trips: head at the start, tail at the end, zeros in the hole.
        let data = fs::read(&dst).unwrap();
        assert_eq!(&data[..4], b"HEAD");
        assert_eq!(&data[hole as usize..hole as usize + 4], b"TAIL");
        assert!(data[100..1000].iter().all(|&b| b == 0));
    }

    #[cfg(unix)]
    #[test]
    fn copy_sparse_writes_far_less_than_apparent_size() {
        use std::os::unix::fs::MetadataExt;

        let dir = tempdir().unwrap();
        let hole = 32 * 1024 * 1024; // 32 MiB hole, ~8 bytes of real data
        let src = make_sparse(dir.path(), "src.bin", hole);
        let dst = dir.path().join("dst.bin");

        let mut buf = vec![0u8; 256 * 1024];
        let written = copy_sparse(&src, &dst, &mut buf).unwrap();

        // Only the head + tail bytes were actually written.
        assert_eq!(written, 8);
        // And the destination allocated far fewer blocks than its apparent size.
        let md = fs::metadata(&dst).unwrap();
        assert!(
            md.blocks().saturating_mul(512) + SPARSE_SLACK < md.len(),
            "destination should be sparse: allocated {} blocks, len {}",
            md.blocks(),
            md.len()
        );
    }

    #[test]
    fn copy_sparse_handles_dense_file() {
        let dir = tempdir().unwrap();
        let src = dir.path().join("dense.bin");
        let payload: Vec<u8> = (0..300_000).map(|i| (i % 251) as u8).collect();
        fs::write(&src, &payload).unwrap();
        let dst = dir.path().join("dst.bin");

        let mut buf = vec![0u8; 64 * 1024];
        let written = copy_sparse(&src, &dst, &mut buf).unwrap();

        // Dense file: everything written, content identical.
        assert_eq!(written, payload.len() as u64);
        assert_eq!(fs::read(&dst).unwrap(), payload);
    }

    #[test]
    fn copy_sparse_handles_empty_file() {
        let dir = tempdir().unwrap();
        let src = dir.path().join("empty");
        fs::write(&src, b"").unwrap();
        let dst = dir.path().join("dst");

        let mut buf = vec![0u8; 1024];
        let written = copy_sparse(&src, &dst, &mut buf).unwrap();
        assert_eq!(written, 0);
        assert_eq!(fs::metadata(&dst).unwrap().len(), 0);
    }

    #[test]
    fn copy_sparse_atomic_leaves_no_temp_and_is_correct() {
        let dir = tempdir().unwrap();
        let hole = 4 * 1024 * 1024;
        let src = make_sparse(dir.path(), "src.bin", hole);
        let dst = dir.path().join("dst.bin");

        let mut buf = vec![0u8; 128 * 1024];
        copy_sparse_atomic(&src, &dst, &mut buf).unwrap();

        assert_eq!(
            fs::metadata(&dst).unwrap().len(),
            fs::metadata(&src).unwrap().len()
        );
        let data = fs::read(&dst).unwrap();
        assert_eq!(&data[..4], b"HEAD");
        assert_eq!(&data[hole as usize..hole as usize + 4], b"TAIL");

        // No temp litter remains.
        let leftovers: Vec<_> = fs::read_dir(dir.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| crate::sync::copy::is_temp_file(&e.path()))
            .collect();
        assert!(leftovers.is_empty(), "temp left behind: {:?}", leftovers);
    }

    #[test]
    fn copy_sparse_atomic_cleans_up_on_missing_source() {
        let dir = tempdir().unwrap();
        let missing = dir.path().join("nope.bin");
        let dst = dir.path().join("dst.bin");

        let mut buf = vec![0u8; 1024];
        assert!(copy_sparse_atomic(&missing, &dst, &mut buf).is_err());
        assert!(!dst.exists());
    }
}
