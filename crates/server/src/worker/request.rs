/// Processing a single inbox request from start to finish.
///
/// A request goes through two phases:
///  - Phase 1 (this module): decode the `.gz`, update SQLite (deletes, renames,
///    upserts), write the activity log, and write a normalised `.gz` to
///    `inbox/to-archive/` for the archive phase.
///  - Phase 2 (archive_batch): read from `to-archive/`, coalesce chunks, rewrite
///    ZIP archives, and update chunk refs in SQLite.
use anyhow::{Context, Result};
use flate2::read::GzDecoder;
use flate2::write::GzEncoder;
use rusqlite::ErrorCode;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::broadcast;

use find_common::api::{BulkRequest, IndexingFailure, RecentAction, RecentFile};
use find_common::path::is_composite;

use crate::archive::SharedArchiveState;
use crate::db;
use crate::normalize;

use super::{StatusHandle, WorkerConfig, timed, warn_slow};
use super::pipeline;

// ── Context structs ─────────────────────────────────────────────────────────────

/// Per-request path context for `process_request_async`.
pub(super) struct RequestContext {
    pub data_dir:       PathBuf,
    pub request_path:   PathBuf,
    pub failed_dir:     PathBuf,
    pub to_archive_dir: PathBuf,
}

/// Worker-lifetime handles passed by reference to every `process_request_async` call.
pub(super) struct IndexerHandles {
    pub status:             StatusHandle,
    pub cfg:                WorkerConfig,
    pub archive_notify:     Arc<tokio::sync::Notify>,
    pub shared_archive:     Arc<SharedArchiveState>,
    pub recent_tx:          broadcast::Sender<RecentFile>,
    pub source_stats_cache: Arc<std::sync::RwLock<crate::stats_cache::SourceStatsCache>>,
    pub stats_watch:        Arc<tokio::sync::watch::Sender<u64>>,
}

// ── Public entry point ─────────────────────────────────────────────────────────

/// Async wrapper: runs `process_request_phase1` in a blocking task with a
/// configurable timeout, then moves the file to `failed/` on error.
pub(super) async fn process_request_async(
    ctx: &RequestContext,
    handles: &IndexerHandles,
) {
    let status_reset = handles.status.clone();
    let request_timeout = handles.cfg.request_timeout;

    let blocking_task = tokio::task::spawn_blocking({
        let data_dir = ctx.data_dir.clone();
        let request_path = ctx.request_path.clone();
        let to_archive_dir = ctx.to_archive_dir.clone();
        let status = handles.status.clone();
        let cfg = handles.cfg.clone();
        let shared_archive = Arc::clone(&handles.shared_archive);
        let recent_tx = handles.recent_tx.clone();
        move || process_request_phase1(&data_dir, &request_path, &to_archive_dir, &status, cfg, &shared_archive, &recent_tx)
    });

    let timed_result = tokio::time::timeout(request_timeout, blocking_task).await;

    if let Ok(mut guard) = status_reset.lock() {
        *guard = find_common::api::WorkerStatus::Idle;
    }

    match timed_result {
        Err(_timeout) => {
            tracing::error!(
                "Request processing timed out after {}s, abandoning: {}",
                request_timeout.as_secs(),
                ctx.request_path.display(),
            );
            handle_failure(
                &ctx.request_path,
                &ctx.failed_dir,
                anyhow::anyhow!("Processing timed out after {}s", request_timeout.as_secs()),
            )
            .await;
        }
        Ok(Ok(Ok(delta))) => {
            // The normalized .gz was already written to to-archive/ by the blocking task.
            // Delete the original from inbox/.
            if let Err(e) = tokio::fs::remove_file(&ctx.request_path).await {
                tracing::error!(
                    "Failed to delete processed request {}: {}",
                    ctx.request_path.display(),
                    e
                );
            } else {
                tracing::debug!("Phase 1 complete, queued for archive: {}", ctx.request_path.display());
                handles.archive_notify.notify_one();
            }
            // Apply incremental stats delta to the cache.
            if let Ok(mut guard) = handles.source_stats_cache.write() {
                guard.apply_delta(&delta);
            }
            handles.stats_watch.send_modify(|v| *v = v.wrapping_add(1));
        }
        Ok(Ok(Err(e))) => {
            if is_db_locked(&e) {
                // File is still in inbox/ — the router will rediscover and
                // retry it on the next scan tick.
                tracing::warn!(
                    "Database locked while processing {}, will retry: {e:#}",
                    ctx.request_path.display(),
                );
            } else {
                handle_failure(&ctx.request_path, &ctx.failed_dir, e).await;
            }
        }
        Ok(Err(e)) => {
            handle_failure(
                &ctx.request_path,
                &ctx.failed_dir,
                anyhow::anyhow!("Task error: {}", e),
            )
            .await;
        }
    }
}

