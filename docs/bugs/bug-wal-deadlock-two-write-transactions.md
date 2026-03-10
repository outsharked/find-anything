# Bug: Worker deadlock on WAL-mode DB with two separate write transactions

## Symptom

After startup with a fresh database, all inbox workers hang indefinitely on a single
source and stop processing new requests. The server log shows workers entering
`process_file` ("Indexing ...") or waiting for the source lock ("Processing N deletes")
and never producing a "done" log line. New bulk requests accumulate in the inbox but
are never started.

## Root cause

`process_file` (in `crates/server/src/worker.rs`) needed the `file_id` (SQLite
`AUTOINCREMENT` primary key) before writing chunks, so that chunk names could be
`{file_id}.{N}`. The straightforward fix was to INSERT the `files` row as an
**auto-commit statement** first, then issue a second `conn.transaction()` later
for the `lines`/FTS writes:

```rust
// Step 1 — auto-commit write
conn.execute("INSERT INTO files ... ON CONFLICT DO UPDATE ...", ...)?;

// Step 2 — read
let file_id = conn.query_row("SELECT id FROM files WHERE path = ?1", ...)?;

// ... ZIP I/O ...

// Step 3 — explicit transaction write
let tx = conn.transaction()?;
tx.execute("DELETE FROM lines ...", ...)?;
// ... insert lines + FTS
tx.commit()?;
```

This exposes a known SQLite WAL-mode issue on **network or WSL mounts** (e.g.
`/mnt/find-anything-db/`): WAL mode uses POSIX advisory file locks for write
serialisation. On these mount types, the lock state left by the Step 1 auto-commit
is not reliably visible to the same process when Step 3 attempts to acquire the
write lock again, causing the second transaction to hang indefinitely waiting for a
lock that is never released.

The existing `source_lock` (an application-level `std::sync::Mutex` keyed by source
name, stored in `SharedArchiveState`) guards **inter-worker** write serialisation for
exactly this reason, but it cannot help with two write operations issued by the
**same worker on the same connection**.

## Fix

Consolidate all DB writes into a **single transaction**, obtaining `file_id` via
`INSERT ... RETURNING id` instead of a separate SELECT:

```rust
let tx = conn.transaction()?;

let file_id: i64 = tx.query_row(
    "INSERT INTO files (...) VALUES (...) ON CONFLICT(path) DO UPDATE SET ... RETURNING id",
    params![...],
    |row| row.get(0),
)?;

// ZIP append_chunks runs inside the open transaction.
// In WAL mode, concurrent readers are not blocked by an open write transaction.
let chunk_refs = archive_mgr.append_chunks(...)?;

tx.execute("DELETE FROM lines WHERE file_id = ?1", ...)?;
// ... insert lines + FTS
tx.commit()?;
```

`RETURNING id` on `INSERT ... ON CONFLICT DO UPDATE` is supported from SQLite 3.35.0
(2021) and returns the row's `id` regardless of whether a new row was inserted or an
existing one updated.

## Key facts

- Only manifests on WAL-mode databases stored on mounts where POSIX advisory
  locking is unreliable (WSL `/mnt/` paths, certain network shares).
- Does **not** reproduce on a local `ext4`/`tmpfs` filesystem.
- Introduced in plan 052 when the chunk-naming scheme changed to `{file_id}.{N}`,
  requiring `file_id` before the ZIP write.
- Fixed in the same commit by using a single transaction with `RETURNING id`.

---

## Second manifestation — delete-only batches (March 2026)

### Symptom

Workers stuck after "Processing N deletes" with no "done" log line.  All
workers on the same source eventually pile up waiting for `source_lock`.

### Root cause

`process_request` issued **two write transactions on the same connection** for
every request that had deletes:

1. `delete_files` → `conn.unchecked_transaction()` → commit  (**T1**)
2. `clear_errors_for_paths(&conn, &request.delete_paths)` → `conn.unchecked_transaction()` → **HANGS**

The same WAL POSIX lock issue: after T1 commits and releases the write lock,
the second `BEGIN` (T2) on the same connection hangs indefinitely because the
POSIX lock state from T1 is not reliably cleared on WSL/network mounts.

T2 holds `source_lock` while hanging, so all other workers on that source pile
up waiting for it — exactly matching the observed log pattern.

### Fix

Two changes to avoid ever issuing a second write on the same connection:

1. **`delete_files` (db/mod.rs)** — error clearing for deleted paths is now
   done inside the `delete_files` transaction (same `BEGIN`/`COMMIT`), so
   callers never need a separate write for that purpose.

2. **Cleanup block (worker.rs)** — replaced four separate write calls
   (`clear_errors_for_paths` × 2, `upsert_indexing_errors` × 2,
   `update_last_scan`, `append_scan_history`) with a single call to
   `db::do_cleanup_writes`, which wraps all of them in **one**
   `unchecked_transaction()`.  For delete-only batches with no scan timestamp
   and no indexing failures, this function is a no-op — zero additional writes
   on the connection.
