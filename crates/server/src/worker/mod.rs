mod archive_batch;
mod pipeline;

use anyhow::{Context, Result};
use flate2::read::GzDecoder;
use rusqlite::ErrorCode;
use std::ffi::OsStr;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use find_common::api::{BulkRequest, IndexingFailure, WorkerStatus};

use crate::archive::SharedArchiveState;
use crate::db;


const POLL_INTERVAL: std::time::Duration = std::time::Duration::from_secs(1);

/// Configuration values for the inbox worker — plain scalars read from the
/// server config at startup. Bundled into a struct so function signatures stay
/// stable when new settings are added.
#[derive(Clone, Copy)]
pub struct WorkerConfig {
    pub log_batch_detail_limit: usize,
    pub request_timeout: std::time::Duration,
    pub inline_threshold_bytes: u64,
    pub archive_batch_size: usize,
    pub activity_log_max_entries: usize,
}

/// Log a warning if `start` is older than `threshold_secs`.
pub(super) fn warn_slow(start: std::time::Instant, threshold_secs: u64, step: &str, context: &str) {
    let elapsed = start.elapsed();
    if elapsed.as_secs() >= threshold_secs {
        tracing::warn!(
            elapsed_secs = elapsed.as_secs(),
            step,
            context,
            "Slow step: {step} took {:.1}s for {context}",
            elapsed.as_secs_f64(),
        );
    }
}

type StatusHandle = std::sync::Arc<std::sync::Mutex<WorkerStatus>>;

/// Ensure inbox subdirectories exist on startup.
///
/// Files left in `inbox/` from a previous run are simply re-processed on the
/// next scan — no explicit recovery is needed because files are never moved out
/// of `inbox/` until processing completes.  Files in `inbox/to-archive/` are
/// left alone; the archive thread picks them up automatically.
pub async fn recover_stranded_requests(data_dir: &Path) -> Result<()> {
    let inbox_dir = data_dir.join("inbox");
    tokio::fs::create_dir_all(&inbox_dir).await?;
    tokio::fs::create_dir_all(inbox_dir.join("failed")).await?;
    tokio::fs::create_dir_all(inbox_dir.join("to-archive")).await?;

    // One-time migration: if a `processing/` directory exists from an older
    // server version, move any stranded files back to `inbox/`.
    let processing_dir = inbox_dir.join("processing");
    if processing_dir.exists() {
        let mut stranded = tokio::fs::read_dir(&processing_dir).await?;
        while let Ok(Some(entry)) = stranded.next_entry().await {
            let src = entry.path();
            if src.extension() == Some(OsStr::new("gz")) {
                let dst = inbox_dir.join(entry.file_name());
                if let Err(e) = tokio::fs::rename(&src, &dst).await {
                    tracing::warn!("Failed to recover stranded request {}: {e}", src.display());
                } else {
                    tracing::info!("Recovered stranded request: {}", dst.display());
                }
            }
        }
    }
    Ok(())
}