// ── Phase 1: synchronous request processing ───────────────────────────────────

/// Phase 1: process a single inbox request — SQLite only, no ZIP I/O.
/// Writes a normalized `.gz` to `to_archive_dir` for the archive phase.
fn process_request_phase1(
    data_dir: &Path,
    request_path: &Path,
    to_archive_dir: &Path,
    status: &StatusHandle,
    cfg: WorkerConfig,
    shared_archive_state: &Arc<SharedArchiveState>,
    recent_tx: &tokio::sync::broadcast::Sender<RecentFile>,
) -> Result<crate::stats_cache::SourceStatsDelta> {
    let request_start = std::time::Instant::now();

    // Use a placeholder tag until we've parsed the request.
    let req_stem = request_path.file_stem().and_then(|s| s.to_str()).unwrap_or("?");
    let pre_tag = format!("[indexer:?:{req_stem}]");

    let (compressed, request): (Vec<u8>, BulkRequest) = timed!(pre_tag, "read+decode gz", {
        let compressed = std::fs::read(request_path)?;
        let mut decoder = GzDecoder::new(&compressed[..]);
        let mut json = String::new();
        decoder.read_to_string(&mut json)?;
        let request: BulkRequest = serde_json::from_str(&json)
            .context("parsing bulk request JSON")?;
        (compressed, request)
    });
    let compressed_bytes = compressed.len();

    let n_files = request.files.len();
    let n_deletes = request.delete_paths.len();
    let n_renames = request.rename_paths.len();
    let total_content_lines: usize = request.files.iter().map(|f| f.lines.len()).sum();
    let total_content_bytes: usize = request.files.iter()
        .flat_map(|f| f.lines.iter())
        .map(|l| l.content.len())
        .sum();

    let tag     = format!("[indexer:{}:{req_stem}]", request.source); // debug-level tag (includes request id)
    let src_tag = format!("[indexer:{}]",            request.source); // info-level tag  (source only)

    let mut delta = crate::stats_cache::SourceStatsDelta {
        source: request.source.clone(),
        ..Default::default()
    };

    tracing::debug!("{tag} start: {} files, {} deletes, {} renames", n_files, n_deletes, n_renames);

    let db_path = data_dir.join("sources").join(format!("{}.db", request.source));
    let mut conn = timed!(tag, "open db", { db::open(&db_path)? });

    // Acquire the per-source write lock before any SQLite writes.
    let source_lock = shared_archive_state.source_lock(&request.source);
    let _source_guard = timed!(tag, "acquire source lock", {
        source_lock.lock()
            .map_err(|_| anyhow::anyhow!("source lock poisoned for {}", request.source))?
    });

    // Process deletes (SQLite only — orphaned ZIP chunks cleaned up by compaction).
    if !request.delete_paths.is_empty() {
        if let Ok(mut guard) = status.lock() {
            *guard = find_common::api::WorkerStatus::Processing {
                source: request.source.clone(),
                file: format!("(deleting {} files)", n_deletes),
            };
        }
        let delete_delta = timed!(tag, format!("delete {} paths", n_deletes), {
            db::delete_files_phase1(&conn, &request.delete_paths)?
        });
        delta.files_delta -= delete_delta.files_removed;
        delta.size_delta  -= delete_delta.size_removed;
        for (kind, (count, size)) in delete_delta.by_kind {
            let e = delta.kind_deltas.entry(kind).or_insert((0, 0));
            e.0 -= count;
            e.1 -= size;
        }
    }

    // Process renames after deletes, before upserts.
    if !request.rename_paths.is_empty() {
        timed!(tag, format!("rename {} paths", n_renames), {
            db::rename_files(&conn, &request.rename_paths)?
        });
    }

    let mut server_side_failures: Vec<IndexingFailure> = Vec::new();
    let mut successfully_indexed: Vec<String> = Vec::new();
    let mut activity_added: Vec<String> = Vec::new();
    let mut activity_modified: Vec<String> = Vec::new();
    let mut normalized_files: Vec<find_common::api::IndexFile> = Vec::with_capacity(request.files.len());
    tracing::debug!("{tag} → index {} files", n_files);
    let index_loop_start = std::time::Instant::now();
    for file in &request.files {
        if let Ok(mut guard) = status.lock() {
            *guard = find_common::api::WorkerStatus::Processing {
                source: request.source.clone(),
                file: file.path.clone(),
            };
        }
        let file_start = std::time::Instant::now();
        let normalized_file;
        let file = if file.kind.is_text_like() {
            let normalized_lines = timed!(tag, format!("normalize {}", file.path), {
                normalize::normalize_lines(
                    file.lines.clone(),
                    &file.path,
                    &cfg.normalization,
                )
            });
            normalized_file = find_common::api::IndexFile {
                lines: normalized_lines,
                ..file.clone()
            };
            &normalized_file
        } else {
            file
        };
        match pipeline::process_file_phase1(&mut conn, file, cfg.inline_threshold_bytes) {
            Ok(outcome) => {
                successfully_indexed.push(file.path.clone());
                if file.mtime != 0 && !is_composite(&file.path) {
                    match &outcome {
                        pipeline::Phase1Outcome::New      => activity_added.push(file.path.clone()),
                        pipeline::Phase1Outcome::Modified { .. } => activity_modified.push(file.path.clone()),
                        pipeline::Phase1Outcome::Skipped  => {}
                    }
                }
                // Accumulate incremental stats delta (composite paths excluded).
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
        normalized_files.push(file.clone());
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
            &conn,
            &successfully_indexed,
            &all_failures,
            now,
            request.scan_timestamp,
        )?
    });

    // Log activity and broadcast SSE events.
    {
        let deleted: Vec<String> = request.delete_paths.iter()
            .filter(|p| !is_composite(p))
            .cloned()
            .collect();
        let renamed: Vec<(String, String)> = request.rename_paths.iter()
            .filter(|r| !is_composite(&r.old_path) && !is_composite(&r.new_path))
            .map(|r| (r.old_path.clone(), r.new_path.clone()))
            .collect();
        if let Err(e) = db::log_activity(&conn, now, &activity_added, &activity_modified, &deleted, &renamed, cfg.activity_log_max_entries) {
            tracing::warn!("Failed to write activity log: {e:#}");
        } else {
            let source = &request.source;
            for path in &activity_added {
                let _ = recent_tx.send(RecentFile { source: source.clone(), path: path.clone(), indexed_at: now, action: RecentAction::Added,    new_path: None });
            }
            for path in &activity_modified {
                let _ = recent_tx.send(RecentFile { source: source.clone(), path: path.clone(), indexed_at: now, action: RecentAction::Modified, new_path: None });
            }
            for path in &deleted {
                let _ = recent_tx.send(RecentFile { source: source.clone(), path: path.clone(), indexed_at: now, action: RecentAction::Deleted,  new_path: None });
            }
            for (old, new) in &renamed {
                let _ = recent_tx.send(RecentFile { source: source.clone(), path: old.clone(),  indexed_at: now, action: RecentAction::Renamed,  new_path: Some(new.clone()) });
            }
        }
    }

    let elapsed = request_start.elapsed();
    let elapsed_secs = elapsed.as_secs_f64();
    let content_kb = total_content_bytes / 1024;
    let compressed_kb = compressed_bytes / 1024;
    tracing::debug!("{tag} done: {} files, {} deletes, {} renames, {} lines, {} KB content, {} KB compressed, {:.1}s",
        n_files, n_deletes, n_renames, total_content_lines, content_kb, compressed_kb, elapsed_secs);
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

    // Skip the archive phase entirely when there is nothing to write.
    if normalized_files.is_empty() && request.rename_paths.is_empty() {
        tracing::debug!("{tag} skipping archive phase (no chunks to write)");
        return Ok(delta);
    }

    // Write a normalized BulkRequest as a .gz to to-archive/.
    timed!(tag, "write normalized gz", {
        let normalized_request = BulkRequest {
            source: request.source.clone(),
            files: normalized_files,
            delete_paths: request.delete_paths.clone(),
            scan_timestamp: request.scan_timestamp,
            indexing_failures: request.indexing_failures.clone(),
            rename_paths: request.rename_paths.clone(),
        };
        let json = serde_json::to_vec(&normalized_request)
            .context("serializing normalized request")?;
        let file_name = request_path.file_name()
            .context("request path has no filename")?;
        let to_archive_path = to_archive_dir.join(file_name);
        let out = std::fs::File::create(&to_archive_path)
            .context("creating to-archive file")?;
        let mut encoder = GzEncoder::new(out, flate2::Compression::default());
        encoder.write_all(&json).context("writing normalized gz")?;
        encoder.finish().context("finalizing normalized gz")?
    });

    Ok(delta)
}

