# 082 — Content Store Backends: Config, Benchmarking, SQLite

## Overview

Plan 081 introduced the `ContentStore` trait and `ZipContentStore`. This plan covers three follow-on areas:

1. **Server configuration** to select which storage backend(s) to use at runtime
2. **Benchmarking harness** to measure read/write performance across implementations
3. **`SqliteContentStore`** — a new implementation storing content as gzip BLOBs in SQLite

---

## 1. Server Configuration — Backend Selection

### Config additions (`[storage]` section in `server.toml`)

```toml
[storage]
# List of content store backends to use.
# A single entry uses that backend directly.
# Multiple entries write to all simultaneously; reads use the first with a hit.
# Options per entry: "zip", "sqlite"
# Default: ["zip"]
backends = ["zip"]

# Dual-write example:
# backends = ["zip", "sqlite"]
```

Parse into a new struct in `find-common`:

```rust
#[derive(Deserialize, Clone)]
pub struct StorageConfig {
    #[serde(default = "StorageConfig::default_backends")]
    pub backends: Vec<StorageBackend>,
}

impl StorageConfig {
    fn default_backends() -> Vec<StorageBackend> { vec![StorageBackend::Zip] }
}

#[derive(Deserialize, Clone, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum StorageBackend { Zip, Sqlite }
```

### `MultiContentStore` — dual-write delegator

A trivial `ContentStore` implementation that forwards writes to all inner stores and reads from the first store that returns `Some`:

```rust
pub struct MultiContentStore {
    stores: Vec<Arc<dyn ContentStore>>,
}

impl ContentStore for MultiContentStore {
    fn put(&self, key: ContentKey, blob: Vec<u8>) -> Result<()> {
        for s in &self.stores { s.put(key.clone(), blob.clone())?; }
        Ok(())
    }
    fn delete(&self, key: &ContentKey) -> Result<()> {
        for s in &self.stores { s.delete(key)?; }
        Ok(())
    }
    fn get_lines(&self, key: &ContentKey, lo: usize, hi: usize) -> Result<Option<Vec<String>>> {
        for s in &self.stores {
            if let Some(lines) = s.get_lines(key, lo, hi)? { return Ok(Some(lines)); }
        }
        Ok(None)
    }
    fn contains(&self, key: &ContentKey) -> Result<bool> {
        self.stores.first().map(|s| s.contains(key)).unwrap_or(Ok(false))
    }
    fn compact(&self, live_keys: &[&ContentKey]) -> Result<CompactResult> {
        let mut total = CompactResult::default();
        for s in &self.stores {
            let r = s.compact(live_keys)?;
            total.bytes_removed += r.bytes_removed;
            total.archives_removed += r.archives_removed;
        }
        Ok(total)
    }
}
```

### Server startup wiring (`find-server/src/lib.rs`)

```rust
fn open_backend(b: &StorageBackend, data_dir: &Path) -> Result<Arc<dyn ContentStore>> {
    match b {
        StorageBackend::Zip    => Ok(Arc::new(ZipContentStore::open(data_dir)?)),
        StorageBackend::Sqlite => Ok(Arc::new(SqliteContentStore::open(data_dir)?)),
    }
}

let content_store: Arc<dyn ContentStore> = match config.storage.backends.as_slice() {
    []  => bail!("storage.backends must not be empty"),
    [b] => open_backend(b, data_dir)?,
    _   => {
        let stores = config.storage.backends.iter()
            .map(|b| open_backend(b, data_dir))
            .collect::<Result<Vec<_>>>()?;
        Arc::new(MultiContentStore { stores })
    }
};
```

### Files changed

- `crates/common/src/config.rs` — add `StorageConfig`, `StorageBackend`
- `crates/server/src/config.rs` — add `storage: StorageConfig` field
- `crates/content-store/src/lib.rs` — export `MultiContentStore`
- `crates/content-store/src/multi_store.rs` — new file, `MultiContentStore`
- `crates/server/src/lib.rs` — backend selection at startup

---

## 2. Benchmarking Harness

### Goals

