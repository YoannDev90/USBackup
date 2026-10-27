//! Decoupled reader/writer pipeline with a bounded buffer ring
//! (design §9bis.5).
//!
//! ## The problem
//!
//! When one thread does `read(source)` then `write(USB)` in sequence, the two
//! devices take turns: the USB sits idle while the local disk reads, and the
//! disk sits idle while the USB writes. The bottleneck (the key) is never
//! saturated — it typically runs at ~60%.
//!
//! ## The fix
//!
//! Run the reader ahead of the writer through a **bounded pool of buffers**:
//!
//! ```text
//! [reader thread]  ->  ring of N buffers (e.g. 8 x 1 MiB)  ->  [writer thread]
//!   reads from NVMe (fast)          bounds memory            writes to USB (slow)
//! ```
//!
//! The reader runs ahead so the writer is never hungry; the USB saturates at
//! ~100% instead of ~60%. Typical gain **1.3–1.6×** on large files. The pool is
//! bounded (default 8 MiB) so resident memory stays flat regardless of file
//! size.
//!
//! The pool doubles as **backpressure**: when all buffers are in flight the
//! reader blocks on an empty pool, so a fast source cannot outrun a slow sink
//! and blow up memory.
//!
//! ## Thread shutdown
//!
//! Buffers cycle through two bounded channels (`free` and `data`). When either
//! side stops — EOF or an error — its channel endpoints are dropped, which
//! unblocks the other side's `recv`/`send` with a disconnect. So a failure on
//! either thread tears the whole pipeline down without deadlocking.
//!
//! O_DIRECT destination writes (§9bis.2) are a separate, platform-specific
//! optimisation (aligned buffers, `unsafe`) and are intentionally not part of
//! this portable core.

use std::fs::File;
use std::io::{self, Read, Write};
use std::path::Path;
use std::sync::mpsc::sync_channel;
use std::thread;

/// Default number of buffers in the ring.
pub const PIPE_DEFAULT_BUFFERS: usize = 8;

/// Default size of each buffer (matches the Bulk copy buffer).
pub const PIPE_DEFAULT_BUFFER_SIZE: usize = 1024 * 1024; // 1 MiB

/// A filled buffer plus how many valid bytes it carries.
struct Packet {
    buf: Vec<u8>,
    len: usize,
}

/// Copy `src` to `dst` using a reader thread that runs ahead of a writer
/// thread through a bounded ring of `num_buffers` buffers of `buffer_size`
/// bytes each.
///
/// Returns the number of bytes written, which always equals the source length.
/// Any read or write error (from either thread) is surfaced here; the pipeline
/// is torn down cleanly first.
pub fn copy_file_pipelined(
    src: &Path,
    dst: &Path,
    num_buffers: usize,
    buffer_size: usize,
) -> io::Result<u64> {
    let num_buffers = num_buffers.max(1);
    let buffer_size = buffer_size.max(1);

    let mut reader = File::open(src)?;
    let mut writer = File::create(dst)?;

    // Free-buffer pool, prefilled so the reader never waits for the first
    // buffer. Capacity `num_buffers` bounds memory and applies backpressure.
    let (free_tx, free_rx) = sync_channel::<Vec<u8>>(num_buffers);
    for _ in 0..num_buffers {
        // A send into a prefilled channel cannot fail here (capacity == count).
        let _ = free_tx.send(vec![0u8; buffer_size]);
    }

    // Data channel between reader and writer.
    let (data_tx, data_rx) = sync_channel::<Packet>(num_buffers);

    // Reader: pull a free buffer, fill it from the source, hand it to writer.
    let reader_handle = thread::spawn(move || -> io::Result<u64> {
        let mut total: u64 = 0;
        // `Err` on recv means the writer is gone -> stop.
        while let Ok(mut buf) = free_rx.recv() {
            let n = reader.read(&mut buf)?;
            if n == 0 {
                break; // EOF; drop this buffer (we are done with the pool).
            }
            total += n as u64;

            // Hand the filled buffer to the writer. `Err` means the writer
            // failed or exited -> stop.
            if data_tx.send(Packet { buf, len: n }).is_err() {
                break;
            }
        }
        Ok(total)
    });

    // Writer: drain filled buffers to the destination, then recycle them.
    let writer_handle = thread::spawn(move || -> io::Result<u64> {
        let mut total: u64 = 0;
        // `Err` on recv means the reader dropped its sender (EOF or read
        // error) -> done.
        while let Ok(packet) = data_rx.recv() {
            writer.write_all(&packet.buf[..packet.len])?;
            total += packet.len as u64;
            // Recycle the buffer back to the reader's pool. A send failure
            // means the reader is gone; harmless to ignore.
            let _ = free_tx.send(packet.buf);
        }
        writer.flush()?;
        Ok(total)
    });

    // Join both threads. A panic in either is a bug; surface it as an error.
    let read_join = reader_handle
        .join()
        .map_err(|_| io::Error::other("sync pipeline reader thread panicked"))?;
    let write_join = writer_handle
        .join()
        .map_err(|_| io::Error::other("sync pipeline writer thread panicked"))?;

    // Prefer reporting a writer (USB-side) error, then a reader error.
    let written = write_join?;
    read_join?;
    Ok(written)
}

