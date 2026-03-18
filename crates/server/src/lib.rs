pub(crate) mod archive;
pub(crate) mod compaction;
pub(crate) mod db;
pub(crate) mod fuzzy;
pub(crate) mod normalize;
pub(crate) mod routes;
pub(crate) mod stats_cache;
pub(crate) mod upload;
pub(crate) mod worker;

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use anyhow::{Context, Result};
use axum::{
    extract::{DefaultBodyLimit, State},
    http::{header, StatusCode},
    response::IntoResponse,
    routing::{delete, get, head, patch, post},
    Router,
};
use tower_http::trace::TraceLayer;

use find_common::api::{RecentFile, WorkerStatus};
use find_common::config::ServerAppConfig;
use archive::SharedArchiveState;

// ── Embedded web UI ────────────────────────────────────────────────────────────

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

// ── Shared state ───────────────────────────────────────────────────────────────

pub struct CachedUpdateCheck {
    pub checked_at: std::time::Instant,
    pub latest_version: String,
    pub asset_url: Option<String>,
}

pub struct AppState {
    pub config: ServerAppConfig,
    pub data_dir: PathBuf,
    pub worker_status: Arc<std::sync::Mutex<WorkerStatus>>,
    pub archive_state: Arc<SharedArchiveState>,
    pub inbox_paused: Arc<AtomicBool>,
    pub compaction_stats: Arc<std::sync::RwLock<Option<compaction::CompactionStats>>>,
    pub source_stats_cache: Arc<std::sync::RwLock<stats_cache::SourceStatsCache>>,
    pub under_systemd: bool,
    pub update_cache: tokio::sync::RwLock<Option<CachedUpdateCheck>>,
    pub recent_tx: tokio::sync::broadcast::Sender<RecentFile>,
    /// Watch channel incremented on every stats cache update; SSE subscribers react to changes.
    pub stats_watch: Arc<tokio::sync::watch::Sender<u64>>,
    /// In-memory rate limiter for `GET /api/v1/links/:code`: maps IP → (count, window_start).
    pub link_rate_limiter: std::sync::Mutex<std::collections::HashMap<std::net::IpAddr, (u32, std::time::Instant)>>,
}

// ── Server initialisation ──────────────────────────────────────────────────────

