//! Bounded probe.

use crate::sync::copy::copy_file_atomic;
use crate::sync::engine::{
    DEFAULT_BULK_CONCURRENCY, DEFAULT_BULK_THRESHOLD, DEFAULT_BUFFER_SIZE,
    DEFAULT_METADATA_CONCURRENCY, EngineParams,
};
use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};
use sysinfo::Disks;

static ENABLED: AtomicBool = AtomicBool::new(false);
static REPROBE: AtomicBool = AtomicBool::new(false);

pub fn set_enabled(v: bool) { ENABLED.store(v, Ordering::Relaxed); }
pub fn enabled() -> bool { ENABLED.load(Ordering::Relaxed) }
pub fn is_enabled() -> bool { enabled() }
pub fn set_reprobe(v: bool) { REPROBE.store(v, Ordering::Relaxed); }
pub fn reprobe() -> bool { REPROBE.load(Ordering::Relaxed) }
pub fn is_reprobe() -> bool { reprobe() }

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Tuning {
    pub metadata_concurrency: usize,
    pub bulk_concurrency: usize,
    pub buffer_size: usize,
    pub bulk_threshold: u64,
    pub hard_links: bool,
    pub symlinks: bool,
    pub free_bytes: u64,
    pub note: Option<String>,
}

impl Default for Tuning {
    fn default() -> Self {
        Tuning {
            metadata_concurrency: DEFAULT_METADATA_CONCURRENCY,
            bulk_concurrency: DEFAULT_BULK_CONCURRENCY,
            buffer_size: DEFAULT_BUFFER_SIZE,
            bulk_threshold: DEFAULT_BULK_THRESHOLD,
            hard_links: false,
            symlinks: false,
            free_bytes: 0,
            note: None,
        }
    }
}

