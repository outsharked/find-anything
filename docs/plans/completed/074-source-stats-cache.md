# Source Stats Cache Implementation Plan

> **For agentic workers:** REQUIRED: Use superpowers:subagent-driven-development (if subagents available) or superpowers:executing-plans to implement this plan. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace live per-request DB queries in `GET /api/v1/stats` with a cached value that is maintained incrementally during indexing and rebuilt fully on startup, daily, and on demand.

**Architecture:** A `SourceStatsCache` lives in `AppState` as an `Arc<RwLock<...>>`. It is populated by a full rebuild (all queries) at startup and daily alongside the compaction scan. During indexing, the worker applies a cheap delta after each batch — no full-table scans. `GET /api/v1/stats` reads the in-memory cache (instant). `find-admin status --refresh` passes `?refresh=true` to the endpoint, which forces a synchronous full rebuild before responding.

**Tech Stack:** Rust, rusqlite, tokio, existing `AppState`/worker/compaction patterns.

---

## Background and Current State

`GET /api/v1/stats` currently calls `query_source_stats` on every request, which runs three expensive full-table-scan queries against a 26 GB database:

- `get_stats` — `SELECT kind, COUNT(*), SUM(size) FROM files GROUP BY kind`
- `get_stats_by_ext` — full scan calling `file_ext(file_basename(path))` scalar on every row
- `get_fts_row_count` — `SELECT COUNT(*) FROM lines_fts`

These cause `find-admin status` to take ~10 seconds.

**Compaction stats are already cached correctly** (`AppState.compaction_stats`, persisted to `server.db`, refreshed daily). This plan follows the same pattern for source stats, adding incremental updates.

---

## What Gets Cached and How

| Stat | Incremental? | Full rebuild? |
|------|-------------|---------------|
| `total_files` | ✓ (count new/deleted files) | ✓ |
| `total_size` | ✓ (size from IndexFile; query old size before modify/delete) | ✓ |
| `by_kind` (count + size) | ✓ (same delta as above) | ✓ |
| `by_kind` (avg_extract_ms) | ✗ (stale between full rebuilds — intentionally omitted from delta) | ✓ |
| `by_ext` | ✗ (scalar fn on every row — only on full rebuild) | ✓ |
| `fts_row_count` | ✗ (FTS COUNT is expensive — only on full rebuild) | ✓ |
| `last_scan`, `history`, `indexing_error_count` | Not cached — live per-source queries (fast indexed reads) | N/A |

Incremental updates keep `total_files`, `total_size`, and `by_kind.count/size` accurate between full rebuilds. `by_kind.avg_extract_ms`, `by_ext`, and `fts_row_count` are slightly stale between daily rebuilds — that is acceptable.

**Note:** `last_scan`, `history`, and `indexing_error_count` are kept as live per-source queries in the route handler. These are fast (`meta` table lookup, indexed query), so they do not need caching. The route handler opens one `db::open_for_stats` connection per source for these, mirroring the existing code.

**New source window:** For a brand-new source (first batch ever), the delta is computed and applied, but `apply_delta` silently ignores it because the source does not yet exist in the cache. The cache will show zeros for that source until the next full rebuild fires (~30 s after startup or at daily cadence). This is an acceptable window.

---

## Files Changed

| File | Change |
|------|--------|
| `crates/server/src/stats_cache.rs` | **New.** `SourceStatsCache` type, full rebuild, incremental delta apply |
| `crates/server/src/lib.rs` | Add `source_stats_cache` field to `AppState`; trigger startup rebuild |
| `crates/server/src/routes/stats.rs` | Read from cache; support `?refresh=true` |
| `crates/server/src/worker/pipeline.rs` | `Phase1Outcome::Modified` carries `old_size`/`old_kind`; `process_file_phase1` queries them from existing record |
| `crates/server/src/worker/request.rs` | Collect delta per batch; `process_request_phase1` returns `Result<SourceStatsDelta>`; applied to cache in `process_request_async` after `spawn_blocking` completes |
| `crates/server/src/db/mod.rs` | `delete_files_phase1` queries size/kind before deleting; returns `DeleteDelta` |
| `crates/server/src/compaction.rs` | Trigger stats full rebuild in daily loop after compaction scan |
| `crates/client/src/admin_main.rs` | Add `--refresh` flag to `Status` subcommand |
| `crates/client/src/api.rs` | Add `refresh: bool` param to `get_stats` |
| `crates/server/tests/stats_cache.rs` | **New.** Integration tests |

