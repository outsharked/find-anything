# Bulk Operation Endpoint

## Overview

Replace the three separate write endpoints (`PUT /api/v1/files`,
`DELETE /api/v1/files`, `POST /api/v1/scan-complete`) with a single
`POST /api/v1/bulk` endpoint that routes everything through the async inbox.
This eliminates all SQLite write contention between the inbox worker and route
handlers, since no route handler ever touches the database directly.

## Design

### New `BulkRequest` type

```rust
pub struct BulkRequest {
    pub source: String,
    pub files: Vec<IndexFile>,        // files to upsert (unchanged)
    pub delete_paths: Vec<String>,    // paths to remove from index
    pub base_url: Option<String>,
    pub scan_timestamp: Option<i64>,  // replaces POST /scan-complete
}
```

### Worker processing order (per request)

1. **Delete** `delete_paths` from the DB and their chunks from ZIPs
2. **Upsert** `files` (chunk → ZIP → SQLite)
3. **Update** `scan_timestamp` if present
4. **Update** `base_url` if present

Deletes-before-upserts handles rename correctly (path appears in both).

### Client scan flow

Intermediate batches:
```
BulkRequest { files: batch, delete_paths: [], scan_timestamp: None, ... }
```

Final request (flushes remaining files + all deletes + completion timestamp):
```
BulkRequest { files: last_batch, delete_paths: to_delete, scan_timestamp: Some(now), ... }
```

Deletes are collected during the walk and emitted only in the final request,
so no extra round trips are needed compared to the current design.

## What Gets Removed

- `PUT /api/v1/files` route and handler
- `DELETE /api/v1/files` route and handler
- `POST /api/v1/scan-complete` route and handler
- `UpsertRequest`, `DeleteRequest`, `ScanCompleteRequest` types from `api.rs`
- `ApiClient::upsert_files`, `delete_files`, `scan_complete` in client `api.rs`
- `busy_timeout` in `db::open` (no longer needed)

## Files Changed

- `crates/common/src/api.rs` — add `BulkRequest`, remove old request types
- `crates/server/src/routes.rs` — add `bulk` handler, remove old handlers
- `crates/server/src/main.rs` — update route registrations
- `crates/server/src/worker.rs` — handle `BulkRequest` instead of `UpsertRequest`
- `crates/server/src/db.rs` — remove `busy_timeout`
- `crates/client/src/api.rs` — add `bulk`, remove old methods
- `crates/client/src/scan.rs` — emit `BulkRequest`; collect deletes and emit in final batch

## Testing

1. Run an incremental scan — verify files are indexed correctly
2. Delete a file from the source, re-run scan — verify it is removed from results
3. Rename a file (appears in both delete_paths and files) — verify correct result
4. Run server under load with concurrent scans — verify no "database is locked" errors
5. Verify `GET /api/v1/sources` and search still work (read paths unchanged)

## Breaking Changes

The three old endpoints are removed. Any external client using them directly
will need to switch to `POST /api/v1/bulk`. Since this project has no external
API consumers yet, this is acceptable.
