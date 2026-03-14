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

### Global worker pool

Spin up N workers globally (default: 3, configurable). A single bounded async
channel holds pending inbox paths. The router task scans the inbox directory
every second, sorts files by modification time (preserving submission order),
and sends each path to the shared channel. Workers pull from the channel
concurrently and each processes one request at a time.

Per-source pools were considered but rejected: sources vary enormously in
activity, so reserving N slots per source wastes resources on quiet sources
and adds routing complexity. A global pool naturally distributes work to
wherever capacity is available.

### Ordering and stale-update guard

With a global pool, two workers could pick up consecutive requests for the
same source and process them out of order (the one that picked up the older
request finishes after the one that picked up the newer). To guard against
this, the DB upsert checks the stored mtime before writing: if the incoming
mtime ≤ stored mtime, the upsert is skipped. This is defence-in-depth; in
practice the FIFO channel and submission-time ordering make true races rare.

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
# Total number of inbox workers. Each worker processes one request at a time.
# Workers share an archive counter but never write to the same archive
# simultaneously. Default: 3.
inbox_workers = 3

# Maximum seconds a single inbox request may run before the worker abandons
# it and moves the file to failed/. Default: 1800 (30 minutes).
inbox_request_timeout_secs = 1800
```

## Files Changed

| File | Change |
|------|--------|
| `crates/server/src/worker.rs` | Rewrite worker loop: single bounded channel + N global workers; router sorts by mtime and sends to channel; stale-mtime guard in upsert |
| `crates/server/src/archive.rs` | Add `SharedArchiveState` (atomic archive counter + per-archive rewrite lock registry); refactor `ArchiveManager` to take `Arc<SharedArchiveState>` and own its exclusive write archive |
| `crates/common/src/config.rs` | Add `inbox_workers` and `inbox_request_timeout_secs` to `ServerAppSettings` and `defaults_server.toml` |

## Implementation Order

1. Add config fields (`inbox_workers`, `inbox_request_timeout_secs`)
2. Refactor `ArchiveManager`:
   - Extract `SharedArchiveState` (atomic counter + rewrite lock registry)
   - `ArchiveManager::new` accepts `Arc<SharedArchiveState>`
   - Replace global "current archive" scan with per-worker allocated archive number
   - Wrap rewrite operations in the per-archive lock
3. Update `process_request` to accept `Arc<SharedArchiveState>` instead of creating a local `ArchiveManager`; add stale-mtime guard
4. Rewrite `start_inbox_worker`:
   - Create `SharedArchiveState` once
   - Spawn N worker tasks, each looping on a shared `Arc<Mutex<Receiver>>`
   - Router loop: scan inbox, sort by mtime, send paths to channel
5. Update `main.rs` to pass new config fields

## Testing

- Unit test: two `ArchiveManager` instances with shared `SharedArchiveState` write concurrently → no duplicate archive numbers, no corruption
- Integration test: submit N requests simultaneously → all processed, no data loss
- Stale-mtime guard unit test: upsert with older mtime is skipped