---

## Task 1: `SourceStatsCache` type and full rebuild

**Files:**
- Create: `crates/server/src/stats_cache.rs`

- [ ] **Write the full rebuild function**

```rust
// crates/server/src/stats_cache.rs

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use find_common::api::{ExtStat, FileKind, KindStats, SourceStats};

/// In-memory cache of per-source stats.  Wrapped in Arc<RwLock<...>> in AppState.
#[derive(Default, Clone)]
pub struct SourceStatsCache {
    pub sources: Vec<CachedSourceStats>,
    /// Unix timestamp of the last full rebuild.
    pub rebuilt_at: Option<i64>,
}

#[derive(Clone, Default)]
pub struct CachedSourceStats {
    pub name: String,
    pub total_files: usize,
    pub total_size:  i64,
    pub by_kind:     HashMap<FileKind, KindStats>,
    /// Only populated on full rebuild.
    pub by_ext:      Vec<ExtStat>,
    /// Only populated on full rebuild.
    pub fts_row_count: i64,
}

/// Run all expensive queries for every source DB and store results in `cache`.
/// Called at startup, daily, and on `?refresh=true`.
pub fn full_rebuild(data_dir: &Path, cache: &Arc<std::sync::RwLock<SourceStatsCache>>) {
    let sources_dir = data_dir.join("sources");
    let mut sources: Vec<CachedSourceStats> = Vec::new();

    let rd = match std::fs::read_dir(&sources_dir) {
        Ok(rd) => rd,
        Err(e) => { tracing::warn!("stats_cache: cannot read sources dir: {e:#}"); return; }
    };

    for entry in rd.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("db") { continue; }
        let source_name = match path.file_stem().and_then(|s| s.to_str()) {
            Some(s) => s.to_string(),
            None => continue,
        };
        let conn = match crate::db::open_for_stats(&path) {
            Ok(c) => c,
            Err(e) => { tracing::debug!("stats_cache: skipping {source_name}: {e:#}"); continue; }
        };
        let (total_files, total_size, by_kind) = crate::db::get_stats(&conn).unwrap_or_default();
        let by_ext     = crate::db::get_stats_by_ext(&conn).unwrap_or_default();
        let fts_row_count = crate::db::get_fts_row_count(&conn).unwrap_or(0);
        sources.push(CachedSourceStats { name: source_name, total_files, total_size, by_kind, by_ext, fts_row_count });
    }

    sources.sort_by(|a, b| a.name.cmp(&b.name));

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;

    if let Ok(mut guard) = cache.write() {
        guard.sources = sources;
        guard.rebuilt_at = Some(now);
    }
    tracing::debug!("stats_cache: full rebuild complete");
}
```

- [ ] **Add `apply_delta` for incremental updates**

```rust
/// Per-source incremental delta — applied after each worker batch.
#[derive(Default)]
pub struct SourceStatsDelta {
    pub source: String,
    pub files_delta: i64,
    pub size_delta:  i64,
    /// Positive = added, negative = removed.
    pub kind_deltas: HashMap<FileKind, (i64, i64)>, // kind → (count_delta, size_delta)
}

impl SourceStatsCache {
    pub fn apply_delta(&mut self, delta: &SourceStatsDelta) {
        if let Some(s) = self.sources.iter_mut().find(|s| s.name == delta.source) {
            s.total_files = (s.total_files as i64 + delta.files_delta).max(0) as usize;
            s.total_size  = (s.total_size  + delta.size_delta).max(0);
            for (kind, (count_d, size_d)) in &delta.kind_deltas {
                let e = s.by_kind.entry(kind.clone()).or_default();
                e.count = (e.count as i64 + count_d).max(0) as usize;
                e.size  = (e.size  + size_d).max(0);
            }
        }
        // If source not yet in cache (e.g. first-ever file for a new source),
        // leave it for the next full rebuild to populate.
    }
}
```

- [ ] **Verify it compiles:** `cargo check -p find-server`

---

## Task 2: Wire `SourceStatsCache` into `AppState`

**Files:**
- Modify: `crates/server/src/lib.rs`

- [ ] **Add field to `AppState`**

```rust
// in AppState struct
pub source_stats_cache: Arc<std::sync::RwLock<SourceStatsCache>>,
```

