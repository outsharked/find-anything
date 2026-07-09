# Inbox Group Coalescing (issue #59) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.
>
> **Project override:** per CLAUDE.md, do **not** run `git commit` without explicit user
> instruction. At each "Checkpoint" step, stop and let the user review/test/commit.

**Goal:** Coalesce processing of multiple consecutive inbox `.gz` files into one held-open SQLite connection with a shared commit cadence, so bursts of single-file `BulkRequest`s (e.g. a file watcher pushing one changed file at a time) no longer pay one full WAL/btree commit per file — the "lever #2" from issue #59.

**Architecture:** The router dispatches a *group* of pending inbox files (bounded by count and compressed bytes) instead of a single path. The blocking worker iterates the group, keeping one `SourceSession` (connection + open transaction) per consecutive same-source run, committing every `PHASE1_BATCH_SIZE` write units *across* request boundaries. The crash-safety invariant becomes: **an inbox `.gz` is deleted only after a COMMIT covering all of its writes** (and after its to-archive payload is on disk). No transaction is ever held open waiting for future arrivals — coalescing applies only to work already queued.

**Tech Stack:** Rust, rusqlite (manual `BEGIN`/`COMMIT` + RAII savepoints), tokio (`spawn_blocking`, `mpsc`, `Notify`, `watch`).

---

## Overview

