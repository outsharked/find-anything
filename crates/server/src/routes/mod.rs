mod admin;
mod bulk;
mod context;
mod errors;
mod file;
mod links;
mod raw;
mod recent;
mod search;
mod session;
mod settings;
mod stats;
mod tree;
pub mod upload;
mod view;

pub use admin::{compact, delete_source, inbox_clear, inbox_pause, inbox_resume, inbox_retry, inbox_show, inbox_status, update_check, update_apply};
pub use bulk::bulk;
pub use context::{context_batch, get_context};
pub use errors::get_errors;
pub use file::{get_file, list_files};
pub use links::{get_link, post_link};
pub use raw::get_raw;
pub use recent::{get_recent, stream_recent};
pub use search::search;
pub use session::{create_session, delete_session};
pub use stats::{get_stats, stream_stats};
pub use tree::{expand_tree, list_dir, list_sources};
pub use upload::{upload_init, upload_patch, upload_status};
pub use self::settings::get_settings;
pub use view::get_view;

use std::net::SocketAddr;
use std::sync::Arc;

use axum::{
    extract::{ConnectInfo, State},
    http::{HeaderMap, Request, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
    Json,
};

use crate::AppState;

// ── Request logger middleware ──────────────────────────────────────────────────

/// Middleware that logs every API request with its method, path, remote
/// address, response status, and elapsed time.  All events are at DEBUG level.
pub async fn log_request(req: Request<axum::body::Body>, next: Next) -> Response {
    let method = req.method().as_str().to_owned();
    let path   = req.uri().path().to_owned();

    // Prefer X-Forwarded-For (set by reverse proxies); fall back to the TCP
    // peer address injected by `into_make_service_with_connect_info`.
    let addr: String = req.headers()
        .get("x-forwarded-for")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.split(',').next())
        .map(|s| s.trim().to_string())
        .or_else(|| {
            req.extensions()
                .get::<ConnectInfo<SocketAddr>>()
                .map(|ci| ci.0.to_string())
        })
        .unwrap_or_else(|| "-".to_string());

    tracing::debug!(method = %method, path = %path, addr = %addr, "→ API");
    let t0 = std::time::Instant::now();

    let response = next.run(req).await;

    let status = response.status().as_u16();
    let ms = t0.elapsed().as_secs_f64() * 1000.0;
    tracing::debug!(method = %method, path = %path, addr = %addr, status, "← API {:.1}ms", ms);

    response
}

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
    // Empty token = no authentication required (e.g. public demo instances).
    if state.config.server.token.is_empty() {
        return Ok(());
    }
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

/// Validate a `link_code` as an alternative credential for read-only file access.
///
/// Checks that the code exists in links.db, is not expired, and the
/// `source` + reconstructed composite path match the stored row.
/// Returns `Ok(())` on success; an appropriate `StatusCode` on failure.
/// Intended to be called from inside a `run_blocking` closure or from
/// `tokio::task::spawn_blocking`.
pub(super) fn check_link_code_auth(
    data_dir: &std::path::Path,
    code: &str,
    source: &str,
    // `full_path`: composite path as it appears in `params.path` (may contain `::`).
    full_path: &str,
) -> Result<(), StatusCode> {
    use crate::db::links::{resolve_link, ResolveResult};
    let db_path = data_dir.join("links.db");
    if !db_path.exists() {
        return Err(StatusCode::NOT_FOUND);
    }
    let conn = rusqlite::Connection::open(&db_path).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    match resolve_link(&conn, code) {
        Ok(ResolveResult::Found(row)) => {
            let link_path = match &row.archive_path {
                Some(ap) => format!("{}::{}", row.path, ap),
                None => row.path.clone(),
            };
            if row.source == source && link_path == full_path {
                Ok(())
            } else {
                Err(StatusCode::FORBIDDEN)
            }
        }
        Ok(ResolveResult::Expired) => Err(StatusCode::GONE),
        Ok(ResolveResult::NotFound) => Err(StatusCode::NOT_FOUND),
        Err(_) => Err(StatusCode::INTERNAL_SERVER_ERROR),
    }
}

pub(super) fn source_db_path(state: &AppState, source: &str) -> Result<std::path::PathBuf, StatusCode> {
    if !source.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') {
        return Err(StatusCode::BAD_REQUEST);
    }
    Ok(state.data_dir.join("sources").join(format!("{}.db", source)))
}

/// Validate a relative path and resolve it to a canonical filesystem path
/// within the source's configured root.
///
/// Returns `(canonical_root, canonical_full)` on success, or an appropriate
/// `StatusCode` error response on failure (bad path, source not configured,
/// file not found, path traversal).
pub(super) fn resolve_source_path(
    state: &AppState,
    source: &str,
    path: &str,
) -> Result<(std::path::PathBuf, std::path::PathBuf), StatusCode> {
    // Reject paths that start with '/' or contain '..' components.
    if path.starts_with('/') || path.starts_with('\\') {
        return Err(StatusCode::BAD_REQUEST);
    }
    for component in std::path::Path::new(path).components() {
        if matches!(
            component,
            std::path::Component::ParentDir
                | std::path::Component::RootDir
                | std::path::Component::Prefix(_)
        ) {
            return Err(StatusCode::BAD_REQUEST);
        }
    }
    let source_root_str = state
        .config
        .sources
        .get(source)
        .and_then(|sc| sc.path.as_deref())
        .ok_or(StatusCode::NOT_FOUND)?;
    let source_root = std::path::Path::new(source_root_str);
    let canonical_root = source_root.canonicalize().map_err(|_| StatusCode::NOT_FOUND)?;
    let canonical_full = source_root.join(path).canonicalize().map_err(|_| StatusCode::NOT_FOUND)?;
    if !canonical_full.starts_with(&canonical_root) {
        return Err(StatusCode::BAD_REQUEST);
    }
    Ok((canonical_root, canonical_full))
}

/// Convert a `Vec<ContextLine>` into `(start, match_index, Vec<ContextLine>)`.
pub(super) fn compact_lines(
    lines: Vec<find_common::api::ContextLine>,
    center: usize,
) -> (usize, Option<usize>, Vec<find_common::api::ContextLine>) {
    let start = lines.first().map_or(0, |l| l.line_number);
    let match_index = lines.iter().position(|l| l.line_number == center);
    (start, match_index, lines)
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

    let content_file_count = {
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
        "content_file_count":    content_file_count,
    }))
    .into_response()
}
