//! Criterion benchmarks for the pure-Rust sync engine (design §13 / M10).
//!
//! The crate is a binary, so the engine is compiled straight into this bench
//! with `#[path]`: `sync/` only ever references `crate::sync::*`, so it builds
//! standalone. Run with `cargo bench`.
//!
//! What each group tells us:
//! - `scan` — metadata (stat) throughput, the floor of every run.
//! - `sync_10k` / `sync_100k` — cold copy and verify-only noop, at both scales.
//! - `snapshot_incremental` — second+ snapshot: the hard-link fast path.
//! - `metadata_concurrency` / `bulk_concurrency` — the two pool sizes.
//! - `bulk_threshold` — size cutoff between the metadata and bulk pools.
//!
//! On removable media (TMPDIR pointed at a USB key) the seeds run before any
//! filtered benchmark, and a cheap flash controller can reset under the
//! 10k/100k-file metadata storm — so the three gating sweeps are registered
//! FIRST (results hit the disk incrementally, per benchmark), and
//! `USBACKUP_BENCH_SWEEPS_ONLY=1` skips the scale groups entirely.

#[path = "../src/sync/mod.rs"]
mod sync;

use criterion::{criterion_group, criterion_main, BatchSize, BenchmarkId, Criterion, Throughput};
use std::fs;
use std::path::Path;
use std::time::Duration;
use sync::scan;
use sync::snapshot::{RetentionPolicy, SnapshotStore};
use sync::SyncEngine;
use tempfile::TempDir;

/// Seed `count` small files spread over 50 subdirectories (metadata-bound).
fn seed_small(root: &Path, count: usize) {
    fs::create_dir_all(root).unwrap();
    for i in 0..count {
        let dir = root.join(format!("d{}", i % 50));
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join(format!("f{i}.txt")), format!("payload {i}")).unwrap();
    }
}

/// Seed `count` files of `size` bytes each under `prefix{i}.bin`.
fn seed_sized(root: &Path, prefix: &str, count: usize, size: usize) {
    fs::create_dir_all(root).unwrap();
    let block = vec![0xA5u8; size];
    for i in 0..count {
        fs::write(root.join(format!("{prefix}{i}.bin")), &block).unwrap();
    }
}

/// Bound every group so a slow I/O bench does not run for minutes: criterion
/// takes at least `sample_size` samples, so a 1 s/iter bench is capped at
/// ~10 s of measurement rather than 100 samples.
fn tune(group: &mut criterion::BenchmarkGroup<'_, criterion::measurement::WallTime>) {
    group.warm_up_time(Duration::from_secs(2));
    group.measurement_time(Duration::from_secs(5));
    group.sample_size(10);
}

/// Skip the 10k/100k scale groups: their seeding alone can reset a cheap
/// flash controller when TMPDIR lives on removable media (see file header).
fn sweeps_only() -> bool {
    std::env::var_os("USBACKUP_BENCH_SWEEPS_ONLY").is_some()
}

/// Metadata throughput: how fast we can stat a tree (the floor of any run).
fn bench_scan(c: &mut Criterion) {
    if sweeps_only() {
        return;
    }
    let mut group = c.benchmark_group("scan");
    tune(&mut group);
    for count in [10_000usize, 100_000] {
        let dir = TempDir::new().unwrap();
        seed_small(dir.path(), count);
        group.throughput(Throughput::Elements(count as u64));
        group.bench_with_input(BenchmarkId::from_parameter(count), &count, |b, _| {
            b.iter(|| scan(dir.path()).unwrap());
        });
    }
    group.finish();
}

/// End-to-end sync: cold copy into an empty destination, and the verify-only
/// path when the destination already matches the source.
fn bench_sync(c: &mut Criterion) {
    if sweeps_only() {
        return;
    }
    let dir = TempDir::new().unwrap();
    let src_10k = dir.path().join("src10k");
    seed_small(&src_10k, 10_000);
    // A destination that already matches, for the no-op (verify) case.
    let dst_noop_10k = dir.path().join("noop10k");
    SyncEngine::default()
        .run(&src_10k, &dst_noop_10k, false)
        .unwrap();

    let mut group = c.benchmark_group("sync_10k");
    tune(&mut group);
    group.throughput(Throughput::Elements(10_000));

    group.bench_function("cold", |b| {
        let dst = dir.path().join("cold");
        b.iter_batched(
            || {
                let _ = fs::remove_dir_all(&dst);
                dst.clone()
            },
            |d| SyncEngine::default().run(&src_10k, &d, false).unwrap(),
            BatchSize::SmallInput,
        );
    });

    group.bench_function("noop", |b| {
        b.iter(|| {
            SyncEngine::default()
                .run(&src_10k, &dst_noop_10k, false)
                .unwrap()
        });
    });
    group.finish();

    // The same no-op at the design's 100k-file scale: pure verify cost.
    let dir_100k = TempDir::new().unwrap();
    let src_100k = dir_100k.path().join("src");
    seed_small(&src_100k, 100_000);
    let dst_noop_100k = dir_100k.path().join("noop");
    SyncEngine::default()
        .run(&src_100k, &dst_noop_100k, false)
        .unwrap();

    let mut group = c.benchmark_group("sync_100k");
    tune(&mut group);
    group.throughput(Throughput::Elements(100_000));

    group.bench_function("cold", |b| {
        let dst = dir_100k.path().join("cold");
        b.iter_batched(
            || {
                let _ = fs::remove_dir_all(&dst);
                dst.clone()
            },
            |d| SyncEngine::default().run(&src_100k, &d, false).unwrap(),
            BatchSize::SmallInput,
        );
    });

    group.bench_function("noop", |b| {
        b.iter(|| {
            SyncEngine::default()
                .run(&src_100k, &dst_noop_100k, false)
                .unwrap()
        });
    });
    group.finish();
}