- [ ] **Initialise in `create_app_state`**

```rust
// after existing Arc constructions
let source_stats_cache = Arc::new(std::sync::RwLock::new(SourceStatsCache::default()));

// in the Arc::new(AppState { ... }) block
source_stats_cache: Arc::clone(&source_stats_cache),
```

- [ ] **Trigger startup full rebuild** (after the existing 30 s settle, same pattern as compaction)

```rust
// after compaction::start_compaction_scanner(...)
{
    let cache = Arc::clone(&source_stats_cache);
    let dd    = data_dir.clone();
    tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_secs(30)).await;
        tokio::task::spawn_blocking(move || {
            crate::stats_cache::full_rebuild(&dd, &cache);
        }).await.ok();
    });
}
```

- [ ] **Verify it compiles:** `cargo check -p find-server`

---

## Task 3: Stats route reads from cache

**Files:**
- Modify: `crates/server/src/routes/stats.rs`

- [ ] **Replace `query_source_stats` call with cache read; support `?refresh=true`**

Add a query param extractor:

```rust
#[derive(serde::Deserialize, Default)]
pub struct StatsQuery {
    #[serde(default)]
    pub refresh: bool,
}
```

Update the handler signature:

```rust
pub async fn get_stats(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    axum::extract::Query(query): axum::extract::Query<StatsQuery>,
) -> impl IntoResponse {
```

Replace the `spawn_blocking` block with:

```rust
if query.refresh {
    // Synchronous full rebuild before responding.
    let cache           = Arc::clone(&state.source_stats_cache);
    let compact_slot    = Arc::clone(&state.compaction_stats);
    let data_dir        = state.data_dir.clone();
    tokio::task::spawn_blocking(move || {
        crate::stats_cache::full_rebuild(&data_dir, &cache);
        // Also refresh compaction stats.
        if let Ok(compact) = crate::compaction::scan_wasted_space(&data_dir) {
            crate::compaction::save_stats_to_slot(&compact_slot, compact);
        }
    }).await.ok();
}

let db_size_bytes = {
    let sources_dir = state.data_dir.join("sources");
    std::fs::read_dir(&sources_dir)
        .map(|rd| rd.flatten()
            .filter(|e| e.path().extension().map(|x| x == "db").unwrap_or(false))
            .filter_map(|e| e.metadata().ok())
            .map(|m| m.len())
            .sum::<u64>())
        .unwrap_or(0)
};

// Read cached aggregate stats under the lock, then release before opening DB connections
// (opening connections while holding the lock would block the worker's apply_delta calls).
let cached: Vec<CachedSourceStats> = {
    let guard = state.source_stats_cache.read().unwrap_or_else(|e| e.into_inner());
    guard.sources.clone()
};

let sources: Vec<SourceStats> = cached.into_iter().map(|s| {
    // For fast per-source live queries, open a read connection after releasing the lock.
    let db_path = state.data_dir.join("sources").join(format!("{}.db", s.name));
    let (last_scan, history, indexing_error_count) = if let Ok(conn) = crate::db::open_for_stats(&db_path) {
        (
            crate::db::get_last_scan(&conn).unwrap_or(None),
            crate::db::get_scan_history(&conn, 100).unwrap_or_default(),
            crate::db::get_indexing_error_count(&conn).unwrap_or(0),
        )
    } else {
        (None, vec![], 0)
    };
    SourceStats {
        name:                 s.name.clone(),
        last_scan,
        total_files:          s.total_files,
        total_size:           s.total_size,
        by_kind:              s.by_kind.clone(),
        by_ext:               s.by_ext.clone(),
        history,
        indexing_error_count,
        fts_row_count:        s.fts_row_count,
    }
}).collect();
```

Note: `last_scan`, `history`, and `indexing_error_count` are fetched via live per-source connections (`open_for_stats`) because they are small indexed reads. Only the three expensive aggregate queries are replaced by the cache.

- [ ] **Expose `save_stats_to_slot` from `compaction.rs`**

```rust
// crates/server/src/compaction.rs — new public helper
pub fn save_stats_to_slot(
    slot: &Arc<std::sync::RwLock<Option<CompactionStats>>>,
    stats: CompactionStats,
) {
    if let Ok(mut g) = slot.write() { *g = Some(stats); }
}
```

Pass `Arc::clone(&state.compaction_stats)` into the refresh handler above.

