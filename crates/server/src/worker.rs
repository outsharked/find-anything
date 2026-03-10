use anyhow::{Context, Result};
use flate2::read::GzDecoder;
use rusqlite::{Connection, ErrorCode, OptionalExtension};
use std::collections::HashMap;
use std::ffi::OsStr;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use find_common::api::{BulkRequest, IndexFile, IndexLine, IndexingFailure, WorkerStatus};

use crate::archive::{self, ArchiveManager, ChunkRef, SharedArchiveState};
use crate::db;

const POLL_INTERVAL: std::time::Duration = std::time::Duration::from_secs(1);

/// Log a warning if `start` is older than `threshold_secs`.
fn warn_slow(start: std::time::Instant, threshold_secs: u64, step: &str, context: &str) {
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
#[allow(clippy::too_many_arguments)]
pub async fn start_inbox_worker(
    data_dir: PathBuf,
    status: StatusHandle,
    log_batch_detail_limit: usize,
    request_timeout: std::time::Duration,
    inline_threshold_bytes: u64,
    archive_batch_size: usize,
    activity_log_max_entries: usize,
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
                    log_batch_detail_limit,
                    request_timeout,
                    inline_threshold_bytes,
                    activity_log_max_entries,
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
                        run_archive_batch(&data, &to_archive, archive_batch_size, &sh, &db, &dn)
                    })
                    .await;

                    match batch_result {
                        Ok(Ok(processed)) => {
                            if processed < archive_batch_size {
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
    log_batch_detail_limit: usize,
    request_timeout: std::time::Duration,
    inline_threshold_bytes: u64,
    activity_log_max_entries: usize,
    archive_notify: &Arc<tokio::sync::Notify>,
    shared_archive_state: Arc<SharedArchiveState>,
) {
    let status_reset = status.clone();

    let blocking_task = tokio::task::spawn_blocking({
        let data_dir = data_dir.to_path_buf();
        let request_path = request_path.to_path_buf();
        move || process_request_phase1(&data_dir, &request_path, &status, log_batch_detail_limit, inline_threshold_bytes, activity_log_max_entries, &shared_archive_state)
    });

    let timed_result = tokio::time::timeout(request_timeout, blocking_task).await;

    if let Ok(mut guard) = status_reset.lock() {
        *guard = WorkerStatus::Idle;
    }

    match timed_result {
        Err(_timeout) => {
            tracing::error!(
                "Request processing timed out after {}s, abandoning: {}",
                request_timeout.as_secs(),
                request_path.display(),
            );
            handle_failure(
                request_path,
                failed_dir,
                anyhow::anyhow!("Processing timed out after {}s", request_timeout.as_secs()),
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
                    tracing::info!("Phase 1 complete, queued for archive: {}", to_archive_path.display());
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
    log_batch_detail_limit: usize,
    inline_threshold_bytes: u64,
    activity_log_max_entries: usize,
    shared_archive_state: &Arc<SharedArchiveState>,
) -> Result<()> {
    let request_start = std::time::Instant::now();

    let inbox_file = request_path.file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("?");

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

    tracing::info!(
        "[phase1] start {inbox_file} [{}]: {} files, {} deletes, {} renames",
        request.source, n_files, n_deletes, n_renames,
    );

    if n_deletes > 0 {
        tracing::info!("[phase1] Processing {} deletes [{}]", n_deletes, request.source);
    }
    if n_files <= log_batch_detail_limit {
        for f in &request.files {
            tracing::info!("[phase1] Indexing [{}] {}", request.source, f.path);
        }
    } else {
        tracing::info!("[phase1] Indexing {} files [{}]", n_files, request.source);
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
        tracing::info!("[phase1] Processed {} renames [{}]", n_renames, request.source);
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
        match process_file_phase1(&mut conn, file, inline_threshold_bytes) {
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
                let (fallback, skip_inner) = if is_outer_archive(&file.path, &file.kind) {
                    (outer_archive_stub(file), true)
                } else {
                    (filename_only_file(file), false)
                };
                if let Err(e2) = process_file_phase1_fallback(&mut conn, &fallback, skip_inner, inline_threshold_bytes) {
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
        if let Err(e) = db::log_activity(&conn, now, &activity_added, &activity_modified, &deleted, &renamed, activity_log_max_entries) {
            tracing::warn!("Failed to write activity log: {e:#}");
        }
    }

    let elapsed = request_start.elapsed();
    let elapsed_secs = elapsed.as_secs_f64();
    let content_kb = total_content_bytes / 1024;
    let compressed_kb = compressed_bytes / 1024;
    tracing::info!(
        "[phase1] done {inbox_file} [{}]: {} files, {} deletes, {} renames, {} lines, \
         {} KB content, {} KB compressed, {:.1}s",
        request.source, n_files, n_deletes, n_renames, total_content_lines,
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
            "slow batch {inbox_file} [{}]: {:.1}s — {} files, {} deletes, {} renames, {} lines, {} KB content, {} KB compressed",
            request.source, elapsed_secs, n_files, n_deletes, n_renames, total_content_lines,
            content_kb, compressed_kb,
        );
    }

    Ok(())
}

/// Phase 1 file processing: write to SQLite only, no ZIP I/O.
fn process_file_phase1(
    conn: &mut Connection,
    file: &IndexFile,
    inline_threshold_bytes: u64,
) -> Result<()> {
    process_file_phase1_fallback(conn, file, false, inline_threshold_bytes)
}

fn process_file_phase1_fallback(
    conn: &mut Connection,
    file: &IndexFile,
    skip_inner_delete: bool,
    inline_threshold_bytes: u64,
) -> Result<()> {
    let now_secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;

    // If re-indexing an outer archive, delete stale inner members first (SQL only).
    if !skip_inner_delete && is_outer_archive(&file.path, &file.kind) && file.mtime == 0 {
        let like_pat = format!("{}::%", file.path);
        // Collect chunk refs for inner members and queue them for removal.
        let file_ids: Vec<i64> = {
            let mut stmt = conn.prepare("SELECT id FROM files WHERE path LIKE ?1")?;
            let ids = stmt.query_map(rusqlite::params![like_pat], |row| row.get(0))?
                .collect::<rusqlite::Result<_>>()?;
            ids
        };
        let tx = conn.unchecked_transaction()?;
        for fid in file_ids {
            tx.execute(
                "INSERT INTO pending_chunk_removes (archive_name, chunk_name)
                 SELECT DISTINCT chunk_archive, chunk_name
                 FROM lines
                 WHERE file_id = ?1 AND chunk_archive IS NOT NULL",
                rusqlite::params![fid],
            )?;
        }
        tx.execute("DELETE FROM files WHERE path LIKE ?1", rusqlite::params![like_pat])?;
        tx.commit()?;
    }

    // Dedup check: if another canonical with the same content hash exists,
    // register this file as an alias and skip chunk/lines/FTS writes.
    if let Some(hash) = &file.content_hash {
        let canonical_id: Option<i64> = conn.query_row(
            "SELECT id FROM files
             WHERE content_hash = ?1
               AND canonical_file_id IS NULL
               AND path != ?2
             LIMIT 1",
            rusqlite::params![hash, file.path],
            |row| row.get(0),
        ).optional()?;

        if let Some(canonical_id) = canonical_id {
            conn.execute(
                "INSERT INTO files (path, mtime, size, kind, indexed_at, extract_ms, content_hash, canonical_file_id)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
                 ON CONFLICT(path) DO UPDATE SET
                   mtime            = excluded.mtime,
                   size             = excluded.size,
                   kind             = excluded.kind,
                   extract_ms       = excluded.extract_ms,
                   content_hash     = excluded.content_hash,
                   canonical_file_id = excluded.canonical_file_id",
                rusqlite::params![
                    file.path, file.mtime, file.size, file.kind,
                    now_secs,
                    file.extract_ms.map(|ms| ms as i64),
                    hash,
                    canonical_id,
                ],
            )?;
            return Ok(());
        }
    }

    // Stale-mtime guard: skip if the stored mtime is already newer.
    let stored_mtime: Option<i64> = conn.query_row(
        "SELECT mtime FROM files WHERE path = ?1",
        rusqlite::params![file.path],
        |row| row.get(0),
    ).optional()?;
    if let Some(stored) = stored_mtime {
        if file.mtime > 0 && file.mtime < stored {
            tracing::debug!(
                "skipping stale upsert for {} (incoming mtime={} < stored={})",
                file.path, file.mtime, stored
            );
            return Ok(());
        }
    }

    // Collect old chunk refs for this file and queue them for removal.
    let existing_id: Option<i64> = conn.query_row(
        "SELECT id FROM files WHERE path = ?1",
        rusqlite::params![file.path],
        |row| row.get(0),
    ).optional()?;

    let line_data: Vec<(usize, String)> = file.lines.iter()
        .map(|l| (l.line_number, l.content.clone()))
        .collect();

    // Decide: inline vs deferred archive storage.
    let total_content_bytes: usize = file.lines.iter().map(|l| l.content.len()).sum();
    let use_inline = inline_threshold_bytes > 0
        && total_content_bytes as u64 <= inline_threshold_bytes;

    // Single transaction for the whole file.
    let t_fts = std::time::Instant::now();
    let tx = conn.transaction()?;

    // Queue old chunk refs for removal (before we overwrite the file record).
    if let Some(fid) = existing_id {
        tx.execute(
            "INSERT INTO pending_chunk_removes (archive_name, chunk_name)
             SELECT DISTINCT chunk_archive, chunk_name
             FROM lines
             WHERE file_id = ?1 AND chunk_archive IS NOT NULL",
            rusqlite::params![fid],
        )?;
    }

    // Upsert the file record and get the stable file_id.
    let file_id: i64 = tx.query_row(
        "INSERT INTO files (path, mtime, size, kind, indexed_at, extract_ms, content_hash, canonical_file_id)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, NULL)
         ON CONFLICT(path) DO UPDATE SET
           mtime             = excluded.mtime,
           size              = excluded.size,
           kind              = excluded.kind,
           indexed_at        = excluded.indexed_at,
           extract_ms        = excluded.extract_ms,
           content_hash      = excluded.content_hash,
           canonical_file_id = NULL
         RETURNING id",
        rusqlite::params![
            file.path, file.mtime, file.size, file.kind,
            now_secs,
            file.extract_ms.map(|ms| ms as i64),
            file.content_hash.as_deref(),
        ],
        |row| row.get(0),
    )?;

    // Delete existing lines and inline content.
    tx.execute("DELETE FROM lines WHERE file_id = ?1", rusqlite::params![file_id])?;

    if use_inline {
        // Store content inline in file_content table.
        let mut sorted_lines = file.lines.iter().collect::<Vec<_>>();
        sorted_lines.sort_by_key(|l| l.line_number);
        let inline_text: String = sorted_lines.iter()
            .map(|l| l.content.as_str())
            .collect::<Vec<_>>()
            .join("\n");

        tx.execute(
            "INSERT INTO file_content (file_id, content) VALUES (?1, ?2)
             ON CONFLICT(file_id) DO UPDATE SET content = excluded.content",
            rusqlite::params![file_id, inline_text],
        )?;

        // Insert line rows with NULL chunk refs; offset = position in inline_text lines.
        for (offset, line) in sorted_lines.iter().enumerate() {
            let line_id = tx.query_row(
                "INSERT INTO lines (file_id, line_number, chunk_archive, chunk_name, line_offset_in_chunk)
                 VALUES (?1, ?2, NULL, NULL, ?3)
                 RETURNING id",
                rusqlite::params![
                    file_id,
                    line.line_number as i64,
                    offset as i64,
                ],
                |row| row.get::<_, i64>(0),
            )?;
            tx.execute(
                "INSERT INTO lines_fts(rowid, content) VALUES (?1, ?2)",
                rusqlite::params![line_id, line.content],
            )?;
        }
    } else {
        // Remove old inline content if this file was previously stored inline.
        tx.execute(
            "DELETE FROM file_content WHERE file_id = ?1",
            rusqlite::params![file_id],
        )?;

        // Deferred archive: insert lines with NULL chunk refs (archive thread will fill them in).
        // FTS is indexed immediately for search availability.
        let mut sorted_lines = file.lines.iter().collect::<Vec<_>>();
        sorted_lines.sort_by_key(|l| l.line_number);
        for line in &sorted_lines {
            let line_id = tx.query_row(
                "INSERT INTO lines (file_id, line_number, chunk_archive, chunk_name, line_offset_in_chunk)
                 VALUES (?1, ?2, NULL, NULL, 0)
                 RETURNING id",
                rusqlite::params![
                    file_id,
                    line.line_number as i64,
                ],
                |row| row.get::<_, i64>(0),
            )?;
            tx.execute(
                "INSERT INTO lines_fts(rowid, content) VALUES (?1, ?2)",
                rusqlite::params![line_id, line.content],
            )?;
        }
    }

    tx.commit()?;
    warn_slow(t_fts, 10, "fts_insert_phase1", &file.path);

    // We keep line_data in scope but don't use it for ZIP — archive thread reads the .gz file.
    let _ = line_data;

    Ok(())
}

// ── Archive thread ────────────────────────────────────────────────────────────

/// Run one batch of the archive thread. Returns the number of .gz files processed.
fn run_archive_batch(
    data_dir: &Path,
    to_archive_dir: &Path,
    archive_batch_size: usize,
    shared_archive_state: &Arc<SharedArchiveState>,
    deleted_bytes_since_scan: &Arc<AtomicU64>,
    delete_notify: &Arc<tokio::sync::Notify>,
) -> Result<usize> {
    // Scan to-archive/ for .gz files sorted by mtime.
    let mut gz_files: Vec<(std::time::SystemTime, PathBuf)> = Vec::new();
    for entry in std::fs::read_dir(to_archive_dir)?.flatten() {
        let path = entry.path();
        if path.extension() == Some(OsStr::new("gz")) {
            let mtime = entry.metadata()
                .ok()
                .and_then(|m| m.modified().ok())
                .unwrap_or(std::time::UNIX_EPOCH);
            gz_files.push((mtime, path));
        }
    }
    gz_files.sort_unstable_by_key(|(mtime, _)| *mtime);

    if gz_files.is_empty() {
        return Ok(0);
    }

    let batch: Vec<PathBuf> = gz_files.into_iter()
        .take(archive_batch_size)
        .map(|(_, p)| p)
        .collect();
    let processed = batch.len();

    process_archive_batch(data_dir, &batch, shared_archive_state, deleted_bytes_since_scan, delete_notify)?;

    Ok(processed)
}

/// Process a batch of .gz files through the archive phase.
///
/// Write serialisation: the archive thread holds the per-source
/// `source_lock` only during SQLite write transactions (take_pending_chunk_removes
/// and the UPDATE lines commit).  The lock is explicitly released before ZIP I/O
/// so the indexing thread is not blocked during expensive disk work.
fn process_archive_batch(
    data_dir: &Path,
    gz_paths: &[PathBuf],
    shared_archive_state: &Arc<SharedArchiveState>,
    deleted_bytes_since_scan: &Arc<AtomicU64>,
    delete_notify: &Arc<tokio::sync::Notify>,
) -> Result<()> {
    // Parse all .gz files.
    struct ParsedRequest {
        gz_path: PathBuf,
        request: BulkRequest,
    }

    let mut parsed_requests: Vec<ParsedRequest> = Vec::new();
    for gz_path in gz_paths {
        match parse_gz_request(gz_path) {
            Ok(request) => parsed_requests.push(ParsedRequest { gz_path: gz_path.clone(), request }),
            Err(e) => {
                tracing::error!("Archive batch: failed to parse {}: {e:#}", gz_path.display());
                // Skip this file but don't delete it — leave for manual inspection.
            }
        }
    }

    if parsed_requests.is_empty() {
        // Delete any files that failed to parse (to avoid stuck queue).
        for gz_path in gz_paths {
            let _ = std::fs::remove_file(gz_path);
        }
        return Ok(());
    }

    // Group by source.
    let mut by_source: HashMap<String, Vec<&ParsedRequest>> = HashMap::new();
    for pr in &parsed_requests {
        by_source.entry(pr.request.source.clone()).or_default().push(pr);
    }

    let mut total_bytes_freed: u64 = 0;

    for (source, requests) in &by_source {
        let db_path = data_dir.join("sources").join(format!("{source}.db"));
        let conn = match db::open(&db_path) {
            Ok(c) => c,
            Err(e) => {
                tracing::error!("Archive batch: failed to open DB for source {source}: {e:#}");
                continue;
            }
        };

        // --- SQLite write segment 1: drain pending_chunk_removes ---
        // Hold source_lock only for this write transaction; release before ZIP I/O.
        let source_lock = shared_archive_state.source_lock(source);
        let chunk_removes = {
            let _guard = source_lock.lock()
                .map_err(|_| anyhow::anyhow!("source lock poisoned for {source}"))?;
            match db::take_pending_chunk_removes(&conn) {
                Ok(r) => r,
                Err(e) => {
                    tracing::error!("Archive batch: failed to take pending_chunk_removes for {source}: {e:#}");
                    vec![]
                }
            }
        }; // _guard dropped here — lock released before ZIP I/O

        // Rewrite ZIPs for pending removes (group by archive_name).
        if !chunk_removes.is_empty() {
            use std::collections::{HashMap as HM, HashSet};
            let mut by_archive: HM<String, HashSet<String>> = HM::new();
            for (archive_name, chunk_name) in chunk_removes {
                by_archive.entry(archive_name).or_default().insert(chunk_name);
            }

            // Use a temporary ArchiveManager for rewrite operations.
            let archive_mgr = ArchiveManager::new(Arc::clone(shared_archive_state));
            let chunk_refs: Vec<ChunkRef> = by_archive.iter()
                .flat_map(|(archive_name, chunk_names)| {
                    chunk_names.iter().map(move |chunk_name| ChunkRef {
                        archive_name: archive_name.clone(),
                        chunk_name: chunk_name.clone(),
                    })
                })
                .collect();

            match archive_mgr.remove_chunks(chunk_refs) {
                Ok(freed) => total_bytes_freed += freed,
                Err(e) => tracing::error!("Archive batch: failed to remove chunks for {source}: {e:#}"),
            }
        }

        // Coalesce upserts: last-writer-wins by submission order (mtime sort).
        // Build a map: path → last IndexFile; also track delete_paths.
        let mut upsert_map: HashMap<String, IndexFile> = HashMap::new();
        let mut delete_set: std::collections::HashSet<String> = std::collections::HashSet::new();

        for pr in requests.iter() {
            for path in &pr.request.delete_paths {
                delete_set.insert(path.clone());
                upsert_map.remove(path);
            }
            for file in &pr.request.files {
                if !delete_set.contains(&file.path) {
                    upsert_map.insert(file.path.clone(), file.clone());
                }
            }
        }

        // Create ArchiveManager for appending new chunks.
        let mut archive_mgr = ArchiveManager::new(Arc::clone(shared_archive_state));

        // Collect (file_id, line_data, path) for files that need archiving.
        struct ArchiveWork {
            file_id: i64,
            path: String,
            line_data: Vec<(usize, String)>,
        }
        let mut archive_works: Vec<ArchiveWork> = Vec::new();

        for (path, file) in &upsert_map {
            // Look up file_id.
            let file_id: Option<i64> = conn.query_row(
                "SELECT id FROM files WHERE path = ?1",
                rusqlite::params![path],
                |row| row.get(0),
            ).optional().unwrap_or(None);

            let Some(file_id) = file_id else {
                continue; // file deleted
            };

            // Check if already archived.
            let already_archived: i64 = conn.query_row(
                "SELECT COUNT(*) FROM lines WHERE file_id = ?1 AND chunk_archive IS NOT NULL",
                rusqlite::params![file_id],
                |row| row.get(0),
            ).unwrap_or(0);
            if already_archived > 0 {
                continue; // processed by a newer batch
            }

            // Check if inline (permanently stored in file_content).
            let is_inline: i64 = conn.query_row(
                "SELECT COUNT(*) FROM file_content WHERE file_id = ?1",
                rusqlite::params![file_id],
                |row| row.get(0),
            ).unwrap_or(0);
            if is_inline > 0 {
                continue; // inline storage, no ZIP needed
            }

            // Check if there are any lines at all (might be empty file).
            let line_count: i64 = conn.query_row(
                "SELECT COUNT(*) FROM lines WHERE file_id = ?1",
                rusqlite::params![file_id],
                |row| row.get(0),
            ).unwrap_or(0);
            if line_count == 0 {
                continue;
            }

            let line_data: Vec<(usize, String)> = file.lines.iter()
                .map(|l| (l.line_number, l.content.clone()))
                .collect();

            archive_works.push(ArchiveWork { file_id, path: path.clone(), line_data });
        }

        // Append chunks and collect mappings.
        struct ArchivedFile {
            file_id: i64,
            line_mappings: Vec<archive::LineMapping>,
            chunk_refs: Vec<ChunkRef>,
        }
        let mut archived_files: Vec<ArchivedFile> = Vec::new();

        for work in archive_works {
            let chunk_result = archive::chunk_lines(work.file_id, &work.path, &work.line_data);
            match archive_mgr.append_chunks(chunk_result.chunks) {
                Ok(chunk_refs) => {
                    archived_files.push(ArchivedFile {
                        file_id: work.file_id,
                        line_mappings: chunk_result.line_mappings,
                        chunk_refs,
                    });
                }
                Err(e) => {
                    tracing::error!("Archive batch: failed to append chunks for {}: {e:#}", work.path);
                }
            }
        }

        // --- SQLite write segment 2: update line refs ---
        // Re-acquire source_lock for this write transaction.
        if !archived_files.is_empty() {
            let _guard = source_lock.lock()
                .map_err(|_| anyhow::anyhow!("source lock poisoned for {source}"))?;
            let tx = conn.unchecked_transaction()?;
            for af in &archived_files {
                // Build chunk_ref_map: chunk_number → ChunkRef.
                // We need the ChunkResult's chunk list to map chunk_number to chunk_ref.
                // The line_mappings carry chunk_number; the chunk_refs are in order.
                // chunk_refs[i] corresponds to chunk i (by chunk_number order after sort).
                // Rebuild: chunk_result.chunks had chunk_number 0,1,2,...; chunk_refs match by index.
                // We stored them in the same order, so line_mappings[j].chunk_number gives the index.
                let mut chunk_ref_by_number: HashMap<usize, &ChunkRef> = HashMap::new();
                // The chunk_refs are indexed by the order they were appended (which = chunk_number order).
                // chunk_result.chunks[i].chunk_number == i for all i (monotonically 0,1,2,...).
                for (i, cr) in af.chunk_refs.iter().enumerate() {
                    chunk_ref_by_number.insert(i, cr);
                }

                for mapping in &af.line_mappings {
                    if let Some(chunk_ref) = chunk_ref_by_number.get(&mapping.chunk_number) {
                        if let Err(e) = tx.execute(
                            "UPDATE lines SET chunk_archive = ?1, chunk_name = ?2, line_offset_in_chunk = ?3
                             WHERE file_id = ?4 AND line_number = ?5",
                            rusqlite::params![
                                chunk_ref.archive_name,
                                chunk_ref.chunk_name,
                                mapping.offset_in_chunk as i64,
                                af.file_id,
                                mapping.line_number as i64,
                            ],
                        ) {
                            tracing::error!(
                                "Archive batch: failed to update line ref for file_id={}: {e}",
                                af.file_id
                            );
                        }
                    }
                }
            }
            tx.commit()?;
        }

        tracing::info!(
            "[archive] processed {} files for source {source}",
            archived_files.len()
        );
    }

    // Delete all processed .gz files from to-archive/.
    for pr in &parsed_requests {
        if let Err(e) = std::fs::remove_file(&pr.gz_path) {
            tracing::error!("Archive batch: failed to delete {}: {e}", pr.gz_path.display());
        }
    }

    if total_bytes_freed > 0 {
        deleted_bytes_since_scan.fetch_add(total_bytes_freed, Ordering::Relaxed);
        delete_notify.notify_one();
    }

    Ok(())
}

fn parse_gz_request(gz_path: &Path) -> Result<BulkRequest> {
    let compressed = std::fs::read(gz_path)?;
    let mut decoder = GzDecoder::new(&compressed[..]);
    let mut json = String::new();
    decoder.read_to_string(&mut json)?;
    let request: BulkRequest = serde_json::from_str(&json)
        .context("parsing bulk request JSON")?;
    Ok(request)
}

/// Returns `true` if `file` is a top-level archive (kind="archive" with no
/// "::" in the path).
pub(crate) fn is_outer_archive(path: &str, kind: &str) -> bool {
    kind == "archive" && !path.contains("::")
}

fn filename_only_file(file: &IndexFile) -> IndexFile {
    IndexFile {
        path: file.path.clone(),
        mtime: file.mtime,
        size: file.size,
        kind: if file.kind == "archive" { "unknown".to_string() } else { file.kind.clone() },
        lines: vec![IndexLine {
            archive_path: None,
            line_number: 0,
            content: file.path.clone(),
        }],
        extract_ms: None,
        content_hash: None,
        scanner_version: file.scanner_version,
        is_new: file.is_new,
    }
}

fn outer_archive_stub(file: &IndexFile) -> IndexFile {
    IndexFile {
        path: file.path.clone(),
        mtime: 0,
        size: file.size,
        kind: "archive".to_string(),
        lines: vec![IndexLine {
            archive_path: None,
            line_number: 0,
            content: file.path.clone(),
        }],
        extract_ms: None,
        content_hash: None,
        scanner_version: file.scanner_version,
        is_new: file.is_new,
    }
}

/// Returns true if `error` (or any cause in its chain) is a SQLite
/// "database is locked" / "database is busy" error.  These are transient:
/// the file should stay in the inbox and be retried on the next poll cycle.
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
    use super::*;
    use find_common::api::IndexLine;

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
