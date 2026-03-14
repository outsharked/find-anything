# Deferred Archive Writer

## Overview

Plan 049 introduced a multi-worker inbox pool to prevent a single slow request from
blocking the entire queue. The added concurrency has introduced SQLite WAL deadlocks
that are persistent on WSL/network mounts where POSIX advisory locking is unreliable.
Every fix so far has been a band-aid against the same underlying tension: parallel
workers = concurrent writes to the same SQLite DB = hard-to-reproduce, hard-to-fix
deadlocks.

This plan returns to a **single SQLite writer** and decouples the slow part — ZIP
archive I/O — onto a separate sequential thread. The inbox queue is no longer blocked
by ZIP I/O; the SQLite index stays fast and deadlock-free; and a second "archive
thread" catches up with ZIP writes in the background.

---

## Root cause of the deadlock problems

The deadlock root cause is not concurrent writers per se — SQLite WAL handles that.
The root cause is two write transactions on the **same connection** in a single
request (or two workers on the same source sharing a connection indirectly via POSIX
advisory file locks on a broken mount). The bug doc (`docs/bugs/`) shows two distinct
manifestations of this pattern.

Every manifestation shares the same shape:

```
conn.write_tx_1()   // acquires WAL write lock, releases on commit
conn.write_tx_2()   // hangs waiting for write lock that never clears (WSL/NFS)
```

The multi-worker design makes this worse by multiplying the number of connections
that may simultaneously hold or wait for write locks on the same DB file.

---

## Proposed design

### Two-phase processing

**Phase 1 — Indexing (single thread, fast)**

One inbox worker thread processes requests sequentially. For each request:

1. **Deletes:** remove from `files`, `lines`, `file_content`. Collect old chunk refs
   into a new `pending_chunk_removes` table for the archive thread.
2. **Renames:** update paths in `files`.
3. **Upserts:** for each file, use a **single transaction**:
   - `INSERT INTO files ... RETURNING id` — get `file_id`
   - Read and save old chunk refs from `lines` into `pending_chunk_removes`
   - `DELETE FROM lines WHERE file_id = ?`
   - `DELETE FROM file_content WHERE file_id = ?`
   - For files **at or below** `inline_threshold_bytes`: insert lines with content
     inline in `file_content` (permanent inline path, unchanged from today).
   - For files **above** `inline_threshold_bytes`: insert `lines` rows with **NULL
     chunk refs and no inline content**. Content is not stored in SQLite at all
     during this phase.
   - Insert into `lines_fts` (all files, regardless of size)
   - Commit

After the request is processed, move the `.gz` file from `processing/` to a new
`to-archive/` directory. Delete it from `to-archive/` after the archive thread
finishes it.

The file is **immediately searchable** (FTS indexed). For large files, context and
file detail views return a "content not yet available — archiving in progress"
response until the archive thread completes. Small files (below inline threshold)
are served normally with no gap.

**Phase 2 — Archive writing (single thread, sequential)**

A second thread polls `to-archive/` and processes each request:

1. Parse the `.gz` to get the list of files (same `BulkRequest` format).
2. For each file (using `path` to look up `file_id` in `files`):
   a. Skip if `file_id` not found (file was deleted after phase 1 — safe to skip).
   b. Skip if `lines` already has chunk refs (file was already archived, e.g. from a
      newer re-index that ran phase 2 first).
   c. Skip if the file is below `inline_threshold_bytes` (permanently inline — no ZIP
      needed). Still process any `pending_chunk_removes` for this file_id.
   d. Process `pending_chunk_removes` for this file_id: rewrite affected ZIP archives
      to remove old chunks.
   e. Chunk the content from the original `.gz` request payload and append to the
      current write archive.
   f. In a single transaction:
      - Update `lines` rows with the new `chunk_archive` / `chunk_name` /
        `line_offset_in_chunk` values.
      - Delete from `pending_chunk_removes` for this file_id.
   g. Commit.

The archive thread reads content directly from the original `.gz` request payload
(already on disk in `to-archive/`), not from SQLite. No transient inline copy of
large file content is ever written to SQLite. The `file_content` table is only used
for permanently-inline small files, exactly as today.

Delete the `.gz` from `to-archive/` when the request is fully processed.

---

## Why this eliminates deadlocks

- Phase 1 uses **exactly one write transaction per file** on one connection. No
  second write follows on the same connection within the same request.
- Phase 2 also uses **exactly one write transaction per file** on its own
  connection. The two threads never share a connection.
- Two threads writing to the same SQLite DB can still cause brief busy-waits, but
  WAL serialises them correctly as long as each connection issues only one
  `BEGIN...COMMIT` at a time — which this design guarantees.
