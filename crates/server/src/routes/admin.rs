use std::sync::Arc;
use std::time::{Duration, SystemTime};

use axum::{
    extract::{Query, State},
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    Json,
};
use anyhow::Context;
use flate2::read::GzDecoder;
use serde::Deserialize;

use std::sync::atomic::Ordering;

use find_common::api::{
    InboxDeleteResponse, InboxItem, InboxPauseResponse, InboxResumeResponse, InboxRetryResponse,
    InboxShowFile, InboxShowResponse, InboxStatusResponse, SourceDeleteResponse,
    UpdateApplyResponse, UpdateCheckResponse,
};

use crate::archive::ArchiveManager;
use crate::{AppState, CachedUpdateCheck};
use crate::db;

use super::{check_auth, run_blocking, source_db_path};

const GITHUB_REPO: &str = "jamietre/find-anything";
const UPDATE_CACHE_TTL: Duration = Duration::from_secs(3600);

// ── GET /api/v1/admin/inbox ───────────────────────────────────────────────────

pub async fn inbox_status(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> impl IntoResponse {
    if let Err(s) = check_auth(&state, &headers) {
        return (s, Json(serde_json::Value::Null)).into_response();
    }

    let inbox_dir = state.data_dir.join("inbox");
    let failed_dir = inbox_dir.join("failed");
    let to_archive_dir = inbox_dir.join("to-archive");

    run_blocking("inbox_status", move || -> anyhow::Result<_> {
        let now = SystemTime::now();

        let read_items = |dir: &std::path::Path| -> Vec<InboxItem> {
            let rd = match std::fs::read_dir(dir) {
                Ok(rd) => rd,
                Err(_) => return vec![],
            };
            let mut items = Vec::new();
            for entry in rd.filter_map(|e| e.ok()) {
                let path = entry.path();
                if path.extension().map(|x| x == "gz").unwrap_or(false) {
                    let filename = entry.file_name().to_string_lossy().into_owned();
                    let meta = match entry.metadata() {
                        Ok(m) => m,
                        Err(_) => continue,
                    };
                    let size_bytes = meta.len();
                    let age_secs = meta
                        .modified()
                        .ok()
                        .and_then(|m| now.duration_since(m).ok())
                        .map(|d| d.as_secs())
                        .unwrap_or(0);
                    items.push(InboxItem { filename, size_bytes, age_secs });
                }
            }
            items.sort_by(|a, b| a.filename.cmp(&b.filename));
            items
        };

        let count_gz = |dir: &std::path::Path| -> usize {
            std::fs::read_dir(dir)
                .map(|rd| {
                    rd.filter_map(|e| e.ok())
                        .filter(|e| e.path().extension().map(|x| x == "gz").unwrap_or(false))
                        .count()
                })
                .unwrap_or(0)
        };

        let paused = state.inbox_paused.load(Ordering::Relaxed);
        let pending = read_items(&inbox_dir);
        let failed = read_items(&failed_dir);
        let archive_queue = count_gz(&to_archive_dir);
        Ok(Json(InboxStatusResponse { pending, failed, paused, archive_queue }))
    }).await
}

// ── DELETE /api/v1/admin/inbox ────────────────────────────────────────────────

#[derive(Deserialize)]
pub struct InboxDeleteQuery {
    #[serde(default = "default_target")]
    target: String,
}

fn default_target() -> String {
    "pending".to_string()
}

pub async fn inbox_clear(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(query): Query<InboxDeleteQuery>,
) -> impl IntoResponse {
    if let Err(s) = check_auth(&state, &headers) {
        return (s, Json(serde_json::Value::Null)).into_response();
    }

    let inbox_dir = state.data_dir.join("inbox");
    let failed_dir = inbox_dir.join("failed");
    let target = query.target.clone();

    run_blocking("inbox_clear", move || -> anyhow::Result<_> {
        let delete_gz_in = |dir: &std::path::Path| -> usize {
            let rd = match std::fs::read_dir(dir) {
                Ok(rd) => rd,
                Err(_) => return 0,
            };
            let mut count = 0;
            for entry in rd.filter_map(|e| e.ok()) {
                let path = entry.path();
                if path.extension().map(|x| x == "gz").unwrap_or(false)
                    && std::fs::remove_file(&path).is_ok()
                {
                    count += 1;
                }
            }
            count
        };

        let deleted = match target.as_str() {
            "failed" => delete_gz_in(&failed_dir),
            "all" => delete_gz_in(&inbox_dir) + delete_gz_in(&failed_dir),
            _ => delete_gz_in(&inbox_dir), // "pending" or anything else
        };
        Ok(Json(InboxDeleteResponse { deleted }))
    }).await
}

// ── POST /api/v1/admin/inbox/retry ────────────────────────────────────────────

pub async fn inbox_retry(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> impl IntoResponse {
    if let Err(s) = check_auth(&state, &headers) {
        return (s, Json(serde_json::Value::Null)).into_response();
    }

    let inbox_dir = state.data_dir.join("inbox");
    let failed_dir = inbox_dir.join("failed");

    run_blocking("inbox_retry", move || -> anyhow::Result<_> {
        let rd = match std::fs::read_dir(&failed_dir) {
            Ok(rd) => rd,
            Err(_) => return Ok(Json(InboxRetryResponse { retried: 0 })),
        };
        let mut count = 0;
        for entry in rd.filter_map(|e| e.ok()) {
            let path = entry.path();
            if path.extension().map(|x| x == "gz").unwrap_or(false) {
                let dest = inbox_dir.join(entry.file_name());
                if std::fs::rename(&path, &dest).is_ok() {
                    count += 1;
                }
            }
        }
        Ok(Json(InboxRetryResponse { retried: count }))
    }).await
}

// ── POST /api/v1/admin/inbox/pause ───────────────────────────────────────────

pub async fn inbox_pause(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> impl IntoResponse {
    if let Err(s) = check_auth(&state, &headers) {
        return (s, Json(serde_json::Value::Null)).into_response();
    }

    state.inbox_paused.store(true, Ordering::Relaxed);

    let processing_dir = state.data_dir.join("inbox").join("processing");
    let inbox_dir = state.data_dir.join("inbox");

    run_blocking("inbox_pause", move || -> anyhow::Result<_> {
        let rd = match std::fs::read_dir(&processing_dir) {
            Ok(rd) => rd,
            Err(_) => return Ok(Json(InboxPauseResponse { returned: 0 })),
        };
        let mut returned = 0;
        for entry in rd.filter_map(|e| e.ok()) {
            let path = entry.path();
            if path.extension().map(|x| x == "gz").unwrap_or(false) {
                let dest = inbox_dir.join(entry.file_name());
                if std::fs::rename(&path, &dest).is_ok() {
                    returned += 1;
                    tracing::info!("Returned in-flight request to inbox: {}", dest.display());
                }
            }
        }
        tracing::info!("Inbox processing paused ({returned} in-flight job(s) returned to inbox)");
        Ok(Json(InboxPauseResponse { returned }))
    }).await
}

// ── POST /api/v1/admin/inbox/resume ──────────────────────────────────────────

pub async fn inbox_resume(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> impl IntoResponse {
    if let Err(s) = check_auth(&state, &headers) {
        return (s, Json(serde_json::Value::Null)).into_response();
    }

    state.inbox_paused.store(false, Ordering::Relaxed);
    tracing::info!("Inbox processing resumed");

    (StatusCode::OK, Json(InboxResumeResponse {})).into_response()
}

// ── GET /api/v1/admin/inbox/show ──────────────────────────────────────────────

#[derive(Deserialize)]
pub struct InboxShowQuery {
    name: String,
}

pub async fn inbox_show(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(query): Query<InboxShowQuery>,
) -> impl IntoResponse {
    if let Err(s) = check_auth(&state, &headers) {
        return (s, Json(serde_json::Value::Null)).into_response();
    }

    let inbox_dir = state.data_dir.join("inbox");
    let failed_dir = inbox_dir.join("failed");

    run_blocking("inbox_show", move || -> anyhow::Result<_> {
        let filename = if query.name.ends_with(".gz") {
            query.name.clone()
        } else {
            format!("{}.gz", query.name)
        };

        let (path, queue) = if inbox_dir.join(&filename).exists() {
            (inbox_dir.join(&filename), "pending")
        } else if failed_dir.join(&filename).exists() {
            (failed_dir.join(&filename), "failed")
        } else {
            return Ok(StatusCode::NOT_FOUND.into_response());
        };

        let raw = std::fs::read(&path)?;
        let req: find_common::api::BulkRequest =
            serde_json::from_reader(GzDecoder::new(raw.as_slice()))?;

        let files = req
            .files
            .iter()
            .map(|f| InboxShowFile {
                path: f.path.clone(),
                kind: f.kind.clone(),
                content_lines: f.lines.iter().filter(|l| l.line_number != 0).count(),
            })
            .collect();

        Ok(Json(InboxShowResponse {
            queue: queue.to_string(),
            source: req.source,
            files,
            delete_paths: req.delete_paths,
            failures: req.indexing_failures,
            scan_timestamp: req.scan_timestamp,
        }).into_response())
    }).await
}

// ── GET /api/v1/admin/update/check ────────────────────────────────────────────

/// Map the current binary's arch+OS to the asset name suffix used in releases.
fn platform_suffix() -> Option<&'static str> {
    match (std::env::consts::ARCH, std::env::consts::OS) {
        ("x86_64", "linux") => Some("x86_64-linux"),
        ("arm",    "linux") => Some("armv7-linux"),
        ("aarch64","linux") => Some("aarch64-linux"),
        _ => None,
    }
}

/// Fetch (or return cached) the latest GitHub release info.
/// Returns `(latest_version, asset_url)` where `asset_url` is None if no
/// matching asset was found for this platform.
async fn fetch_latest(state: &AppState) -> anyhow::Result<(String, Option<String>)> {
    // Fast path: return cached value if still fresh.
    {
        let cache = state.update_cache.read().await;
        if let Some(c) = cache.as_ref() {
            if c.checked_at.elapsed() < UPDATE_CACHE_TTL {
                return Ok((c.latest_version.clone(), c.asset_url.clone()));
            }
        }
    }

    // Slow path: call GitHub API.
    let url = format!("https://api.github.com/repos/{GITHUB_REPO}/releases/latest");
    let body: serde_json::Value = reqwest::Client::builder()
        .user_agent(concat!("find-anything/", env!("CARGO_PKG_VERSION")))
        .build()?
        .get(&url)
        .send()
        .await
        .context("GitHub API request")?
        .error_for_status()
        .context("GitHub API error")?
        .json()
        .await
        .context("parsing GitHub API response")?;

    let latest_version = body["tag_name"]
        .as_str()
        .unwrap_or("")
        .trim_start_matches('v')
        .to_string();

    let suffix = platform_suffix();
    let asset_url = suffix.and_then(|sfx| {
        body["assets"]
            .as_array()?
            .iter()
            .find(|a| a["name"].as_str().map(|n| n.contains(sfx)).unwrap_or(false))
            .and_then(|a| a["browser_download_url"].as_str())
            .map(str::to_string)
    });

    // Update cache.
    *state.update_cache.write().await = Some(CachedUpdateCheck {
        checked_at: std::time::Instant::now(),
        latest_version: latest_version.clone(),
        asset_url: asset_url.clone(),
    });

    Ok((latest_version, asset_url))
}

fn version_gt(a: &str, b: &str) -> bool {
    fn parse(v: &str) -> Option<(u64, u64, u64)> {
        let mut p = v.split('.');
        Some((p.next()?.parse().ok()?, p.next()?.parse().ok()?, p.next()?.parse().ok()?))
    }
    match (parse(a), parse(b)) {
        (Some(a), Some(b)) => a > b,
        _ => false,
    }
}

pub async fn update_check(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> impl IntoResponse {
    if let Err(s) = check_auth(&state, &headers) {
        return (s, Json(serde_json::Value::Null)).into_response();
    }

    let current = env!("CARGO_PKG_VERSION").to_string();

    let (restart_supported, restart_unsupported_reason) = if state.under_systemd {
        (true, None)
    } else {
        (false, Some("Server is not running under systemd".to_string()))
    };

    match fetch_latest(&state).await {
        Ok((latest, asset_url)) => {
            let update_available = version_gt(&latest, &current) && asset_url.is_some();
            let (restart_supported, restart_unsupported_reason) =
                if !restart_supported {
                    (false, restart_unsupported_reason)
                } else if asset_url.is_none() {
                    (false, Some(format!(
                        "No release asset found for this platform ({})",
                        platform_suffix().unwrap_or("unknown")
                    )))
                } else {
                    (true, None)
                };
            Json(UpdateCheckResponse {
                current,
                latest,
                update_available,
                restart_supported,
                restart_unsupported_reason,
            }).into_response()
        }
        Err(e) => {
            tracing::warn!("update check failed: {e:#}");
            (StatusCode::BAD_GATEWAY, Json(serde_json::json!({
                "error": format!("Could not reach GitHub: {e:#}")
            }))).into_response()
        }
    }
}

// ── POST /api/v1/admin/update/apply ───────────────────────────────────────────

pub async fn update_apply(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> impl IntoResponse {
    if let Err(s) = check_auth(&state, &headers) {
        return (s, Json(serde_json::Value::Null)).into_response();
    }

    if !state.under_systemd {
        return (StatusCode::BAD_REQUEST, Json(UpdateApplyResponse {
            ok: false,
            message: "Self-update requires systemd".to_string(),
        })).into_response();
    }

    let current = env!("CARGO_PKG_VERSION");

    let (latest, asset_url) = match fetch_latest(&state).await {
        Ok(v) => v,
        Err(e) => return (StatusCode::BAD_GATEWAY, Json(UpdateApplyResponse {
            ok: false,
            message: format!("Could not reach GitHub: {e:#}"),
        })).into_response(),
    };

    if !version_gt(&latest, current) {
        return (StatusCode::BAD_REQUEST, Json(UpdateApplyResponse {
            ok: false,
            message: format!("Already on the latest version ({current})"),
        })).into_response();
    }

    let asset_url = match asset_url {
        Some(u) => u,
        None => return (StatusCode::BAD_REQUEST, Json(UpdateApplyResponse {
            ok: false,
            message: format!(
                "No release asset for this platform ({})",
                platform_suffix().unwrap_or("unknown")
            ),
        })).into_response(),
    };

    // Resolve current exe path (follow symlinks).
    let exe = match std::env::current_exe()
        .context("resolving current exe")
        .and_then(|p| std::fs::canonicalize(&p).context("canonicalizing exe path"))
    {
        Ok(p) => p,
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, Json(UpdateApplyResponse {
            ok: false,
            message: format!("Could not resolve executable path: {e:#}"),
        })).into_response(),
    };

    let exe_dir = match exe.parent() {
        Some(d) => d.to_path_buf(),
        None => return (StatusCode::INTERNAL_SERVER_ERROR, Json(UpdateApplyResponse {
            ok: false,
            message: "Could not determine executable directory".to_string(),
        })).into_response(),
    };

    let tmp_path = exe_dir.join("find-server.new");

    // Download the new binary.
    let download_result: anyhow::Result<()> = async {
        let bytes = reqwest::Client::builder()
            .user_agent(concat!("find-anything/", env!("CARGO_PKG_VERSION")))
            .build()?
            .get(&asset_url)
            .send()
            .await
            .context("downloading update")?
            .error_for_status()
            .context("download HTTP error")?
            .bytes()
            .await
            .context("reading download body")?;

        std::fs::write(&tmp_path, &bytes)
            .context("writing temporary binary")?;

        // chmod +x
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = std::fs::metadata(&tmp_path)
                .context("reading temp file metadata")?
                .permissions();
            perms.set_mode(0o755);
            std::fs::set_permissions(&tmp_path, perms)
                .context("setting executable bit")?;
        }

        // Atomic rename over the current binary.
        std::fs::rename(&tmp_path, &exe)
            .context("replacing binary")?;

        Ok(())
    }.await;

    if let Err(e) = download_result {
        // Clean up temp file on failure.
        let _ = std::fs::remove_file(&tmp_path);
        return (StatusCode::INTERNAL_SERVER_ERROR, Json(UpdateApplyResponse {
            ok: false,
            message: format!("Update failed: {e:#}"),
        })).into_response();
    }

    // Schedule clean exit so systemd restarts onto the new binary.
    tokio::spawn(async {
        tokio::time::sleep(Duration::from_millis(300)).await;
        std::process::exit(0);
    });

    tracing::info!("Update to v{latest} applied — restarting");

    (StatusCode::ACCEPTED, Json(UpdateApplyResponse {
        ok: true,
        message: format!("Update to v{latest} applied. Restarting…"),
    })).into_response()
}

// ── POST /api/v1/admin/compact ────────────────────────────────────────────────

#[derive(Deserialize)]
pub struct CompactQuery {
    #[serde(default)]
    dry_run: bool,
}

pub async fn compact(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(query): Query<CompactQuery>,
) -> impl IntoResponse {
    if let Err(s) = check_auth(&state, &headers) {
        return (s, Json(serde_json::Value::Null)).into_response();
    }

    let data_dir = state.data_dir.clone();
    let shared   = Arc::clone(&state.archive_state);
    let dry_run  = query.dry_run;

    run_blocking("compact", move || -> anyhow::Result<_> {
        let resp = crate::compaction::compact_archives(&data_dir, &shared, dry_run)?;
        if dry_run {
            tracing::info!(
                "compact (dry-run): {} archives, {} orphaned chunks, {} bytes would be freed",
                resp.archives_scanned, resp.chunks_removed, resp.bytes_freed,
            );
        } else {
            tracing::info!(
                "compact: rewrote {}/{} archives, removed {} chunks, freed {} bytes",
                resp.archives_rewritten, resp.archives_scanned,
                resp.chunks_removed, resp.bytes_freed,
            );
        }
        Ok(Json(resp))
    }).await
}

// ── DELETE /api/v1/admin/source ───────────────────────────────────────────────

#[derive(Deserialize)]
pub struct DeleteSourceQuery {
    source: String,
}

pub async fn delete_source(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(query): Query<DeleteSourceQuery>,
) -> impl IntoResponse {
    if let Err(s) = check_auth(&state, &headers) {
        return (s, Json(serde_json::Value::Null)).into_response();
    }

    let db_path = match source_db_path(&state, &query.source) {
        Ok(p) => p,
        Err(s) => return (s, Json(serde_json::Value::Null)).into_response(),
    };

    if !db_path.exists() {
        return (StatusCode::NOT_FOUND, Json(serde_json::json!({ "error": "source not found" }))).into_response();
    }

    let archive_state = Arc::clone(&state.archive_state);

    run_blocking("delete_source", move || -> anyhow::Result<_> {
        let conn = db::open(&db_path)?;

        let files_deleted = db::count_files(&conn)?;
        let chunk_refs = db::collect_all_chunk_refs(&conn)?;
        let chunks_removed = chunk_refs.len();

        // Close the DB before deleting it.
        drop(conn);

        // Use the shared archive state so that rewrite locks are coordinated
        // with any concurrent inbox workers.
        let archive_mgr = ArchiveManager::new(archive_state);
        if !chunk_refs.is_empty() {
            archive_mgr.remove_chunks(chunk_refs)?;
        }

        std::fs::remove_file(&db_path)
            .with_context(|| format!("removing {}", db_path.display()))?;

        Ok(Json(SourceDeleteResponse { files_deleted, chunks_removed }))
    }).await
}
