mod archive;
mod db;
mod fuzzy;
mod routes;
mod upload;
mod worker;

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
use axum::{
    extract::{DefaultBodyLimit, State},
    http::{header, StatusCode},
    response::IntoResponse,
    routing::{delete, get, head, patch, post},
    Router,
};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt, Layer};

use clap::Parser;

use find_common::api::WorkerStatus;
use find_common::config::{default_server_config_path, parse_server_config, ServerAppConfig};
use find_common::logging::LogIgnoreFilter;

#[derive(Parser)]
#[command(name = "find-server", about = "find-anything index server", version)]
struct Args {
    /// Path to server config file.
    /// Defaults to $XDG_CONFIG_HOME/find-anything/server.toml,
    /// or /etc/find-anything/server.toml when running as root.
    #[arg(long, env = "FIND_ANYTHING_SERVER_CONFIG")]
    config: Option<String>,
}

// ── Embedded web UI ────────────────────────────────────────────────────────────
// In release builds, all files under web/build/ are compiled into the binary.
// In debug builds (no `debug-embed` feature), they are read from disk at runtime.

#[derive(rust_embed::RustEmbed)]
#[folder = "../../web/build/"]
struct WebAssets;

async fn serve_static(
    State(state): State<Arc<AppState>>,
    uri: axum::http::Uri,
) -> impl IntoResponse {
    let path = uri.path().trim_start_matches('/');
    let path = if path.is_empty() { "index.html" } else { path };

    match WebAssets::get(path) {
        Some(content) => {
            if path == "index.html" {
                return serve_index_html(&state, content.data.as_ref()).into_response();
            }
            let mime = mime_guess::from_path(path).first_or_octet_stream();
            ([(header::CONTENT_TYPE, mime.essence_str())], content.data).into_response()
        }
        None => {
            // SPA fallback — serve index.html for any unknown path so the
            // SvelteKit client-side router can handle it.
            match WebAssets::get("index.html") {
                Some(content) => serve_index_html(&state, content.data.as_ref()).into_response(),
                None => StatusCode::NOT_FOUND.into_response(),
            }
        }
    }
}

fn serve_index_html(state: &AppState, html: &[u8]) -> impl IntoResponse {
    let config_json = serde_json::json!({
        "download_zip_member_levels": state.config.server.download_zip_member_levels,
    });
    let script = format!("<script>window.find_anything_config={config_json};</script>");
    let html_str = String::from_utf8_lossy(html);
    let injected = html_str.replacen("</head>", &format!("{script}</head>"), 1);
    ([(header::CONTENT_TYPE, "text/html")], injected).into_response()
}

pub struct CachedUpdateCheck {
    pub checked_at: std::time::Instant,
    pub latest_version: String,
    /// Download URL for the matching platform asset; None if no asset found.
    pub asset_url: Option<String>,
}

pub struct AppState {
    pub config: ServerAppConfig,
    pub data_dir: PathBuf,
    /// Shared worker status: idle or processing a specific file.
    /// Updated by the inbox worker; read by the stats route.
    pub worker_status: Arc<std::sync::Mutex<WorkerStatus>>,
    /// True when running under systemd (INVOCATION_ID is set).
    pub under_systemd: bool,
    /// Cached result of the last GitHub update check (refreshed at most once per hour).
    pub update_cache: tokio::sync::RwLock<Option<CachedUpdateCheck>>,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::registry()
        .with(tracing_subscriber::EnvFilter::try_from_default_env()
            .unwrap_or_else(|_| "warn,find_server=info,tower_http=info".into()))
        .with(tracing_subscriber::fmt::layer().with_filter(LogIgnoreFilter))
        .init();

    let args = Args::parse();
    let config_path = args.config.unwrap_or_else(default_server_config_path);

    let config_str = std::fs::read_to_string(&config_path)
        .with_context(|| format!("reading config: {config_path}"))?;
    let config = parse_server_config(&config_str)?;

    if let Err(e) = find_common::logging::set_ignore_patterns(&config.log.ignore) {
        tracing::warn!("invalid log ignore pattern: {e}");
    }