/// Start the two-phase inbox worker.
///
/// Phase 1 (indexing loop): a single worker processes inbox requests
/// sequentially, writing to SQLite only (no ZIP I/O). On success, moves
/// the .gz to `inbox/to-archive/` and signals the archive thread.
///
/// Phase 2 (archive loop): a single archive thread batches up to
/// `archive_batch_size` requests from `to-archive/`, coalesces work, rewrites
/// ZIPs, and updates line refs in SQLite.
pub async fn start_inbox_worker(
    data_dir: PathBuf,
    cfg: WorkerConfig,
    status: StatusHandle,
    shared_archive_state: Arc<SharedArchiveState>,
    inbox_paused: Arc<AtomicBool>,
    deleted_bytes_since_scan: Arc<AtomicU64>,
    delete_notify: Arc<tokio::sync::Notify>,
) -> Result<()> {
    let inbox_dir = data_dir.join("inbox");
    let failed_dir = inbox_dir.join("failed");
    let to_archive_dir = inbox_dir.join("to-archive");

    tokio::fs::create_dir_all(&to_archive_dir).await?;

    tracing::info!(
        "Starting two-phase inbox worker: {}",
        inbox_dir.display()
    );

    let archive_notify = Arc::new(tokio::sync::Notify::new());

    // Channel from router → worker (capacity 1: at most one file buffered ahead).
    let (work_tx, mut work_rx) = tokio::sync::mpsc::channel::<PathBuf>(1);
    // Channel from worker → router: signals that a path is no longer in-flight.
    let (done_tx, done_rx) = tokio::sync::mpsc::channel::<PathBuf>(64);

    // Spawn the single indexing worker task.
    {
        let data_dir = data_dir.clone();
        let failed_dir = failed_dir.clone();
        let to_archive_dir_clone = to_archive_dir.clone();
        let status = status.clone();
        let archive_notify = Arc::clone(&archive_notify);
        let shared = Arc::clone(&shared_archive_state);

        tokio::spawn(async move {
            tracing::debug!("Indexing worker started");
            while let Some(path) = work_rx.recv().await {
                process_request_async(
                    &data_dir,
                    &path,
                    &failed_dir,
                    &to_archive_dir_clone,
                    status.clone(),
                    cfg,
                    &archive_notify,
                    Arc::clone(&shared),
                )
                .await;
                // Signal the router that this path is done (success or failure).
                // The router removes it from in_flight so it can dispatch the next.
                let _ = done_tx.send(path).await;
            }
            tracing::debug!("Indexing worker exited");
        });
    }

    // Spawn the archive loop (blocking, spawn_blocking wrapper).
    {
        let data_dir = data_dir.clone();
        let to_archive_dir = to_archive_dir.clone();
        let shared = Arc::clone(&shared_archive_state);
        let deleted_bytes = Arc::clone(&deleted_bytes_since_scan);
        let notify = Arc::clone(&delete_notify);
        let archive_notify = Arc::clone(&archive_notify);

        tokio::spawn(async move {
            tracing::debug!("Archive worker started");
            loop {
                // Wait for signal OR 60 s timeout.
                let _ = tokio::time::timeout(
                    std::time::Duration::from_secs(60),
                    archive_notify.notified(),
                )
                .await;

                // Sleep 5 s to allow accumulation before processing.
                tokio::time::sleep(std::time::Duration::from_secs(5)).await;

                // Drain queue in batches (blocking).
                loop {
                    let to_archive = to_archive_dir.clone();
                    let data = data_dir.clone();
                    let sh = Arc::clone(&shared);
                    let db = Arc::clone(&deleted_bytes);
                    let dn = Arc::clone(&notify);
                    let batch_result = tokio::task::spawn_blocking(move || {
                        archive_batch::run_archive_batch(&data, &to_archive, cfg, &sh, &db, &dn)
                    })
                    .await;

                    match batch_result {
                        Ok(Ok(processed)) => {
                            if processed < cfg.archive_batch_size {
                                break; // queue drained
                            }
                            // else loop immediately for next batch
                        }
                        Ok(Err(e)) => {
                            tracing::error!("Archive batch failed: {e:#}");
                            break;
                        }
                        Err(e) => {
                            tracing::error!("Archive batch task error: {e}");
                            break;
                        }
                    }
                }
            }
        });
    }

    // Router loop: poll inbox, dispatch files not already in-flight to the worker.
    // Files stay in inbox/ until the worker finishes; `in_flight` prevents
    // re-dispatching the same file on the next scan tick.
    //
    // Wake-up sources:
    //  • 1-second interval tick (catches newly arriving inbox files)
    //  • done_rx signal (worker finished — re-scan immediately so the next
    //    file is dispatched without waiting up to 1 s for the next tick)
    let mut interval = tokio::time::interval(POLL_INTERVAL);
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut in_flight: std::collections::HashSet<PathBuf> = std::collections::HashSet::new();
    let mut done_rx = done_rx;

    loop {
        tokio::select! {
            _ = interval.tick() => {}
            Some(done_path) = done_rx.recv() => {
                in_flight.remove(&done_path);
                // drain any further completions that arrived simultaneously
                while let Ok(p) = done_rx.try_recv() { in_flight.remove(&p); }
            }
        }

        // Drain any remaining completion signals (interval-tick path).
        while let Ok(done_path) = done_rx.try_recv() {
            in_flight.remove(&done_path);
        }

        let mut entries = match tokio::fs::read_dir(&inbox_dir).await {
            Ok(e) => e,
            Err(e) => {
                tracing::error!("Failed to read inbox dir: {e}");
                continue;
            }
        };

        let mut gz_files: Vec<(std::time::SystemTime, PathBuf)> = Vec::new();
        while let Ok(Some(entry)) = entries.next_entry().await {
            let path = entry.path();
            if path.extension() == Some(OsStr::new("gz")) {
                let mtime = entry.metadata().await
                    .ok()
                    .and_then(|m| m.modified().ok())
                    .unwrap_or(std::time::UNIX_EPOCH);
                gz_files.push((mtime, path));
            }
        }
        // Sort ascending by mtime so older submissions are processed first.
        gz_files.sort_unstable_by_key(|(mtime, _)| *mtime);

        // When paused, do not dispatch any new work — leave files in inbox/.
        if inbox_paused.load(Ordering::Relaxed) {
            continue;
        }

        for (_, inbox_path) in gz_files {
            if in_flight.contains(&inbox_path) {
                continue; // already dispatched, worker has it
            }
            match work_tx.try_send(inbox_path.clone()) {
                Ok(()) => {
                    in_flight.insert(inbox_path);
                }
                Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => {
                    break; // worker busy; try again next tick
                }
                Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => {
                    tracing::error!("Worker channel closed unexpectedly; stopping router");
                    return Ok(());
                }
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn process_request_async(
    data_dir: &Path,
    request_path: &Path,
    failed_dir: &Path,
    to_archive_dir: &Path,
    status: StatusHandle,
    cfg: WorkerConfig,
    archive_notify: &Arc<tokio::sync::Notify>,
    shared_archive_state: Arc<SharedArchiveState>,
) {
    let status_reset = status.clone();

    let blocking_task = tokio::task::spawn_blocking({
        let data_dir = data_dir.to_path_buf();
        let request_path = request_path.to_path_buf();
        move || process_request_phase1(&data_dir, &request_path, &status, cfg, &shared_archive_state)
    });

    let timed_result = tokio::time::timeout(cfg.request_timeout, blocking_task).await;

    if let Ok(mut guard) = status_reset.lock() {
        *guard = WorkerStatus::Idle;
    }

    match timed_result {
        Err(_timeout) => {
            tracing::error!(
                "Request processing timed out after {}s, abandoning: {}",
                cfg.request_timeout.as_secs(),
                request_path.display(),
            );
            handle_failure(
                request_path,
                failed_dir,
                anyhow::anyhow!("Processing timed out after {}s", cfg.request_timeout.as_secs()),
            )
            .await;
        }
        Ok(Ok(Ok(()))) => {
            // Move to to-archive/ for the archive thread.
            if let Some(file_name) = request_path.file_name() {
                let to_archive_path = to_archive_dir.join(file_name);
                if let Err(e) = tokio::fs::rename(request_path, &to_archive_path).await {
                    tracing::error!(
                        "Failed to move processed request {} to to-archive/: {}",
                        request_path.display(),
                        e
                    );
                } else {
                    tracing::debug!("Phase 1 complete, queued for archive: {}", to_archive_path.display());
                    archive_notify.notify_one();
                }
            }
        }
        Ok(Ok(Err(e))) => {
            if is_db_locked(&e) {
                // File is still in inbox/ — the router will rediscover and
                // retry it on the next scan tick.
                tracing::warn!(
                    "Database locked while processing {}, will retry: {e:#}",
                    request_path.display(),
                );
            } else {
                handle_failure(request_path, failed_dir, e).await;
            }
        }
        Ok(Err(e)) => {
            handle_failure(
                request_path,
                failed_dir,
                anyhow::anyhow!("Task error: {}", e),
            )
            .await;
        }
    }
}

/// Phase 1: process a single inbox request — SQLite only, no ZIP I/O.
fn process_request_phase1(
    data_dir: &Path,
    request_path: &Path,
    status: &StatusHandle,
    cfg: WorkerConfig,
    shared_archive_state: &Arc<SharedArchiveState>,
) -> Result<()> {
    let request_start = std::time::Instant::now();

    let compressed = std::fs::read(request_path)?;
    let compressed_bytes = compressed.len();
    let mut decoder = GzDecoder::new(&compressed[..]);
    let mut json = String::new();
    decoder.read_to_string(&mut json)?;

    let request: BulkRequest = serde_json::from_str(&json)
        .context("parsing bulk request JSON")?;

    let n_files = request.files.len();
    let n_deletes = request.delete_paths.len();
    let n_renames = request.rename_paths.len();
    let total_content_lines: usize = request.files.iter().map(|f| f.lines.len()).sum();
    let total_content_bytes: usize = request.files.iter()
        .flat_map(|f| f.lines.iter())
        .map(|l| l.content.len())
        .sum();

    // req_tag is the filename stem (without .gz), used as the log prefix.
    let req_tag = request_path.file_stem().and_then(|s| s.to_str()).unwrap_or("?");
    let tag = format!("[indexer:{}:{}]", request.source, req_tag);

    tracing::debug!("{tag} start: {} files, {} deletes, {} renames", n_files, n_deletes, n_renames);

    if n_files <= cfg.log_batch_detail_limit {
        for f in &request.files {
            tracing::info!("{tag} indexing {}", f.path);
        }
    } else if n_files > 0 {
        tracing::info!("{tag} indexing {} files", n_files);
    }

    let db_path = data_dir.join("sources").join(format!("{}.db", request.source));
    let mut conn = db::open(&db_path)?;

    // Acquire the per-source write lock before any SQLite writes.
    // The archive thread also holds this lock while writing to the same DB,
    // so this prevents concurrent write transactions even if SQLite WAL
    // advisory locking is unreliable on WSL/network mounts.
    let source_lock = shared_archive_state.source_lock(&request.source);
    let _source_guard = source_lock.lock()
        .map_err(|_| anyhow::anyhow!("source lock poisoned for {}", request.source))?;

    // Process deletes (SQLite only — queues chunk refs to pending_chunk_removes).
    if !request.delete_paths.is_empty() {
        if let Ok(mut guard) = status.lock() {
            *guard = WorkerStatus::Processing {
                source: request.source.clone(),
                file: format!("(deleting {} files)", n_deletes),
            };
        }
        db::delete_files_phase1(&conn, &request.delete_paths)?;
    }

    // Process renames after deletes, before upserts.
    if !request.rename_paths.is_empty() {
        db::rename_files(&conn, &request.rename_paths)?;
    }

    let mut server_side_failures: Vec<IndexingFailure> = Vec::new();
    let mut successfully_indexed: Vec<String> = Vec::new();
    let mut activity_added: Vec<String> = Vec::new();
    let mut activity_modified: Vec<String> = Vec::new();
    for file in &request.files {
        if let Ok(mut guard) = status.lock() {
            *guard = WorkerStatus::Processing {
                source: request.source.clone(),
                file: file.path.clone(),
            };
        }
        let file_start = std::time::Instant::now();
        match pipeline::process_file_phase1(&mut conn, file, cfg.inline_threshold_bytes) {
            Ok(()) => {
                successfully_indexed.push(file.path.clone());
                // Track adds vs modifies for the activity log.
                // Skip mtime=0 archive sentinels and composite archive-member paths.
                if file.mtime != 0 && !file.path.contains("::") {
                    if file.is_new {
                        activity_added.push(file.path.clone());
                    } else {
                        activity_modified.push(file.path.clone());
                    }
                }
            }
            Err(e) => {
                if is_db_locked(&e) {
                    tracing::warn!("Failed to index {} (db locked, will retry): {e:#}", file.path);
                } else {
                    tracing::error!("Failed to index {}: {e:#}", file.path);
                }
                let (fallback, skip_inner) = if pipeline::is_outer_archive(&file.path, &file.kind) {
                    (pipeline::outer_archive_stub(file), true)
                } else {
                    (pipeline::filename_only_file(file), false)
                };
                if let Err(e2) = pipeline::process_file_phase1_fallback(&mut conn, &fallback, skip_inner, cfg.inline_threshold_bytes) {
                    if is_db_locked(&e2) {
                        tracing::warn!("Filename-only fallback also failed for {} (db locked, will retry): {e2:#}", file.path);
                    } else {
                        tracing::error!("Filename-only fallback also failed for {}: {e2:#}", file.path);
                    }
                }
                server_side_failures.push(IndexingFailure {
                    path: file.path.clone(),
                    error: format!("{e:#}"),
                });
            }
        }
        warn_slow(file_start, 30, "process_file_phase1", &file.path);
    }

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
    db::do_cleanup_writes(
        &conn,
        &successfully_indexed,
        &all_failures,
        now,
        request.scan_timestamp,
    )?;

    // Log activity: adds/modifies were accumulated during the loop above;
    // deletes and renames come directly from the request.
    {
        let deleted: Vec<String> = request.delete_paths.iter()
            .filter(|p| !p.contains("::"))
            .cloned()
            .collect();
        let renamed: Vec<(String, String)> = request.rename_paths.iter()
            .filter(|r| !r.old_path.contains("::") && !r.new_path.contains("::"))
            .map(|r| (r.old_path.clone(), r.new_path.clone()))
            .collect();
        if let Err(e) = db::log_activity(&conn, now, &activity_added, &activity_modified, &deleted, &renamed, cfg.activity_log_max_entries) {
            tracing::warn!("Failed to write activity log: {e:#}");
        }
    }

    let elapsed = request_start.elapsed();
    let elapsed_secs = elapsed.as_secs_f64();
    let content_kb = total_content_bytes / 1024;
    let compressed_kb = compressed_bytes / 1024;
    tracing::info!(
        "{tag} indexed {} files, {} deletes, {} renames, {} lines, \
         {} KB content, {} KB compressed, {:.1}s",
        n_files, n_deletes, n_renames, total_content_lines,
        content_kb, compressed_kb, elapsed_secs,
    );
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

    Ok(())
}

fn is_db_locked(error: &anyhow::Error) -> bool {
    for cause in error.chain() {
        if let Some(rusqlite::Error::SqliteFailure(e, _)) = cause.downcast_ref::<rusqlite::Error>() {
            if matches!(e.code, ErrorCode::DatabaseBusy | ErrorCode::DatabaseLocked) {
                return true;
            }
        }
    }
    false
}

async fn handle_failure(path: &Path, failed_dir: &Path, error: anyhow::Error) {
    tracing::error!("Failed to process {}: {}", path.display(), error);

    let failed_path = failed_dir.join(path.file_name().unwrap());
    if let Err(e) = tokio::fs::rename(path, &failed_path).await {
        tracing::error!(
            "Failed to move {} to failed directory: {}",
            path.display(),
            e
        );
    } else {
        tracing::warn!("Moved failed request to: {}", failed_path.display());
    }
}

#[cfg(test)]
mod tests {
    use super::pipeline::{filename_only_file, is_outer_archive, outer_archive_stub};
    use find_common::api::{IndexFile, IndexLine};

    fn make_file(path: &str, kind: &str) -> IndexFile {
        IndexFile {
            path: path.to_string(),
            mtime: 1000,
            size: Some(100),
            kind: kind.to_string(),
            lines: vec![IndexLine {
                archive_path: None,
                line_number: 0,
                content: path.to_string(),
            }],
            extract_ms: None,
            content_hash: None,
            scanner_version: 0,
            is_new: false,
        }
    }

    #[test]
    fn outer_archive_detected() {
        assert!(is_outer_archive("data.zip", "archive"));
    }

    #[test]
    fn archive_member_not_outer() {
        assert!(!is_outer_archive("data.zip::inner.txt", "archive"));
    }

    #[test]
    fn non_archive_kind_not_outer() {
        assert!(!is_outer_archive("data.zip", "text"));
    }

    #[test]
    fn filename_only_converts_archive_kind_to_unknown() {
        let f = make_file("data.zip", "archive");
        let fallback = filename_only_file(&f);
        assert_eq!(fallback.kind, "unknown");
    }

    #[test]
    fn filename_only_keeps_non_archive_kind() {
        let f = make_file("notes.md", "text");
        let fallback = filename_only_file(&f);
        assert_eq!(fallback.kind, "text");
    }

    #[test]
    fn filename_only_has_single_path_line() {
        let f = make_file("docs/report.pdf", "pdf");
        let fallback = filename_only_file(&f);
        assert_eq!(fallback.lines.len(), 1);
        assert_eq!(fallback.lines[0].line_number, 0);
        assert_eq!(fallback.lines[0].content, "docs/report.pdf");
    }

    #[test]
    fn filename_only_preserves_mtime_and_size() {
        let f = make_file("file.txt", "text");
        let fallback = filename_only_file(&f);
        assert_eq!(fallback.mtime, f.mtime);
        assert_eq!(fallback.size, f.size);
    }

    #[test]
    fn outer_archive_stub_preserves_archive_kind() {
        let f = make_file("backup.7z", "archive");
        let stub = outer_archive_stub(&f);
        assert_eq!(stub.kind, "archive");
    }

    #[test]
    fn outer_archive_stub_uses_zero_mtime() {
        let f = make_file("backup.7z", "archive");
        let stub = outer_archive_stub(&f);
        assert_eq!(stub.mtime, 0);
    }

    #[test]
    fn outer_archive_stub_has_single_path_line() {
        let f = make_file("backups/big.tar.gz", "archive");
        let stub = outer_archive_stub(&f);
        assert_eq!(stub.lines.len(), 1);
        assert_eq!(stub.lines[0].line_number, 0);
        assert_eq!(stub.lines[0].content, "backups/big.tar.gz");
    }
}
