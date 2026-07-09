/// Group-coalesced phase 1 (issue #59, plan 093).
///
/// The router dispatches a bounded group of inbox `.gz` files. This module
/// processes the group under per-source `SourceSession`s: one SQLite
/// connection with an open transaction, committed every `PHASE1_BATCH_SIZE`
/// write units *across* request boundaries — so a burst of single-file
/// requests no longer pays one full commit per request.
///
/// Crash-safety invariant: an inbox `.gz` is deleted only at a flush point —
/// after the COMMIT covering all of its writes and after its normalized
/// to-archive `.gz` is on disk. A crash mid-group leaves unflushed files in
/// `inbox/`; reprocessing them is idempotent.
use anyhow::Result;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

use find_common::api::RecentFile;
use find_content_store::ContentStore;

use crate::db;
use crate::stats_cache::{SourceStatsCache, SourceStatsDelta};

use super::{StatusHandle, WorkerConfig};
use super::request::{self, is_db_locked};

/// How many write units (files, plus one per delete/rename batch) to
/// accumulate before committing the source-DB transaction. Each commit
/// rewrites WAL frames and interior btree pages of the source DB; on a
/// multi-GB DB, committing after every request (as opposed to a batch of
/// them) is the dominant write cost when many small requests arrive in a
/// burst. The cadence is shared across all requests in a group.
pub(super) const PHASE1_BATCH_SIZE: usize = 25;

/// Router-side group caps. A group always contains at least one file, so a
/// single oversized request degrades to today's one-at-a-time behavior.
pub(super) const MAX_GROUP_REQUESTS: usize = 32;
pub(super) const MAX_GROUP_GZ_BYTES: u64 = 8 * 1024 * 1024;

/// Handles needed by the blocking phase-1 code. Split out of `IndexerHandles`
/// so the whole bundle can be cloned into `spawn_blocking` (project
/// convention: config struct over threaded parameters).
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
/// inbox file was in flight (poison-quarantine target).
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

/// One source-DB connection with an open transaction and the side effects
/// accumulated since the last flush point.
pub(super) struct SourceSession {
    pub source: String,
    pub conn: rusqlite::Connection,
    since_commit: usize,
    /// Inbox gz files fully processed but not yet covered by a COMMIT.
    deletable_gz: Vec<PathBuf>,
    /// Normalized to-archive gz bytes, written at flush (after COMMIT) —
    /// phase 2's stale-hash check reads `files.file_hash`, so a payload must
    /// never be visible on disk before the hash it matches is committed.
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

