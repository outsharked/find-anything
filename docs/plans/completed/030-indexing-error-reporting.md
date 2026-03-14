# Plan: Indexing Error Reporting

## Overview

When `find-scan` fails to extract content from a file (e.g. corrupt PDF, unsupported
archive codec, permission error), it logs a `WARN` locally and continues, indexing only
the filename. The server has no record of these failures. Diagnosing missed content
currently requires reading client-side logs. This feature closes that gap by:

1. Having the client transmit extraction failures alongside each bulk upload
2. Storing them per-file on the server (deduped, with recurrence tracking)
3. Surfacing them in the UI: an **Errors panel** in Settings and an **inline warning**
   on the file detail view

---

## Limits

- **Client-side per-batch cap**: `MAX_FAILURES_PER_BATCH = 100`. If more than 100
  extraction errors occur in one batch interval, only the first 100 are sent; the rest
  are warn-logged locally as before. This prevents large payloads on pathological sources.
- **Error message truncation**: `MAX_ERROR_LEN = 500` characters (UTF-8 safe, truncate
  at a char boundary), suffixed with `…`.
- **Server deduplication**: `UPSERT ON CONFLICT(path)` — repeated failures for the same
  file update `last_seen` and increment `count` rather than creating new rows.
- **Auto-clear**: when a file is successfully re-indexed, its error row is deleted. When
  a file is explicitly deleted from the source, its error row is also deleted.

---

## Part 1 — API Types (`crates/common/src/api.rs`)

### New structs

```rust
/// One extraction failure reported by the client.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct IndexingFailure {
    pub path: String,   // relative path of the file that failed extraction
    pub error: String,  // error message, truncated to MAX_ERROR_LEN
}

/// One row from the server's `indexing_errors` table.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IndexingError {
    pub path: String,
    pub error: String,
    pub first_seen: i64,  // Unix seconds
    pub last_seen: i64,
    pub count: i64,       // how many scans reported this error
}

/// `GET /api/v1/errors` response.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ErrorsResponse {
    pub errors: Vec<IndexingError>,
    pub total: usize,  // total rows (for pagination)
}
```

### Modified structs

`BulkRequest` — add field (`serde(default)` for backward compat with old clients):
```rust
#[serde(default)]
pub indexing_failures: Vec<IndexingFailure>,
```

`FileResponse` — add field:
```rust
pub indexing_error: Option<String>,
```

`SourceStats` — add field:
```rust
pub indexing_error_count: usize,
```

---

## Part 2 — Client (`crates/client/src/scan.rs` + `crates/client/src/batch.rs`)

### Constants (scan.rs)
```rust
const MAX_FAILURES_PER_BATCH: usize = 100;
const MAX_ERROR_LEN: usize = 500;
```

### Error collection (scan.rs, in the extraction loop around line 95)

Maintain `failures: Vec<IndexingFailure>` alongside `batch`. Change the error arm:

```rust
Err(e) => {
    let msg = format!("{e:#}");
    let truncated = truncate_error(&msg, MAX_ERROR_LEN);
    warn!("extract {}: {}", abs_path.display(), truncated);
    if failures.len() < MAX_FAILURES_PER_BATCH {
        failures.push(IndexingFailure { path: rel_path.clone(), error: truncated });
    }
    vec![]
}
```

`truncate_error(s, max)`: floor to a UTF-8 char boundary at ≤ `max` bytes, append `…`
if truncated. Private helper in scan.rs.

### Batch submission (scan.rs + batch.rs)

`submit_batch` gains a `failures: &mut Vec<IndexingFailure>` parameter. The failures
are moved into `BulkRequest.indexing_failures` via `std::mem::take` (same pattern as
`batch`). This means each intermediate batch submission clears `failures`, so each set
of errors is only reported once. The final batch gets whatever failures remain.

---

## Part 3 — Schema (`crates/server/src/schema_v2.sql` + `crates/server/src/db.rs`)

### New table (schema_v2.sql)

```sql
CREATE TABLE IF NOT EXISTS indexing_errors (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    path       TEXT    NOT NULL UNIQUE,
    error      TEXT    NOT NULL,
    first_seen INTEGER NOT NULL,
    last_seen  INTEGER NOT NULL,
    count      INTEGER NOT NULL DEFAULT 1
);
```

### Schema version bump

