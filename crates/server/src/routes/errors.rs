use std::sync::Arc;

use axum::{
    extract::{Query, State},
    http::HeaderMap,
    response::IntoResponse,
    Json,
};
use serde::Deserialize;

use find_common::api::ErrorsResponse;

use crate::{db, AppState};

use super::{check_scope, run_blocking, source_db_path};
use find_common::api::Scope;

// ── GET /api/v1/errors?source=X[&limit=200&offset=0] ─────────────────────────

#[derive(Deserialize)]
pub struct ErrorsParams {
    pub source: String,
    #[serde(default = "default_limit")]
    pub limit: usize,
    #[serde(default)]
    pub offset: usize,
}

fn default_limit() -> usize { 200 }

pub async fn get_errors(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(params): Query<ErrorsParams>,
) -> impl IntoResponse {
    if let Err(s) = check_scope(&state, &headers, Scope::Read) {
        return (s, Json(serde_json::Value::Null)).into_response();
    }

    let db_path = match source_db_path(&state, &params.source) {
        Ok(p) => p,
        Err(s) => return (s, Json(serde_json::Value::Null)).into_response(),
    };

    let limit = params.limit.min(1000);
    let offset = params.offset;

    run_blocking("get_errors", move || {
        let conn = db::open(&db_path)?;
        let total = db::get_indexing_error_count(&conn)?;
        let errors = db::get_indexing_errors(&conn, limit, offset)?;
        Ok(Json(ErrorsResponse { errors, total }))
    }).await
}
