# 083 — Storage Benchmarking (`find-test`)

## Overview

Add a new `find-test` binary to the server crate that measures write throughput
and read latency for any configured `ContentStore` backend.  The goal is
reproducible numbers that drive decisions about chunk size, backend choice, and
SQLite connection pool sizing.

`find-test` is a **server-side tool** — it runs on the same machine as
`find-server`, reads `server.toml` directly, and accesses store files without
going through HTTP.  It is distributed alongside `find-server` in the server
package.  There is no client-side counterpart.

---

## CLI

```
find-test bench-storage [OPTIONS]

Options:
  --config <PATH>        Path to server.toml (default: /etc/find-anything/server.toml)
  --data-dir <PATH>      Override data_dir from config
  --backend <NAME>       Which named backend instance to benchmark.
                         Can be repeated to select a subset.
                         Default: all configured backends.
  --mode <write|read|all>  What to measure.  Default: all
  --blobs <N>            Number of blobs to write in the write phase.  Default: 1000
  --blob-size-kb <N>     Average blob size in KB.  Default: 4
  --reads <N>            Number of get_lines() calls in the read phase.  Default: 2000
  --concurrency <N>      Threads for the read phase.  Can be repeated to sweep:
                         --concurrency 1 --concurrency 4 --concurrency 16
  --json                 Emit JSON instead of human-readable table
```

Example — compare all configured backends in one pass:

```
$ find-test bench-storage --config /etc/find-anything/server.toml \
    --blobs 5000 --blob-size-kb 4 --reads 10000 \
    --concurrency 1 --concurrency 8 --concurrency 32
```

Sample output:

```
Backend       Write                    Read (c=1)           Read (c=8)           Read (c=32)
              MB/s    blobs/s          p50    p95    p99    p50    p95    p99    p50    p95    p99
zip           12.4    3100             0.8ms  2.1ms  4.3ms  0.9ms  2.4ms  5.1ms  1.1ms  3.0ms  6.2ms
sqlite_1k     18.7    4680             0.2ms  0.5ms  1.1ms  0.2ms  0.6ms  1.3ms  0.3ms  0.7ms  1.8ms
sqlite_4k     21.3    5325             0.1ms  0.3ms  0.7ms  0.1ms  0.3ms  0.8ms  0.2ms  0.5ms  1.2ms
sqlite_10k    22.0    5500             0.1ms  0.2ms  0.5ms  0.1ms  0.3ms  0.7ms  0.2ms  0.4ms  1.0ms
```

---

## Benchmark library (`crates/content-store/src/bench.rs`)

Pure library functions — no CLI concerns, usable in tests.

```rust
pub struct WriteBenchOpts {
    pub num_blobs: usize,
    pub blob_size_bytes: usize,
    /// RNG seed for reproducibility. Same seed → same blobs every run.
    pub seed: u64,
}

pub struct WriteBenchResult {
    pub blobs_written: usize,
    pub bytes_written: u64,
    pub elapsed: Duration,
}

impl WriteBenchResult {
    pub fn mb_per_sec(&self) -> f64 { ... }
    pub fn blobs_per_sec(&self) -> f64 { ... }
}

pub struct ReadBenchOpts {
    pub num_reads: usize,
    pub concurrency: usize,
    /// Keys to sample from — returned by `bench_write` or loaded from an existing store.
    pub keys: Vec<ContentKey>,
    pub seed: u64,
}

pub struct ReadBenchResult {
    pub reads: usize,
    pub concurrency: usize,
    /// Per-call durations, sorted ascending. Index directly for percentiles.
    pub latencies: Vec<Duration>,
}

impl ReadBenchResult {
    /// Return the duration at the given percentile (0.0–1.0).
    pub fn percentile(&self, p: f64) -> Duration { ... }
    pub fn ops_per_sec(&self) -> f64 { ... }
}

/// Write phase: generate synthetic blobs, put them into the store.
/// Returns the list of inserted ContentKeys for use in the read phase.
pub fn bench_write(
    store: &dyn ContentStore,
    opts: &WriteBenchOpts,
) -> Result<(WriteBenchResult, Vec<ContentKey>)>;

/// Read phase: call get_lines() against random keys, record per-call latency.
pub fn bench_read(
    store: &dyn ContentStore,
    opts: &ReadBenchOpts,
) -> Result<ReadBenchResult>;
```

### Write phase detail

Generates `num_blobs` synthetic text blobs using a seeded PRNG.  Each blob is
`blob_size_bytes` of lorem-ipsum-style line-delimited text (lines of ~60–80
chars) so chunking behaviour matches realistic content.  Calls `store.put()`
for each, recording wall-clock elapsed time around the full loop.

The key for each blob is the blake3 hash of its content — same as the real
write path.

### Read phase detail

Samples `num_reads` (key, line-range) pairs uniformly from the provided key
list, where the line range is chosen to guarantee a cache-miss-resistant hit
(mid-file offset, not line 0).  Dispatches calls across `concurrency` threads
using `std::thread::scope` — no async, just raw OS threads so we measure
uncontended store throughput per thread.

Each call's elapsed time is recorded in a `Vec<Duration>`, which is sorted at
the end to compute percentiles.  No external histogram crate needed.

---

## `find-test` binary (`crates/server/src/find_test_main.rs`)

New binary in the existing server crate — it already depends on
`find-content-store` and `find-common` (for config parsing), so no new crate
or dependency additions are needed beyond `rand`.

```
[[bin]]
name = "find-test"
path = "src/find_test_main.rs"
```

The binary:
1. Parses CLI args (clap)
2. Loads `server.toml` → `ServerAppConfig`
3. Resolves `data_dir` (CLI override wins)
4. Opens each selected backend via `open_content_store()` (reused from `lib.rs`)
5. For each backend, runs the selected bench phases
6. Prints the result table (or JSON)

---

## Files changed

| File | Change |
|------|--------|
| `crates/content-store/src/bench.rs` | New: bench library |
| `crates/content-store/src/lib.rs` | Export `pub mod bench` |
| `crates/content-store/Cargo.toml` | Add `rand = "0.9"` |
| `crates/server/src/find_test_main.rs` | New: `find-test` binary entry point |
| `crates/server/Cargo.toml` | Add `[[bin]]` entry for `find-test` |

`open_content_store()` in `crates/server/src/lib.rs` needs to be made
accessible from `find_test_main.rs` — move it to `pub(crate)` or extract to a
small helper module.

---

## Testing strategy

- **Library unit test** in `bench.rs`: run `bench_write` + `bench_read` against
  a `SqliteContentStore` in a temp dir with small opts (50 blobs, 100 reads,
  concurrency 2).  Assert `mb_per_sec() > 0.0` and `percentile(0.99) > Duration::ZERO`.
- **Smoke test** (manual or CI): `find-test bench-storage --blobs 10 --reads 20`
  exits 0 and prints a non-empty table.
- No numeric assertions — values vary by machine.

---

## Non-goals for this plan

- Cold-read benchmarking (requires root to drop OS page cache — skip for now)
- Persisting results to disk / history tracking
- HTTP-based benchmark that goes through the live server
- Subcommands other than `bench-storage` (reserved for future `find-test` growth)