- The source_lock (`SharedArchiveState::source_lock`) and per-archive rewrite lock
  are both **no longer needed**. The single-writer design eliminates inter-worker
  contention entirely.

---

## Schema changes

Add one new table:

```sql
CREATE TABLE IF NOT EXISTS pending_chunk_removes (
    id          INTEGER PRIMARY KEY,
    file_id     INTEGER NOT NULL,    -- for association only; may be reused after delete
    archive_name TEXT NOT NULL,
    chunk_name   TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_pcr_file_id ON pending_chunk_removes(file_id);
```

This table is written by phase 1 (before clearing old lines) and consumed by phase 2.

It is per-source DB (same as `lines`).

Schema version bump required (current: 9 → 10).

---

## Directory layout change

```
inbox/
  processing/    ← (unchanged) claimed by router, moved back on crash
  failed/        ← (unchanged) permanently failed requests
  to-archive/    ← (new) phase 1 done, phase 2 pending
```

On startup, `to-archive/` files are re-queued by the archive thread (they survived a
crash with phase 2 incomplete). Unlike `processing/`, files in `to-archive/` should
**not** be moved back to `inbox/` — they are already indexed; only the ZIP write is
pending.

---

## Thread model

```
Router loop (async)
  ↓ channel
Indexing thread (1× spawn_blocking)   →  writes SQLite   →  moves .gz to to-archive/
                                                              ↓
Archive thread (1× spawn_blocking)    →  reads to-archive/ → writes ZIPs → writes SQLite
```

The `inbox_workers` config setting becomes obsolete (or is ignored / always treated as
1). The archive thread count is always 1 (sequential ZIP writes prevent corruption).

---

## Handling edge cases

### File deleted while in to-archive queue

Phase 1 deletes it from `files` and `lines` immediately. Phase 2 looks up the
file by path, finds no row, and skips it. `pending_chunk_removes` may still have
entries for the old chunk refs — phase 2 processes these regardless (the file_id
check is only for the new content write).

### File re-indexed while old to-archive entry is queued

Phase 1 upserts the new version (new `file_id` or same path with updated mtime).
The old `to-archive` `.gz` still references the original content. When phase 2
processes the old entry, it looks up the file by path and checks whether `lines` rows
still have NULL chunk refs. If the newer phase 2 run has already written chunk refs
(for the newer version), the old entry skips the chunk-write and lines-update steps
but still processes any `pending_chunk_removes` for the old file_id.

The check: query `lines` for this file_id; if chunk refs are already non-NULL, skip
content write. If `file_id` is gone (deleted), skip entirely but still clean up
removes.

### Archive compaction (plan 050)

Compaction rewrites existing archives to reclaim space from deleted chunks. It must
not run concurrently with the archive thread, as both rewrite ZIP files. Options:

1. Run compaction on the archive thread itself (pause archiving during compaction).
2. The archive thread acquires a mutex before each ZIP operation; compaction acquires
   the same mutex.

Option 1 is simpler: compaction is triggered only when the archive queue is empty (or
paused), ensuring no overlap.

### Inline storage threshold

The existing `inline_threshold_bytes` config is unchanged in semantics. Files at or
below the threshold are written to `file_content` by phase 1 (permanently inline, no
ZIP). Files above the threshold have NULL chunk refs in `lines` and no SQLite content
until phase 2 archives them. There is no transient double-storage of content: large
files never touch `file_content` at all.

---

## UX — content availability gap

For large files (above `inline_threshold_bytes`), there is a window between phase 1
(file is searchable) and phase 2 (content is available in ZIP). During this window:

- **Search results** appear normally — the file is FTS-indexed and ranked.
- **Context snippets** (the matched-line excerpt in results) cannot be served — the
  chunk refs are NULL and there is no inline copy.
- **File detail / full context views** return a clear error or placeholder:
  `"Content not yet available — indexing in progress"`.
- **The file can still be opened from its source path** via the OS if the user needs
  immediate access.

The archive thread runs on a deliberate delay (see open question 2) to maximise
coalescing — a lag of minutes is acceptable and expected. Files added and then
deleted within the batch window generate no ZIP work at all. Users who need immediate
access to content can always use "Show original" to open the file from its source
path.

The server should expose a count of pending-archive files in the `GET /api/v1/stats`
response so the UI can show a subtle "Archiving content… N files pending" indicator.

---

## Performance characteristics

| Metric | Before (plan 049) | After (this plan) |
|--------|------------------|-------------------|
| Time to searchability | ZIP write completes | FTS write only (~fast) |
| Time to context/detail | ZIP write completes | Archive thread catches up |
| Context during gap | N/A | "Content pending" message |
| SQLite contention | High (N workers, same DB) | Minimal (2 threads, WAL serialises) |
| ZIP contention | Requires per-archive locks | None (single archive thread) |
| Deadlock risk | Present on WSL/network | Eliminated |
| Large-file SQLite footprint | None (content stays in ZIPs) | None (content stays in .gz until archived) |