- [ ] **Write integration tests** in `crates/server/tests/stats_cache.rs`

```rust
// Test: stats route returns without delay (cache hit)
// Test: ?refresh=true returns fresh data
// See Task 6 for full test list.
```

- [ ] **Run existing stats tests:** `cargo test --test smoke -p find-server`

- [ ] **Commit:** `fix: serve /api/v1/stats from in-memory cache`

---

## Task 4: Incremental delta — deletes

**Files:**
- Modify: `crates/server/src/db/mod.rs`

The goal: before deleting, capture `(total_count, total_size, by_kind)` of the paths being removed so the worker can subtract them from the cache.

- [ ] **Add `DeleteDelta` and modify `delete_files_phase1`**

```rust
/// Stats captured from files that are about to be deleted.
pub struct DeleteDelta {
    pub files_removed: i64,
    pub size_removed:  i64,
    pub by_kind: HashMap<FileKind, (i64, i64)>, // kind → (count, size)
}

pub fn delete_files_phase1(conn: &Connection, paths: &[String]) -> Result<DeleteDelta> {
    let mut delta = DeleteDelta { files_removed: 0, size_removed: 0, by_kind: HashMap::new() };

    // Open transaction first, then query-then-delete inside it to avoid a
    // TOCTOU window where a concurrent write could change the row between
    // the read and the delete.
    let tx = conn.unchecked_transaction()?;

    for path in paths {
        // Composite (archive member) paths don't appear in outer-file stats.
        if !find_common::path::is_composite(path) {
            let row: Option<(i64, String)> = tx.query_row(
                "SELECT COALESCE(size,0), kind FROM files WHERE path = ?1",
                params![path],
                |r| Ok((r.get(0)?, r.get(1)?)),
            ).optional()?;
            if let Some((size, kind_str)) = row {
                let kind = FileKind::from(kind_str.as_str());
                delta.files_removed += 1;
                delta.size_removed  += size;
                let e = delta.by_kind.entry(kind).or_insert((0, 0));
                e.0 += 1;
                e.1 += size;
            }
        }
        delete_one_path_phase1(&tx, path)?;
        tx.execute("DELETE FROM indexing_errors WHERE path = ?1", params![path])?;
        tx.execute(
            "DELETE FROM indexing_errors WHERE path LIKE ?1",
            params![format!("{}::%", path)],
        )?;
    }
    tx.commit()?;

    Ok(delta)
}
```

- [ ] **Update callers of `delete_files_phase1`** — the only caller is `request.rs` (Task 6 handles it). The `delete_source` route uses a different function (`delete_files`, not `delete_files_phase1`) and is unaffected.

- [ ] **Verify:** `cargo check -p find-server`

---

## Task 5: Incremental delta — inserts and modifications

**Files:**
- Modify: `crates/server/src/worker/pipeline.rs`

`process_file_phase1` already queries `(id, mtime)` from the existing record. Extend to also fetch `(size, kind)` so that for modifications we know the old values.

- [ ] **Extend `Phase1Outcome::Modified` to carry old values**

```rust
pub(super) enum Phase1Outcome {
    New,
    Modified { old_size: i64, old_kind: FileKind },
    Skipped,
}
```

- [ ] **Extend the existing record query**

Change:
```rust
let existing_record: Option<(i64, i64)> = conn.query_row(
    "SELECT id, mtime FROM files WHERE path = ?1",
    ...
```
To:
```rust
let existing_record: Option<(i64, i64, i64, String)> = conn.query_row(
    "SELECT id, mtime, COALESCE(size,0), kind FROM files WHERE path = ?1",
    rusqlite::params![file.path],
    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
).optional()?;
let existing_id    = existing_record.as_ref().map(|(id, _, _, _)| *id);
let stored_mtime   = existing_record.as_ref().map(|(_, mtime, _, _)| *mtime);
let old_size_kind  = existing_record.as_ref().map(|(_, _, size, kind)| (*size, FileKind::from(kind.as_str())));
```

- [ ] **Return `Modified { old_size, old_kind }` instead of `Modified`**

```rust
// At the point where Modified is returned:
Ok(if existing_id.is_none() {
    Phase1Outcome::New
} else {
    let (old_size, old_kind) = old_size_kind.unwrap_or((0, FileKind::Unknown));
    Phase1Outcome::Modified { old_size, old_kind }
})
```

