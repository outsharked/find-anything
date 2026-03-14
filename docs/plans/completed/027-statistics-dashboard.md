# Plan: Statistics Dashboard

## Context

The index currently has no visibility into its own health — how many files are indexed, how large the index is, what kinds of files dominate it, or how fast different file types extract. The existing `GET /api/v1/metrics` endpoint only returns inbox queue depths and ZIP archive counts. This plan adds a proper statistics dashboard with per-kind breakdowns, extraction timing, and a time-series view of index growth over time.

---

## What We're Building

A **Dashboard panel** in the web UI (accessible from the main nav) that shows:

- **Per-source summary**: total files, total size, last scan time
- **By kind breakdown**: count + size for each kind (text, pdf, image, audio, video, document, archive, executable)
- **Extraction time**: average `extract_ms` per kind — identifies slow file types (e.g. large PDFs, nested archives)
- **Items over time**: line chart showing total file count at each completed scan — visualizes index growth

---

## Schema Changes (v3 migration)

Add to each per-source SQLite DB:

```sql
-- New columns on files table (via ALTER TABLE migration)
ALTER TABLE files ADD COLUMN indexed_at INTEGER;   -- epoch seconds, set once on first insert
ALTER TABLE files ADD COLUMN extract_ms INTEGER;   -- nullable, ms measured by client

-- New table: one row per completed scan
CREATE TABLE IF NOT EXISTS scan_history (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    scanned_at  INTEGER NOT NULL,   -- epoch seconds (from BulkRequest.scan_timestamp)
    total_files INTEGER NOT NULL,
    total_size  INTEGER NOT NULL,
    by_kind     TEXT    NOT NULL    -- compact JSON: {"text":{"count":N,"size":N},...}
);
```

**Migration strategy**: Use `PRAGMA user_version`. Current value is 0 (never set). `db::open()` will check; if < 3, run the two ALTER TABLEs and CREATE TABLE, then set user_version = 3. The schema_v2.sql `CREATE TABLE IF NOT EXISTS` calls remain unchanged and still run first.

---

## Files Changed

### Backend

| File | Change |
|------|--------|
| `crates/common/src/api.rs` | Add `extract_ms: Option<u64>` to `IndexFile` |
| `crates/server/src/db.rs` | Add migration in `open()`, add `get_stats()` and `append_scan_history()` functions |
| `crates/server/src/worker.rs` | Store `extract_ms` + `indexed_at` on upsert; call `append_scan_history()` when `scan_timestamp` present |
| `crates/server/src/routes/stats.rs` (new) | `GET /api/v1/stats` handler |
| `crates/server/src/routes/mod.rs` | Export `get_stats`, add `mod stats` |
| `crates/server/src/main.rs` | Register `/api/v1/stats` route |
| `crates/client/src/scan.rs` | Wrap `extract::extract()` with `Instant::now()`, set `file.extract_ms` |

### Frontend

| File | Change |
|------|--------|
| `web/src/lib/api.ts` | Add `StatsResponse` types and `getStats()` function |
| `web/src/lib/Dashboard.svelte` (new) | Dashboard component |
| `web/src/routes/+page.svelte` | Add `showDashboard` state, chart icon button in header nav |

---

## Detailed Implementation

### 1. `crates/common/src/api.rs`

Add to `IndexFile`:
```rust
pub struct IndexFile {
    pub path: String,
    pub mtime: i64,
    pub size: i64,
    pub kind: String,
    pub lines: Vec<IndexLine>,
    #[serde(default)]
    pub extract_ms: Option<u64>,   // ← new
}
```

Add stats response types:
```rust
pub struct KindStats {
    pub count: usize,
    pub size: i64,
    pub avg_extract_ms: Option<f64>,
}

pub struct SourceStats {
    pub name: String,
    pub last_scan: Option<i64>,
    pub total_files: usize,
    pub total_size: i64,
    pub by_kind: std::collections::HashMap<String, KindStats>,
    pub history: Vec<ScanHistoryPoint>,
}

pub struct ScanHistoryPoint {
    pub scanned_at: i64,
    pub total_files: usize,
    pub total_size: i64,
}

pub struct StatsResponse {
    pub sources: Vec<SourceStats>,
    pub inbox_pending: usize,
    pub failed_requests: usize,
    pub total_archives: usize,
}
```

### 2. `crates/server/src/db.rs`

**Migration in `open()`**:
```rust
pub fn open(db_path: &Path) -> Result<Connection> {
    let conn = Connection::open(db_path)?;
    conn.execute_batch(include_str!("schema_v2.sql"))?;
    migrate_v3(&conn)?;
    Ok(conn)
}

fn migrate_v3(conn: &Connection) -> Result<()> {
    let version: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    if version >= 3 { return Ok(()); }
    conn.execute_batch("
        ALTER TABLE files ADD COLUMN indexed_at INTEGER;
        ALTER TABLE files ADD COLUMN extract_ms INTEGER;
        CREATE TABLE IF NOT EXISTS scan_history (
            id         INTEGER PRIMARY KEY AUTOINCREMENT,
            scanned_at INTEGER NOT NULL,
            total_files INTEGER NOT NULL,
            total_size  INTEGER NOT NULL,
            by_kind     TEXT    NOT NULL
        );
        PRAGMA user_version = 3;
    ")?;
    Ok(())
}
```