---

## Files Changed

| File | Change |
|------|--------|
| `crates/server/src/schema_v2.sql` | Add `pending_chunk_removes` table; bump SCHEMA_VERSION → 10 |
| `crates/server/src/db/mod.rs` | Update `SCHEMA_VERSION`; add `delete_files` to populate `pending_chunk_removes`; add `flush_pending_removes` helper for archive thread |
| `crates/server/src/worker.rs` | Remove multi-worker pool; single indexing thread; move .gz to `to-archive/` after phase 1; add archive thread loop |
| `crates/server/src/archive.rs` | Remove `SharedArchiveState::source_locks` (no longer needed); simplify `ArchiveManager` (no shared counter — single thread owns the current archive) |
| `crates/server/src/main.rs` | Remove `num_workers` / `shared_archive_state` setup; create archive thread |
| `crates/common/src/config.rs` | Remove `inbox_workers`; add `archive_batch_size` (default 200) |
| `crates/common/src/defaults_server.toml` | Replace `inbox_workers` with `archive_batch_size` |

---

## Testing

1. **Basic indexing:** Index a source, verify files are searchable immediately
   (inline), verify ZIP archives contain chunks after archive thread runs.
2. **Re-indexing:** Re-index a file; verify old chunks are removed from ZIPs; new
   content replaces old.
3. **Delete while queued:** Delete a file between phase 1 and phase 2; verify archive
   thread skips gracefully, no errors.
4. **Crash recovery:** Kill server mid-to-archive; restart; verify `to-archive/` files
   are picked up and completed.
5. **No deadlocks:** Run a full scan on a WSL/network-mounted DB; verify no workers
   hang.

---

## Rollback alternative

The user noted that rolling back to the commit before plan 049 is an option, with
plans 050–052 rebased on top. Assessment:

- Plan 049 did not land as a single clean commit; it was interleaved with other
  work. The rebase surface area is significant (`worker.rs` and `archive.rs` are
  heavily modified in every plan since 049).
- Plans 050–052 each touch `worker.rs`, so rebase conflicts would be non-trivial.
- The forward path (this plan) is architecturally cleaner: it preserves all features,
  eliminates the root cause, and does not require conflict resolution.

**Recommendation: implement this plan rather than rolling back.**

---

## Open questions

1. Should `inline_threshold_bytes` continue to be honoured as a permanent setting
   (small files stay inline forever) or should it become purely a "temporary storage
   during archiving" mechanism with all files eventually in ZIPs? The current
   behaviour (small files permanently inline) is fine to keep.

2. **Archive thread batching strategy.** A delay of minutes before ZIP content is
   available is perfectly acceptable — the index is searchable immediately and the
   original file is always accessible via "Show original". The priority is
   minimising ZIP rewrites, not minimising latency.

   **Trigger mechanism:** The indexing thread signals a `Notify` each time it
   deposits a file into `to-archive/`. The archive thread waits on that `Notify`
   (with a maximum idle timeout, e.g. 60 s, so it eventually runs even if signals
   are missed). On wake, it does not start immediately — it first waits a short
   settling period (e.g. 5 s) to allow more files to accumulate, then picks up the
   next batch.

   **Batch size:** configurable (`archive_batch_size`, default: 200). On each
   activation the archive thread reads at most `archive_batch_size` `.gz` files from
   `to-archive/` (sorted by mtime, oldest first). If the queue still has files after
   the batch completes, the thread loops immediately without waiting for another
   signal — it keeps processing batches until the queue is empty, then returns to
   sleep.

   **Per-batch work plan:** across the N files in a batch:
   - For each path, keep only the **latest** upsert (by submission order). Earlier
     versions of the same path within the batch are superseded — skip their content
     write.
   - Collect all `pending_chunk_removes` entries for the batch. Deduplicate by
     `(archive_name, chunk_name)`.
   - If a path appears in both an upsert and a delete within the batch, cancel the
     content write and still process the chunk remove.

   **Execution order within a batch:**
   a. Process all removes first, grouped by archive — each affected archive is
      rewritten **at most once** per batch regardless of how many files it contained.
   b. Write new chunks for all surviving upserts, appending sequentially to the
      current write archive.
   c. Commit all SQLite line-ref updates (one transaction per source DB).
   d. Delete the processed `.gz` files from `to-archive/`.

   A file added and deleted within the same batch generates zero ZIP writes. A file
   re-indexed N times within the same batch generates one chunk write and at most one
   archive rewrite.
