use std::sync::Arc;

use axum::{
    extract::{Query, State},
    http::HeaderMap,
    response::IntoResponse,
    Json,
};
use serde::Deserialize;

use find_common::api::{
    ContextBatchRequest, ContextBatchResponse, ContextBatchResult, ContextResponse, FileKind,
};

use crate::{db, AppState};

use super::{check_scope, compact_lines, composite_path, run_blocking, source_db_path};
use find_common::api::Scope;

// ── GET /api/v1/context ───────────────────────────────────────────────────────

#[derive(Deserialize)]
pub struct ContextParams {
    pub source: String,
    pub path: String,
    /// Legacy: combined with `path` into a composite path if provided.
    pub archive_path: Option<String>,
    pub line: usize,
    /// If omitted, the server's configured `search.context_window` is used.
    pub window: Option<usize>,
}

pub async fn get_context(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(params): Query<ContextParams>,
) -> impl IntoResponse {
    if let Err(s) = check_scope(&state, &headers, Scope::Read) { return (s, Json(serde_json::Value::Null)).into_response(); }

    let db_path = match source_db_path(&state, &params.source) {
        Ok(p) => p,
        Err(s) => return (s, Json(serde_json::Value::Null)).into_response(),
    };

    let full_path = composite_path(&params.path, params.archive_path.as_deref());
    let window = params.window.unwrap_or(state.config.search.context_window);
    let content_store = Arc::clone(&state.content_store);

    run_blocking("context", move || {
        let conn = db::open(&db_path)?;
        let kind: FileKind = conn.query_row(
            "SELECT kind FROM files WHERE path = ?1",
            rusqlite::params![full_path],
            |row| row.get::<_, String>(0),
        ).map(|s| FileKind::from(s.as_str())).unwrap_or(FileKind::Text);
        let raw = db::get_context(&conn, content_store.as_ref(), &full_path, params.line, window)?;
        let (start, match_index, lines) = compact_lines(raw, params.line);
        Ok(Json(ContextResponse { start, match_index, lines, kind }))
    }).await
}

// ── POST /api/v1/context-batch ────────────────────────────────────────────────

pub async fn context_batch(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(req): Json<ContextBatchRequest>,
) -> impl IntoResponse {
    if let Err(s) = check_scope(&state, &headers, Scope::Read) {
        return (s, Json(serde_json::Value::Null)).into_response();
    }

    let content_store = Arc::clone(&state.content_store);
    let data_dir = state.data_dir.clone();

    run_blocking("context_batch", move || {

        // Group items by source so we open each DB at most once.
        let mut by_source: std::collections::HashMap<String, (std::path::PathBuf, Vec<find_common::api::ContextBatchItem>)> = std::collections::HashMap::new();
        for item in req.requests {
            let valid = item.source.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
            if valid {
                let db_path = data_dir.join("sources").join(format!("{}.db", item.source));
                by_source.entry(item.source.clone()).or_insert_with(|| (db_path, vec![])).1.push(item);
            }
        }

        let mut results: Vec<ContextBatchResult> = Vec::new();
        for (_source_name, (db_path, items)) in by_source {
            let conn = match db::open(&db_path) {
                Ok(c) => c,
                Err(e) => {
                    tracing::warn!("context_batch open {}: {e:#}", db_path.display());
                    for item in items {
                        results.push(ContextBatchResult { source: item.source, path: item.path, line: item.line, start: 0, match_index: None, lines: vec![], kind: FileKind::Unknown });
                    }
                    continue;
                }
            };

            for item in items {
                let full_path = composite_path(&item.path, item.archive_path.as_deref());

                let (kind, start, match_index, lines) = match (|| -> anyhow::Result<_> {
                    let kind: FileKind = conn
                        .query_row("SELECT kind FROM files WHERE path = ?1", rusqlite::params![full_path], |row| row.get::<_, String>(0))
                        .map(|s| FileKind::from(s.as_str()))
                        .unwrap_or(FileKind::Text);
                    let raw = db::get_context(&conn, content_store.as_ref(), &full_path, item.line, item.window)?;
                    let (start, match_index, lines) = compact_lines(raw, item.line);
                    Ok((kind, start, match_index, lines))
                })() {
                    Ok(t) => t,
                    Err(e) => {
                        tracing::warn!("context_batch item {}/{}: {e:#}", item.source, item.path);
                        (FileKind::Unknown, 0_usize, None, vec![])
                    }
                };

                results.push(ContextBatchResult { source: item.source, path: item.path, line: item.line, start, match_index, lines, kind });
            }
        }

        Ok(Json(ContextBatchResponse { results }))
    }).await
}
