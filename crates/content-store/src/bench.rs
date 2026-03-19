//! Storage benchmarking library for `ContentStore` implementations.
//!
//! Used by the `find-test` binary. No CLI concerns — pure measurement logic.

use std::sync::Mutex;
use std::time::{Duration, Instant};

use anyhow::Result;
use rand::SeedableRng;
use rand::rngs::StdRng;
use rand::Rng;

use crate::{ContentKey, ContentStore};

// ── Write benchmark ───────────────────────────────────────────────────────────

pub struct WriteBenchOpts {
    pub num_blobs: usize,
    /// Median blob size in bytes. Actual sizes follow a log-normal distribution
    /// so that the mix of small/medium/large files is realistic.
    pub blob_size_bytes: usize,
    /// Log-normal σ (spread). 0.0 = fixed size; 1.5 = realistic file-size spread
    /// (~1 KB–10 MB from a 4 KB median). Ignored when `blob_size_bytes` is 0.
    pub blob_size_sigma: f64,
    /// RNG seed. Same seed → identical blobs and keys on every run.
    pub seed: u64,
    /// Vocabulary for synthetic text generation. Should be frequency-ordered
    /// (most common words first); sampling is biased toward the front of the
    /// list to approximate a Zipf distribution.
    pub wordlist: Vec<String>,
}

pub struct WriteBenchResult {
    pub blobs_written: usize,
    pub bytes_written: u64,
    pub elapsed: Duration,
}

impl WriteBenchResult {
    pub fn mb_per_sec(&self) -> f64 {
        let secs = self.elapsed.as_secs_f64();
        if secs == 0.0 { return 0.0; }
        self.bytes_written as f64 / secs / 1_048_576.0
    }

    pub fn blobs_per_sec(&self) -> f64 {
        let secs = self.elapsed.as_secs_f64();
        if secs == 0.0 { return 0.0; }
        self.blobs_written as f64 / secs
    }
}

/// Write phase: generate and store synthetic blobs.
///
/// Returns the result and a list of `(key, line_count)` pairs for use in `bench_read`.
/// Blob sizes follow a log-normal distribution around `blob_size_bytes` when
/// `blob_size_sigma > 0`, producing a realistic mix of small and large files.
pub fn bench_write(
    store: &dyn ContentStore,
    opts: &WriteBenchOpts,
) -> Result<(WriteBenchResult, Vec<(ContentKey, usize)>)> {
    let mut key_meta: Vec<(ContentKey, usize)> = Vec::with_capacity(opts.num_blobs);
    let mut bytes_written: u64 = 0;

    // Separate RNG for size sampling so blob content is unaffected by sigma.
    let mut size_rng = StdRng::seed_from_u64(opts.seed.wrapping_add(0x5eed_5eed_5eed_5eed));

    let t0 = Instant::now();
    for i in 0..opts.num_blobs {
        let size = sample_lognormal(&mut size_rng, opts.blob_size_bytes, opts.blob_size_sigma);
        let blob = synthetic_blob(opts.seed, i, size, &opts.wordlist);
        let key = blob_key(opts.seed, i);
        let line_count = blob.bytes().filter(|&b| b == b'\n').count();
        bytes_written += blob.len() as u64;
        store.put(&key, &blob)?;
        key_meta.push((key, line_count));
    }
    let elapsed = t0.elapsed();

    Ok((
        WriteBenchResult {
            blobs_written: opts.num_blobs,
            bytes_written,
            elapsed,
        },
        key_meta,
    ))
}

// ── Read benchmark ────────────────────────────────────────────────────────────

pub struct ReadBenchOpts {
    pub num_reads: usize,
    pub concurrency: usize,
    /// Keys and their line counts — returned by `bench_write`.
    /// Line count is used to pick read windows proportional to blob size.
    pub keys: Vec<(ContentKey, usize)>,
    pub seed: u64,
}

pub struct ReadBenchResult {
    pub reads: usize,
    pub concurrency: usize,
    /// Per-call durations, sorted ascending.  Index by percentile directly.
    pub latencies: Vec<Duration>,
}

impl ReadBenchResult {
    /// Duration at the given percentile (0.0–1.0).
    pub fn percentile(&self, p: f64) -> Duration {
        if self.latencies.is_empty() {
            return Duration::ZERO;
        }
        let idx = ((self.latencies.len() as f64 - 1.0) * p.clamp(0.0, 1.0)) as usize;
        self.latencies[idx]
    }