// ── Tests ──────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use find_common::api::{BulkRequest, FileKind, IndexFile, IndexLine, PathRename};
    use std::sync::{Arc, Mutex};
    use tempfile::TempDir;

    fn make_worker_config() -> WorkerConfig {
        WorkerConfig {
            request_timeout: std::time::Duration::from_secs(30),
            inline_threshold_bytes: 0,
            archive_batch_size: 100,
            activity_log_max_entries: 1000,
            normalization: find_common::config::NormalizationSettings::default(),
        }
    }

    fn make_status() -> StatusHandle {
        Arc::new(Mutex::new(find_common::api::WorkerStatus::Idle))
    }

    fn make_index_file(path: &str, kind: FileKind) -> IndexFile {
        IndexFile {
            path: path.to_string(),
            mtime: 1_000_000,
            size: Some(42),
            kind,
            scanner_version: 1,
            lines: vec![IndexLine {
                archive_path: None,
                line_number: 0,
                content: path.to_string(),
            }],
            extract_ms: None,
            content_hash: None,
            is_new: true,
        }
    }

    fn write_bulk_request_gz(path: &std::path::Path, req: &BulkRequest) {
        let json = serde_json::to_vec(req).unwrap();
        let file = std::fs::File::create(path).unwrap();
        let mut enc = GzEncoder::new(file, flate2::Compression::default());
        enc.write_all(&json).unwrap();
        enc.finish().unwrap();
    }

    fn setup_dirs() -> (TempDir, std::path::PathBuf, std::path::PathBuf, std::path::PathBuf) {
        let tmp = TempDir::new().unwrap();
        let data_dir = tmp.path().to_path_buf();
        std::fs::create_dir_all(data_dir.join("sources/content")).unwrap();
        let to_archive_dir = tmp.path().join("to-archive");
        std::fs::create_dir_all(&to_archive_dir).unwrap();
        let inbox_dir = tmp.path().join("inbox");
        std::fs::create_dir_all(&inbox_dir).unwrap();
        (tmp, data_dir, to_archive_dir, inbox_dir)
    }

    #[test]
    fn upsert_writes_db_record_and_archive_gz() {
        let (_tmp, data_dir, to_archive_dir, inbox_dir) = setup_dirs();
        let shared = crate::archive::SharedArchiveState::new(data_dir.clone()).unwrap();
        let (recent_tx, _rx) = tokio::sync::broadcast::channel::<RecentFile>(16);

        let req = BulkRequest {
            source: "testsource".to_string(),
            files: vec![make_index_file("docs/readme.txt", FileKind::Text)],
            delete_paths: vec![],
            rename_paths: vec![],
            scan_timestamp: Some(1_000_000),
            indexing_failures: vec![],
        };

        let request_path = inbox_dir.join("req001.gz");
        write_bulk_request_gz(&request_path, &req);

        process_request_phase1(
            &data_dir,
            &request_path,
            &to_archive_dir,
            &make_status(),
            make_worker_config(),
            &shared,
            &recent_tx,
        )
        .unwrap();

        // A .gz should have been written to to_archive_dir.
        let gz_files: Vec<_> = std::fs::read_dir(&to_archive_dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.path().extension().and_then(|x| x.to_str()) == Some("gz"))
            .collect();
        assert_eq!(gz_files.len(), 1, "expected one .gz in to_archive_dir");

        // The DB should have the file record.
        let db_path = data_dir.join("sources").join("testsource.db");
        let conn = crate::db::open(&db_path).unwrap();
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM files WHERE path = 'docs/readme.txt'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 1, "expected file record in DB");
    }

    #[test]
    fn delete_removes_db_record() {
        let (_tmp, data_dir, to_archive_dir, inbox_dir) = setup_dirs();
        let shared = crate::archive::SharedArchiveState::new(data_dir.clone()).unwrap();
        let (recent_tx, _rx) = tokio::sync::broadcast::channel::<RecentFile>(16);

        // First, index the file.
        let upsert_req = BulkRequest {
            source: "testsource".to_string(),
            files: vec![make_index_file("notes/todo.txt", FileKind::Text)],
            delete_paths: vec![],
            rename_paths: vec![],
            scan_timestamp: Some(1_000_000),
            indexing_failures: vec![],
        };
        let req_path1 = inbox_dir.join("req001.gz");
        write_bulk_request_gz(&req_path1, &upsert_req);
        process_request_phase1(
            &data_dir,
            &req_path1,
            &to_archive_dir,
            &make_status(),
            make_worker_config(),
            &shared,
            &recent_tx,
        )
        .unwrap();

        // Confirm it's in the DB.
        let db_path = data_dir.join("sources").join("testsource.db");
        {
            let conn = crate::db::open(&db_path).unwrap();
            let count: i64 = conn
                .query_row("SELECT COUNT(*) FROM files WHERE path = 'notes/todo.txt'", [], |r| r.get(0))
                .unwrap();
            assert_eq!(count, 1, "file should be present before delete");
        }

        // Now delete it.
        let delete_req = BulkRequest {
            source: "testsource".to_string(),
            files: vec![],
            delete_paths: vec!["notes/todo.txt".to_string()],
            rename_paths: vec![],
            scan_timestamp: Some(1_000_001),
            indexing_failures: vec![],
        };
        let req_path2 = inbox_dir.join("req002.gz");
        write_bulk_request_gz(&req_path2, &delete_req);

        // Use a fresh to_archive_dir for the delete call so we can check it independently.
        let to_archive_dir2 = TempDir::new().unwrap();

        process_request_phase1(
            &data_dir,
            &req_path2,
            to_archive_dir2.path(),
            &make_status(),
            make_worker_config(),
            &shared,
            &recent_tx,
        )
        .unwrap();

        // File should be removed from DB.
        let conn = crate::db::open(&db_path).unwrap();
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM files WHERE path = 'notes/todo.txt'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 0, "file should be absent after delete");

        // With no files and no renames the archive phase IS skipped, so to_archive_dir2
        // should be empty here.
        let gz_files: Vec<_> = std::fs::read_dir(to_archive_dir2.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.path().extension().and_then(|x| x.to_str()) == Some("gz"))
            .collect();
        assert_eq!(
            gz_files.len(),
            0,
            "delete-only request should NOT write a .gz to to_archive_dir"
        );
    }

    #[test]
    fn rename_updates_path_in_db() {
        let (_tmp, data_dir, to_archive_dir, inbox_dir) = setup_dirs();
        let shared = crate::archive::SharedArchiveState::new(data_dir.clone()).unwrap();
        let (recent_tx, _rx) = tokio::sync::broadcast::channel::<RecentFile>(16);

        // Index file at original path.
        let upsert_req = BulkRequest {
            source: "testsource".to_string(),
            files: vec![make_index_file("src/old_name.rs", FileKind::Text)],
            delete_paths: vec![],
            rename_paths: vec![],
            scan_timestamp: Some(1_000_000),
            indexing_failures: vec![],
        };
        let req_path1 = inbox_dir.join("req001.gz");
        write_bulk_request_gz(&req_path1, &upsert_req);
        process_request_phase1(
            &data_dir,
            &req_path1,
            &to_archive_dir,
            &make_status(),
            make_worker_config(),
            &shared,
            &recent_tx,
        )
        .unwrap();

        // Rename file.
        let rename_req = BulkRequest {
            source: "testsource".to_string(),
            files: vec![],
            delete_paths: vec![],
            rename_paths: vec![PathRename {
                old_path: "src/old_name.rs".to_string(),
                new_path: "src/new_name.rs".to_string(),
            }],
            scan_timestamp: Some(1_000_001),
            indexing_failures: vec![],
        };
        let req_path2 = inbox_dir.join("req002.gz");
        write_bulk_request_gz(&req_path2, &rename_req);

        // Use a fresh to_archive_dir for the rename call so we can assert exactly 1 .gz.
        let to_archive_dir2 = TempDir::new().unwrap();

        process_request_phase1(
            &data_dir,
            &req_path2,
            to_archive_dir2.path(),
            &make_status(),
            make_worker_config(),
            &shared,
            &recent_tx,
        )
        .unwrap();

        let db_path = data_dir.join("sources").join("testsource.db");
        let conn = crate::db::open(&db_path).unwrap();

        let old_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM files WHERE path = 'src/old_name.rs'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(old_count, 0, "old path should be gone after rename");

        let new_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM files WHERE path = 'src/new_name.rs'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(new_count, 1, "new path should exist after rename");

        // A rename-only request DOES write a .gz to to_archive_dir (only skipped when
        // BOTH normalized_files AND rename_paths are empty).
        let gz_files: Vec<_> = std::fs::read_dir(to_archive_dir2.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.path().extension().and_then(|x| x.to_str()) == Some("gz"))
            .collect();
        assert_eq!(gz_files.len(), 1, "rename-only request should write exactly 1 .gz to to_archive_dir");
    }

    #[test]
    fn empty_files_no_archive_gz_written() {
        let (_tmp, data_dir, to_archive_dir, inbox_dir) = setup_dirs();
        let shared = crate::archive::SharedArchiveState::new(data_dir.clone()).unwrap();
        let (recent_tx, _rx) = tokio::sync::broadcast::channel::<RecentFile>(16);

        let req = BulkRequest {
            source: "testsource".to_string(),
            files: vec![],
            delete_paths: vec![],
            rename_paths: vec![],
            scan_timestamp: None,
            indexing_failures: vec![],
        };
        let request_path = inbox_dir.join("req001.gz");
        write_bulk_request_gz(&request_path, &req);

        process_request_phase1(
            &data_dir,
            &request_path,
            &to_archive_dir,
            &make_status(),
            make_worker_config(),
            &shared,
            &recent_tx,
        )
        .unwrap();

        let gz_files: Vec<_> = std::fs::read_dir(&to_archive_dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.path().extension().and_then(|x| x.to_str()) == Some("gz"))
            .collect();
        assert_eq!(
            gz_files.len(),
            0,
            "empty request should not write any .gz to to_archive_dir"
        );
    }

    /// When a path appears in both `delete_paths` and `files` in the same request,
    /// the delete runs first, then the upsert re-adds it. The file should be present
    /// at the end. This documents the "deletes before upserts" ordering invariant.
    #[test]
    fn deletes_processed_before_upserts_in_same_request() {
        let (_tmp, data_dir, to_archive_dir, inbox_dir) = setup_dirs();
        let shared = crate::archive::SharedArchiveState::new(data_dir.clone()).unwrap();
        let (recent_tx, _rx) = tokio::sync::broadcast::channel::<RecentFile>(16);

        // First, seed the file so there is something to delete.
        let seed_req = BulkRequest {
            source: "testsource".to_string(),
            files: vec![make_index_file("data/file.txt", FileKind::Text)],
            delete_paths: vec![],
            rename_paths: vec![],
            scan_timestamp: Some(1_000_000),
            indexing_failures: vec![],
        };
        let req_path1 = inbox_dir.join("req001.gz");
        write_bulk_request_gz(&req_path1, &seed_req);
        process_request_phase1(
            &data_dir,
            &req_path1,
            &to_archive_dir,
            &make_status(),
            make_worker_config(),
            &shared,
            &recent_tx,
        )
        .unwrap();

        // Now send a request that both deletes AND upserts the same path.
        let combined_req = BulkRequest {
            source: "testsource".to_string(),
            files: vec![make_index_file("data/file.txt", FileKind::Text)],
            delete_paths: vec!["data/file.txt".to_string()],
            rename_paths: vec![],
            scan_timestamp: Some(1_000_001),
            indexing_failures: vec![],
        };
        let req_path2 = inbox_dir.join("req002.gz");
        write_bulk_request_gz(&req_path2, &combined_req);
        process_request_phase1(
            &data_dir,
            &req_path2,
            &to_archive_dir,
            &make_status(),
            make_worker_config(),
            &shared,
            &recent_tx,
        )
        .unwrap();

        // The upsert should win (delete happened first, then upsert re-added it).
        let db_path = data_dir.join("sources").join("testsource.db");
        let conn = crate::db::open(&db_path).unwrap();
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM files WHERE path = 'data/file.txt'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 1, "file should be present after delete+upsert in same request (upsert wins)");
    }
}

// ── Helpers ────────────────────────────────────────────────────────────────────

pub(super) fn is_db_locked(error: &anyhow::Error) -> bool {
    for cause in error.chain() {
        if let Some(rusqlite::Error::SqliteFailure(e, _)) = cause.downcast_ref::<rusqlite::Error>() {
            if matches!(e.code, ErrorCode::DatabaseBusy | ErrorCode::DatabaseLocked) {
                return true;
            }
        }
    }
    false
}

pub(super) async fn handle_failure(path: &Path, failed_dir: &Path, error: anyhow::Error) {
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