- Measure **write throughput** (bytes/sec for `put()`)
- Measure **read latency** (ms per `get_lines()` for random keys)
- Work against any `ContentStore` implementation
- Runnable as a standalone CLI command: `find-admin bench-storage`

### Approach

Add a `bench_storage` function in `find-content-store`:

```rust
pub struct BenchResult {
    pub write_bytes: u64,
    pub write_elapsed: Duration,
    pub read_count: u64,
    pub read_elapsed: Duration,
}

pub fn bench_storage(store: &dyn ContentStore, opts: BenchOpts) -> Result<BenchResult> {
    // Write phase: generate `opts.num_blobs` random blobs of `opts.blob_size_kb` KB each
    // Read phase: random-sample `opts.num_reads` keys from written set, call get_lines()
}
```

`find-admin bench-storage [--backend zip|sqlite|multi] [--blobs 1000] [--blob-size-kb 4] [--reads 500]`

Output:

```
Write: 1000 blobs × 4 KB = 4.0 MB in 0.82s → 4.9 MB/s
Read:  500 random reads in 0.34s → 1.5 ms/read
```

### Files changed

- `crates/content-store/src/bench.rs` — `BenchOpts`, `BenchResult`, `bench_storage()`
- `crates/content-store/src/lib.rs` — export `bench`
- `crates/admin/src/main.rs` or `crates/admin/src/bench_cmd.rs` — `bench-storage` subcommand

---

## 3. `SqliteContentStore`

### Motivation

The current `ZipContentStore` has a significant read inefficiency: `read_chunk()` loads an entire 10 MB ZIP archive into memory to retrieve a single chunk. SQLite offers O(log n) PK lookups, returning only the requested row — ideal for random-access read patterns.

### Schema

A single `blobs` table in `content_blobs.db`:

```sql
CREATE TABLE IF NOT EXISTS blobs (
    key        TEXT    NOT NULL,  -- blake3 hex hash
    chunk_num  INTEGER NOT NULL,  -- 0-based chunk index
    start_line INTEGER NOT NULL,
    end_line   INTEGER NOT NULL,
    data       BLOB    NOT NULL,  -- gzip-compressed chunk text
    PRIMARY KEY (key, chunk_num)
);
CREATE INDEX IF NOT EXISTS idx_blobs_key_start ON blobs(key, start_line);
```

No separate metadata database needed — everything is in one table.

### Implementation

```rust
pub struct SqliteContentStore {
    conn: Mutex<Connection>,  // single write connection (WAL mode)
    data_dir: PathBuf,
}

impl SqliteContentStore {
    pub fn open(data_dir: &Path) -> Result<Self> { ... }
}

impl ContentStore for SqliteContentStore {
    fn put(&self, key: ContentKey, blob: Vec<u8>) -> Result<()> {
        // chunk blob into ~1 KB pieces
        // gzip each chunk
        // INSERT OR IGNORE INTO blobs for each chunk
    }
    fn delete(&self, key: &ContentKey) -> Result<()> {
        // DELETE FROM blobs WHERE key = ?
    }
    fn get_lines(&self, key: &ContentKey, lo: usize, hi: usize) -> Result<Option<Vec<String>>> {
        // SELECT data, start_line FROM blobs WHERE key=? AND start_line<=hi AND end_line>=lo ORDER BY chunk_num
        // decompress each chunk, slice to requested line range
    }
    fn contains(&self, key: &ContentKey) -> Result<bool> {
        // SELECT COUNT(*) FROM blobs WHERE key=? LIMIT 1
    }
    fn compact(&self, live_keys: &[&ContentKey]) -> Result<CompactResult> {
        // DELETE FROM blobs WHERE key NOT IN (...)
        // measure bytes freed via page_count * page_size before/after
        // PRAGMA incremental_vacuum or VACUUM to reclaim pages
    }
}
```

### Read performance advantage

For a `get_lines(key, lo=100, hi=110)` call:
- **ZIP**: open archive (10 MB), seek to member, decompress all chunks for key, filter lines
- **SQLite**: `WHERE key=? AND start_line<=110 AND end_line>=100` → PK index hit, returns 1-2 rows; decompress only those rows

### Compaction