impl Tuning {
    pub fn sanitized(&self) -> Self {
        Tuning {
            metadata_concurrency: self.metadata_concurrency.clamp(1, 32),
            bulk_concurrency: self.bulk_concurrency.clamp(1, 8),
            buffer_size: self.buffer_size.clamp(4 * 1024, 64 * 1024 * 1024),
            bulk_threshold: self.bulk_threshold.clamp(4 * 1024, 64 * 1024 * 1024),
            hard_links: self.hard_links,
            symlinks: self.symlinks,
            free_bytes: self.free_bytes,
            note: self.note.clone(),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum TuneError {
    #[error("destination does not exist: {0}")]
    DestMissing(String),
    #[error("i/o error: {0}")]
    Io(#[from] io::Error),
    #[error("probe skipped: {0}")]
    Skipped(String),
}

impl From<&Tuning> for EngineParams {
    fn from(t: &Tuning) -> Self {
        EngineParams {
            metadata_concurrency: t.metadata_concurrency,
            bulk_concurrency: t.bulk_concurrency,
            buffer_size: t.buffer_size,
            bulk_threshold: t.bulk_threshold,
        }
    }
}

const PROBE_DIR: &str = ".usbackup-probe";
const MIN_FREE_BYTES: u64 = 64 * 1024 * 1024;
const BUDGET: Duration = Duration::from_secs(20);
const SMALL_FILE_SIZE: usize = 4 * 1024;
const SMALL_FILE_COUNT: usize = 64;

pub fn probe(dest: &Path) -> Result<Tuning, TuneError> {
    if !dest.is_dir() {
        return Err(TuneError::DestMissing(dest.display().to_string()));
    }
    let probe_root = dest.join(PROBE_DIR);
    let _ = fs::remove_dir_all(&probe_root);
    fs::create_dir_all(&probe_root).map_err(TuneError::Io)?;
    let started = Instant::now();
    let result = run_probe(dest, &probe_root, started);
    let _ = fs::remove_dir_all(&probe_root);
    result
}

fn run_probe(dest: &Path, probe_root: &Path, started: Instant) -> Result<Tuning, TuneError> {
    let free_bytes = free_bytes_for(dest);
    if free_bytes < MIN_FREE_BYTES {
        return Ok(Tuning {
            free_bytes,
            note: Some("skipped: low space".to_string()),
            ..Tuning::default()
        });
    }

    let src_dir = probe_root.join("src_small");
    fs::create_dir_all(&src_dir).map_err(TuneError::Io)?;
    let payload = vec![0xABu8; SMALL_FILE_SIZE];
    for i in 0..SMALL_FILE_COUNT {
        let dir = src_dir.join(format!("d{}", i % 50));
        fs::create_dir_all(&dir).map_err(TuneError::Io)?;
        fs::write(dir.join(format!("f{i}.bin")), &payload).map_err(TuneError::Io)?;
    }
    let mut files: Vec<PathBuf> = Vec::with_capacity(SMALL_FILE_COUNT);
    for i in 0..SMALL_FILE_COUNT {
        files.push(src_dir.join(format!("d{}", i % 50)).join(format!("f{i}.bin")));
    }

    let mut rates: Vec<(usize, f64)> = Vec::new();
    for &c in &[1usize, 2, 4, 8] {
        if started.elapsed() > BUDGET { break; }
        let dst_dir = probe_root.join(format!("dst_small_c{c}"));
        let _ = fs::remove_dir_all(&dst_dir);
        fs::create_dir_all(&dst_dir).map_err(TuneError::Io)?;
        let t0 = Instant::now();
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(c.max(1))
            .build()
            .map_err(|e| TuneError::Io(io::Error::other(e)))?;
        let res = pool.install(|| {
            files.par_iter().try_for_each(|src| {
                let rel = src.strip_prefix(&src_dir).unwrap_or(src.as_path());
                let dst = dst_dir.join(rel);
                if let Some(p) = dst.parent() {
                    fs::create_dir_all(p).map_err(TuneError::Io)?;
                }
                let mut buf = vec![0u8; 64 * 1024];
                copy_file_atomic(src, &dst, &mut buf).map_err(|e| TuneError::Io(e.into()))?;
                Ok::<(), TuneError>(())
            })
        });
        res?;
        let elapsed = t0.elapsed().as_secs_f64().max(1e-9);
        let rate = SMALL_FILE_COUNT as f64 / elapsed;
        rates.push((c, rate));
    }

    let best_rate = rates.iter().map(|r| r.1).fold(0.0f64, f64::max);
    let mut metadata_concurrency = rates.first().map(|r| r.0).unwrap_or(1);
    for &(c, r) in &rates {
        if r >= best_rate * 0.9 {
            metadata_concurrency = c;
            break;
        }
    }

    let f_cost = rates
        .iter()
        .find(|r| r.0 == 1)
        .map(|r| 1.0 / r.1.max(1e-12))
        .unwrap_or(0.05);

    let est_src = probe_root.join("bulk_est.bin");
    {
        let data = vec![0u8; 1024 * 1024];
        fs::write(&est_src, &data).map_err(TuneError::Io)?;
    }
    let est_dst = probe_root.join("bulk_est_out.bin");
    let est_t0 = Instant::now();
    {
        let mut buf = vec![0u8; 256 * 1024];
        copy_file_atomic(&est_src, &est_dst, &mut buf).map_err(|e| TuneError::Io(e.into()))?;
    }
    let est_elapsed = est_t0.elapsed().as_secs_f64().max(1e-9);
    let est_speed = (1024 * 1024) as f64 / est_elapsed;

    let s = if est_speed >= 5.0 * 1024.0 * 1024.0 {
        4 * 1024 * 1024
    } else if est_speed >= 1.0 * 1024.0 * 1024.0 {
        2 * 1024 * 1024
    } else {
        512 * 1024
    };

    let bulk_src = probe_root.join("bulk.bin");
    {
        let data = vec![0u8; s];
        fs::write(&bulk_src, &data).map_err(TuneError::Io)?;
    }

    let mut best_buf = 1024 * 1024usize;
    let mut best_tp = 0.0f64;
    let mut buf_results: Vec<(usize, f64)> = Vec::new();
    for &b in &[256 * 1024usize, 1024 * 1024, 4 * 1024 * 1024] {
        if started.elapsed() > BUDGET { break; }
        let dst = probe_root.join(format!("bulk_buf_{}.bin", b));
        let t0 = Instant::now();
        let mut buf = vec![0u8; b];
        copy_file_atomic(&bulk_src, &dst, &mut buf).map_err(|e| TuneError::Io(e.into()))?;
        let elapsed = t0.elapsed().as_secs_f64().max(1e-9);
        let tp = s as f64 / elapsed;
        buf_results.push((b, tp));
        if tp > best_tp * 1.1 {
            best_tp = tp;
            best_buf = b;
        }
        let _ = fs::remove_file(&dst);
    }
    for &(b, tp) in &buf_results {
        if b == 1024 * 1024 && tp >= best_tp * 0.9 {
            best_buf = 1024 * 1024;
            best_tp = tp.max(best_tp);
            break;
        }
    }
    let bw = if best_tp > 0.0 { best_tp } else { est_speed };

    let mut bulk_concurrency = 1usize;
    if started.elapsed() <= BUDGET {
        let dst1 = probe_root.join("bulk_dual_1.bin");
        let dst2 = probe_root.join("bulk_dual_2.bin");
        let t0 = Instant::now();
        let h1 = std::thread::spawn({
            let src = bulk_src.clone();
            let dst = dst1.clone();
            move || {
                let mut buf = vec![0u8; best_buf];
                copy_file_atomic(&src, &dst, &mut buf)
            }
        });
        let h2 = std::thread::spawn({
            let src = bulk_src.clone();
            let dst = dst2.clone();
            move || {
                let mut buf = vec![0u8; best_buf];
                copy_file_atomic(&src, &dst, &mut buf)
            }
        });
        let r1 = h1.join().map_err(|_| TuneError::Io(io::Error::other("thread panicked")))?;
        let r2 = h2.join().map_err(|_| TuneError::Io(io::Error::other("thread panicked")))?;
        r1.map_err(|e| TuneError::Io(e.into()))?;
        r2.map_err(|e| TuneError::Io(e.into()))?;
        let elapsed = t0.elapsed().as_secs_f64().max(1e-9);
        let dual_tp = (2 * s) as f64 / elapsed;
        if dual_tp >= bw * 1.15 {
            bulk_concurrency = 2;
        }
        let _ = fs::remove_file(&dst1);
        let _ = fs::remove_file(&dst2);
    }

    let hard_link_src = probe_root.join("hl_src.txt");
    fs::write(&hard_link_src, b"x").map_err(TuneError::Io)?;
    let hard_link_dst = probe_root.join("hl_dst.txt");
    let hard_links = fs::hard_link(&hard_link_src, &hard_link_dst).is_ok();
    let _ = fs::remove_file(&hard_link_dst);

    let sym_src = probe_root.join("sym_src.txt");
    fs::write(&sym_src, b"x").map_err(TuneError::Io)?;
    let sym_dst = probe_root.join("sym_link");
    let symlinks = create_symlink_best_effort(&sym_src, &sym_dst);
    let _ = fs::remove_file(&sym_dst);

    let threshold_raw = f_cost * bw;
    let bulk_threshold = clamp_pow2(threshold_raw, 64 * 1024, 8 * 1024 * 1024);

    Ok(Tuning {
        metadata_concurrency,
        bulk_concurrency,
        buffer_size: best_buf,
        bulk_threshold,
        hard_links,
        symlinks,
        free_bytes,
        note: None,
    })
}

fn create_symlink_best_effort(src: &Path, dst: &Path) -> bool {
    #[cfg(unix)]
    { std::os::unix::fs::symlink(src, dst).is_ok() }
    #[cfg(windows)]
    { std::os::windows::fs::symlink_file(src, dst).is_ok() }
    #[cfg(not(any(unix, windows)))]
    { let _ = (src, dst); false }
}

fn clamp_pow2(value: f64, min: u64, max: u64) -> u64 {
    let mut best = min;
    let mut best_diff = (min as f64 - value).abs();
    let mut pow = 1u64;
    while pow <= max {
        if pow >= min {
            let diff = (pow as f64 - value).abs();
            if diff < best_diff {
                best_diff = diff;
                best = pow;
            }
        }
        pow = pow.saturating_mul(2);
    }
    best.clamp(min, max).max(min)
}

fn free_bytes_for(dest: &Path) -> u64 {
    let disks = Disks::new_with_refreshed_list();
    let mut best_len = 0usize;
    let mut free = 0u64;
    for d in disks.list() {
        let mp = d.mount_point();
        if dest.starts_with(mp) && mp.as_os_str().len() > best_len {
            best_len = mp.as_os_str().len();
            free = d.available_space();
        }
    }
    free
}


