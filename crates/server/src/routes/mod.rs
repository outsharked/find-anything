mod admin;
mod bulk;
mod context;
mod errors;
mod file;
mod raw;
mod recent;
mod search;
mod session;
mod settings;
mod stats;
mod tree;
pub mod upload;

pub use admin::{delete_source, inbox_clear, inbox_retry, inbox_show, inbox_status, update_check, update_apply};
pub use bulk::bulk;
pub use context::{context_batch, get_context};
pub use errors::get_errors;
pub use file::{get_file, list_files};
pub use raw::get_raw;
pub use recent::get_recent;
pub use search::search;
pub use session::{create_session, delete_session};
pub use stats::get_stats;
pub use tree::{list_dir, list_sources};
pub use upload::{upload_init, upload_patch, upload_status};
pub use self::settings::get_settings;

use std::sync::Arc;

use axum::{
    extract::State,
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    Json,
};

use crate::AppState;

// ── Shared helpers ─────────────────────────────────────────────────────────────

/// Build a composite path from a base path and an optional legacy `archive_path`.
/// If `archive_path` is `Some` and non-empty, returns `"{path}::{archive_path}"`.
pub(super) fn composite_path(path: &str, archive_path: Option<&str>) -> String {
    match archive_path {
        Some(ap) if !ap.is_empty() => format!("{path}::{ap}"),
        _ => path.to_string(),
    }
}

/// Run a blocking closure on the blocking thread pool, converting the result to
/// an HTTP response. On error, logs with the given label and returns 500.
pub(super) async fn run_blocking<F, T>(label: &'static str, f: F) -> Response
where
    F: FnOnce() -> anyhow::Result<T> + Send + 'static,
    T: IntoResponse + Send + 'static,
{
    match tokio::task::spawn_blocking(f)
        .await
        .unwrap_or_else(|e| Err(anyhow::anyhow!(e)))
    {
        Ok(val) => val.into_response(),
        Err(e) => {
            tracing::error!("{label}: {e:#}");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

pub(super) fn check_auth(state: &AppState, headers: &HeaderMap) -> Result<(), StatusCode> {
    // 1. Check Authorization: Bearer header (existing API clients).
    if headers
        .get("Authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .map(|t| t == state.config.server.token)
        .unwrap_or(false)
    {
        return Ok(());
    }
    // 2. Check find_session cookie (browser-native requests like <img src>).
    if let Some(Ok(cookies)) = headers.get("cookie").map(|v| v.to_str()) {
        for part in cookies.split(';') {
            if let Some(val) = part.trim().strip_prefix("find_session=") {
                if val == state.config.server.token {
                    return Ok(());
                }
            }
        }
    }
    Err(StatusCode::UNAUTHORIZED)
}

pub(super) fn source_db_path(state: &AppState, source: &str) -> Result<std::path::PathBuf, StatusCode> {
    if !source.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') {
        return Err(StatusCode::BAD_REQUEST);
    }
    Ok(state.data_dir.join("sources").join(format!("{}.db", source)))
}

/// Convert a `Vec<ContextLine>` into `(start, match_index, Vec<String>)`.
pub(super) fn compact_lines(
    lines: Vec<find_common::api::ContextLine>,
    center: usize,
) -> (usize, Option<usize>, Vec<String>) {
    let start = lines.first().map_or(0, |l| l.line_number);
    let match_index = lines.iter().position(|l| l.line_number == center);
    (start, match_index, lines.into_iter().map(|l| l.content).collect())
}

// ── GET /api/v1/metrics ────────────────────────────────────────────────────────

pub async fn get_metrics(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> impl IntoResponse {
    if let Err(s) = check_auth(&state, &headers) {
        return (s, Json(serde_json::Value::Null)).into_response();
    }

    let inbox_dir = state.data_dir.join("inbox");
    let failed_dir = inbox_dir.join("failed");
    let sources_dir = state.data_dir.join("sources");

    let count_gz = |dir: &std::path::Path| -> usize {
        std::fs::read_dir(dir)
            .map(|rd| {
                rd.filter_map(|e| e.ok())
                    .filter(|e| e.path().extension().map(|x| x == "gz").unwrap_or(false))
                    .count()
            })
            .unwrap_or(0)
    };

    let total_archives = {
        let content_dir = sources_dir.join("content");
        let mut count = 0;
        if let Ok(rd) = std::fs::read_dir(&content_dir) {
            for entry in rd.filter_map(|e| e.ok()) {
                if entry.path().is_dir() {
                    if let Ok(subdir) = std::fs::read_dir(entry.path()) {
                        count += subdir
                            .filter_map(|e| e.ok())
                            .filter(|e| e.path().extension().map(|x| x == "zip").unwrap_or(false))
                            .count();
                    }
                }
            }
        }
        count
    };

    Json(serde_json::json!({
        "inbox_queue_depth": count_gz(&inbox_dir),
        "failed_requests":   count_gz(&failed_dir),
        "total_archives":    total_archives,
    }))
    .into_response()
}
