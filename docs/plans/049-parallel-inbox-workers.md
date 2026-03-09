# Parallel Inbox Workers

## Overview

The inbox worker currently processes requests one at a time. A single slow request
(e.g. a large ZIP extraction that took 1696s) blocks the entire queue. This plan
introduces a per-source worker pool so that slow requests do not block other work,
and so that multiple requests for the same source can proceed in parallel.

## Background

The server receives indexed batches from clients as `.gz` files dropped into
`data_dir/inbox/`. A single background worker polls this directory every second
and processes files sequentially. Each request is handled by a blocking thread
via `tokio::task::spawn_blocking`.

Key constraints:
- **SQLite**: one DB per source; WAL mode serialises concurrent writers automatically
  without blocking readers.
- **ZIP archives**: all content chunks live in rotating ZIP archives under
  `data_dir/sources/content/`. Concurrent access to the same archive file would
  corrupt it.
- **Ordering**: for a single file, a later update must not be shadowed by an
  earlier one processed out of order.

## Design

### Worker pool per source

Spin up N worker tasks per source (default: 3, configurable). Each source has
its own async channel (bounded). A router task reads the inbox, peeks the source
name from each `.gz` filename, and sends it to the right channel. Workers drain
their channel sequentially with respect to each other but run concurrently across
sources and (with the archive scheme below) within a source.

### Inbox filename includes source name

To allow O(1) routing without decompressing each file, encode the source name in
the filename at write time:

```
req_{timestamp}_{source}_{uuid}.gz
```

The server's `/api/v1/bulk` handler already knows the source name; it just needs
to include it in the filename. Old filenames (without a source component) are
handled by peeking inside — backward-compatible fallback.

### Per-worker ZIP archive (no shared write target)

Instead of a single global "current archive" that all workers would fight over,
each worker owns its own in-progress archive for appending new chunks. Archives
are allocated from a shared atomic counter so no two workers ever receive the
same archive number.

```
Worker 1 → archive_00100.zip (its exclusive write target)
Worker 2 → archive_00101.zip (its exclusive write target)
Worker 3 → archive_00102.zip (its exclusive write target)
```

When a worker's archive reaches the 10 MB target size it atomically increments
the counter and starts a new archive. Up to N archives may be "in progress"
simultaneously.

### Per-archive rewrite lock

The only remaining conflict: re-indexing a file requires rewriting the archive
that holds its old chunks. Two workers could both need to rewrite the same old
archive simultaneously.

A shared lock registry (`Arc<Mutex<HashMap<PathBuf, Arc<Mutex<()>>>>>`) keyed
by archive path handles this. Before any archive rewrite the worker acquires
that archive's lock. Workers appending to their exclusive write archive never
need a lock at all — they are the sole writer by construction.

The write-target archive of worker A will almost never need rewriting by worker B,
because re-indexing touches old sealed archives, not freshly allocated ones.

### SQLite concurrency

Each worker opens its own `rusqlite::Connection`. WAL mode allows concurrent
readers and serialises writers automatically. No additional locking is needed
at the application level. Set a short busy timeout (e.g. 5 s) so a writer that
can't acquire the WAL lock fails fast rather than hanging indefinitely.

### Stale-update guard

If two requests for the same file arrive out of order (e.g. a watch event
overtakes a scan event in the queue), the later mtime wins. The DB upsert
already stores mtime; add a check: if the incoming mtime < stored mtime, skip
the upsert. This is a defence-in-depth measure; with FIFO per-source queues
out-of-order delivery should be rare.

### Timeout per worker

With N parallel workers a single stuck request no longer blocks the queue.
The per-request timeout (currently `REQUEST_TIMEOUT = 600s`) can be raised to
something generous like 2 hours. If a worker's blocking thread is abandoned at
timeout, it continues running in the background (blocking threads cannot be
cancelled) but the slot is freed for new work. Log a prominent error; the
request is moved to `failed/`.

## Configuration

Add to `[server]` in `server.toml`:

```toml
# Number of worker goroutines per source. Each worker can process one request
# at a time. Workers share an archive counter but never write to the same
# archive simultaneously. Default: 3.
inbox_workers_per_source = 3

# Maximum seconds a single inbox request may run before the worker abandons
# it and moves the file to failed/. Default: 7200 (2 hours).
inbox_request_timeout_secs = 7200
```

## Files Changed

| File | Change |
|------|--------|
| `crates/server/src/worker.rs` | Full rewrite of worker loop: router task + per-source channel + N workers per source; `SharedArchiveState` and per-archive rewrite lock |
| `crates/server/src/archive.rs` | Refactor `ArchiveManager` to hold `Arc<SharedArchiveState>` with atomic archive counter; per-archive rewrite locking; remove `&mut self` where possible |
| `crates/server/src/routes/bulk.rs` | Include source name in `.gz` filename |
| `crates/common/src/config.rs` | Add `inbox_workers_per_source` and `inbox_request_timeout_secs` to `ServerAppSettings` and `defaults_server.toml` |
| `crates/common/src/api.rs` | Update `WorkerStatus` to reflect N workers |

## Implementation Order

1. Add config fields (`inbox_workers_per_source`, `inbox_request_timeout_secs`)
2. Update bulk route to include source in filename; add backward-compatible fallback peek
3. Refactor `ArchiveManager`:
   - Extract `SharedArchiveState` (atomic counter + rewrite lock registry)
   - Change `ArchiveManager::new` to accept `Arc<SharedArchiveState>`
   - Add `SharedArchiveState::new_for_data_dir` constructor (server creates one per data_dir)
   - Replace global "current archive" with per-instance write archive
   - Wrap all rewrite operations in the per-archive lock
4. Update `process_request` to accept `Arc<SharedArchiveState>` instead of creating a local `ArchiveManager`
5. Rewrite `start_inbox_worker`:
   - Router task: scan inbox, parse source from filename (or peek), send to source channel
   - Per-source worker spawner: create N worker tasks per source on first request seen
   - Each worker task: pull from channel, `spawn_blocking(process_request(...))` with timeout
6. Update `WorkerStatus` to expose per-worker state
7. Tests: concurrent archive writes don't corrupt; out-of-order mtime guard

## Testing

- Unit test: two `ArchiveManager` instances with shared `SharedArchiveState` write concurrently → no duplicate archive numbers, no corruption
- Integration test: submit 6 requests for the same source simultaneously → all processed, no data loss
- Manual: confirm `find-admin status` shows per-worker state