/// Convenience wrapper using the default ring geometry.
pub fn copy_file_pipelined_default(src: &Path, dst: &Path) -> io::Result<u64> {
    copy_file_pipelined(src, dst, PIPE_DEFAULT_BUFFERS, PIPE_DEFAULT_BUFFER_SIZE)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn copies_content_exactly() {
        let dir = tempdir().unwrap();
        let src = dir.path().join("a.bin");
        // A payload that is not a multiple of the buffer size, so the final
        // packet is short.
        let payload: Vec<u8> = (0..100_007).map(|i| (i % 251) as u8).collect();
        fs::write(&src, &payload).unwrap();
        let dst = dir.path().join("b.bin");

        let n = copy_file_pipelined(&src, &dst, 4, 4096).unwrap();

        assert_eq!(n, payload.len() as u64);
        assert_eq!(fs::read(&dst).unwrap(), payload);
    }

    #[test]
    fn copies_empty_file() {
        let dir = tempdir().unwrap();
        let src = dir.path().join("empty");
        fs::write(&src, b"").unwrap();
        let dst = dir.path().join("out");

        let n = copy_file_pipelined(&src, &dst, 4, 4096).unwrap();
        assert_eq!(n, 0);
        assert_eq!(fs::read(&dst).unwrap(), b"");
    }

    #[test]
    fn recycles_buffers_across_many_rounds() {
        let dir = tempdir().unwrap();
        let src = dir.path().join("big.bin");
        // 100 KiB through a 4 x 4 KiB ring forces ~6 recycle rounds, proving
        // buffers flow back from writer to reader correctly.
        let payload: Vec<u8> = (0..100_000).map(|i| (i as u8).wrapping_mul(31)).collect();
        fs::write(&src, &payload).unwrap();
        let dst = dir.path().join("out.bin");

        let n = copy_file_pipelined(&src, &dst, 4, 4096).unwrap();
        assert_eq!(n, payload.len() as u64);
        assert_eq!(fs::read(&dst).unwrap(), payload);
    }

    #[test]
    fn single_buffer_still_works() {
        // Degenerate ring of one buffer must not deadlock.
        let dir = tempdir().unwrap();
        let src = dir.path().join("a");
        let payload = vec![9u8; 50_000];
        fs::write(&src, &payload).unwrap();
        let dst = dir.path().join("b");

        let n = copy_file_pipelined(&src, &dst, 1, 8192).unwrap();
        assert_eq!(n, payload.len() as u64);
        assert_eq!(fs::read(&dst).unwrap(), payload);
    }

    #[test]
    fn missing_source_errors() {
        let dir = tempdir().unwrap();
        let missing = dir.path().join("nope.bin");
        let dst = dir.path().join("out");

        let err = copy_file_pipelined(&missing, &dst, 2, 4096);
        assert!(err.is_err());
        assert!(!dst.exists());
    }

    #[test]
    fn zero_size_clamps_to_one() {
        // A caller passing 0 for geometry must not divide by zero or deadlock.
        let dir = tempdir().unwrap();
        let src = dir.path().join("a");
        let payload = vec![1u8; 3000];
        fs::write(&src, &payload).unwrap();
        let dst = dir.path().join("b");

        let n = copy_file_pipelined(&src, &dst, 0, 0).unwrap();
        assert_eq!(n, payload.len() as u64);
        assert_eq!(fs::read(&dst).unwrap(), payload);
    }

    #[test]
    fn large_file_through_default_geometry() {
        let dir = tempdir().unwrap();
        let src = dir.path().join("large.bin");
        // ~9 MiB, larger than the default 8 MiB ring, so it exercises real
        // recycling under the production geometry.
        let payload: Vec<u8> = (0..9 * 1024 * 1024).map(|i| (i % 253) as u8).collect();
        fs::write(&src, &payload).unwrap();
        let dst = dir.path().join("out.bin");

        let n = copy_file_pipelined_default(&src, &dst).unwrap();
        assert_eq!(n, payload.len() as u64);
        assert_eq!(fs::read(&dst).unwrap(), payload);
    }
}
