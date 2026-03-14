# Plan 004 Implementation Status

**Date:** 2026-02-13
**Feature:** ZIP Content Storage with Async Processing
**Status:** Complete (v0.1.1)

## Completed Tasks ✅

### 1. Dependencies Added
- **Server**: Added zip, flate2, notify, uuid, chrono to `crates/server/Cargo.toml`
- **Client**: Added flate2 to `crates/client/Cargo.toml`
- **Common**: Already had zip and flate2 for archive extraction

### 2. Database Schema Updated
- Created `crates/server/src/schema_v2.sql` with:
  - Updated `lines` table with chunk references: `chunk_archive`, `chunk_name`, `line_offset_in_chunk`
  - FTS5 with `content=''` (index-only, no content storage)
  - New `archives` table for tracking ZIP files
  - Removed triggers (manual FTS5 management)

### 3. Archive Management Module
- Created `crates/server/src/archive.rs` with:
  - `ArchiveManager` struct for ZIP operations
  - `append_chunks()` - add chunks to archives with 10MB rotation
  - `remove_chunks()` - rewrite archives to delete entries
  - `read_chunk()` - extract content from archives
  - **Proper chunking implementation**: `chunk_lines()` returns `ChunkResult` with line mappings
  - Tracks which lines ended up in which chunks at what offset
  - Unit tests for chunking logic

### 4. Async Inbox Worker
- Created `crates/server/src/worker.rs` with:
  - `start_inbox_worker()` - monitors inbox directory using `notify` crate
  - `process_request()` - decompresses gzipped requests, processes files
  - `process_file()` - chunks content, appends to ZIPs, updates DB with line mappings
  - `handle_failure()` - moves failed requests to `inbox/failed/`
  - Properly tracks line offsets within chunks for retrieval

### 5. Module Declarations
- Updated `crates/server/src/main.rs`:
  - Added `mod archive;`
  - Added `mod worker;`

## Key Design Decisions Made

### Chunking Strategy
- **Decision**: Chunk lines into ~1KB pieces, track line offsets
- **Rationale**: Avoids overhead of per-line ZIP entries while maintaining line-level granularity
- **Implementation**: Each line knows its chunk and offset within that chunk
- **Alternative considered**: Store each line as separate ZIP entry (rejected due to massive overhead)

### Transport Compression
- **Decision**: Use gzip (flate2) for HTTP transport, ZIP for storage
- **Rationale**: gzip is HTTP standard, simpler for single payloads; ZIP is better for multi-entry archives
- **Libraries**: Already have both as dependencies, minimal overhead

### Mailbox Pattern
- **Decision**: Filesystem-based inbox with `notify` watcher
- **Rationale**: Crash-resistant, observable, debuggable vs in-memory channels
- **Structure**: `inbox/*.gz` for pending, `inbox/failed/` for errors

## Remaining Tasks 🔨

### 5. Update routes.rs for async inbox (IN PROGRESS)
**Status**: Started, need to complete
**What's needed**:
- Modify `upsert_files()` endpoint to accept gzipped body
- Check `Content-Encoding: gzip` header
- Generate request ID with timestamp and UUID
- Write compressed payload to `inbox/{request_id}.gz`
- Return `202 Accepted` immediately
- Remove old synchronous processing code

**Current file state**: Added `Bytes` to imports, started modifications

### 6. Update client to send gzip-compressed requests
**Status**: Not started
**What's needed**:
- Modify `crates/client/src/scan.rs`
- Compress `IndexRequest` JSON with `GzEncoder`
- Add `Content-Encoding: gzip` header
- Handle `202 Accepted` response (instead of `200 OK`)
- Update error handling for async processing

### 7. Update context retrieval to read from ZIP archives
**Status**: Not started
**What's needed**:
- Modify `crates/server/src/db.rs` context functions
- Update `get_line_context()`:
  - Query lines table for chunk references
  - Read chunks from ZIP using `ArchiveManager`
  - Extract specific lines using `line_offset_in_chunk`
  - Split chunk content by `\n`, get line at offset
- Update `get_pdf_context()` similarly
- Update `get_metadata_context()` similarly
- Pass `ArchiveManager` to context functions

### 8. Update main.rs to start worker thread
**Status**: Not started
**What's needed**:
- Spawn worker thread on startup with `tokio::spawn`
- Pass `data_dir` to worker
- Ensure `inbox/` and `inbox/failed/` directories exist
- Add graceful shutdown handling
- Update `AppState` if needed for `ArchiveManager`