    /// COMMIT and release side effects; consumes the session (group end /
    /// source switch).
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
    ///
    /// The inbox gz (the recovery unit) is deleted only after its to-archive
    /// payload exists on disk; a crash between any two steps just means the
    /// unflushed requests replay from inbox on restart.
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

/// Handles for the indexing worker task: the blocking-side bundle plus the
/// async-only circuit-breaker state.
pub(super) struct IndexerHandles {
    pub phase1: Phase1Handles,
    /// Shared flag used to pause inbox processing.  Set to `true` by the
    /// circuit breaker when consecutive timeouts reach the configured limit.
    pub inbox_paused: Arc<AtomicBool>,
    /// Counts consecutive timeouts for the circuit-breaker check.
    pub consecutive_timeouts: Arc<AtomicU32>,
}

// ── Async entry point ─────────────────────────────────────────────────────────

/// Run a group through phase 1 in a blocking task with the configured timeout.
///
/// The blocking task owns all per-request outcomes (flush deletions, moves to
/// failed/); this wrapper handles only timeout and panic.
///
/// When the timeout fires, we call `sqlite3_interrupt` on the connection
/// currently open inside the blocking task (via the shared `GroupProgress`
/// slot, refreshed on every session open).  This causes any in-progress
/// SQLite call on that connection to return `SQLITE_INTERRUPT`, which
/// unblocks the thread, rolls back its transaction, and drops the connection —
/// releasing whatever write lock it was holding.  Without the interrupt, a
/// detached blocking task holding a SQLite RESERVED lock would cause every
/// subsequent write to the same source DB to block for the full
/// `busy_timeout` before failing, cascading into every subsequent request
/// also timing out.
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
            // Interrupt the stuck SQLite connection so the detached blocking
            // thread unblocks, rolls back its transaction, and drops the
            // connection — releasing any RESERVED lock on the source DB.
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
                current.as_ref()
                    .map(|c| format!(", quarantining in-flight request: {}", c.display()))
                    .unwrap_or_default(),
            );
            // Poison protection: quarantine the in-flight request; already-
            // flushed files are gone from inbox; the rest retry next tick.
            if let Some(cur) = current {
                move_to_failed(&cur, &ctx.failed_dir);
            }

            // Circuit breaker: if consecutive timeouts reach the configured
            // limit, auto-pause the inbox and send an alert email.
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
            // Group completed (individual requests may have failed or been
            // left for retry, but the worker is not stuck) — reset the
            // consecutive timeout counter.
            handles.consecutive_timeouts.store(0, Ordering::Relaxed);
            tracing::debug!(
                "Group done: {} processed, {} failed, {} left for retry",
                outcome.processed, outcome.failed, outcome.retry
            );
        }
        Ok(Err(e)) => {
            // Blocking task panic or join error — quarantine the in-flight
            // request (poison protection) and reset the timeout counter.
            handles.consecutive_timeouts.store(0, Ordering::Relaxed);
            let current = progress.current.lock().ok().and_then(|c| c.clone());
            tracing::error!("Group task error: {e}");
            if let Some(cur) = current {
                move_to_failed(&cur, &ctx.failed_dir);
            }
        }
    }
}

// ── Group loop ────────────────────────────────────────────────────────────────

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
        let s = session.as_mut().expect("session opened above");

        match request::process_one_request(s, request, gz_path, &ctx.to_archive_dir, compressed_bytes, h) {
            Ok(()) => out.processed += 1,
            Err(e) if is_db_locked(&e) => {
                // Contention: roll back everything unflushed (those gz files
                // stay in inbox and are rediscovered by the router) and end
                // the group. A lock error is transient, not a poison request.
                session.take().expect("session is Some here").abort();
                return finish_group(out, ctx.paths.len() - i, progress,
                    &format!("processing {}", gz_path.display()), &e);
            }
            Err(e) => {
                // Request-level failure: roll back the open transaction
                // (earlier unflushed requests stay in inbox and are replayed —
                // idempotent), quarantine only the failing gz, and continue
                // with a fresh session for the next request.
                tracing::error!("Failed to process {}: {e:#}", gz_path.display());
                session.take().expect("session is Some here").abort();
                move_to_failed(gz_path, &ctx.failed_dir);
                out.failed += 1;
            }
        }
    }

    if let Some(s) = session.take() {
        if let Err(e) = s.finalize(h) {
            tracing::error!("Failed to finalize group (unflushed requests stay in inbox for retry): {e:#}");
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

/// Quarantine an inbox request that cannot be processed.
pub(super) fn move_to_failed(path: &Path, failed_dir: &Path) {
    let Some(name) = path.file_name() else { return };
    let dst = failed_dir.join(name);
    if let Err(e) = std::fs::rename(path, &dst) {
        tracing::error!("Failed to move {} to failed dir: {e}", path.display());
    } else {
        tracing::warn!("Moved failed request to: {}", dst.display());
    }
}

// ── Tests ──────────────────────────────────────────────────────────────────────

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
        // Seed two files, then one request that deletes one, renames the
        // other, and upserts a third — all inside the group transaction.
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

    #[test]
    fn empty_group_is_a_noop() {
        let env = setup();
        let out = run_group(&env, vec![]);
        assert_eq!(out.processed, 0);
        assert_eq!(out.failed, 0);
        assert_eq!(out.retry, 0);
    }
}
