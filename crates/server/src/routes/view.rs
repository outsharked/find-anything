use std::sync::Arc;
use std::time::Duration;

use axum::{
    body::Body,
    extract::{Query, State},
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
};
use serde::Deserialize;

use find_common::path::split_composite;
use rusqlite::OptionalExtension as _;

use crate::AppState;
use super::{check_auth, source_db_path};

#[derive(Deserialize)]
pub struct ViewParams {
    source: String,
    /// Composite path (e.g. `photos/sunset.tiff` or `archive.zip::photo.tiff`).
    path: String,
}

/// GET /api/v1/view?source=<name>&path=<relative_path>
///
/// Serves an inline image for any file the server has indexed as `image` or
/// `dicom` kind.  The server decides the representation:
///
/// - **image**: read the file bytes, sniff the true format, serve natively if
///   the browser can display it (JPEG, PNG, GIF, WebP, …); otherwise decode
///   and re-encode as PNG.
/// - **dicom**: spawn `find-preview-dicom`, return the PNG output.
///
/// Archive members (composite paths with `::`) are supported for `image` kind
/// by extracting the member bytes before conversion.
pub async fn get_view(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(params): Query<ViewParams>,
) -> Response {
    if let Err(s) = check_auth(&state, &headers) {
        return s.into_response();
    }

    // ── Validate source and look up file kind in the DB ──────────────────────

    let db_path = match source_db_path(&state, &params.source) {
        Ok(p) => p,
        Err(s) => return s.into_response(),
    };

    let query_path = params.path.clone();
    let kind_result: Result<Option<String>, StatusCode> = tokio::task::spawn_blocking(move || {
        if !db_path.exists() {
            return Ok(None);
        }
        // Open a lightweight read-only connection — no migrations, no index creation,
        // just a short busy timeout so we never block the async runtime for long.
        let conn = rusqlite::Connection::open(&db_path)
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
        conn.busy_timeout(std::time::Duration::from_secs(5))
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
        conn.query_row(
            "SELECT kind FROM files WHERE path = ?1 LIMIT 1",
            rusqlite::params![query_path],
            |row| row.get::<_, String>(0),
        ).optional()
         .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)
    }).await
    .unwrap_or(Err(StatusCode::INTERNAL_SERVER_ERROR));

    let kind = match kind_result {
        Ok(Some(k)) => k,
        Ok(None) => return StatusCode::NOT_FOUND.into_response(),
        Err(s) => return s.into_response(),
    };

    // ── Dispatch on kind ─────────────────────────────────────────────────────

    match kind.as_str() {
        "image" => serve_image(&state, &params.source, &params.path).await,
        "dicom" => serve_dicom(&state, &params.source, &params.path).await,
        _ => StatusCode::UNPROCESSABLE_ENTITY.into_response(),
    }
}

// ── Image serving ─────────────────────────────────────────────────────────────

async fn serve_image(state: &AppState, source: &str, path: &str) -> Response {
    // Fetch the raw bytes — archive member or direct file.
    let bytes_result = if let Some((outer, member)) = split_composite(path) {
        get_archive_member_bytes(state, source, outer, member).await
    } else {
        get_direct_file_bytes(state, source, path).await
    };

    let bytes = match bytes_result {
        Ok(b) => b,
        Err(s) => return s.into_response(),
    };

    // Fast path: native browser format.
    if let Some((mime, ext)) = crate::image_util::sniff_browser_format(&bytes) {
        let stem = stem_from_path(path).replace('"', "");
        return Response::builder()
            .status(StatusCode::OK)
            .header(header::CONTENT_TYPE, mime)
            .header(header::CONTENT_DISPOSITION, format!("inline; filename=\"{stem}.{ext}\""))
            .header(header::CONTENT_LENGTH, bytes.len().to_string())
            .body(Body::from(bytes))
            .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response());
    }

    // Decode and re-encode as PNG.
    let png_bytes = match tokio::task::spawn_blocking(move || -> Result<Vec<u8>, ()> {
        let img = crate::image_util::load_image(&bytes).map_err(|_| ())?;
        let mut out = Vec::new();
        img.write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png)
            .map_err(|_| ())?;
        Ok(out)
    }).await {
        Ok(Ok(b)) => b,
        _ => return StatusCode::UNPROCESSABLE_ENTITY.into_response(),
    };

    let stem = stem_from_path(path).replace('"', "");
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "image/png")
        .header(header::CONTENT_DISPOSITION, format!("inline; filename=\"{stem}.png\""))
        .header(header::CONTENT_LENGTH, png_bytes.len().to_string())
        .body(Body::from(png_bytes))
        .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
}

// ── DICOM serving ─────────────────────────────────────────────────────────────