### 9. Add metrics endpoint
**Status**: Not started
**What's needed**:
- Add `GET /api/v1/metrics` endpoint
- Return JSON with:
  - `inbox_queue_depth`: count of `.gz` files in inbox
  - `failed_requests`: count in failed directory
  - `total_archives`: count of content ZIP files
  - Optional: total chunks (may be expensive)

## Migration Strategy

For initial release: **Fresh index approach**
1. Users backup existing database
2. Update server binary
3. Delete old database
4. Rescan from scratch with new schema

**Note**: In-place migration can be added later if needed.

## Testing Checklist

Once implementation is complete, test:
- [ ] Build server and client
- [ ] Start server, verify worker starts
- [ ] Client sends compressed index request
- [ ] Verify request appears in inbox
- [ ] Worker processes request successfully
- [ ] Verify chunks appear in content ZIP archives
- [ ] Query database, verify lines have chunk references
- [ ] Perform search, verify results returned
- [ ] Request context, verify content extracted from ZIPs
- [ ] Delete files, verify chunks removed from ZIPs
- [ ] Mass reorganization test (move files, rescan)
- [ ] Check metrics endpoint

## Known Issues / TODOs

1. **Archive path handling**: Current `process_file()` sets `archive_path` to `None`. Need to handle files extracted from archives (ZIP, tar, etc.)
2. **Schema migration**: Only using `schema_v2.sql`, need to update `db::open()` to use new schema or add versioning
3. **Error recovery**: Failed requests go to `inbox/failed/` but no retry mechanism
4. **Metrics**: Not yet implemented
5. **Documentation**: Need to update README for new architecture

## File Status

### New Files Created
- ✅ `docs/plans/004-zip-content-storage.md` - Full plan document
- ✅ `crates/server/src/schema_v2.sql` - Updated schema
- ✅ `crates/server/src/archive.rs` - ZIP management (227 lines)
- ✅ `crates/server/src/worker.rs` - Async processing (226 lines)
- ✅ `docs/plans/004-implementation-status.md` - This file

### Files Modified
- ✅ `crates/server/Cargo.toml` - Dependencies added
- ✅ `crates/client/Cargo.toml` - flate2 added
- ✅ `crates/server/src/main.rs` - Module declarations added
- 🔨 `crates/server/src/routes.rs` - Partially modified (imports updated)

### Files To Modify
- ⏳ `crates/server/src/routes.rs` - Complete inbox endpoint
- ⏳ `crates/server/src/main.rs` - Start worker, update AppState
- ⏳ `crates/server/src/db.rs` - Update context retrieval
- ⏳ `crates/client/src/scan.rs` - Add compression

## Next Steps

1. **Complete routes.rs** (Task #5):
   - Finish modifying `upsert_files()` endpoint
   - Add request ID generation with uuid + timestamp
   - Write to inbox directory
   - Return 202 Accepted

2. **Update main.rs** (Task #8):
   - Spawn worker on startup
   - Create inbox directories
   - Update AppState if needed

3. **Update db.rs** (Task #7):
   - Modify context retrieval to read from ZIPs
   - Test chunk extraction with offsets

4. **Update client** (Task #6):
   - Add gzip compression to scan.rs
   - Handle async responses

5. **Test end-to-end**:
   - Build and run
   - Index some files
   - Perform searches
   - Verify storage efficiency

6. **Add metrics endpoint** (Task #9):
   - Simple endpoint for monitoring

## Code Snippets for Resume

### Request ID Generation (for routes.rs)
```rust
let request_id = format!(
    "req_{}_{}",
    chrono::Utc::now().format("%Y%m%d_%H%M%S"),
    uuid::Uuid::new_v4().simple()
);
```

### Chunk Extraction (for db.rs context retrieval)
```rust
// Read chunk from ZIP
let chunk_content = archive_mgr.read_chunk(&chunk_ref)?;

// Extract specific line using offset
let lines: Vec<&str> = chunk_content.lines().collect();
let line_content = lines.get(line_offset_in_chunk)
    .context("line offset out of bounds")?;
```

### Worker Spawn (for main.rs)
```rust
tokio::spawn(async move {
    if let Err(e) = worker::start_inbox_worker(data_dir).await {
        tracing::error!("Inbox worker failed: {}", e);
    }
});
```

## Questions to Resolve

None currently - implementation path is clear.

---

**To resume**: Start with Task #5 (complete routes.rs), then proceed sequentially through tasks #8, #7, #6, #9.