- [ ] **Verify:** `cargo check -p find-server`

---

## Task 6: Apply delta in the worker

**Files:**
- Modify: `crates/server/src/worker/request.rs`
- Modify: `crates/server/src/lib.rs` (pass cache into `process_request_async`)

**Design note:** `process_request_phase1` runs inside `spawn_blocking` and does not receive `WorkerHandles`. The clean solution is to change its return type to `Result<SourceStatsDelta>`, then apply the delta in the async wrapper `process_request_async` *after* `spawn_blocking` completes. This avoids capturing an `Arc<RwLock<...>>` inside the blocking closure.

- [ ] **Change `process_request_phase1` return type to `Result<SourceStatsDelta>`**

```rust
// crates/server/src/worker/request.rs
pub(super) fn process_request_phase1(
    data_dir: &Path,
    request_path: &Path,
    to_archive_dir: &Path,
    status: &StatusHandle,
    cfg: ExtractorConfig,
    shared_archive: &SharedArchiveState,
    recent_tx: &broadcast::Sender<RecentFile>,
) -> Result<SourceStatsDelta> {
```

- [ ] **Build and accumulate delta inside `process_request_phase1`**

```rust
let request: BulkRequest = /* existing deserialization */;
let mut delta = crate::stats_cache::SourceStatsDelta {
    source: request.source.clone(),
    ..Default::default()
};

// inside the for file in &request.files loop, after process_file_phase1:
match &outcome {
    Phase1Outcome::New => {
        if !find_common::path::is_composite(&file.path) {
            delta.files_delta += 1;
            let size = file.size.unwrap_or(0);
            delta.size_delta  += size;
            let e = delta.kind_deltas.entry(file.kind.clone()).or_default();
            e.0 += 1;
            e.1 += size;
        }
    }
    Phase1Outcome::Modified { old_size, old_kind } => {
        if !find_common::path::is_composite(&file.path) {
            let new_size = file.size.unwrap_or(0);
            delta.size_delta += new_size - old_size;
            // by_kind: subtract old, add new
            let old_e = delta.kind_deltas.entry(old_kind.clone()).or_default();
            old_e.0 -= 1; old_e.1 -= old_size;
            let new_e = delta.kind_deltas.entry(file.kind.clone()).or_default();
            new_e.0 += 1; new_e.1 += new_size;
        }
    }
    Phase1Outcome::Skipped => {}
}
```

For deletes, `delete_files_phase1` now returns `DeleteDelta`. Fold it into `delta`:

```rust
let delete_delta = db::delete_files_phase1(&conn, &request.delete_paths)?;
delta.files_delta -= delete_delta.files_removed;
delta.size_delta  -= delete_delta.size_removed;
for (kind, (count, size)) in delete_delta.by_kind {
    let e = delta.kind_deltas.entry(kind).or_default();
    e.0 -= count; e.1 -= size;
}
```

At the end of the function, return the delta:

```rust
Ok(delta)
```

- [ ] **Apply delta in `process_request_async` after `spawn_blocking`**

`process_request_async` uses `tokio::time::timeout` + `spawn_blocking` with a `match timed_result` block that handles timeouts, `JoinError`, `is_db_locked` retries, and `handle_failure` move-to-failed logic. **Do not replace this structure.** Only extract the delta from the success arm.

The existing success arm looks like:
```rust
Ok(Ok(Ok(()))) => { /* success */ }
```
Change it to:
```rust
Ok(Ok(Ok(delta))) => {
    // existing success logic unchanged...
    // Then apply delta to cache:
    if let Ok(mut guard) = source_stats_cache.write() {
        guard.apply_delta(&delta);
    }
}
```
All other match arms (`Ok(Ok(Err(e)))` for `is_db_locked`, `Err(timeout)`, `Ok(Err(join_err))`) remain unchanged — they do not touch the cache.

Pass `source_stats_cache: Arc<std::sync::RwLock<SourceStatsCache>>` as a parameter to `process_request_async` and thread it from `start_inbox_worker` in `mod.rs`. Add `source_stats_cache` to `WorkerHandles` so `start_inbox_worker` can receive it:

```rust
// crates/server/src/worker/mod.rs
pub struct WorkerHandles {
    pub status:             StatusHandle,
    pub archive_state:      Arc<SharedArchiveState>,
    pub inbox_paused:       Arc<AtomicBool>,
    pub recent_tx:          broadcast::Sender<RecentFile>,
    pub source_stats_cache: Arc<std::sync::RwLock<crate::stats_cache::SourceStatsCache>>,
}
```

