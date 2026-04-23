use std::cmp::Reverse;
use std::sync::Arc;
use std::time::Duration;

use axum::{
    extract::{Query, State},
    http::HeaderMap,
    response::{sse::{Event, KeepAlive, Sse}, IntoResponse},
    Json,
};
use serde::Deserialize;
use tokio::task::spawn_blocking;
use tokio_stream::wrappers::BroadcastStream;
use tokio_stream::StreamExt as _;

use find_common::api::{RecentAction, RecentFile, RecentResponse};

use crate::{db, AppState};

use super::check_auth;

// ── GET /api/v1/recent ────────────────────────────────────────────────────────

#[derive(Deserialize)]
pub struct RecentQuery {
    #[serde(default = "default_limit")]
    limit: usize,
    /// `mtime` = sort by file modification time; anything else (or absent) = sort by indexed time.
    #[serde(default)]
    sort: String,
}

const MAX_RECENT_LIMIT: usize = 1000;

fn default_limit() -> usize { 20 }

pub async fn get_recent(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(query): Query<RecentQuery>,
) -> impl IntoResponse {
    if let Err(s) = check_auth(&state, &headers) {
        return (s, Json(serde_json::Value::Null)).into_response();
    }

    let sources_dir = state.data_dir.join("sources");
    if query.limit > MAX_RECENT_LIMIT {
        return (
            axum::http::StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": format!("limit exceeds maximum of {MAX_RECENT_LIMIT}") })),
        ).into_response();
    }
    let limit = query.limit;
    let sort_by_mtime = query.sort == "mtime";

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

    let handles: Vec<_> = source_dbs
        .into_iter()
        .map(|(source_name, db_path)| {
            spawn_blocking(move || -> anyhow::Result<Vec<RecentFile>> {
                if !db_path.exists() {
                    return Ok(vec![]);
                }
                let conn = db::open(&db_path)?;
                if sort_by_mtime {
                    let rows = db::recent_files(&conn, limit, true)?;
                    Ok(rows
                        .into_iter()
                        .map(|(path, indexed_at)| RecentFile {
                            source: source_name.clone(),
                            path,
                            indexed_at,
                            action: RecentAction::Modified,
                            new_path: None,
                        })
                        .collect())
                } else {
                    let rows = db::recent_activity(&conn, limit)?;
                    Ok(rows
                        .into_iter()
                        .map(|(action_str, path, new_path, occurred_at)| RecentFile {
                            source: source_name.clone(),
                            path,
                            indexed_at: occurred_at,
                            action: RecentAction::from(action_str.as_str()),
                            new_path,
                        })
                        .collect())
                }
            })
        })
        .collect();

    let mut all: Vec<RecentFile> = Vec::new();
    for handle in handles {
        match handle.await.unwrap_or_else(|e| Err(anyhow::anyhow!(e))) {
            Ok(files) => all.extend(files),
            Err(e) => tracing::warn!("recent files error: {e:#}"),
        }
    }

    all.sort_by_key(|a| Reverse(a.indexed_at));
    all.truncate(limit);

    Json(RecentResponse { files: all }).into_response()
}

// ── GET /api/v1/recent/stream (SSE) ──────────────────────────────────────────

/// Shared helper: fetch recent files from all source DBs, sorted newest-first.
async fn fetch_recent_from_dbs(state: &AppState, limit: usize, sort_by_mtime: bool) -> Vec<RecentFile> {
    let sources_dir = state.data_dir.join("sources");
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
    let handles: Vec<_> = source_dbs
        .into_iter()
        .map(|(source_name, db_path)| {
            spawn_blocking(move || -> anyhow::Result<Vec<RecentFile>> {
                if !db_path.exists() {
                    return Ok(vec![]);
                }
                let conn = db::open(&db_path)?;
                if sort_by_mtime {
                    db::recent_files(&conn, limit, true).map(|rows| {
                        rows.into_iter().map(|(path, indexed_at)| RecentFile {
                            source: source_name.clone(),
                            path,
                            indexed_at,
                            action: RecentAction::Modified,
                            new_path: None,
                        }).collect()
                    })
                } else {
                    db::recent_activity(&conn, limit).map(|rows| {
                        rows.into_iter().map(|(action_str, path, new_path, occurred_at)| RecentFile {
                            source: source_name.clone(),
                            path,
                            indexed_at: occurred_at,
                            action: RecentAction::from(action_str.as_str()),
                            new_path,
                        }).collect()
                    })
                }
            })
        })
        .collect();
    let mut all = Vec::new();
    for handle in handles {
        match handle.await.unwrap_or_else(|e| Err(anyhow::anyhow!(e))) {
            Ok(files) => all.extend(files),
            Err(e) => tracing::warn!("recent files error: {e:#}"),
        }
    }
    all.sort_by_key(|a| Reverse(a.indexed_at));
    all.truncate(limit);
    all
}

pub async fn stream_recent(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(query): Query<RecentQuery>,
) -> impl IntoResponse {
    if let Err(s) = check_auth(&state, &headers) {
        return (s, "Unauthorized").into_response();
    }

    let limit = query.limit.min(MAX_RECENT_LIMIT);
    let sort_by_mtime = query.sort == "mtime";

    // Subscribe before the DB query so we don't miss events that arrive
    // while we're fetching history.
    let rx = state.recent_tx.subscribe();

    // Fetch historical entries; send them oldest-first (tail -f style).
    let mut initial = fetch_recent_from_dbs(&state, limit, sort_by_mtime).await;
    initial.reverse();

    let make_event = |f: RecentFile| -> Result<Event, std::convert::Infallible> {
        Ok(Event::default().json_data(&f).unwrap_or_default())
    };

    let initial_stream = tokio_stream::iter(initial).map(make_event);

    let live_stream = BroadcastStream::new(rx)
        .filter_map(|r| r.ok())
        .map(|f| Ok::<Event, std::convert::Infallible>(
            Event::default().json_data(&f).unwrap_or_default()
        ));

    let combined = initial_stream.chain(live_stream);

    Sse::new(combined)
        .keep_alive(KeepAlive::new().interval(Duration::from_secs(30)))
        .into_response()
}