    /// Operations per second, computed as (total reads) / (wall time per thread).
    pub fn ops_per_sec(&self) -> f64 {
        if self.latencies.is_empty() || self.concurrency == 0 {
            return 0.0;
        }
        let total_secs: f64 = self.latencies.iter().map(|d| d.as_secs_f64()).sum();
        let wall_secs = total_secs / self.concurrency as f64;
        if wall_secs == 0.0 { return 0.0; }
        self.reads as f64 / wall_secs
    }
}

/// Read phase: call `get_lines()` against random keys across multiple threads.
pub fn bench_read(store: &dyn ContentStore, opts: &ReadBenchOpts) -> Result<ReadBenchResult> {
    if opts.keys.is_empty() || opts.num_reads == 0 {
        return Ok(ReadBenchResult {
            reads: 0,
            concurrency: opts.concurrency,
            latencies: vec![],
        });
    }

    let concurrency = opts.concurrency.max(1);
    let all_latencies: Mutex<Vec<Duration>> = Mutex::new(Vec::with_capacity(opts.num_reads));

    std::thread::scope(|scope| {
        let reads_per_thread = opts.num_reads.div_ceil(concurrency);

        for t in 0..concurrency {
            let latencies = &all_latencies;
            let keys = &opts.keys;
            let seed = opts.seed ^ (t as u64).wrapping_mul(0x9e3779b97f4a7c15);
            let reads_this_thread = if t == concurrency - 1 {
                // Last thread handles the remainder.
                opts.num_reads.saturating_sub(reads_per_thread * (concurrency - 1))
            } else {
                reads_per_thread
            };

            scope.spawn(move || {
                let mut rng = StdRng::seed_from_u64(seed);
                let mut local: Vec<Duration> = Vec::with_capacity(reads_this_thread);

                for _ in 0..reads_this_thread {
                    let (key, line_count) = &keys[rng.random_range(0..keys.len())];
                    // Window size: ~10% of the blob's lines, clamped to [5, 500].
                    // This keeps read ranges proportional to blob size — a 50-line
                    // file gets a 5-line window, a 5000-line file gets a 500-line window.
                    let window = (line_count / 10).clamp(5, 500);
                    let max_lo = line_count.saturating_sub(window).max(1);
                    let lo = if max_lo > 1 { rng.random_range(1..max_lo) } else { 1 };
                    let hi = lo + window;

                    let t0 = Instant::now();
                    let _ = store.get_lines(key, lo, hi);
                    local.push(t0.elapsed());
                }

                latencies.lock().unwrap().extend(local);
            });
        }
    });

    let mut latencies = all_latencies.into_inner().unwrap();
    latencies.sort_unstable();

    Ok(ReadBenchResult {
        reads: opts.num_reads,
        concurrency,
        latencies,
    })
}

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Sample a blob size from a log-normal distribution with the given median.
///
/// Uses the Box-Muller transform to produce a standard normal variate, then
/// exponentiates: `size = exp(ln(median) + σ * N(0,1))`.
/// When `sigma == 0.0` or `median == 0`, returns `median` unchanged.
/// Result is clamped to a minimum of 64 bytes.
fn sample_lognormal(rng: &mut StdRng, median: usize, sigma: f64) -> usize {
    if sigma == 0.0 || median == 0 {
        return median;
    }
    let u1 = rng.random::<f64>().max(1e-10);
    let u2 = rng.random::<f64>();
    let z = (-2.0 * u1.ln()).sqrt() * (std::f64::consts::TAU * u2).cos();
    (((median as f64).ln() + sigma * z).exp() as usize).max(64)
}

/// Generate a deterministic content key for blob index `i` under `seed`.
pub fn blob_key(seed: u64, i: usize) -> ContentKey {
    // 64 hex chars from two u64 values derived from seed and index.
    let a = seed.wrapping_add(i as u64).wrapping_mul(6364136223846793005);
    let b = (i as u64).wrapping_add(1).wrapping_mul(seed ^ 0xdeadbeefcafe1234);
    ContentKey::new(format!("{a:016x}{b:016x}{a:016x}{b:016x}").as_str())
}

