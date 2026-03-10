use std::sync::Arc;

use axum::{extract::State, http::HeaderMap, response::IntoResponse, Json};

use find_common::api::{SourceStats, StatsResponse, WorkerStatus};

use crate::{db, AppState};

use super::check_auth;

// ── GET /api/v1/stats ─────────────────────────────────────────────────────────

pub async fn get_stats(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> impl IntoResponse {
    if let Err(s) = check_auth(&state, &headers) {
        return (s, Json(serde_json::Value::Null)).into_response();
    }

    let inbox_dir = state.data_dir.join("inbox");
    let failed_dir = inbox_dir.join("failed");
    let to_archive_dir = inbox_dir.join("to-archive");

    let count_gz = |dir: &std::path::Path| -> usize {
        std::fs::read_dir(dir)
            .map(|rd| {
                rd.filter_map(|e| e.ok())
                    .filter(|e| e.path().extension().map(|x| x == "gz").unwrap_or(false))
                    .count()
            })
            .unwrap_or(0)
    };

    let inbox_pending = count_gz(&inbox_dir);
    let failed_requests = count_gz(&failed_dir);
    let archive_queue = count_gz(&to_archive_dir);

    // Archive totals are maintained incrementally — instant reads, no I/O.
    let total_archives = state.archive_state.total_archives() as usize;
    let archive_size_bytes = state.archive_state.archive_size_bytes();

    let worker_status = state.worker_status
        .lock()
        .map(|g| g.clone())
        .unwrap_or(WorkerStatus::Idle);

    let deleted_since_scan = state.deleted_bytes_since_scan
        .load(std::sync::atomic::Ordering::Relaxed);

    let (orphaned_bytes, orphaned_stats_age_secs) = state.compaction_stats
        .read()
        .ok()
        .and_then(|g| g.as_ref().map(|s| {
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs() as i64;
            let age = (now - s.scanned_at).max(0) as u64;
            let estimated = s.orphaned_bytes.saturating_add(deleted_since_scan);
            (Some(estimated), Some(age))
        }))
        .unwrap_or((None, None));

    // The only blocking work: per-source DB queries + DB file sizes.
    // Uses open_for_stats (1 s busy-timeout) so a locked DB is skipped quickly.
    let data_dir = state.data_dir.clone();
    let (sources, db_size_bytes) = match tokio::task::spawn_blocking(move || {
        query_source_stats(&data_dir)
    }).await {
        Ok(Ok(r))  => r,
        Ok(Err(e)) => { tracing::warn!("stats DB query error: {e:#}"); (vec![], 0) }
        Err(e)     => { tracing::warn!("stats DB query panicked: {e}"); (vec![], 0) }
    };

    Json(StatsResponse {
        sources,
        inbox_pending,
        failed_requests,
        archive_queue,
        total_archives,
        db_size_bytes,
        archive_size_bytes,
        worker_status,
        orphaned_bytes,
        orphaned_stats_age_secs,
    }).into_response()
}

/// Query per-source stats from each source DB and sum up DB file sizes.
/// Uses `open_for_stats` (1 s busy-timeout) so a locked DB is skipped quickly.
fn query_source_stats(
    data_dir: &std::path::Path,
) -> anyhow::Result<(Vec<SourceStats>, u64)> {
    let sources_dir = data_dir.join("sources");

    let db_size_bytes: u64 = std::fs::read_dir(&sources_dir)
        .map(|rd| {
            rd.filter_map(|e| e.ok())
                .filter(|e| e.path().extension().map(|x| x == "db").unwrap_or(false))
                .filter_map(|e| e.metadata().ok())
                .map(|m| m.len())
                .sum()
        })
        .unwrap_or(0);

    let source_dbs: Vec<(String, std::path::PathBuf)> = match std::fs::read_dir(&sources_dir) {
        Err(_) => vec![],
        Ok(rd) => rd
            .filter_map(|e| {
                let e = e.ok()?;
                let name = e.file_name().into_string().ok()?;
                let source_name = name.strip_suffix(".db")?.to_string();
                Some((source_name, e.path()))
            })
            .collect(),
    };

    let mut sources: Vec<SourceStats> = Vec::new();
    for (source_name, db_path) in source_dbs {
        if !db_path.exists() {
            sources.push(SourceStats {
                name: source_name,
                last_scan: None,
                total_files: 0,
                total_size: 0,
                by_kind: Default::default(),
                by_ext: vec![],
                history: vec![],
                indexing_error_count: 0,
                fts_row_count: 0,
            });
            continue;
        }
        match db::open_for_stats(&db_path) {
            Err(e) => tracing::debug!("stats: skipping {}: {e:#}", db_path.display()),
            Ok(conn) => {
                let last_scan = db::get_last_scan(&conn).unwrap_or(None);
                let (total_files, total_size, by_kind) =
                    db::get_stats(&conn).unwrap_or_default();
                let by_ext = db::get_stats_by_ext(&conn).unwrap_or_default();
                let history = db::get_scan_history(&conn, 100).unwrap_or_default();
                let indexing_error_count =
                    db::get_indexing_error_count(&conn).unwrap_or(0);
                let fts_row_count = db::get_fts_row_count(&conn).unwrap_or(0);
                sources.push(SourceStats {
                    name: source_name,
                    last_scan,
                    total_files,
                    total_size,
                    by_kind,
                    by_ext,
                    history,
                    indexing_error_count,
                    fts_row_count,
                });
            }
        }
    }
    sources.sort_by(|a, b| a.name.cmp(&b.name));

    Ok((sources, db_size_bytes))
}
