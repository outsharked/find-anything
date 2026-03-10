/// Upload routes: POST, PATCH, HEAD /api/v1/upload
use std::sync::Arc;

use axum::{
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    Json,
};
use tracing::warn;
use uuid::Uuid;

use find_common::api::{UploadInitRequest, UploadInitResponse, UploadPatchResponse, UploadStatusResponse};
use find_common::config::extractor_config_from_extraction;

use crate::upload::{index_upload, part_path, part_size, read_meta, touch_meta, uploads_dir, write_meta, UploadMeta};
use crate::AppState;
use crate::routes::check_auth;

/// `POST /api/v1/upload` — initiate a resumable upload.
pub async fn upload_init(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(req): Json<UploadInitRequest>,
) -> impl IntoResponse {
    if let Err(s) = check_auth(&state, &headers) {
        return (s, Json(serde_json::Value::Null)).into_response();
    }

    let uploads = uploads_dir(&state.data_dir);
    if let Err(e) = std::fs::create_dir_all(&uploads) {
        warn!("failed to create uploads dir: {e}");
        return (StatusCode::INTERNAL_SERVER_ERROR, Json(serde_json::Value::Null)).into_response();
    }

    let id = Uuid::new_v4().to_string();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;

    let meta = UploadMeta {
        source: req.source,
        rel_path: req.rel_path,
        mtime: req.mtime,
        total_size: req.size,
        created_at: now,
    };

    if let Err(e) = write_meta(&uploads, &id, &meta) {
        warn!("failed to write upload meta for {id}: {e:#}");
        return (StatusCode::INTERNAL_SERVER_ERROR, Json(serde_json::Value::Null)).into_response();
    }

    (
        StatusCode::CREATED,
        Json(UploadInitResponse { upload_id: id }),
    )
        .into_response()
}

/// `PATCH /api/v1/upload/{id}` — send a chunk of the file.
///
/// Requires `Content-Range: bytes <start>-<end>/<total>` header.
/// Returns 409 if `start` doesn't match the current file size (gap).
pub async fn upload_patch(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
    body: axum::body::Bytes,
) -> impl IntoResponse {
    if let Err(s) = check_auth(&state, &headers) {
        return (s, Json(serde_json::Value::Null)).into_response();
    }

    let uploads = uploads_dir(&state.data_dir);

    let meta = match read_meta(&uploads, &id) {
        Some(m) => m,
        None => return (StatusCode::NOT_FOUND, Json(serde_json::Value::Null)).into_response(),
    };

    // Parse Content-Range header: "bytes <start>-<end>/<total>"
    let range_str = match headers
        .get("Content-Range")
        .and_then(|v| v.to_str().ok())
    {
        Some(s) => s.to_string(),
        None => {
            return (StatusCode::BAD_REQUEST, Json(serde_json::Value::Null)).into_response()
        }
    };

    let range_start = parse_content_range_start(&range_str);
    let current_size = part_size(&uploads, &id);

    if let Some(start) = range_start {
        if start != current_size {
            return (
                StatusCode::CONFLICT,
                Json(serde_json::json!({ "received": current_size })),
            )
                .into_response();
        }
    }

    // Append bytes to .part file.
    let part = part_path(&uploads, &id);
    if let Err(e) = append_bytes(&part, &body) {
        warn!("failed to write chunk for upload {id}: {e:#}");
        return (StatusCode::INTERNAL_SERVER_ERROR, Json(serde_json::Value::Null)).into_response();
    }

    let received = part_size(&uploads, &id);
    touch_meta(&uploads, &id);

    // If fully received, trigger extraction asynchronously.
    if received >= meta.total_size {
        let data_dir = state.data_dir.clone();
        let extractor_dir = state.config.server.extractor_dir.clone();
        let ext_cfg = extractor_config_from_extraction(&state.config.extraction);
        let meta_clone = meta.clone();
        let id_clone = id.clone();
        tokio::spawn(async move {
            index_upload(id_clone, meta_clone, data_dir, extractor_dir, ext_cfg).await;
        });
    }

    (StatusCode::OK, Json(UploadPatchResponse { received })).into_response()
}

/// `HEAD /api/v1/upload/{id}` — query upload progress (for resume).
pub async fn upload_status(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> impl IntoResponse {
    if let Err(s) = check_auth(&state, &headers) {
        return (s, Json(serde_json::Value::Null)).into_response();
    }

    let uploads = uploads_dir(&state.data_dir);

    let meta = match read_meta(&uploads, &id) {
        Some(m) => m,
        None => return (StatusCode::NOT_FOUND, Json(serde_json::Value::Null)).into_response(),
    };

    let received = part_size(&uploads, &id);
    (
        StatusCode::OK,
        Json(UploadStatusResponse {
            received,
            total: meta.total_size,
        }),
    )
        .into_response()
}

// ── Helpers ───────────────────────────────────────────────────────────────────

fn append_bytes(path: &std::path::Path, data: &[u8]) -> anyhow::Result<()> {
    use std::io::Write;
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    file.write_all(data)?;
    Ok(())
}

/// Parse the start byte offset from a `Content-Range: bytes start-end/total` header.
/// Returns None if the header is missing or malformed.
fn parse_content_range_start(header: &str) -> Option<u64> {
    // Format: "bytes <start>-<end>/<total>"
    let without_prefix = header.strip_prefix("bytes ")?;
    let dash_pos = without_prefix.find('-')?;
    without_prefix[..dash_pos].parse().ok()
}