Wire it from `create_app_state` in `lib.rs`:
```rust
let worker_handles = worker::WorkerHandles {
    // existing fields ...
    source_stats_cache: Arc::clone(&source_stats_cache),
};
```

- [ ] **Verify:** `cargo check -p find-server`

---

## Task 7: Daily rebuild trigger

**Files:**
- Modify: `crates/server/src/compaction.rs`

The compaction scheduler already runs daily. After its scan completes, trigger a stats full rebuild too.

- [ ] **Add stats cache rebuild to the daily loop**

In `start_compaction_scanner`, the function receives `stats_slot` and `data_dir`. Add `source_stats_cache: Arc<std::sync::RwLock<SourceStatsCache>>` as a new parameter (update the call site in `lib.rs` too).

```rust
// After run_scan_and_log in the daily loop:
{
    let cache = Arc::clone(&source_stats_cache);
    let dd    = data_dir.clone();
    tokio::task::spawn_blocking(move || {
        crate::stats_cache::full_rebuild(&dd, &cache);
    }).await.ok();
    tracing::debug!("stats_cache: daily full rebuild complete");
}
```

- [ ] **Verify:** `cargo check -p find-server`
- [ ] **Commit:** `feat: incremental source stats cache with daily full rebuild`

---

## Task 8: `find-admin status --refresh`

**Files:**
- Modify: `crates/client/src/admin_main.rs`
- Modify: `crates/client/src/api.rs`

- [ ] **Add `--refresh` flag to `Status` subcommand**

```rust
Status {
    #[arg(long, short)]
    watch: bool,
    /// Force a full stats rebuild on the server before displaying
    #[arg(long)]
    refresh: bool,
},
```

- [ ] **Pass `refresh` to `client.get_stats()`**

```rust
// In admin_main.rs Status handler:
let stats = client.get_stats(refresh).await.context("fetching stats")?;
```

- [ ] **Add `refresh` param to `get_stats` in `api.rs`**

```rust
pub async fn get_stats(&self, refresh: bool) -> Result<StatsResponse> {
    let url = if refresh {
        format!("{}/api/v1/stats?refresh=true", self.base_url)
    } else {
        format!("{}/api/v1/stats", self.base_url)
    };
    // ... existing request logic
}
```

- [ ] **Update all other callers of `get_stats`** (e.g. `DeleteSource` confirmation) to pass `false`.

- [ ] **Verify:** `cargo check -p find-client`

---

## Task 9: Integration tests

**Files:**
- Create: `crates/server/tests/stats_cache.rs`

- [ ] **Write tests**

```rust
// Test 1: stats route is fast (cache populated at startup equivalent)
// Index some files, then call GET /api/v1/stats — should return immediately.
// Assert total_files matches indexed count without timing (just correctness).

// Test 2: incremental update — index a file, check total_files incremented
#[tokio::test]
async fn incremental_new_file_updates_total_files() {
    let server = TestServer::spawn().await;
    // index one file
    server.index_files("src", vec![make_text_file("a.txt", "hello")]).await;
    // poll stats until cache is non-zero (worker is async)
    let stats = server.get_stats().await;
    assert_eq!(stats.sources[0].total_files, 1);
}

// Test 3: delete decrements total_files
#[tokio::test]
async fn incremental_delete_updates_total_files() { ... }

// Test 4: ?refresh=true returns fresh data
#[tokio::test]
async fn refresh_flag_returns_fresh_stats() { ... }

// Test 5: by_kind is incrementally maintained
#[tokio::test]
async fn incremental_by_kind_is_updated() { ... }
```

- [ ] **Run tests:** `cargo test --test stats_cache -p find-server`
- [ ] **Run full test suite:** `cargo test --workspace`
- [ ] **Run clippy:** `mise run clippy`
- [ ] **Commit:** `test: source stats cache integration tests`

---

## Task 10: Final cleanup and changelog

- [ ] **Update `CHANGELOG.md`** under `[Unreleased]`:
  - `**Source stats cache** — ...`
- [ ] **Run full test suite one more time:** `cargo test --workspace`
- [ ] **Run clippy:** `mise run clippy`
- [ ] **Final commit:** `/commit`
