use std::sync::Arc;

use axum::{extract::State, http::HeaderMap, response::IntoResponse, Json};

use find_common::api::AppSettingsResponse;

use crate::{db, AppState};

use super::check_auth;

// ── GET /api/v1/settings ──────────────────────────────────────────────────────

pub async fn get_settings(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> impl IntoResponse {
    if let Err(s) = check_auth(&state, &headers) {
        return (s, Json(serde_json::Value::Null)).into_response();
    }

    let version = env!("CARGO_PKG_VERSION");
    let hash = option_env!("GIT_HASH").unwrap_or("unknown");
    let tag = option_env!("GIT_TAG").unwrap_or("").trim();
    let dirty = option_env!("GIT_DIRTY").unwrap_or("").trim();

    // "release" if built from an exact version tag with a clean working tree;
    // "dev" if there were uncommitted changes at build time;
    // otherwise the short commit hash (e.g. a post-release or local dev build).
    let git_hash = if !dirty.is_empty() {
        "dev".to_string()
    } else if tag == format!("v{version}") {
        "release".to_string()
    } else {
        hash.to_string()
    };

    Json(AppSettingsResponse {
        context_window: state.config.search.context_window,
        version: version.to_string(),
        schema_version: db::SCHEMA_VERSION,
        git_hash,
        min_client_version: find_common::api::MIN_CLIENT_VERSION.to_string(),
    })
    .into_response()
}