/// Build `AppState`, create data directories, check source schemas, and spawn
/// all background workers (inbox, upload cleanup, compaction scanner).
pub async fn create_app_state(config: ServerAppConfig) -> Result<Arc<AppState>> {
    let data_dir = PathBuf::from(&config.server.data_dir);

    std::fs::create_dir_all(data_dir.join("sources"))
        .context("creating sources directory")?;
    std::fs::create_dir_all(data_dir.join("inbox").join("failed"))
        .context("creating inbox directory")?;

    db::check_all_sources(&data_dir.join("sources"))
        .context("schema version check failed — delete the listed database(s) and re-run `find-scan`")?;

    let under_systemd = std::env::var("INVOCATION_ID").is_ok();
    let worker_status = Arc::new(std::sync::Mutex::new(WorkerStatus::Idle));
    let inbox_paused = Arc::new(AtomicBool::new(false));
    let archive_state = SharedArchiveState::new(data_dir.clone())
        .context("initialising archive state")?;
    let initial_compaction_stats = compaction::load_cached_stats(&data_dir);
    let compaction_stats = Arc::new(std::sync::RwLock::new(initial_compaction_stats));
    let source_stats_cache = Arc::new(std::sync::RwLock::new(stats_cache::SourceStatsCache::default()));
    let (recent_tx, _) = tokio::sync::broadcast::channel::<RecentFile>(256);
    let (stats_watch_tx, _stats_watch_rx) = tokio::sync::watch::channel(0u64);
    let stats_watch = Arc::new(stats_watch_tx);

    // Open links.db (creates table on first use).
    if let Err(e) = db::links::open_links_db(&data_dir) {
        tracing::warn!("Failed to open links.db (share links will be unavailable): {e:#}");
    }

    let state = Arc::new(AppState {
        config,
        data_dir: data_dir.clone(),
        worker_status: Arc::clone(&worker_status),
        archive_state: Arc::clone(&archive_state),
        inbox_paused: Arc::clone(&inbox_paused),
        compaction_stats: Arc::clone(&compaction_stats),
        source_stats_cache: Arc::clone(&source_stats_cache),
        under_systemd,
        update_cache: tokio::sync::RwLock::new(None),
        recent_tx,
        stats_watch: Arc::clone(&stats_watch),
        link_rate_limiter: std::sync::Mutex::new(std::collections::HashMap::new()),
    });

    if let Err(e) = worker::recover_stranded_requests(&data_dir).await {
        tracing::error!("Failed to recover stranded requests: {e}");
    }

    let worker_cfg = worker::WorkerConfig {
        request_timeout: std::time::Duration::from_secs(
            state.config.server.inbox_request_timeout_secs,
        ),
        inline_threshold_bytes: state.config.server.inline_threshold_bytes,
        archive_batch_size: state.config.server.archive_batch_size,
        activity_log_max_entries: state.config.server.activity_log_max_entries,
        normalization: state.config.normalization.clone(),
    };
    let worker_handles = worker::WorkerHandles {
        status: worker_status,
        archive_state,
        inbox_paused,
        recent_tx: state.recent_tx.clone(),
        source_stats_cache: Arc::clone(&source_stats_cache),
        stats_watch: Arc::clone(&stats_watch),
    };
    let worker_data_dir = data_dir.clone();
    tokio::spawn(async move {
        if let Err(e) = worker::start_inbox_worker(worker_data_dir, worker_cfg, worker_handles).await {
            tracing::error!("Inbox worker failed: {e}");
        }
    });

    let cleanup_data_dir = data_dir.clone();
    tokio::spawn(async move {
        upload::start_cleanup_task(cleanup_data_dir).await;
    });

    compaction::start_compaction_scanner(
        data_dir.clone(),
        compaction_stats,
        Arc::clone(&state.archive_state),
        state.config.compaction.clone(),
        Arc::clone(&source_stats_cache),
        Arc::clone(&stats_watch),
    );

    // Startup full rebuild of source stats cache (delayed 30 s to let the inbox
    // worker settle before running expensive DB queries).
    {
        let cache = Arc::clone(&source_stats_cache);
        let dd = data_dir.clone();
        let sw = Arc::clone(&stats_watch);
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_secs(30)).await;
            tokio::task::spawn_blocking(move || {
                stats_cache::full_rebuild(&dd, &cache);
            }).await.ok();
            sw.send_modify(|v| *v = v.wrapping_add(1));
        });
    }

    // Hourly task to remove expired share links from links.db.
    let sweep_data_dir = data_dir.clone();
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(3600));
        loop {
            interval.tick().await;
            let dir = sweep_data_dir.clone();
            if let Err(e) = tokio::task::spawn_blocking(move || {
                let conn = db::links::open_links_db(&dir)?;
                let n = db::links::sweep_expired(&conn)?;
                if n > 0 { tracing::info!("Swept {n} expired share links"); }
                Ok::<_, anyhow::Error>(())
            }).await {
                tracing::warn!("Link expiry sweep failed: {e:#}");
            }
        }
    });

    Ok(state)
}

/// Build the Axum router from the given shared state.
pub fn build_router(state: Arc<AppState>) -> Router {
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
        .route("/api/v1/stats/stream",   get(routes::stream_stats))
        .route("/api/v1/errors",         get(routes::get_errors))
        .route("/api/v1/recent",         get(routes::get_recent))
        .route("/api/v1/recent/stream",  get(routes::stream_recent))
        .route("/api/v1/tree",           get(routes::list_dir))
        .route("/api/v1/raw",            get(routes::get_raw))
        .route("/api/v1/links",          post(routes::post_link))
        .route("/api/v1/links/{code}",   get(routes::get_link))
        .route("/api/v1/auth/session",   post(routes::create_session).delete(routes::delete_session))
        .route("/api/v1/admin/compact",        post(routes::compact))
        .route("/api/v1/admin/source",         delete(routes::delete_source))
        .route("/api/v1/admin/inbox",          get(routes::inbox_status).delete(routes::inbox_clear))
        .route("/api/v1/admin/inbox/retry",    post(routes::inbox_retry))
        .route("/api/v1/admin/inbox/pause",    post(routes::inbox_pause))
        .route("/api/v1/admin/inbox/resume",   post(routes::inbox_resume))
        .route("/api/v1/admin/inbox/show",     get(routes::inbox_show))
        .route("/api/v1/admin/update/check",   get(routes::update_check))
        .route("/api/v1/admin/update/apply",   post(routes::update_apply))
        .fallback(serve_static)
        .layer(DefaultBodyLimit::max(32 * 1024 * 1024))
        .with_state(Arc::clone(&state));

    upload_routes.merge(app)
        .layer(TraceLayer::new_for_http())
}