    let data_dir = PathBuf::from(&config.server.data_dir);
    std::fs::create_dir_all(data_dir.join("sources"))
        .context("creating sources directory")?;
    std::fs::create_dir_all(data_dir.join("inbox").join("failed"))
        .context("creating inbox directory")?;

    // Fail fast if any existing source DB has an incompatible schema.
    db::check_all_sources(&data_dir.join("sources"))
        .context("schema version check failed — delete the listed database(s) and re-run `find-scan`")?;

    let bind = config.server.bind.clone();
    let under_systemd = std::env::var("INVOCATION_ID").is_ok();
    let worker_status = Arc::new(std::sync::Mutex::new(WorkerStatus::Idle));
    let state = Arc::new(AppState {
        config,
        data_dir: data_dir.clone(),
        worker_status: Arc::clone(&worker_status),
        under_systemd,
        update_cache: tokio::sync::RwLock::new(None),
    });

    // Spawn the async inbox worker, sharing the status handle.
    let worker_data_dir = data_dir.clone();
    let log_batch_detail_limit = state.config.server.log_batch_detail_limit;
    tokio::spawn(async move {
        if let Err(e) = worker::start_inbox_worker(worker_data_dir, worker_status, log_batch_detail_limit).await {
            tracing::error!("Inbox worker failed: {e}");
        }
    });

    // Spawn the upload cleanup task.
    let cleanup_data_dir = data_dir.clone();
    tokio::spawn(async move {
        upload::start_cleanup_task(cleanup_data_dir).await;
    });

    // Upload routes are mounted WITHOUT the DefaultBodyLimit so that large files
    // can be uploaded in chunks.  All other routes keep the 32 MB limit.
    let upload_routes = Router::new()
        .route("/api/v1/upload",        post(routes::upload_init))
        .route("/api/v1/upload/{id}",   patch(routes::upload_patch))
        .route("/api/v1/upload/{id}",   head(routes::upload_status))
        .with_state(Arc::clone(&state));

    let app = Router::new()
        .route("/api/v1/sources",        get(routes::list_sources))
        .route("/api/v1/file",           get(routes::get_file))
        .route("/api/v1/files",          get(routes::list_files))
        .route("/api/v1/bulk",           post(routes::bulk))
        .route("/api/v1/search",         get(routes::search))
        .route("/api/v1/context",        get(routes::get_context))
        .route("/api/v1/context-batch",  post(routes::context_batch))
        .route("/api/v1/settings",       get(routes::get_settings))
        .route("/api/v1/metrics",        get(routes::get_metrics))
        .route("/api/v1/stats",          get(routes::get_stats))
        .route("/api/v1/errors",         get(routes::get_errors))
        .route("/api/v1/recent",         get(routes::get_recent))
        .route("/api/v1/tree",           get(routes::list_dir))
        .route("/api/v1/raw",            get(routes::get_raw))
        .route("/api/v1/auth/session",   post(routes::create_session).delete(routes::delete_session))
        .route("/api/v1/admin/source",         delete(routes::delete_source))
        .route("/api/v1/admin/inbox",          get(routes::inbox_status).delete(routes::inbox_clear))
        .route("/api/v1/admin/inbox/retry",    post(routes::inbox_retry))
        .route("/api/v1/admin/inbox/show",     get(routes::inbox_show))
        .route("/api/v1/admin/update/check",   get(routes::update_check))
        .route("/api/v1/admin/update/apply",   post(routes::update_apply))
        .fallback(serve_static)
        .layer(DefaultBodyLimit::max(32 * 1024 * 1024))
        .with_state(Arc::clone(&state));

    // Merge upload routes (no body limit) with the main router.
    let app = upload_routes.merge(app);

    let listener = tokio::net::TcpListener::bind(&bind)
        .await
        .with_context(|| format!("binding to {bind}"))?;

    tracing::info!("listening on {bind}");
    axum::serve(listener, app).await.context("server error")?;

    Ok(())
}