/// Generate a deterministic synthetic text blob of approximately `target_bytes`.
///
/// Words are drawn from `wordlist` with a Zipf-like bias: squaring a uniform
/// variate before indexing means the front of the list (common words) is sampled
/// much more often than the tail, approximating natural language frequency
/// distribution and producing compression ratios representative of real text.
fn synthetic_blob(seed: u64, i: usize, target_bytes: usize, wordlist: &[String]) -> String {
    if target_bytes == 0 || wordlist.is_empty() {
        return String::new();
    }

    let n = wordlist.len() as f64;
    let mut rng = StdRng::seed_from_u64(seed ^ (i as u64).wrapping_mul(0x517cc1b727220a95));
    let mut out = String::with_capacity(target_bytes + 120);

    while out.len() < target_bytes {
        let target_line_len = rng.random_range(40..=80usize);
        let mut line = String::new();
        while line.len() < target_line_len {
            if !line.is_empty() {
                line.push(' ');
            }
            // Zipf approximation: square a uniform variate so low indices
            // (common words) are sampled far more often than high indices.
            let u: f64 = rng.random();
            let idx = (u * u * n) as usize;
            line.push_str(&wordlist[idx]);
        }
        out.push_str(&line);
        out.push('\n');
    }

    out
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::SqliteContentStore;
    use tempfile::TempDir;

    fn make_store() -> (SqliteContentStore, TempDir) {
        let dir = TempDir::new().unwrap();
        (SqliteContentStore::open(dir.path(), None, None, None).unwrap(), dir)
    }

    fn test_wordlist() -> Vec<String> {
        "the a and to of in is it that for on are with as be this was by from or an have".
            split_whitespace().map(str::to_string).collect()
    }

    #[test]
    fn write_bench_produces_nonzero_throughput() {
        let (store, _dir) = make_store();
        let opts = WriteBenchOpts {
            num_blobs: 50, blob_size_bytes: 512, blob_size_sigma: 0.0, seed: 42,
            wordlist: test_wordlist(),
        };
        let (result, keys) = bench_write(&store, &opts).unwrap();
        assert_eq!(result.blobs_written, 50);
        assert!(result.bytes_written > 0);
        assert!(result.mb_per_sec() > 0.0);
        assert!(result.blobs_per_sec() > 0.0);
        assert_eq!(keys.len(), 50);
    }

    #[test]
    fn read_bench_produces_nonzero_latencies() {
        let (store, _dir) = make_store();
        let write_opts = WriteBenchOpts {
            num_blobs: 50, blob_size_bytes: 512, blob_size_sigma: 0.0, seed: 99,
            wordlist: test_wordlist(),
        };
        let (_, keys) = bench_write(&store, &write_opts).unwrap();

        let read_opts = ReadBenchOpts { num_reads: 100, concurrency: 2, keys, seed: 99 };
        let result = bench_read(&store, &read_opts).unwrap();
        assert_eq!(result.reads, 100);
        assert_eq!(result.latencies.len(), 100);
        assert!(result.percentile(0.99) > Duration::ZERO);
    }

    #[test]
    fn lognormal_sizes_vary_around_median() {
        let mut rng = StdRng::seed_from_u64(42);
        let median = 4096usize;
        let sizes: Vec<usize> = (0..200).map(|_| sample_lognormal(&mut rng, median, 1.5)).collect();
        // With σ=1.5 the distribution spans ~3 orders of magnitude; all values must be ≥ 64.
        assert!(sizes.iter().all(|&s| s >= 64));
        // At least some blobs should be smaller and some larger than the median.
        assert!(sizes.iter().any(|&s| s < median));
        assert!(sizes.iter().any(|&s| s > median));
    }

    #[test]
    fn blob_keys_are_unique() {
        let keys: Vec<ContentKey> = (0..100).map(|i| blob_key(1234, i)).collect();
        let unique: std::collections::HashSet<_> = keys.iter().map(|k| k.as_str()).collect();
        assert_eq!(unique.len(), 100);
    }

    #[test]
    fn synthetic_blob_reaches_target_size() {
        let wl = test_wordlist();
        for target in [128, 512, 4096, 16384] {
            let blob = synthetic_blob(7, 0, target, &wl);
            assert!(blob.len() >= target, "blob len {} < target {target}", blob.len());
        }
    }

    #[test]
    fn read_bench_empty_keys_is_ok() {
        let (store, _dir) = make_store();
        let opts = ReadBenchOpts { num_reads: 10, concurrency: 1, keys: vec![], seed: 0 };
        let result = bench_read(&store, &opts).unwrap();
        assert_eq!(result.reads, 0);
    }
}