async fn serve_dicom(state: &AppState, source: &str, path: &str) -> Response {
    // DICOM files are never archive members (canViewInline enforces !isArchiveMember).
    let (_, canonical_full) = match super::resolve_source_path(state, source, path) {
        Ok(p) => p,
        Err(s) => return s.into_response(),
    };

    let timeout_secs = state.config.scan.dicom_preview_timeout_secs;
    let binary = resolve_preview_binary();

    let result = tokio::task::spawn_blocking(move || {
        run_preview_binary(&binary, &canonical_full, timeout_secs)
    })
    .await
    .unwrap_or_else(|e| Err(format!("task panic: {e}")));

    match result {
        Ok(png_bytes) => Response::builder()
            .status(StatusCode::OK)
            .header(header::CONTENT_TYPE, "image/png")
            .header(header::CONTENT_LENGTH, png_bytes.len().to_string())
            .body(Body::from(png_bytes))
            .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response()),
        Err(e) => {
            tracing::warn!(source, path, error = %e, "view(dicom): conversion failed");
            StatusCode::UNPROCESSABLE_ENTITY.into_response()
        }
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Get the last path component's file stem from a possibly composite path.
fn stem_from_path(path: &str) -> String {
    let leaf = path.rsplit("::").next().unwrap_or(path);
    let leaf = leaf.rsplit('/').next().unwrap_or(leaf);
    std::path::Path::new(leaf)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("file")
        .to_owned()
}

/// Read bytes for a direct (non-composite) file from the source root.
async fn get_direct_file_bytes(
    state: &AppState,
    source: &str,
    path: &str,
) -> Result<Vec<u8>, StatusCode> {
    let (_, canonical_full) = super::resolve_source_path(state, source, path)?;
    tokio::fs::read(&canonical_full).await.map_err(|_| StatusCode::NOT_FOUND)
}

/// Extract a member from a ZIP archive and return its raw bytes.
async fn get_archive_member_bytes(
    state: &AppState,
    source: &str,
    outer_path: &str,
    member_name: &str,
) -> Result<Vec<u8>, StatusCode> {
    let (_, canonical_outer) = super::resolve_source_path(state, source, outer_path)?;
    let member_name = member_name.to_owned();

    tokio::task::spawn_blocking(move || -> Result<Vec<u8>, StatusCode> {
        use std::io::Read as _;
        const MAX_MEMBER_BYTES: u64 = 64 * 1024 * 1024;

        let file = std::fs::File::open(&canonical_outer).map_err(|_| StatusCode::NOT_FOUND)?;
        let mut zip = zip::ZipArchive::new(file).map_err(|_| StatusCode::UNPROCESSABLE_ENTITY)?;

        let bytes = if let Some((mid_path, leaf)) = split_composite(&member_name) {
            let mid_bytes = {
                let mut entry = zip.by_name(mid_path).map_err(|_| StatusCode::NOT_FOUND)?;
                if entry.size() > MAX_MEMBER_BYTES { return Err(StatusCode::PAYLOAD_TOO_LARGE); }
                let mut b = Vec::with_capacity(entry.size() as usize);
                entry.read_to_end(&mut b).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
                b
            };
            let cursor = std::io::Cursor::new(mid_bytes);
            let mut inner_zip = zip::ZipArchive::new(cursor).map_err(|_| StatusCode::UNPROCESSABLE_ENTITY)?;
            let mut entry = inner_zip.by_name(leaf).map_err(|_| StatusCode::NOT_FOUND)?;
            if entry.size() > MAX_MEMBER_BYTES { return Err(StatusCode::PAYLOAD_TOO_LARGE); }
            let mut b = Vec::with_capacity(entry.size() as usize);
            entry.read_to_end(&mut b).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
            b
        } else {
            let mut entry = zip.by_name(&member_name).map_err(|_| StatusCode::NOT_FOUND)?;
            if entry.size() > MAX_MEMBER_BYTES { return Err(StatusCode::PAYLOAD_TOO_LARGE); }
            let mut b = Vec::with_capacity(entry.size() as usize);
            entry.read_to_end(&mut b).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
            b
        };

        Ok(bytes)
    })
    .await
    .unwrap_or(Err(StatusCode::INTERNAL_SERVER_ERROR))
}

fn resolve_preview_binary() -> String {
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let candidate = dir.join("find-preview-dicom");
            if candidate.exists() {
                return candidate.to_string_lossy().into_owned();
            }
            if let Some(parent) = dir.parent() {
                let candidate = parent.join("find-preview-dicom");
                if candidate.exists() {
                    return candidate.to_string_lossy().into_owned();
                }
            }
        }
    }
    "find-preview-dicom".to_string()
}

fn run_preview_binary(
    binary: &str,
    path: &std::path::Path,
    timeout_secs: u64,
) -> Result<Vec<u8>, String> {
    use std::io::Read as _;
    use std::process::Command;

    let mut child = Command::new(binary)
        .arg(path)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| format!("failed to spawn {binary}: {e}"))?;

    // Drain stdout in a background thread to avoid pipe-buffer deadlock.
    let stdout_thread = {
        let mut stdout = child.stdout.take().ok_or("no stdout")?;
        std::thread::spawn(move || {
            let mut buf = Vec::new();
            stdout.read_to_end(&mut buf).map(|_| buf)
        })
    };

    let deadline = std::time::Instant::now() + Duration::from_secs(timeout_secs);
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {
                if std::time::Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = stdout_thread.join();
                    return Err(format!("timed out after {timeout_secs}s"));
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(e) => return Err(format!("wait error: {e}")),
        }
    };

    let png_bytes = stdout_thread
        .join()
        .map_err(|_| "stdout thread panicked".to_string())?
        .map_err(|e| e.to_string())?;

    if status.success() {
        Ok(png_bytes)
    } else {
        let mut stderr_buf = Vec::new();
        if let Some(mut se) = child.stderr.take() {
            let _ = se.read_to_end(&mut stderr_buf);
        }
        Err(format!("exit {:?}: {}", status.code(), String::from_utf8_lossy(&stderr_buf)))
    }
}
