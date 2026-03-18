use std::sync::Arc;

use axum::{
    extract::{Query, State},
    http::HeaderMap,
    response::IntoResponse,
    Json,
};
use serde::Deserialize;

use find_common::api::{SourceStats, StatsResponse, WorkerStatus};

use crate::{db, AppState};

use super::check_auth;

// ── GET /api/v1/stats ─────────────────────────────────────────────────────────

#[derive(Deserialize, Default)]
pub(crate) struct StatsQuery {
    #[serde(default)]
    refresh: bool,
}

pub async fn get_stats(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(query): Query<StatsQuery>,
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

    let inbox_paused = state.inbox_paused.load(std::sync::atomic::Ordering::Relaxed);

    // If ?refresh=true, rebuild the cache and refresh compaction stats before reading.
    if query.refresh {
        let cache        = Arc::clone(&state.source_stats_cache);
        let compact_slot = Arc::clone(&state.compaction_stats);
        let data_dir     = state.data_dir.clone();
        tokio::task::spawn_blocking(move || {
            crate::stats_cache::full_rebuild(&data_dir, &cache);
            if let Ok(compact) = crate::compaction::scan_wasted_space(&data_dir) {
                crate::compaction::save_stats_to_slot(&compact_slot, &data_dir, compact);
            }
        }).await.ok();
        state.stats_watch.send_modify(|v| *v = v.wrapping_add(1));
    }

    let (orphaned_bytes, orphaned_stats_age_secs) = state.compaction_stats
        .read()
        .ok()
        .and_then(|g| g.as_ref().map(|s| {
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs() as i64;
            let age = (now - s.scanned_at).max(0) as u64;
            (Some(s.orphaned_bytes), Some(age))
        }))
        .unwrap_or((None, None));

    // Compute DB file sizes (fast filesystem metadata, no DB connections needed).
    let db_size_bytes: u64 = {
        let sources_dir = state.data_dir.join("sources");
        std::fs::read_dir(&sources_dir)
            .map(|rd| rd.flatten()
                .filter(|e| e.path().extension().map(|x| x == "db").unwrap_or(false))
                .filter_map(|e| e.metadata().ok())
                .map(|m| m.len())
                .sum::<u64>())
            .unwrap_or(0)
    };

    // Read cached aggregate stats under the lock, then release before opening DB connections
    // (avoids holding the lock while opening DB connections, which would block worker's apply_delta).
    let cached: Vec<crate::stats_cache::CachedSourceStats> = {
        let guard = state.source_stats_cache.read().unwrap_or_else(|e| e.into_inner());
        guard.sources.clone()
    };

    let sources: Vec<SourceStats> = cached.into_iter().map(|s| {
        let db_path = state.data_dir.join("sources").join(format!("{}.db", s.name));
        let (last_scan, history, indexing_error_count) = if let Ok(conn) = db::open_for_stats(&db_path) {
            (
                db::get_last_scan(&conn).unwrap_or(None),
                db::get_scan_history(&conn, 100).unwrap_or_default(),
                db::get_indexing_error_count(&conn).unwrap_or(0),
            )
        } else {
            (None, vec![], 0)
        };
        SourceStats {
            name:                 s.name.clone(),
            last_scan,
            total_files:          s.total_files,
            total_size:           s.total_size,
            by_kind:              s.by_kind.clone(),
            by_ext:               s.by_ext.clone(),
            history,
            indexing_error_count,
            fts_row_count:        s.fts_row_count,
        }
    }).collect();

    Json(StatsResponse {
        sources,
        inbox_pending,
        failed_requests,
        archive_queue,
        total_archives,
        db_size_bytes,
        archive_size_bytes,
        worker_status,
        inbox_paused,
        orphaned_bytes,
        orphaned_stats_age_secs,
    }).into_response()
}