`SCHEMA_VERSION` in `db.rs` → **4**.

**`check_schema_version`** — allow v3 DBs through for migration.

**`migrate_v4`** — new function, called in `open()` after `migrate_v3`.

### New db.rs functions

| Function | SQL |
|---|---|
| `upsert_indexing_errors(conn, &[IndexingFailure], now: i64)` | UPSERT with conflict update |
| `clear_errors_for_paths(conn, &[String])` | DELETE WHERE path IN (...) |
| `get_indexing_errors(conn, limit, offset) -> Result<Vec<IndexingError>>` | Paginated SELECT |
| `get_indexing_error_count(conn) -> Result<usize>` | SELECT COUNT(*) |
| `get_indexing_error(conn, path) -> Result<Option<String>>` | SELECT WHERE path = ? |

---

## Part 4 — Worker (`crates/server/src/worker.rs`)

After existing delete and upsert phases:
1. Clear errors for successfully indexed paths
2. Clear errors for explicitly deleted paths
3. Store new failures reported by the client

---

## Part 5 — Routes (`crates/server/src/routes/`)

### New route

`GET /api/v1/errors?source=X&limit=200&offset=0` → `ErrorsResponse`

### Modified routes

- **`GET /api/v1/file`** — set `FileResponse.indexing_error`
- **`GET /api/v1/stats`** — set `SourceStats.indexing_error_count`

---

## Part 6 — Web API client (`web/src/lib/api.ts`)

New interfaces: `IndexingError`, `ErrorsResponse`.
Updated interfaces: `FileResponse.indexing_error?`, `SourceStats.indexing_error_count`.
New function: `getErrors(source, limit, offset)`.

---

## Part 7 — Web UI

- **`FileViewer.svelte`**: Amber warning banner if `indexing_error` is set
- **`ErrorsPanel.svelte`**: New component — source selector, error table with path/error/last-seen/count
- **`settings/+page.svelte`**: Add Errors nav item between Stats and About
- **`StatsPanel.svelte`**: Amber error count badge in summary cards, links to Errors section

---

## Files Changed

| File | Change |
|---|---|
| `crates/common/src/api.rs` | Add `IndexingFailure`, `IndexingError`, `ErrorsResponse`; update `BulkRequest`, `FileResponse`, `SourceStats` |
| `crates/client/src/scan.rs` | Collect failures in scan loop; pass to `submit_batch`; add constants and `truncate_error` helper |
| `crates/client/src/batch.rs` | `submit_batch` gains `failures: &mut Vec<IndexingFailure>` param |
| `crates/server/src/schema_v2.sql` | Add `indexing_errors` table |
| `crates/server/src/db.rs` | `SCHEMA_VERSION` → 4; update `check_schema_version`; add `migrate_v4`; add 5 new functions |
| `crates/server/src/worker.rs` | Clear + upsert errors after processing each BulkRequest |
| `crates/server/src/routes/errors.rs` | New route handler for `GET /api/v1/errors` |
| `crates/server/src/routes/mod.rs` | Export new errors route |
| `crates/server/src/routes/file.rs` | Include `indexing_error` in FileResponse |
| `crates/server/src/routes/stats.rs` | Include `indexing_error_count` in SourceStats |
| `crates/server/src/main.rs` | Register new `/api/v1/errors` route |
| `web/src/lib/api.ts` | New interfaces + `getErrors()` |
| `web/src/lib/FileViewer.svelte` | Indexing error banner |
| `web/src/lib/ErrorsPanel.svelte` | New component |
| `web/src/routes/settings/+page.svelte` | Add Errors nav item and section |
| `web/src/lib/StatsPanel.svelte` | Error count badge in source summary |

---

## Verification

1. `cargo build --workspace` — clean compile
2. `cargo test --workspace` — all tests pass
3. `pnpm run check` in `web/` — TypeScript type-checks clean
4. Create a corrupt file and scan — verify error appears in `/api/v1/errors`
5. Open the file in the UI — verify amber error banner in file detail view
6. Navigate to Settings → Errors — verify file appears in error list
7. Fix or delete the file and re-scan — verify error row disappears
8. Check Stats panel — verify error count appears/disappears for the source
9. Scan with > 100 extraction failures — verify server receives exactly 100 per batch
10. Existing v3 DB — verify it migrates cleanly to v4