No separate metadata DB or archive rewriting needed. Compaction is:
1. `DELETE FROM blobs WHERE key NOT IN (...live_keys...)`
2. `PRAGMA wal_checkpoint(TRUNCATE)`
3. Optional `VACUUM` to reclaim disk space (can be deferred)

WAL mode means readers are not blocked during compaction.

### Files changed

- `crates/content-store/src/sqlite_store/mod.rs` — new, `SqliteContentStore`
- `crates/content-store/src/sqlite_store/db.rs` — schema + SQL helpers
- `crates/content-store/src/lib.rs` — export `SqliteContentStore`
- `crates/content-store/Cargo.toml` — no new deps (already has `rusqlite`)

---

## 4. Other Considerations

### Migration path

No migration. When switching backends, delete the old store's data files and run a full re-scan. This is sufficient for testing and avoids migration code complexity.

### Chunk size tuning and named instances

To enable side-by-side comparison of configurations, `backends` should support named instances with per-instance options rather than just a type string:

```toml
[storage]
backends = [
    { name = "zip_default", type = "zip" },
    { name = "sqlite_1k",   type = "sqlite", chunk_size_kb = 1 },
    { name = "sqlite_4k",   type = "sqlite", chunk_size_kb = 4 },
    { name = "sqlite_10k",  type = "sqlite", chunk_size_kb = 10 },
]
```

Each named instance gets its own data directory (e.g. `data_dir/stores/sqlite_4k/`). The `name` also appears in benchmark output, making it easy to compare results across configurations in a single run. The `chunk_size_kb` field (and any future per-backend options) is defined per-entry; defaults apply when omitted.

This allows spinning up three SQLite backends with different chunk sizes and a ZIP baseline simultaneously, populating all four via a single scan, then benchmarking all four in one pass.

### Connection pool for reads

`SqliteContentStore` uses a single `Mutex<Connection>`. For read-heavy workloads, consider a small pool of read-only connections (e.g. 4) alongside the single write connection. This mirrors how `ZipContentStore` handles concurrent reads today.

### Feature flags

No feature flags needed. Both implementations compile unconditionally; the active backend is selected by config at runtime.

---

## Read Performance Testing Strategy

> **Scope note:** A rigorous read benchmark is important but out of scope for this first pass. Captured here for planning purposes.

The core question is: **how does random-access read latency scale with corpus size and concurrency across backends?** Interactive use (manually searching and observing) is not a reliable way to answer this — it conflates network, UI rendering, and FTS query time with content retrieval time, and doesn't produce reproducible numbers.

### What to measure

- **p50 / p95 / p99 latency** for `get_lines()` calls at varying corpus sizes (10k, 100k, 1M files)
- **Throughput under concurrency** — N simultaneous `get_lines()` calls (simulating N open search results)
- **Cold vs warm** — first read from a freshly opened store vs. repeated reads (OS page cache effects)

### Proposed approach

A dedicated benchmark binary (or `find-admin bench-read`) that:

1. Populates a store with a known corpus (e.g. 100k synthetic files, controlled chunk size)
2. Generates a random workload of `get_lines()` calls against random keys and line offsets
3. Reports p50/p95/p99 latency and total throughput per backend

The named-instance config (above) means a single scan populates all backends simultaneously, so the benchmark runs against identical data across all configurations.

Concurrency is the key variable for real-world relevance — the server handles many simultaneous search requests. The benchmark should sweep from 1 to 32 concurrent readers.

**Not covered by `find-admin bench-storage`** (the write benchmark): that tool focuses on write throughput and is a separate concern. Read benchmarking is a distinct workload and warrants its own command.

---

## Implementation Order

1. `MultiContentStore` + named-instance config parsing
2. `SqliteContentStore` (self-contained new implementation)
3. Write benchmarking harness (`find-admin bench-storage`)
4. Read performance benchmark (separate planning pass)

---

## Testing Strategy

- `SqliteContentStore`: same unit test suite as `ZipContentStore` (`put/get_lines/delete/compact` round-trips)
- `MultiContentStore`: test that writes go to both stores, reads fall through correctly
- Bench: smoke test that it runs without panic and returns non-zero throughput numbers
- Config parsing: unit tests for `StorageConfig` deserialization
