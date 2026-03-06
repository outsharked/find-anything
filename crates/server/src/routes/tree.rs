use std::sync::Arc;

use axum::{
    extract::{Query, State},
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    Json,
};
use serde::Deserialize;

use find_common::api::{SourceInfo, TreeResponse};

use crate::AppState;

use crate::db;
use super::{check_auth, run_blocking, source_db_path};

// ── GET /api/v1/sources ───────────────────────────────────────────────────────

pub async fn list_sources(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> impl IntoResponse {
    if let Err(s) = check_auth(&state, &headers) {
        return (s, Json(serde_json::Value::Null)).into_response();
    }
    let sources_dir = state.data_dir.join("sources");
    let names: Vec<String> = match std::fs::read_dir(&sources_dir) {
        Err(_) => vec![],
        Ok(rd) => rd
            .filter_map(|e| {
                let e = e.ok()?;
                let name = e.file_name().into_string().ok()?;
                name.strip_suffix(".db").map(|s| s.to_string())
            })
            .collect(),
    };
    let mut infos: Vec<SourceInfo> = names
        .into_iter()
        .map(|name| SourceInfo { name })
        .collect();
    infos.sort_by(|a, b| a.name.cmp(&b.name));
    Json(infos).into_response()
}

// ── GET /api/v1/tree ──────────────────────────────────────────────────────────

#[derive(Deserialize)]
pub struct TreeParams {
    pub source: String,
    /// Directory prefix to list (empty string = root). Must end with `/` for
    /// non-root queries, e.g. `"src/"`.
    #[serde(default)]
    pub prefix: String,
}

pub async fn list_dir(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(params): Query<TreeParams>,
) -> impl IntoResponse {
    if let Err(s) = check_auth(&state, &headers) {
        return (s, Json(serde_json::Value::Null)).into_response();
    }

    let db_path = match source_db_path(&state, &params.source) {
        Ok(p) => p,
        Err(s) => return (s, Json(serde_json::Value::Null)).into_response(),
    };

    if !db_path.exists() {
        return (StatusCode::NOT_FOUND, Json(serde_json::Value::Null)).into_response();
    }

    let prefix = params.prefix.clone();
    run_blocking("list_dir", move || {
        let conn = db::open(&db_path)?;
        db::list_dir(&conn, &prefix).map(|entries| Json(TreeResponse { entries }))
    }).await
}