**New `get_stats()` function**:
```rust
pub fn get_stats(conn: &Connection) -> Result<(usize, i64, HashMap<String, KindStats>)>
// Returns (total_files, total_size, by_kind map)
// SQL: SELECT kind, COUNT(*), SUM(size), AVG(extract_ms) FROM files GROUP BY kind
```

**New `append_scan_history()` function**:
```rust
pub fn append_scan_history(conn: &Connection, scanned_at: i64) -> Result<()>
// Queries current totals and writes one row to scan_history
```

**New `get_scan_history()` function**:
```rust
pub fn get_scan_history(conn: &Connection, limit: usize) -> Result<Vec<ScanHistoryPoint>>
// SELECT scanned_at, total_files, total_size FROM scan_history ORDER BY scanned_at ASC LIMIT ?
```

### 3. `crates/server/src/worker.rs`

In the `process_file()` upsert:
```rust
// indexed_at: set only on first insert (not on update)
conn.execute(
    "INSERT INTO files (path, mtime, size, kind, indexed_at, extract_ms)
     VALUES (?1, ?2, ?3, ?4, ?5, ?6)
     ON CONFLICT(path) DO UPDATE SET
       mtime      = excluded.mtime,
       size       = excluded.size,
       kind       = excluded.kind,
       extract_ms = excluded.extract_ms",
       -- indexed_at intentionally NOT updated on conflict
    params![file.path, file.mtime, file.size, file.kind, now_secs, file.extract_ms.map(|ms| ms as i64)],
)?;
```

When `scan_timestamp` is present in BulkRequest, after processing all files:
```rust
db::append_scan_history(&conn, scan_timestamp)?;
```

### 4. `crates/server/src/routes/stats.rs` (new)

`GET /api/v1/stats`:
- Requires auth (same as all other endpoints)
- Iterates all source DBs (same pattern as search route)
- For each source: calls `db::get_stats()` and `db::get_scan_history()`
- Also gathers the global inbox/archive counts (reuse logic from `get_metrics`)
- Returns `StatsResponse` as JSON

### 5. `crates/client/src/scan.rs`

Wrap the `extract::extract()` call:
```rust
let t0 = std::time::Instant::now();
let lines = match extract::extract(abs_path, &cfg) { ... };
let extract_ms = t0.elapsed().as_millis() as u64;
```

Set `extract_ms` on the outer IndexFile only (archive members inherit `None`).

### 6. `web/src/lib/Dashboard.svelte` (new)

Layout:
```
┌─────────────────────────────────────────────────────────┐
│  Index Dashboard                          [Close ×]      │
├─────────────────────────────────────────────────────────┤
│  Source: my-source  ▼                                    │
│                                                          │
│  ┌──────────┐  ┌──────────┐  ┌──────────┐               │
│  │  5,432   │  │ 1.2 GB   │  │ 2h ago   │               │
│  │  files   │  │  indexed │  │last scan │               │
│  └──────────┘  └──────────┘  └──────────┘               │
│                                                          │
│  By Kind                                                 │
│  text      ████████████░░░  3,201  (412 MB)  12ms avg   │
│  image     ████░░░░░░░░░░░  1,021  (580 MB)   8ms avg   │
│  pdf       ██░░░░░░░░░░░░░    312  (180 MB) 340ms avg   │
│  archive   █░░░░░░░░░░░░░░     89  ( 45 MB) 1.2s  avg   │
│  ...                                                     │
│                                                          │
│  Files over time                                         │
│  6000 ┤                                          ╭─      │
│  4000 ┤                               ╭──────────╯       │
│  2000 ┤                    ╭──────────╯                   │
│     0 ┤────────────────────╯                             │
│       Jan    Feb    Mar    Apr    May    Jun              │
└─────────────────────────────────────────────────────────┘
```

**Access**: Chart icon button in the header bar alongside the existing Settings (⚙) button.
Opens as an overlay (same pattern as the Settings panel).

**Charts**: Hand-rolled SVG — no new dependencies:
- Kind breakdown: CSS flexbox bar (proportional width = count / total_files)
- Items over time: SVG polyline with basic axis labels

**Source selector**: `<select>` bound to active source; switches which source's stats are shown.

---

## API Response Shape

```json
{
  "sources": [
    {
      "name": "my-docs",
      "last_scan": 1740000000,
      "total_files": 5432,
      "total_size": 1234567890,
      "by_kind": {
        "text":     { "count": 3201, "size": 412000000, "avg_extract_ms": 12.5 },
        "image":    { "count": 1021, "size": 580000000, "avg_extract_ms": 8.3  },
        "pdf":      { "count": 312,  "size": 180000000, "avg_extract_ms": 340.0 }
      },
      "history": [
        { "scanned_at": 1739000000, "total_files": 5100, "total_size": 1200000000 },
        { "scanned_at": 1740000000, "total_files": 5432, "total_size": 1234567890 }
      ]
    }
  ],
  "inbox_pending": 0,
  "failed_requests": 0,
  "total_archives": 45
}
```

---

## Verification

1. Run `cargo build --workspace` — no errors
2. Run `cargo test --workspace` — all pass
3. Re-index a source → confirm `scan_history` row is written
4. `GET /api/v1/stats` → check response has correct file counts matching `SELECT COUNT(*) FROM files`
5. Check `avg_extract_ms` is non-null for kinds that were indexed after this change
6. `pnpm run check` in `web/` — no type errors
7. Open dashboard in browser:
   - Totals match expected counts
   - Kind bars proportion correctly
   - "Items over time" chart renders with at least 1 point per completed scan
   - Source selector switches between sources