Issue #59 background: a production deployment with a 19 GB source DB showed ~100-250x
write amplification during upload bursts. Commit `def666f` (plan-level lever #1) batched
commits every `PHASE1_BATCH_SIZE = 25` files *within* one request's file list. That does
nothing for the motivating burst pattern: many **single-file** `BulkRequest`s arriving in
quick succession — each request still costs one full commit (WAL frame + interior btree
page rewrite of a multi-GB DB).

This plan implements lever #2: sharing one connection/transaction cadence across
*multiple consecutive inbox files*, so 25 single-file requests cost ~1 commit instead of 25.

## Design Decisions

### How each risk from issue #59 is resolved

1. **"Fully sequential today, by design"** — kept. There is still exactly one indexing
   worker task processing one unit at a time. Only the *unit of dispatch* changes: the
   router→worker channel carries `Vec<PathBuf>` (a group) instead of `PathBuf`. Channel
   capacity stays 1.

2. **Lock-widening** — bounded and accepted, for three reasons:
   - The phase-1 worker is effectively the **sole writer** to source DBs (verified:
     phase 2 / `archive_batch.rs` only *reads* `files.file_hash`; route handlers never
     write — the "all DB writes go through the inbox worker" invariant).
   - The commit cadence is unchanged (every 25 write units), so the maximum
     uncommitted-work window per commit is the same order as today's within-request
     batching from `def666f`. Content-store reads already happen inside the enclosing
     transaction since that change.
   - The `sqlite3_interrupt` escape hatch is preserved: the group holds a shared
     `Mutex<Option<InterruptHandle>>` slot, refreshed each time a session opens a
     connection, so the timeout path can always interrupt the *current* connection.
   - Group size caps (`MAX_GROUP_REQUESTS = 32`, `MAX_GROUP_GZ_BYTES = 8 MiB` of
     compressed input) bound total blocking-task duration under the existing
     `inbox_request_timeout_secs` (default 1800 s), which now applies per **group**.

3. **Search-visibility latency** — eliminated by construction, no time-based flush
   needed: a group is formed only from files **already in the inbox** at dispatch time.
   The worker never waits for future arrivals while holding a transaction. A lone file
   in a quiet period forms a group of one and behaves exactly like today. The only
   added visibility delay is the time to process up to 24 more already-queued files
   before the next cadence commit — during a burst, which is when it's acceptable.

4. **Crash-recovery blast radius** — the recovery unit stays "one inbox `.gz` file".
   New invariant: an inbox `.gz` is deleted only at a **flush point** (after the COMMIT
   covering all of its writes, and after its normalized to-archive `.gz` is written to
   disk). A crash mid-group leaves every not-yet-flushed `.gz` in `inbox/`; on restart
   they are simply reprocessed. Reprocessing an already-committed request is the same
   hazard class that exists **today** (crash after COMMIT but before the async-side
   `remove_file`) and is idempotent: upserts by path, FTS delete+reinsert, content-store
   `put` keyed by hash, deletes/renames all tolerate replay.

### Flush points and ordering

A `SourceSession` accumulates per-request side effects and releases them only at a
flush point (cadence commit, source switch, or group end), **in this order**:

1. `COMMIT` the source-DB transaction.
2. Write each pending normalized to-archive `.gz` (buffered as gzipped bytes in memory).
3. Notify the archive worker.
4. Delete the covered inbox `.gz` files.
5. Apply pending `SourceStatsDelta`s to the stats cache and tick `stats_watch`.

Why the to-archive write is deferred to *after* COMMIT (step 2): phase 2's stale-content
check reads `files.file_hash` from the source DB. If the to-archive `.gz` were written
while phase 1's transaction was still open, the archive worker's periodic 60 s tick could
read the *old* committed hash, conclude the gz is stale, skip the blob, and delete the
gz — permanently losing content until the next reindex. Buffering the compressed bytes
in memory is bounded by `MAX_GROUP_GZ_BYTES` (normalized output compresses to roughly
input size), and a single oversized request still forms a group of one, exactly like
today's memory profile.

Why inbox deletion is after the to-archive write (step 4 after step 2): never delete the
recovery unit before its phase-2 payload exists on disk.

### Failure semantics (per outcome)

| Failure | Handling |
|---|---|
| gz decode/parse error | Move that `.gz` to `failed/`, continue group. No DB state touched. |
| `SQLITE_BUSY/LOCKED` anywhere | `ROLLBACK`, end group. All unflushed `.gz` (including current) stay in `inbox/` and are retried next router tick. Matches today's "will retry" path; timeout counter untouched. |
| Other request-level error | `ROLLBACK` (unflushed earlier requests stay in inbox → retried, idempotent), move only the **failing** `.gz` to `failed/`, open a fresh session, continue with the next file. |
| Finalize (COMMIT) error, non-lock | Log error; session's `.gz` files stay in inbox and retry next tick. (Disk-full/corruption territory — retrying with error logs is the sane behavior; there is no single file to blame.) |
| Group timeout | Interrupt current connection via the shared handle; move the **current** `.gz` (from the shared progress slot) to `failed/` (poison protection, as today); already-flushed files are gone; the rest stay in inbox and retry. Circuit breaker counts the timeout as today. |
| Blocking-task panic | Move the current `.gz` to `failed/`; rest retried. |

Known benign race (exists today in equivalent form): a timed-out-but-still-running
blocking task may reach a flush and delete a `.gz` the async side just moved to
`failed/` — one of the two fs ops fails and is logged; data-wise both orders are safe.

### What is deliberately NOT done

- No config surface for group caps — constants, like `PHASE1_BATCH_SIZE` (YAGNI).
- No same-source filtering in the router (it can't know the source without decoding);
  mixed-source groups are handled by switching sessions mid-group, which finalizes the
  previous source's transaction first. Consecutive same-source runs — the burst pattern —
  get the full benefit.
- No change to phase 2, `pipeline.rs` file processing, FTS handling, or the schema.
- No `MIN_CLIENT_VERSION` bump — zero HTTP API change.

## Files Changed

| File | Change |
|---|---|
| `crates/server/src/db/mod.rs` | Make `delete_files_phase1`, `rename_files`, `log_activity` safe inside an open transaction (autocommit guard) |
| `crates/server/src/db/stats.rs` | Same guard for `do_cleanup_writes` |
| `crates/server/src/worker/group.rs` | **New**: `Phase1Handles`, `GroupContext`, `GroupProgress`, `SourceSession`, `process_group_phase1`, `process_group_async`, group caps |
| `crates/server/src/worker/request.rs` | `process_request_phase1`/`process_request_async` replaced by `decode_request` + `process_one_request` (operates on a `SourceSession`); tests rewired through group-of-one |
| `crates/server/src/worker/mod.rs` | Router forms bounded groups (`form_group` + tests); channel becomes `Vec<PathBuf>`; worker task calls `process_group_async`; `IndexerHandles` split into `Phase1Handles` + async-side fields |
| `crates/server/tests/burst_coalescing.rs` | **New** integration test: burst of single-file bulk posts |
| `CLAUDE.md`, `docs/ARCHITECTURE.md` | Update write-path / worker description |
| `CHANGELOG.md` | `[Unreleased]` entry |

---

## Task 1: Transaction-nesting-safe DB write helpers

Today `delete_files_phase1`, `rename_files`, `log_activity` (`db/mod.rs`) and
`do_cleanup_writes` (`db/stats.rs`) each open their own `conn.unchecked_transaction()`.
Under grouping they run **inside** an already-open transaction, where the inner `BEGIN`
fails with *"cannot start a transaction within a transaction"*. (Today they happen to run
in autocommit mode — deletes/renames run before the file-loop `BEGIN`, cleanup/activity
after the final `COMMIT`.)

Fix pattern for all four: only open the inner transaction when in autocommit mode, and
run statements against `conn` directly so they work either way.

**Files:**
- Modify: `crates/server/src/db/mod.rs` (`delete_files_phase1` ~line 547, `rename_files` ~line 589, `log_activity` ~line 328)
- Modify: `crates/server/src/db/stats.rs` (`do_cleanup_writes` ~line 189)
- Test: `crates/server/src/db/mod.rs` tests module (or a new `#[cfg(test)] mod nesting_tests`)

- [x] **Step 1.1: Write the failing tests**

Add to the tests in `crates/server/src/db/mod.rs` (create a local in-memory conn helper
matching the one in `worker/pipeline.rs` if the module doesn't already have one):

```rust
#[cfg(test)]
mod tx_nesting_tests {
    use super::*;

    fn test_conn() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("PRAGMA foreign_keys = ON;").unwrap();
        conn.execute_batch(include_str!("../schema_v4.sql")).unwrap();
        crate::db::register_scalar_functions(&conn).unwrap();
        conn
    }

    #[test]
    fn delete_files_phase1_works_inside_open_transaction() {
        let conn = test_conn();
        conn.execute_batch("BEGIN").unwrap();
        delete_files_phase1(&conn, &["nope.txt".to_string()]).unwrap();
        conn.execute_batch("COMMIT").unwrap();
    }

    #[test]
    fn rename_files_works_inside_open_transaction() {
        let conn = test_conn();
        conn.execute_batch("BEGIN").unwrap();
        rename_files(&conn, &[find_common::api::PathRename {
            old_path: "a.txt".to_string(),
            new_path: "b.txt".to_string(),
        }]).unwrap();
        conn.execute_batch("COMMIT").unwrap();
    }

    #[test]
    fn log_activity_works_inside_open_transaction() {
        let conn = test_conn();
        conn.execute_batch("BEGIN").unwrap();
        log_activity(&conn, 1000, &["x.txt".to_string()], &[], &[], &[], 100).unwrap();
        conn.execute_batch("COMMIT").unwrap();
        let n: i64 = conn.query_row("SELECT COUNT(*) FROM activity_log", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 1);
    }

    #[test]
    fn do_cleanup_writes_works_inside_open_transaction() {
        let conn = test_conn();
        conn.execute_batch("BEGIN").unwrap();
        crate::db::do_cleanup_writes(&conn, &["x.txt".to_string()], &[], 1000, Some(999)).unwrap();
        conn.execute_batch("COMMIT").unwrap();
    }
}
```

(Adjust the `do_cleanup_writes` import path to wherever it is re-exported; it lives in
`db/stats.rs`. If it is `pub` from `crate::db`, call it as above.)

- [x] **Step 1.2: Run tests to verify they fail**

Run: `cargo test -p find-server --lib tx_nesting`
Expected: all 4 FAIL with `SqliteFailure(... "cannot start a transaction within a transaction")`

- [x] **Step 1.3: Apply the guard to all four functions**

The transform, shown for `delete_files_phase1` (apply the same shape to the other three):

```rust
pub fn delete_files_phase1(conn: &Connection, paths: &[String]) -> Result<DeleteDelta> {
    let mut delta = DeleteDelta { files_removed: 0, size_removed: 0, by_kind: HashMap::new() };

    // Only open an inner transaction when in autocommit mode; when the caller
    // already holds one (group-coalesced phase 1), run inside it directly.
    let tx = if conn.is_autocommit() { Some(conn.unchecked_transaction()?) } else { None };

    for path in paths {
        if !is_composite(path) {
            let row: Option<(i64, String)> = conn.query_row(
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
        delete_one_path_simple(conn, path)?;
        conn.execute("DELETE FROM indexing_errors WHERE path = ?1", params![path])?;
        conn.execute(
            "DELETE FROM indexing_errors WHERE path LIKE ?1",
            params![format!("{}::%", path)],
        )?;
    }

    cleanup_singleton_duplicates_tx(conn)?;

    if let Some(tx) = tx { tx.commit()?; }
    Ok(delta)
}
```

Key mechanics: every statement previously issued through `tx.…` moves to `conn.…`
(`Transaction` derefs to `Connection`, so helper fns taking `&Connection` accept `conn`
unchanged). The `tx` binding becomes a guard held for the duration and committed at the
end when present; dropping it early on `?`-return still rolls back, exactly as before.
Apply identically in `rename_files`, `log_activity`, and `do_cleanup_writes`.

- [x] **Step 1.4: Run the tests and the existing db suite**

Run: `cargo test -p find-server --lib db`
Expected: PASS (new tests plus all pre-existing db tests — autocommit callers are unaffected)

- [x] **Step 1.5: Checkpoint** — clippy clean (`mise run clippy`), pause for user review.

---

## Task 2: Extract `decode_request` + `process_one_request` (behavior-preserving)

Split `process_request_phase1` so the per-request logic no longer owns the connection
lifecycle. After this task the old orchestration still exists and behaves identically;
existing tests pass unchanged. The session type it will target arrives in Task 3, so in
this task `process_one_request` takes `&mut Connection` plus an explicit
"commit cadence" closure seam — concretely: keep it simple and mechanical:

- `decode_request(path) -> Result<(BulkRequest, usize /*compressed bytes*/)>` — lines
  currently at `request.rs:232-240` plus the metadata read.
- `process_request_phase1` keeps: open db, send interrupt handle, `BEGIN`/cadence/`COMMIT`
  loop control, final to-archive write — i.e. it stays the single-request orchestrator,
  but the *decode* is now a named function. (Full extraction of the per-request body
  happens in Task 4 when the session exists to receive it; doing it in two passes keeps
  each diff reviewable and the test suite green in between.)

**Files:**
- Modify: `crates/server/src/worker/request.rs`

- [x] **Step 2.1: Extract `decode_request`**

```rust
/// Read and parse one inbox `.gz` into a `BulkRequest`.
/// Returns the request and the compressed file size (for logging).
fn decode_request(request_path: &Path) -> Result<(BulkRequest, usize)> {
    let compressed_bytes = std::fs::metadata(request_path)
        .map(|m| m.len() as usize)
        .unwrap_or(0);
    let file = std::fs::File::open(request_path)?;
    let decoder = GzDecoder::new(BufReader::new(file));
    let request: BulkRequest =
        serde_json::from_reader(decoder).context("parsing bulk request JSON")?;
    Ok((request, compressed_bytes))
}
```

Replace the inline block in `process_request_phase1` with a call to it (keep the
`timed!` wrapper at the call site).

- [x] **Step 2.2: Run the worker test suite**

Run: `cargo test -p find-server --lib worker`
Expected: PASS, unchanged behavior

- [x] **Step 2.3: Checkpoint** — pause for user review.

---

## Task 3: `worker/group.rs` — `Phase1Handles`, `GroupProgress`, `SourceSession`

**Files:**
- Create: `crates/server/src/worker/group.rs`
- Modify: `crates/server/src/worker/mod.rs` (add `mod group;`)

- [x] **Step 3.1: Create the module with the shared types**

```rust
/// Group-coalesced phase 1 (issue #59, plan 093).
///
/// The router dispatches a bounded group of inbox `.gz` files. This module
/// processes the group under per-source `SourceSession`s: one SQLite
/// connection with an open transaction, committed every `PHASE1_BATCH_SIZE`
/// write units *across* request boundaries.
///
/// Crash-safety invariant: an inbox `.gz` is deleted only at a flush point —
/// after the COMMIT covering all of its writes and after its normalized
/// to-archive `.gz` is on disk. A crash mid-group leaves unflushed files in
/// `inbox/`; reprocessing them is idempotent.
use anyhow::Result;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::Ordering;

use find_common::api::RecentFile;
use find_content_store::ContentStore;

use crate::db;
use crate::stats_cache::{SourceStatsCache, SourceStatsDelta};

use super::{StatusHandle, WorkerConfig};
use super::request::{self, is_db_locked};

/// How many write units (files, plus one per delete/rename batch) to
/// accumulate before committing the source-DB transaction. Shared cadence
/// across all requests in a group — this is what collapses one-commit-per-
/// single-file-request bursts into one commit per 25 requests.
pub(super) const PHASE1_BATCH_SIZE: usize = 25;

/// Router-side group caps. A group always contains at least one file, so a
/// single oversized request degrades to today's one-at-a-time behavior.
pub(super) const MAX_GROUP_REQUESTS: usize = 32;
pub(super) const MAX_GROUP_GZ_BYTES: u64 = 8 * 1024 * 1024;

/// Handles needed by the blocking phase-1 code. Split out of `IndexerHandles`
/// so the whole bundle can be cloned into `spawn_blocking` (project convention:
/// config struct over threaded parameters).
#[derive(Clone)]
pub(super) struct Phase1Handles {
    pub status:             StatusHandle,
    pub cfg:                WorkerConfig,
    pub recent_tx:          tokio::sync::broadcast::Sender<RecentFile>,
    pub stats_watch:        Arc<tokio::sync::watch::Sender<u64>>,
    pub content_store:      Arc<dyn ContentStore>,
    pub source_stats_cache: Arc<std::sync::RwLock<SourceStatsCache>>,
    pub archive_notify:     Arc<tokio::sync::Notify>,
}

/// Path context for one dispatched group.
#[derive(Clone)]
pub(super) struct GroupContext {
    pub data_dir:       PathBuf,
    /// Inbox `.gz` files in mtime order.
    pub paths:          Vec<PathBuf>,
    pub failed_dir:     PathBuf,
    pub to_archive_dir: PathBuf,
}

/// Shared between the async wrapper and the blocking task so the timeout path
/// can (a) interrupt whichever connection is currently open and (b) know which
/// inbox file was in flight (poison-move target).
#[derive(Default)]
pub(super) struct GroupProgress {
    pub interrupt: std::sync::Mutex<Option<rusqlite::InterruptHandle>>,
    pub current:   std::sync::Mutex<Option<PathBuf>>,
}

/// Per-group outcome counts (logging only).
#[derive(Default, Debug)]
pub(super) struct GroupOutcome {
    /// Requests fully processed and flushed (inbox gz deleted).
    pub processed: usize,
    /// Requests moved to failed/.
    pub failed: usize,
    /// Requests left in inbox/ for retry (lock contention or finalize error).
    pub retry: usize,
}

/// A to-archive payload buffered until the covering COMMIT: destination path
/// and the already-gzipped normalized `BulkRequest` bytes.
pub(super) type ArchivePayload = (PathBuf, Vec<u8>);

/// One source DB connection with an open transaction and the side effects
/// accumulated since the last flush point.
pub(super) struct SourceSession {
    pub source: String,
    pub conn: rusqlite::Connection,
    since_commit: usize,
    /// Inbox gz files fully processed but not yet covered by a COMMIT.
    deletable_gz: Vec<PathBuf>,
    /// Normalized to-archive gz bytes, written at flush (after COMMIT).
    pending_archive: Vec<ArchivePayload>,
    /// Stats deltas applied at flush.
    pending_deltas: Vec<SourceStatsDelta>,
}

impl SourceSession {
    pub(super) fn open(data_dir: &Path, source: &str, progress: &GroupProgress) -> Result<Self> {
        let db_path = data_dir.join("sources").join(format!("{source}.db"));
        let conn = db::open(&db_path)?;
        // Refresh the shared interrupt slot so the async timeout path can
        // always unblock the connection currently doing work.
        if let Ok(mut slot) = progress.interrupt.lock() {
            *slot = Some(conn.get_interrupt_handle());
        }
        conn.execute_batch("BEGIN")?;
        Ok(Self {
            source: source.to_string(),
            conn,
            since_commit: 0,
            deletable_gz: Vec::new(),
            pending_archive: Vec::new(),
            pending_deltas: Vec::new(),
        })
    }

    /// Count `n` write units toward the commit cadence; flush if due.
    pub(super) fn add_units_and_maybe_commit(&mut self, n: usize, h: &Phase1Handles) -> Result<()> {
        self.since_commit += n;
        if self.since_commit >= PHASE1_BATCH_SIZE {
            self.commit_point(h)?;
        }
        Ok(())
    }

    /// Record a fully-processed request; its side effects release at the next flush.
    pub(super) fn request_done(
        &mut self,
        inbox_gz: PathBuf,
        delta: SourceStatsDelta,
        archive_payload: Option<ArchivePayload>,
    ) {
        self.deletable_gz.push(inbox_gz);
        self.pending_deltas.push(delta);
        if let Some(p) = archive_payload {
            self.pending_archive.push(p);
        }
    }

    /// COMMIT and release accumulated side effects, then reopen a transaction.
    fn commit_point(&mut self, h: &Phase1Handles) -> Result<()> {
        self.conn.execute_batch("COMMIT")?;
        self.flush_committed(h);
        self.conn.execute_batch("BEGIN")?;
        self.since_commit = 0;
        Ok(())
    }

    /// COMMIT and release side effects; consumes the session (group end / source switch).
    pub(super) fn finalize(mut self, h: &Phase1Handles) -> Result<()> {
        self.conn.execute_batch("COMMIT")?;
        self.flush_committed(h);
        Ok(())
    }

    /// Roll back uncommitted work. Unflushed inbox gz files remain on disk
    /// and are rediscovered by the router (retry).
    pub(super) fn abort(self) {
        if let Err(e) = self.conn.execute_batch("ROLLBACK") {
            tracing::warn!("Rollback failed for source {}: {e}", self.source);
        }
    }

    /// Post-COMMIT release, in crash-safe order:
    /// 1. write to-archive payloads  2. notify archive worker
    /// 3. delete covered inbox gz    4. apply stats deltas + tick watch.
    fn flush_committed(&mut self, h: &Phase1Handles) {
        let mut archived_any = false;
        for (path, bytes) in self.pending_archive.drain(..) {
            match std::fs::write(&path, &bytes) {
                Ok(()) => archived_any = true,
                Err(e) => tracing::error!("Failed to write to-archive {}: {e}", path.display()),
            }
        }
        if archived_any {
            h.archive_notify.notify_one();
        }
        for gz in self.deletable_gz.drain(..) {
            if let Err(e) = std::fs::remove_file(&gz) {
                tracing::error!("Failed to delete processed request {}: {e}", gz.display());
            }
        }
        if !self.pending_deltas.is_empty() {
            if let Ok(mut guard) = h.source_stats_cache.write() {
                for delta in self.pending_deltas.drain(..) {
                    guard.apply_delta(&delta);
                }
            } else {
                self.pending_deltas.clear();
            }
            h.stats_watch.send_modify(|v| *v = v.wrapping_add(1));
        }
    }
}

pub(super) fn move_to_failed(path: &Path, failed_dir: &Path) {
    let Some(name) = path.file_name() else { return };
    let dst = failed_dir.join(name);
    if let Err(e) = std::fs::rename(path, &dst) {
        tracing::error!("Failed to move {} to failed dir: {e}", path.display());
    } else {
        tracing::warn!("Moved failed request to: {}", dst.display());
    }
}
```

Note: `PHASE1_BATCH_SIZE` moves here from `request.rs` — delete the old constant and
fix the one existing test import (`use super::*` in request.rs tests picks it up via a
re-export: add `pub(super) use group::PHASE1_BATCH_SIZE;` in `worker/mod.rs`, or import
it directly in the test module).

- [x] **Step 3.2: Compile**

Run: `cargo check -p find-server`
Expected: compiles (new module unused yet is fine; silence dead-code warnings only if
clippy complains, by wiring in Task 4 immediately — do Tasks 3+4 as one clippy unit).

---

## Task 4: `process_one_request` on a session + `process_group_phase1`

This is the core change: the body of `process_request_phase1` moves into
`process_one_request(session, …)`; the connection/transaction lifecycle moves into the
group loop.

**Files:**
- Modify: `crates/server/src/worker/request.rs` (replace `process_request_phase1`)
- Modify: `crates/server/src/worker/group.rs` (add `process_group_phase1`)

- [x] **Step 4.1: Write the failing group tests**

In `group.rs`, a `#[cfg(test)]` module. Test helper first:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use find_common::api::{BulkRequest, FileKind, IndexFile, IndexLine};
    use find_content_store::SqliteContentStore;
    use flate2::write::GzEncoder;
    use std::io::Write as _;
    use tempfile::TempDir;

    struct TestEnv {
        _tmp: TempDir,
        data_dir: PathBuf,
        inbox_dir: PathBuf,
        failed_dir: PathBuf,
        to_archive_dir: PathBuf,
        handles: Phase1Handles,
    }

    fn setup() -> TestEnv {
        let tmp = TempDir::new().unwrap();
        let data_dir = tmp.path().to_path_buf();
        let inbox_dir = data_dir.join("inbox");
        let failed_dir = inbox_dir.join("failed");
        let to_archive_dir = inbox_dir.join("to-archive");
        std::fs::create_dir_all(&failed_dir).unwrap();
        std::fs::create_dir_all(&to_archive_dir).unwrap();
        std::fs::create_dir_all(data_dir.join("sources")).unwrap();
        let (recent_tx, _rx) = tokio::sync::broadcast::channel(64);
        let (watch_tx, _watch_rx) = tokio::sync::watch::channel(0u64);
        let handles = Phase1Handles {
            status: Arc::new(std::sync::Mutex::new(find_common::api::WorkerStatus::Idle)),
            cfg: WorkerConfig {
                request_timeout: std::time::Duration::from_secs(30),
                archive_batch_size: 100,
                activity_log_max_entries: 1000,
                normalization: find_common::config::NormalizationSettings::default(),
                consecutive_timeout_limit: 0,
                alerts: find_common::config::AlertsConfig::default(),
            },
            recent_tx,
            stats_watch: Arc::new(watch_tx),
            content_store: Arc::new(SqliteContentStore::open(&data_dir, None, None, None).unwrap()),
            source_stats_cache: Arc::new(std::sync::RwLock::new(Default::default())),
            archive_notify: Arc::new(tokio::sync::Notify::new()),
        };
        TestEnv { _tmp: tmp, data_dir, inbox_dir, failed_dir, to_archive_dir, handles }
    }

    fn make_file(path: &str, mtime: i64) -> IndexFile {
        IndexFile {
            path: path.to_string(),
            mtime,
            size: Some(42),
            kind: FileKind::Text,
            scanner_version: 1,
            lines: vec![IndexLine { archive_path: None, line_number: 0, content: path.to_string() }],
            extract_ms: None,
            file_hash: None,
            is_new: true,
            force: false,
        }
    }

    fn single_file_request(source: &str, path: &str, mtime: i64) -> BulkRequest {
        BulkRequest {
            source: source.to_string(),
            files: vec![make_file(path, mtime)],
            delete_paths: vec![],
            rename_paths: vec![],
            scan_timestamp: Some(mtime),
            indexing_failures: vec![],
        }
    }

    fn write_gz(env: &TestEnv, name: &str, req: &BulkRequest) -> PathBuf {
        let path = env.inbox_dir.join(name);
        let file = std::fs::File::create(&path).unwrap();
        let mut enc = GzEncoder::new(file, flate2::Compression::default());
        serde_json::to_writer(&mut enc, req).unwrap();
        enc.finish().unwrap();
        path
    }

    fn run_group(env: &TestEnv, paths: Vec<PathBuf>) -> GroupOutcome {
        let ctx = GroupContext {
            data_dir: env.data_dir.clone(),
            paths,
            failed_dir: env.failed_dir.clone(),
            to_archive_dir: env.to_archive_dir.clone(),
        };
        process_group_phase1(&ctx, &GroupProgress::default(), &env.handles)
    }

    fn file_count(env: &TestEnv, source: &str) -> i64 {
        let conn = db::open(&env.data_dir.join("sources").join(format!("{source}.db"))).unwrap();
        conn.query_row("SELECT COUNT(*) FROM files", [], |r| r.get(0)).unwrap()
    }

    fn gz_count(dir: &Path) -> usize {
        std::fs::read_dir(dir).unwrap().flatten()
            .filter(|e| e.path().extension().and_then(|x| x.to_str()) == Some("gz"))
            .count()
    }

    #[test]
    fn group_of_single_file_requests_persists_all_and_drains_inbox() {
        let env = setup();
        let paths: Vec<PathBuf> = (0..10)
            .map(|i| write_gz(&env, &format!("req{i:03}.gz"),
                &single_file_request("src1", &format!("f{i}.txt"), 1000 + i)))
            .collect();
        let out = run_group(&env, paths);
        assert_eq!(out.processed, 10);
        assert_eq!(out.failed, 0);
        assert_eq!(file_count(&env, "src1"), 10);
        assert_eq!(gz_count(&env.inbox_dir), 0, "all inbox gz deleted after flush");
        assert_eq!(gz_count(&env.to_archive_dir), 10, "one to-archive gz per request");
    }

    #[test]
    fn group_crossing_batch_size_persists_every_request() {
        let env = setup();
        let n = PHASE1_BATCH_SIZE + 5;
        let paths: Vec<PathBuf> = (0..n)
            .map(|i| write_gz(&env, &format!("req{i:03}.gz"),
                &single_file_request("src1", &format!("f{i}.txt"), 1000 + i as i64)))
            .collect();
        let out = run_group(&env, paths);
        assert_eq!(out.processed, n);
        assert_eq!(file_count(&env, "src1"), n as i64);
        assert_eq!(gz_count(&env.inbox_dir), 0);
    }

    #[test]
    fn mixed_source_group_switches_sessions() {
        let env = setup();
        let paths = vec![
            write_gz(&env, "req000.gz", &single_file_request("alpha", "a1.txt", 1000)),
            write_gz(&env, "req001.gz", &single_file_request("alpha", "a2.txt", 1001)),
            write_gz(&env, "req002.gz", &single_file_request("beta",  "b1.txt", 1002)),
            write_gz(&env, "req003.gz", &single_file_request("alpha", "a3.txt", 1003)),
        ];
        let out = run_group(&env, paths);
        assert_eq!(out.processed, 4);
        assert_eq!(file_count(&env, "alpha"), 3);
        assert_eq!(file_count(&env, "beta"), 1);
        assert_eq!(gz_count(&env.inbox_dir), 0);
    }

    #[test]
    fn corrupt_gz_moves_to_failed_and_group_continues() {
        let env = setup();
        let p0 = write_gz(&env, "req000.gz", &single_file_request("src1", "before.txt", 1000));
        // Corrupt file: valid gzip, invalid JSON.
        let p1 = env.inbox_dir.join("req001.gz");
        {
            let file = std::fs::File::create(&p1).unwrap();
            let mut enc = GzEncoder::new(file, flate2::Compression::default());
            enc.write_all(b"not json").unwrap();
            enc.finish().unwrap();
        }
        let p2 = write_gz(&env, "req002.gz", &single_file_request("src1", "after.txt", 1002));
        let out = run_group(&env, vec![p0, p1, p2]);
        assert_eq!(out.processed, 2);
        assert_eq!(out.failed, 1);
        assert_eq!(file_count(&env, "src1"), 2);
        assert_eq!(gz_count(&env.failed_dir), 1, "corrupt gz moved to failed/");
        assert_eq!(gz_count(&env.inbox_dir), 0);
    }

    #[test]
    fn cross_request_order_upsert_then_delete_leaves_file_absent() {
        let env = setup();
        let p0 = write_gz(&env, "req000.gz", &single_file_request("src1", "victim.txt", 1000));
        let del = BulkRequest {
            source: "src1".to_string(),
            files: vec![],
            delete_paths: vec!["victim.txt".to_string()],
            rename_paths: vec![],
            scan_timestamp: Some(1001),
            indexing_failures: vec![],
        };
        let p1 = write_gz(&env, "req001.gz", &del);
        let out = run_group(&env, vec![p0, p1]);
        assert_eq!(out.processed, 2);
        let conn = db::open(&env.data_dir.join("sources/src1.db")).unwrap();
        let n: i64 = conn.query_row(
            "SELECT COUNT(*) FROM files WHERE path='victim.txt'", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 0, "delete in later request must win over upsert in earlier one");
    }

    #[test]
    fn request_with_deletes_and_renames_works_inside_group_transaction() {
        let env = setup();
        // Seed two files, then one request that deletes one and renames the other.
        let p0 = write_gz(&env, "req000.gz", &single_file_request("src1", "old.txt", 1000));
        let p1 = write_gz(&env, "req001.gz", &single_file_request("src1", "gone.txt", 1001));
        let mixed = BulkRequest {
            source: "src1".to_string(),
            files: vec![make_file("new_file.txt", 1002)],
            delete_paths: vec!["gone.txt".to_string()],
            rename_paths: vec![find_common::api::PathRename {
                old_path: "old.txt".to_string(),
                new_path: "renamed.txt".to_string(),
            }],
            scan_timestamp: Some(1002),
            indexing_failures: vec![],
        };
        let p2 = write_gz(&env, "req002.gz", &mixed);
        let out = run_group(&env, vec![p0, p1, p2]);
        assert_eq!(out.processed, 3);
        let conn = db::open(&env.data_dir.join("sources/src1.db")).unwrap();
        let paths: Vec<String> = {
            let mut stmt = conn.prepare("SELECT path FROM files ORDER BY path").unwrap();
            stmt.query_map([], |r| r.get(0)).unwrap().collect::<rusqlite::Result<_>>().unwrap()
        };
        assert_eq!(paths, vec!["new_file.txt".to_string(), "renamed.txt".to_string()]);
    }
}
```

- [x] **Step 4.2: Run tests to verify they fail**

Run: `cargo test -p find-server --lib worker::group`
Expected: FAIL to compile (`process_group_phase1` not defined) — that's the red state.

- [x] **Step 4.3: Rewrite `request.rs`'s per-request body as `process_one_request`**

Replace `process_request_phase1` with this function (the body is today's
`request.rs:222-537` with the connection/BEGIN/COMMIT/cadence lines removed and the
to-archive write buffered instead of written):

```rust
/// Process one decoded request against an open `SourceSession`.
///
/// SQLite writes land in the session's open transaction; commit cadence is
/// driven through `session.add_units_and_maybe_commit`. The normalized
/// to-archive payload is *buffered* (gzipped bytes) and only written to disk
/// by the session at the flush point after the covering COMMIT — phase 2's
/// stale-hash check reads `files.file_hash` from the source DB, so the
/// payload must never be visible before the hash it matches is committed.
///
/// On success the request is registered via `session.request_done` (inbox gz
/// deletion also deferred to the flush point).
pub(super) fn process_one_request(
    session: &mut super::group::SourceSession,
    mut request: BulkRequest,
    request_path: &Path,
    to_archive_dir: &Path,
    compressed_bytes: usize,
    h: &super::group::Phase1Handles,
) -> Result<()> {
    let request_start = std::time::Instant::now();
    let req_stem = request_path.file_stem().and_then(|s| s.to_str()).unwrap_or("?");

    let n_files = request.files.len();
    let n_deletes = request.delete_paths.len();
    let n_renames = request.rename_paths.len();
    let total_content_lines: usize = request.files.iter().map(|f| f.lines.len()).sum();
    let total_content_bytes: usize = request.files.iter()
        .flat_map(|f| f.lines.iter())
        .map(|l| l.content.len())
        .sum();

    let tag     = format!("[indexer:{}:{req_stem}]", request.source);
    let src_tag = format!("[indexer:{}]",            request.source);

    let mut delta = crate::stats_cache::SourceStatsDelta {
        source: request.source.clone(),
        ..Default::default()
    };

    if let Ok(mut guard) = h.status.lock() {
        *guard = find_common::api::WorkerStatus::Processing {
            source: request.source.clone(),
            file: format!("(0/{n_files})"),
        };
    }
    h.stats_watch.send_modify(|v| *v = v.wrapping_add(1));

    tracing::debug!("{tag} start: {} files, {} deletes, {} renames", n_files, n_deletes, n_renames);

    let conn = &mut session.conn;

    // Deletes first (invariant: deletes before upserts within a request).
    if !request.delete_paths.is_empty() {
        if let Ok(mut guard) = h.status.lock() {
            *guard = find_common::api::WorkerStatus::Processing {
                source: request.source.clone(),
                file: format!("(deleting {} files)", n_deletes),
            };
        }
        let delete_delta = timed!(tag, format!("delete {} paths", n_deletes), {
            db::delete_files_phase1(conn, &request.delete_paths)?
        });
        delta.files_delta -= delete_delta.files_removed;
        delta.size_delta  -= delete_delta.size_removed;
        for (kind, (count, size)) in delete_delta.by_kind {
            let e = delta.kind_deltas.entry(kind).or_insert((0, 0));
            e.0 -= count;
            e.1 -= size;
        }
    }

    if !request.rename_paths.is_empty() {
        timed!(tag, format!("rename {} paths", n_renames), {
            db::rename_files(conn, &request.rename_paths)?
        });
    }

    let mut server_side_failures: Vec<IndexingFailure> = Vec::new();
    let mut successfully_indexed: Vec<String> = Vec::new();
    let mut activity_added: Vec<String> = Vec::new();
    let mut activity_modified: Vec<String> = Vec::new();

    let mut files_owned = std::mem::take(&mut request.files);
    let mut normalized_files: Vec<find_common::api::IndexFile> = Vec::with_capacity(files_owned.len());

    tracing::debug!("{tag} → normalize {} files", n_files);
    let norm_start = std::time::Instant::now();
    {
        let mut to_normalize: Vec<(usize, String, Vec<find_common::api::IndexLine>)> =
            files_owned.iter_mut()
                .enumerate()
                .filter(|(_, f)| f.kind.is_text_like())
                .map(|(i, f)| (i, f.path.clone(), std::mem::take(&mut f.lines)))
                .collect();
        normalize::normalize_batch_indexed(&mut to_normalize, &h.cfg.normalization);
        for (i, _, lines) in to_normalize {
            files_owned[i].lines = lines;
        }
    }
    tracing::debug!("{tag} ← normalize {} files ({:.1}ms)", n_files, norm_start.elapsed().as_secs_f64() * 1000.0);

    tracing::debug!("{tag} → index {} files", n_files);
    let index_loop_start = std::time::Instant::now();
    for file in files_owned {
        if let Ok(mut guard) = h.status.lock() {
            *guard = find_common::api::WorkerStatus::Processing {
                source: request.source.clone(),
                file: file.path.clone(),
            };
        }
        let file_start = std::time::Instant::now();

        match pipeline::process_file_phase1(&mut session.conn, &file, Some(h.content_store.as_ref())) {
            Ok(outcome) => {
                successfully_indexed.push(file.path.clone());
                if file.mtime != 0 && !is_composite(&file.path) {
                    match &outcome {
                        pipeline::Phase1Outcome::New      => activity_added.push(file.path.clone()),
                        pipeline::Phase1Outcome::Modified { .. } => activity_modified.push(file.path.clone()),
                        pipeline::Phase1Outcome::Skipped  => {}
                    }
                }
                if !is_composite(&file.path) {
                    match &outcome {
                        pipeline::Phase1Outcome::New => {
                            delta.files_delta += 1;
                            let size = file.size.unwrap_or(0);
                            delta.size_delta += size;
                            let e = delta.kind_deltas.entry(file.kind.clone()).or_insert((0, 0));
                            e.0 += 1;
                            e.1 += size;
                        }
                        pipeline::Phase1Outcome::Modified { old_size, old_kind } => {
                            let new_size = file.size.unwrap_or(0);
                            delta.size_delta += new_size - old_size;
                            let old_e = delta.kind_deltas.entry(old_kind.clone()).or_insert((0, 0));
                            old_e.0 -= 1;
                            old_e.1 -= old_size;
                            let new_e = delta.kind_deltas.entry(file.kind.clone()).or_insert((0, 0));
                            new_e.0 += 1;
                            new_e.1 += new_size;
                        }
                        pipeline::Phase1Outcome::Skipped => {}
                    }
                }
            }
            Err(e) => {
                if is_db_locked(&e) {
                    return Err(e); // group loop rolls back and leaves gz for retry
                }
                tracing::error!("Failed to index {}: {e:#}", file.path);
                let (fallback, skip_inner) = if pipeline::is_outer_archive(&file.path, &file.kind) {
                    (pipeline::outer_archive_stub(&file), true)
                } else {
                    (pipeline::filename_only_file(&file), false)
                };
                if let Err(e2) = pipeline::process_file_phase1_fallback(&mut session.conn, &fallback, skip_inner, Some(h.content_store.as_ref())) {
                    if is_db_locked(&e2) {
                        return Err(e2);
                    }
                    tracing::error!("Filename-only fallback also failed for {}: {e2:#}", file.path);
                }
                server_side_failures.push(IndexingFailure {
                    path: file.path.clone(),
                    error: format!("{e:#}"),
                });
            }
        }
        warn_slow(file_start, 30, "process_file_phase1", &file.path);
        normalized_files.push(file);

        session.add_units_and_maybe_commit(1, h)?;
    }
    tracing::debug!("{tag} ← index {} files ({:.1}ms)", n_files, index_loop_start.elapsed().as_secs_f64() * 1000.0);

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;

    let all_failures: Vec<find_common::api::IndexingFailure> = request
        .indexing_failures
        .iter()
        .chain(server_side_failures.iter())
        .cloned()
        .collect();
    timed!(tag, "cleanup writes", {
        db::do_cleanup_writes(
            &session.conn,
            &successfully_indexed,
            &all_failures,
            now,
            request.scan_timestamp,
        )?
    });

    // Activity log + SSE events (SSE fires slightly ahead of the covering
    // COMMIT; events are transient UI hints, and a crash before the commit
    // just means the request is replayed and the events re-sent).
    {
        let deleted: Vec<String> = request.delete_paths.iter()
            .filter(|p| !is_composite(p))
            .cloned()
            .collect();
        let renamed: Vec<(String, String)> = request.rename_paths.iter()
            .filter(|r| !is_composite(&r.old_path) && !is_composite(&r.new_path))
            .map(|r| (r.old_path.clone(), r.new_path.clone()))
            .collect();
        if let Err(e) = db::log_activity(&session.conn, now, &activity_added, &activity_modified, &deleted, &renamed, h.cfg.activity_log_max_entries) {
            tracing::warn!("Failed to write activity log: {e:#}");
        } else {
            let source = &request.source;
            for path in &activity_added {
                let _ = h.recent_tx.send(RecentFile { source: source.clone(), path: path.clone(), indexed_at: now, action: RecentAction::Added,    new_path: None });
            }
            for path in &activity_modified {
                let _ = h.recent_tx.send(RecentFile { source: source.clone(), path: path.clone(), indexed_at: now, action: RecentAction::Modified, new_path: None });
            }
            for path in &deleted {
                let _ = h.recent_tx.send(RecentFile { source: source.clone(), path: path.clone(), indexed_at: now, action: RecentAction::Deleted,  new_path: None });
            }
            for (old, new) in &renamed {
                let _ = h.recent_tx.send(RecentFile { source: source.clone(), path: old.clone(),  indexed_at: now, action: RecentAction::Renamed,  new_path: Some(new.clone()) });
            }
        }
    }

    // Count non-file write batches toward the commit cadence so delete-only
    // bursts also coalesce but still commit regularly.
    let mut extra_units = 0usize;
    if !request.delete_paths.is_empty() { extra_units += 1; }
    if !request.rename_paths.is_empty() { extra_units += 1; }
    if extra_units > 0 {
        session.add_units_and_maybe_commit(extra_units, h)?;
    }

    let elapsed = request_start.elapsed();
    let elapsed_secs = elapsed.as_secs_f64();
    let content_kb = total_content_bytes / 1024;
    let compressed_kb = compressed_bytes / 1024;
    tracing::info!("{src_tag} indexed {} files, {} deletes, {} renames, {} lines, {} KB content, {} KB compressed, {:.1}s",
        n_files, n_deletes, n_renames, total_content_lines, content_kb, compressed_kb, elapsed_secs);
    if elapsed.as_secs() >= 120 {
        tracing::warn!(
            elapsed_secs = elapsed.as_secs(),
            files = n_files,
            deletes = n_deletes,
            renames = n_renames,
            content_lines = total_content_lines,
            content_kb,
            compressed_kb,
            "{tag} slow batch: {:.1}s — {} files, {} deletes, {} renames, {} lines, {} KB content, {} KB compressed",
            elapsed_secs, n_files, n_deletes, n_renames, total_content_lines,
            content_kb, compressed_kb,
        );
    }

    // Buffer the normalized to-archive payload; the session writes it after
    // the covering COMMIT. Empty requests skip the archive phase entirely.
    let archive_payload = if normalized_files.is_empty() && request.rename_paths.is_empty() {
        tracing::debug!("{tag} skipping archive phase (no chunks to write)");
        None
    } else {
        let payload = timed!(tag, "encode normalized gz", {
            let normalized_request = BulkRequest {
                source: request.source.clone(),
                files: normalized_files,
                delete_paths: request.delete_paths.clone(),
                scan_timestamp: request.scan_timestamp,
                indexing_failures: request.indexing_failures.clone(),
                rename_paths: request.rename_paths.clone(),
            };
            let file_name = request_path.file_name()
                .context("request path has no filename")?;
            let mut encoder = GzEncoder::new(Vec::new(), flate2::Compression::default());
            serde_json::to_writer(&mut encoder, &normalized_request)
                .context("serializing normalized request")?;
            (to_archive_dir.join(file_name), encoder.finish().context("finalizing normalized gz")?)
        });
        Some(payload)
    };

    session.request_done(request_path.to_path_buf(), delta, archive_payload);
    Ok(())
}
```

Behavior deltas vs today, all intentional:
- per-file `db locked, will retry` no longer falls through to the fallback path —
  a lock error aborts the whole request (and group) for retry, which is what the
  request-level handler did anyway once the error propagated;
- the to-archive gz is encoded to memory here and written by the session post-COMMIT;
- `stats delta` / inbox-gz deletion are deferred to the flush point via `request_done`;
- normalized gz encodes into a `Vec<u8>` (bounded by the group byte cap) instead of
  streaming to a file.

Delete the now-unused `process_request_phase1` and its `interrupt_tx` plumbing.
Keep `is_db_locked` and `handle_failure` (make `is_db_locked` `pub(super)` if not already).

- [x] **Step 4.4: Add `process_group_phase1` to `group.rs`**

```rust
/// Blocking group processor. All per-file/per-request error handling and
/// filesystem outcomes (delete on flush, move to failed/) happen here; the
/// async wrapper only handles timeout and panic.
pub(super) fn process_group_phase1(
    ctx: &GroupContext,
    progress: &GroupProgress,
    h: &Phase1Handles,
) -> GroupOutcome {
    let mut session: Option<SourceSession> = None;
    let mut out = GroupOutcome::default();

    for (i, gz_path) in ctx.paths.iter().enumerate() {
        if let Ok(mut cur) = progress.current.lock() {
            *cur = Some(gz_path.clone());
        }

        let (request, compressed_bytes) = match request::decode_request(gz_path) {
            Ok(r) => r,
            Err(e) => {
                tracing::error!("Failed to decode {}: {e:#}", gz_path.display());
                move_to_failed(gz_path, &ctx.failed_dir);
                out.failed += 1;
                continue;
            }
        };

        // Source switch: flush the previous source before opening the next.
        if session.as_ref().is_some_and(|s| s.source != request.source) {
            let prev = session.take().unwrap();
            let prev_source = prev.source.clone();
            if let Err(e) = prev.finalize(h) {
                // `remaining` includes the current (not yet processed) request.
                return finish_group(out, ctx.paths.len() - i, progress,
                    &format!("finalizing source {prev_source}"), &e);
            }
        }
        if session.is_none() {
            match SourceSession::open(&ctx.data_dir, &request.source, progress) {
                Ok(s) => session = Some(s),
                Err(e) if is_db_locked(&e) => {
                    return finish_group(out, ctx.paths.len() - i, progress,
                        &format!("opening source {}", request.source), &e);
                }
                Err(e) => {
                    tracing::error!("Failed to open source {}: {e:#}", request.source);
                    move_to_failed(gz_path, &ctx.failed_dir);
                    out.failed += 1;
                    continue;
                }
            }
        }
        let s = session.as_mut().unwrap();

        match request::process_one_request(s, request, gz_path, &ctx.to_archive_dir, compressed_bytes, h) {
            Ok(()) => out.processed += 1,
            Err(e) if is_db_locked(&e) => {
                // Contention: roll back everything unflushed (those gz files stay
                // in inbox and are rediscovered by the router) and end the group.
                session.take().unwrap().abort();
                return finish_group(out, ctx.paths.len() - i, progress,
                    &format!("processing {}", gz_path.display()), &e);
            }
            Err(e) => {
                // Request-level failure: roll back the open transaction (earlier
                // unflushed requests stay in inbox and are replayed — idempotent),
                // quarantine only the failing gz, continue with a fresh session.
                tracing::error!("Failed to process {}: {e:#}", gz_path.display());
                session.take().unwrap().abort();
                move_to_failed(gz_path, &ctx.failed_dir);
                out.failed += 1;
            }
        }
    }

    if let Some(s) = session.take() {
        if let Err(e) = s.finalize(h) {
            tracing::error!("Failed to finalize group (will retry unflushed requests): {e:#}");
            out.retry += 1;
        }
    }
    if let Ok(mut cur) = progress.current.lock() {
        *cur = None;
    }
    out
}

/// Shared exit path for lock-contention / finalize failures: everything not
/// yet flushed stays in inbox for the router to retry next tick.
fn finish_group(
    mut out: GroupOutcome,
    remaining: usize,
    progress: &GroupProgress,
    what: &str,
    e: &anyhow::Error,
) -> GroupOutcome {
    tracing::warn!("Group ended early while {what} (will retry remaining {remaining}): {e:#}");
    out.retry += remaining;
    if let Ok(mut cur) = progress.current.lock() {
        *cur = None;
    }
    out
}
```

Make `decode_request` `pub(super)` in `request.rs`.

- [x] **Step 4.5: Rewire `request.rs`'s existing tests through a group of one**

Replace the `call_phase1` helper so all existing assertions keep working:

```rust
/// Test shim: run one inbox gz through the group path (group of one).
fn call_phase1(
    data_dir: &std::path::Path,
    request_path: &std::path::Path,
    to_archive_dir: &std::path::Path,
    status: &StatusHandle,
    cfg: WorkerConfig,
    recent_tx: &tokio::sync::broadcast::Sender<RecentFile>,
    stats_watch: &Arc<tokio::sync::watch::Sender<u64>>,
) -> Result<()> {
    let failed_dir = data_dir.join("inbox/failed");
    std::fs::create_dir_all(&failed_dir).unwrap();
    let h = super::group::Phase1Handles {
        status: status.clone(),
        cfg,
        recent_tx: recent_tx.clone(),
        stats_watch: Arc::clone(stats_watch),
        content_store: make_content_store(data_dir),
        source_stats_cache: Arc::new(std::sync::RwLock::new(Default::default())),
        archive_notify: Arc::new(tokio::sync::Notify::new()),
    };
    let ctx = super::group::GroupContext {
        data_dir: data_dir.to_path_buf(),
        paths: vec![request_path.to_path_buf()],
        failed_dir,
        to_archive_dir: to_archive_dir.to_path_buf(),
    };
    let out = super::group::process_group_phase1(&ctx, &super::group::GroupProgress::default(), &h);
    anyhow::ensure!(out.failed == 0 && out.retry == 0, "group reported failures: {out:?}");
    Ok(())
}
```

Notes for adapting individual tests:
- tests that `unwrap()` the returned `SourceStatsDelta` no longer can — delete those
  bindings (delta assertions, if any, move to checking the stats cache is untouched
  or are simply dropped; the existing tests only unwrap and discard it);
- `PHASE1_BATCH_SIZE` now comes from `super::group::PHASE1_BATCH_SIZE`;
- the group path **deletes** the inbox gz on success — no existing assertion depends
  on it remaining, so tests pass as-is.

- [x] **Step 4.6: Run the full worker test suite**

Run: `cargo test -p find-server --lib worker`
Expected: PASS — all new group tests and all pre-existing request/pipeline tests.

- [x] **Step 4.7: Checkpoint** — `mise run clippy`, pause for user review.

---

## Task 5: Async wrapper, worker task, and router group dispatch

**Files:**
- Modify: `crates/server/src/worker/group.rs` (add `process_group_async`)
- Modify: `crates/server/src/worker/request.rs` (delete `process_request_async`, `RequestContext`, `IndexerHandles`)
- Modify: `crates/server/src/worker/mod.rs` (channel type, worker task, router loop, `form_group`)

- [x] **Step 5.1: Write the failing `form_group` unit tests in `worker/mod.rs`**

```rust
#[cfg(test)]
mod group_dispatch_tests {
    use super::*;

    fn p(name: &str) -> PathBuf { PathBuf::from(format!("/inbox/{name}.gz")) }

    #[test]
    fn form_group_takes_all_when_under_caps() {
        let candidates = vec![(100u64, p("a")), (100, p("b")), (100, p("c"))];
        assert_eq!(form_group(&candidates).len(), 3);
    }

    #[test]
    fn form_group_caps_request_count() {
        let candidates: Vec<(u64, PathBuf)> =
            (0..group::MAX_GROUP_REQUESTS + 10).map(|i| (10u64, p(&format!("f{i}")))).collect();
        assert_eq!(form_group(&candidates).len(), group::MAX_GROUP_REQUESTS);
    }

    #[test]
    fn form_group_caps_total_bytes() {
        let half = group::MAX_GROUP_GZ_BYTES / 2 + 1;
        let candidates = vec![(half, p("a")), (half, p("b")), (half, p("c"))];
        assert_eq!(form_group(&candidates).len(), 1, "second file would exceed the byte cap");
    }

    #[test]
    fn form_group_always_takes_at_least_one() {
        let candidates = vec![(group::MAX_GROUP_GZ_BYTES * 10, p("huge"))];
        assert_eq!(form_group(&candidates).len(), 1);
    }

    #[test]
    fn form_group_empty_input() {
        assert!(form_group(&[]).is_empty());
    }
}
```

Run: `cargo test -p find-server --lib group_dispatch` — FAIL to compile (red).

- [x] **Step 5.2: Add `process_group_async` to `group.rs`**

```rust
/// Async-side fields that stay out of the blocking task.
pub(super) struct IndexerHandles {
    pub phase1: Phase1Handles,
    pub inbox_paused: Arc<std::sync::atomic::AtomicBool>,
    pub consecutive_timeouts: Arc<std::sync::atomic::AtomicU32>,
}

/// Run a group through phase 1 in a blocking task with the configured timeout.
///
/// The blocking task owns all per-request outcomes; this wrapper handles only:
/// timeout (interrupt the current connection, quarantine the in-flight gz,
/// circuit breaker) and panic (quarantine the in-flight gz).
pub(super) async fn process_group_async(ctx: GroupContext, handles: &IndexerHandles) {
    let progress = Arc::new(GroupProgress::default());
    let request_timeout = handles.phase1.cfg.request_timeout;

    let blocking_task = tokio::task::spawn_blocking({
        let ctx = ctx.clone();
        let progress = Arc::clone(&progress);
        let h = handles.phase1.clone();
        move || process_group_phase1(&ctx, &progress, &h)
    });

    let timed_result = tokio::time::timeout(request_timeout, blocking_task).await;

    if let Ok(mut guard) = handles.phase1.status.lock() {
        *guard = find_common::api::WorkerStatus::Idle;
    }

    match timed_result {
        Err(_timeout) => {
            // Unblock the current connection so the detached thread rolls back
            // and releases its RESERVED lock (see pre-group comments on the
            // same mechanism in git history of request.rs).
            if let Ok(slot) = progress.interrupt.lock() {
                if let Some(handle) = slot.as_ref() {
                    handle.interrupt();
                }
            }
            let current = progress.current.lock().ok().and_then(|c| c.clone());
            tracing::error!(
                "Group processing timed out after {}s (group of {}), abandoning{}",
                request_timeout.as_secs(),
                ctx.paths.len(),
                current.as_deref().map(Path::display)
                    .map(|d| format!(", quarantining in-flight request: {d}"))
                    .unwrap_or_default(),
            );
            // Poison protection: quarantine the in-flight request; already-
            // flushed files are gone from inbox; the rest retry next tick.
            if let Some(cur) = current {
                move_to_failed(&cur, &ctx.failed_dir);
            }

            let limit = handles.phase1.cfg.consecutive_timeout_limit;
            if limit > 0 {
                let count = handles.consecutive_timeouts.fetch_add(1, Ordering::Relaxed) + 1;
                if count >= limit {
                    handles.inbox_paused.store(true, Ordering::Relaxed);
                    tracing::error!(
                        "Inbox worker auto-paused after {count} consecutive processing timeouts. \
                         Manual intervention required. \
                         Use `find-admin inbox resume` or POST /api/v1/admin/inbox/resume to restart."
                    );
                    crate::alerts::send_inbox_paused_alert(
                        &handles.phase1.cfg.alerts,
                        count,
                        request_timeout.as_secs(),
                    );
                }
            }
        }
        Ok(Ok(outcome)) => {
            // Lock-contention retries end the group early but are not timeouts;
            // reset the breaker as today's non-timeout paths do.
            handles.consecutive_timeouts.store(0, Ordering::Relaxed);
            tracing::debug!(
                "Group done: {} processed, {} failed, {} left for retry",
                outcome.processed, outcome.failed, outcome.retry
            );
        }
        Ok(Err(e)) => {
            handles.consecutive_timeouts.store(0, Ordering::Relaxed);
            let current = progress.current.lock().ok().and_then(|c| c.clone());
            tracing::error!("Group task panicked: {e}");
            if let Some(cur) = current {
                move_to_failed(&cur, &ctx.failed_dir);
            }
        }
    }
}
```

Note the timeout-path compile detail: build the log message however clippy prefers —
the intent is "log timeout + group size + which file was in flight".

Delete `process_request_async`, `RequestContext`, and the old `IndexerHandles` from
`request.rs` (the new `IndexerHandles` above replaces it; `handle_failure` becomes
unused — delete it too, `move_to_failed` in group.rs is its sync replacement).

- [x] **Step 5.3: Rework `worker/mod.rs` — channel, worker task, router**

Channel and worker task (replacing lines ~145-187):

```rust
    // Channel from router → worker (capacity 1: at most one group buffered ahead).
    let (work_tx, mut work_rx) = tokio::sync::mpsc::channel::<Vec<PathBuf>>(1);
    // Channel from worker → router: signals that a path is no longer in-flight.
    let (done_tx, done_rx) = tokio::sync::mpsc::channel::<PathBuf>(64);

    // Spawn the single indexing worker task.
    {
        let data_dir = data_dir.clone();
        let failed_dir = failed_dir.clone();
        let to_archive_dir_clone = to_archive_dir.clone();
        let handles = group::IndexerHandles {
            phase1: group::Phase1Handles {
                status: status.clone(),
                cfg: cfg.clone(),
                recent_tx,
                stats_watch,
                content_store: Arc::clone(&content_store),
                source_stats_cache,
                archive_notify: Arc::clone(&archive_notify),
            },
            inbox_paused: Arc::clone(&inbox_paused),
            consecutive_timeouts: Arc::clone(&consecutive_timeouts),
        };

        tokio::spawn(async move {
            tracing::debug!("Indexing worker started");
            while let Some(paths) = work_rx.recv().await {
                let ctx = group::GroupContext {
                    data_dir: data_dir.clone(),
                    paths: paths.clone(),
                    failed_dir: failed_dir.clone(),
                    to_archive_dir: to_archive_dir_clone.clone(),
                };
                group::process_group_async(ctx, &handles).await;
                // Signal the router that these paths are done (whatever the outcome:
                // deleted, failed, or left in inbox for rediscovery).
                for path in paths {
                    let _ = done_tx.send(path).await;
                }
            }
            tracing::debug!("Indexing worker exited");
        });
    }
```

(The archive worker already receives `archive_notify` — flush points call
`notify_one` from the blocking thread, which is a sync-safe call. The worker task no
longer notifies; remove that responsibility comment if present.)

Router loop: extend the directory scan to capture file size, then dispatch groups:

```rust
        let mut gz_files: Vec<(std::time::SystemTime, u64, PathBuf)> = Vec::new();
        while let Ok(Some(entry)) = entries.next_entry().await {
            let path = entry.path();
            if path.extension() == Some(OsStr::new("gz")) {
                let meta = entry.metadata().await.ok();
                let mtime = meta.as_ref()
                    .and_then(|m| m.modified().ok())
                    .unwrap_or(std::time::UNIX_EPOCH);
                let size = meta.map(|m| m.len()).unwrap_or(0);
                gz_files.push((mtime, size, path));
            }
        }
        gz_files.sort_unstable_by_key(|(mtime, _, _)| *mtime);

        if inbox_paused.load(Ordering::Relaxed) {
            continue;
        }

        let candidates: Vec<(u64, PathBuf)> = gz_files
            .into_iter()
            .filter(|(_, _, p)| !in_flight.contains(p))
            .map(|(_, size, p)| (size, p))
            .collect();

        let mut remaining = candidates.as_slice();
        while !remaining.is_empty() {
            let group_paths = form_group(remaining);
            let n = group_paths.len();
            match work_tx.try_send(group_paths.clone()) {
                Ok(()) => {
                    for p in group_paths {
                        in_flight.insert(p);
                    }
                    remaining = &remaining[n..];
                }
                Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => {
                    break;
                }
                Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => {
                    tracing::error!("Worker channel closed unexpectedly; stopping router");
                    return Ok(());
                }
            }
        }
```

And the pure grouping function:

```rust
/// Take a bounded prefix of the pending inbox files (mtime order) as one
/// dispatch group. Caps: MAX_GROUP_REQUESTS files or MAX_GROUP_GZ_BYTES of
/// compressed input, whichever comes first — but always at least one file,
/// so an oversized request degrades to a group of one.
fn form_group(candidates: &[(u64, PathBuf)]) -> Vec<PathBuf> {
    let mut group = Vec::new();
    let mut bytes: u64 = 0;
    for (size, path) in candidates {
        if !group.is_empty()
            && (group.len() >= group::MAX_GROUP_REQUESTS || bytes + size > group::MAX_GROUP_GZ_BYTES)
        {
            break;
        }
        group.push(path.clone());
        bytes += size;
    }
    group
}
```

- [x] **Step 5.4: Run the whole server unit suite**

Run: `cargo test -p find-server --lib`
Expected: PASS (group dispatch tests, group tests, reworked request tests, db tests).

- [x] **Step 5.5: Run the full integration suite**

Run: `cargo test -p find-server`
Expected: PASS — in particular `inbox_resilience.rs` (failed-dir behavior, retry, pause)
and `stats_cache.rs` (deltas now applied at flush points; `wait_for_idle` still holds
because inbox files are deleted at flush, before the group ends). If a stats test
observes intermediate counts mid-batch, re-read it against the new flush timing before
touching it — the end-state numbers must be identical.

- [x] **Step 5.6: Checkpoint** — `mise run clippy`, pause for user review.

---

## Task 6: Integration test — upload burst

**Files:**
- Create: `crates/server/tests/burst_coalescing.rs`

- [x] **Step 6.1: Write the test**

```rust
//! Issue #59: bursts of single-file BulkRequests must be fully indexed and
//! searchable, with the inbox drained, under group-coalesced processing.
mod helpers;
use helpers::{make_text_bulk, TestServer};

use find_common::api::SearchResponse;

#[tokio::test]
async fn burst_of_single_file_requests_all_indexed_and_searchable() {
    let srv = TestServer::spawn().await;

    // Fire 40 single-file requests back-to-back (more than one group's worth)
    // without waiting in between — the watcher-burst pattern.
    for i in 0..40 {
        let req = make_text_bulk(
            "burst",
            &format!("dir/file{i:02}.txt"),
            &format!("needle{i:02} burst coalescing content"),
        );
        srv.post_bulk(&req).await;
    }

    srv.wait_for_idle().await;

    // Every file must be searchable.
    for i in 0..40 {
        let resp: SearchResponse = srv
            .client
            .get(srv.url(&format!("/api/v1/search?q=needle{i:02}&source=burst")))
            .send().await.unwrap()
            .json().await.unwrap();
        assert!(
            resp.results.iter().any(|r| r.path == format!("dir/file{i:02}.txt")),
            "file{i:02} not found after burst"
        );
    }

    // Inbox must be drained (no gz left behind, none quarantined).
    let inbox = srv.data_dir_path().join("inbox");
    let count = |d: &std::path::Path| std::fs::read_dir(d).map(|rd| rd
        .flatten()
        .filter(|e| e.path().extension().and_then(|x| x.to_str()) == Some("gz"))
        .count()).unwrap_or(0);
    assert_eq!(count(&inbox), 0, "inbox not drained");
    assert_eq!(count(&inbox.join("failed")), 0, "requests were quarantined");
}

#[tokio::test]
async fn burst_with_interleaved_delete_applies_in_order() {
    let srv = TestServer::spawn().await;

    srv.post_bulk(&make_text_bulk("burst", "victim.txt", "shortlived content")).await;
    let del = find_common::api::BulkRequest {
        source: "burst".to_string(),
        files: vec![],
        delete_paths: vec!["victim.txt".to_string()],
        rename_paths: vec![],
        scan_timestamp: Some(2),
        indexing_failures: vec![],
    };
    srv.post_bulk(&del).await;
    srv.wait_for_idle().await;

    let resp: SearchResponse = srv
        .client
        .get(srv.url("/api/v1/search?q=shortlived&source=burst"))
        .send().await.unwrap()
        .json().await.unwrap();
    assert!(
        !resp.results.iter().any(|r| r.path == "victim.txt"),
        "delete arriving after upsert must win"
    );
}
```

- [x] **Step 6.2: Run it**

Run: `cargo test -p find-server --test burst_coalescing`
Expected: PASS

- [x] **Step 6.3: Checkpoint** — pause for user review; good point for the user to run a
manual smoke test (`mise run dev`, index something, watch the logs for grouped
`indexed N files` lines).

---

## Task 7: Documentation

**Files:**
- Modify: `CLAUDE.md` (Write path section)
- Modify: `docs/ARCHITECTURE.md` (worker design)
- Modify: `CHANGELOG.md` (`[Unreleased]`)

- [x] **Step 7.1: Update CLAUDE.md's "Write path (indexing)" key invariants**

Amend the invariants list to reflect grouping:
- "The worker processes inbox files sequentially (one at a time)" → "The worker
  processes inbox files sequentially in bounded *groups* (≤32 files / ≤8 MiB compressed
  per group); consecutive same-source requests share one connection and commit every 25
  write units, so single-file upload bursts don't pay one commit per request. There is
  still never concurrent write access to a source database."
- Add the flush-point invariant: "An inbox `.gz` is deleted only after the COMMIT
  covering all of its writes and after its normalized to-archive `.gz` is on disk;
  crash recovery = reprocess whatever is left in `inbox/` (idempotent)."
- Update the key-files table: add `crates/server/src/worker/group.rs`.

- [x] **Step 7.2: Update `docs/ARCHITECTURE.md`'s worker section with the same changes**
(mirror the wording; include the flush ordering list from this plan's Design Decisions).

- [x] **Step 7.3: CHANGELOG**

Under `[Unreleased]`:

```markdown
### Changed
- Inbox worker now coalesces consecutive same-source requests into shared SQLite
  transactions (bounded groups, commit every 25 write units), eliminating
  one-commit-per-request write amplification during single-file upload bursts (#59).
```

- [x] **Step 7.4: Final verification**

Run: `mise run clippy && cargo test -p find-server && (cd web && pnpm run check)`
Expected: all green (web check is untouched but cheap insurance).

- [ ] **Step 7.5: Checkpoint** — hand to user for commit, version bump decision, and
closing issue #59.

---

## Testing (summary)

- **Unit**: tx-nesting guards (Task 1); `SourceSession`/group behavior — persistence,
  cadence crossing, source switching, corrupt-gz quarantine, cross-request ordering,
  deletes/renames inside the group transaction (Task 4); `form_group` caps (Task 5).
- **Integration**: 40-request single-file burst fully indexed/searchable with drained
  inbox; upsert-then-delete ordering across requests (Task 6).
- **Regression**: entire existing `cargo test -p find-server` suite, especially
  `inbox_resilience.rs` and `stats_cache.rs`.
- **Manual** (user): run against a copy of the production data dir, replay a watcher
  burst, compare `archived 1 files` / commit-frequency log lines and disk-write volume
  (e.g. `/proc/<pid>/io` write_bytes) before/after.

## Breaking Changes

None. No HTTP API change (`MIN_CLIENT_VERSION` unchanged), no schema change, no config
change. Internal behavior changes: inbox `.gz` deletion moves from the async wrapper to
post-COMMIT flush points; the request timeout now bounds a bounded *group* rather than a
single request; SSE activity events can fire up to one commit-cadence ahead of
durability.
