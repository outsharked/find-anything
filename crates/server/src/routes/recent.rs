use std::sync::Arc;

use axum::{
    extract::{Query, State},
    http::HeaderMap,
    response::IntoResponse,
    Json,
};
use serde::Deserialize;
use tokio::task::spawn_blocking;

use find_common::api::{RecentFile, RecentResponse};

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
                            action: "modified".to_string(),
                            new_path: None,
                        })
                        .collect())
                } else {
                    let rows = db::recent_activity(&conn, limit)?;
                    Ok(rows
                        .into_iter()
                        .map(|(action, path, new_path, occurred_at)| RecentFile {
                            source: source_name.clone(),
                            path,
                            indexed_at: occurred_at,
                            action,
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

    all.sort_by(|a, b| b.indexed_at.cmp(&a.indexed_at));
    all.truncate(limit);

    Json(RecentResponse { files: all }).into_response()
}