/// Second-and-later snapshot of an unchanged tree: every file is a hard link,
/// so this is the ×1000 path that makes frequent snapshots affordable.
fn bench_snapshot(c: &mut Criterion) {
    if sweeps_only() {
        return;
    }
    let dir = TempDir::new().unwrap();
    let src = dir.path().join("src");
    seed_small(&src, 10_000);
    let store = SnapshotStore::new(dir.path());
    // First snapshot: full copy (untimed setup).
    store
        .create_snapshot(&src, &RetentionPolicy::default())
        .unwrap();

    let mut group = c.benchmark_group("snapshot_incremental");
    tune(&mut group);
    group.throughput(Throughput::Elements(10_000));
    group.bench_function("link_10k", |b| {
        b.iter(|| {
            store
                .create_snapshot(&src, &RetentionPolicy::default())
                .unwrap()
        });
    });
    group.finish();
}

/// Sweep the `Metadata` pool size on a 10k-small-file cold sync: the knob the
/// design calls out for tuning (latency-bound class, default 8).
fn bench_metadata_concurrency(c: &mut Criterion) {
    let dir = TempDir::new().unwrap();
    let src = dir.path().join("src");
    seed_small(&src, 10_000);
    let dst = dir.path().join("dst");

    let mut group = c.benchmark_group("metadata_concurrency");
    tune(&mut group);
    group.throughput(Throughput::Elements(10_000));
    for n in [1usize, 2, 4, 8, 16, 32] {
        let engine = SyncEngine {
            metadata_concurrency: n,
            ..SyncEngine::default()
        };
        group.bench_with_input(BenchmarkId::from_parameter(n), &n, |b, _| {
            b.iter_batched(
                || {
                    let _ = fs::remove_dir_all(&dst);
                    dst.clone()
                },
                |d| engine.run(&src, &d, false).unwrap(),
                BatchSize::SmallInput,
            );
        });
    }
    group.finish();
}

/// Sweep the `Bulk` pool size on large-file copies (bandwidth-bound class,
/// default 2). Note: this runs on local disk, which parallelises writes far
/// better than a USB key — treat a higher local optimum as an upper bound, not
/// the shipped value (the design gates the default on a real-key benchmark).
fn bench_bulk_concurrency(c: &mut Criterion) {
    let dir = TempDir::new().unwrap();
    let src = dir.path().join("src");
    seed_sized(&src, "big", 4, 8 * 1024 * 1024);
    let dst = dir.path().join("dst");

    let mut group = c.benchmark_group("bulk_concurrency");
    tune(&mut group);
    group.throughput(Throughput::Bytes(4 * 8 * 1024 * 1024));
    for n in [1usize, 2, 4, 8] {
        let engine = SyncEngine {
            bulk_concurrency: n,
            ..SyncEngine::default()
        };
        group.bench_with_input(BenchmarkId::from_parameter(n), &n, |b, _| {
            b.iter_batched(
                || {
                    let _ = fs::remove_dir_all(&dst);
                    dst.clone()
                },
                |d| engine.run(&src, &d, false).unwrap(),
                BatchSize::SmallInput,
            );
        });
    }
    group.finish();
}

/// Sweep `bulk_threshold` on a mixed workload: the size at/above which a copy
/// joins the bandwidth-bound `Bulk` pool instead of the latency-bound
/// `Metadata` pool. The 512 KiB files straddle the swept values, so the split
/// actually changes. Local-disk numbers only — the shipped value is gated on a
/// real-key benchmark (design §9.3).
fn bench_bulk_threshold(c: &mut Criterion) {
    let dir = TempDir::new().unwrap();
    let src = dir.path().join("src");
    seed_small(&src, 1_000); // 1_000 × ~8 KiB (always Metadata)
    seed_sized(&src, "med", 40, 512 * 1024); // 40 × 512 KiB (straddles)
    seed_sized(&src, "big", 4, 4 * 1024 * 1024); // 4 × 4 MiB (always Bulk)
    let dst = dir.path().join("dst");

    let mut group = c.benchmark_group("bulk_threshold");
    tune(&mut group);
    for kib in [64u64, 256, 1_024, 4_096] {
        let engine = SyncEngine {
            bulk_threshold: kib * 1024,
            ..SyncEngine::default()
        };
        group.bench_with_input(
            BenchmarkId::from_parameter(format!("{kib}KiB")),
            &kib,
            |b, _| {
                b.iter_batched(
                    || {
                        let _ = fs::remove_dir_all(&dst);
                        dst.clone()
                    },
                    |d| engine.run(&src, &d, false).unwrap(),
                    BatchSize::SmallInput,
                );
            },
        );
    }
    group.finish();
}

criterion_group!(
    benches,
    // The gating sweeps first: on removable media their results must be on
    // disk before any heavy seed is attempted (see file header).
    bench_bulk_concurrency,
    bench_bulk_threshold,
    bench_metadata_concurrency,
    bench_scan,
    bench_sync,
    bench_snapshot
);
criterion_main!(benches);
