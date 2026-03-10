use std::io::Read as _;
use std::sync::Arc;

use axum::{
    body::Body,
    extract::{Query, State},
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
};
use serde::Deserialize;
use tokio::fs::File;
use tokio_util::io::ReaderStream;

use find_common::path::split_composite;

use crate::AppState;

use super::check_auth;

#[derive(Deserialize)]
pub struct RawParams {
    source: String,
    path: String,
    /// When `convert=png`, the server decodes the file with the `image` crate
    /// and re-encodes it as PNG. Useful for formats browsers cannot display
    /// natively (e.g. TIFF).
    convert: Option<String>,
}

/// GET /api/v1/raw?source=<name>&path=<relative_path>[&convert=png]
///
/// Streams the original file from the source's configured filesystem root.
/// Requires the source to have a `path` configured in `[sources.<name>]`.
pub async fn get_raw(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(params): Query<RawParams>,
) -> Response {
    if let Err(s) = check_auth(&state, &headers) {
        return s.into_response();
    }

    // Archive member paths (contain "::") — extract from the outer ZIP.
    if let Some((outer, member)) = split_composite(&params.path) {
        return serve_archive_member(
            &state, &params.source, outer, member, params.convert.as_deref(),
        ).await;
    }

    // Reject paths that start with '/' or contain '..' components.
    if params.path.starts_with('/') || params.path.starts_with('\\') {
        tracing::warn!(source = %params.source, path = %params.path, "raw: rejected path with leading slash");
        return StatusCode::BAD_REQUEST.into_response();
    }
    for component in std::path::Path::new(&params.path).components() {
        if matches!(
            component,
            std::path::Component::ParentDir
                | std::path::Component::RootDir
                | std::path::Component::Prefix(_)
        ) {
            tracing::warn!(source = %params.source, path = %params.path, "raw: rejected path with illegal component");
            return StatusCode::BAD_REQUEST.into_response();
        }
    }

    // Look up the source's configured filesystem root.
    let source_root_str = match state
        .config
        .sources
        .get(&params.source)
        .and_then(|sc| sc.path.as_deref())
    {
        Some(p) => p.to_owned(),
        None => {
            tracing::warn!(source = %params.source, path = %params.path, "raw: source not configured or has no path");
            return StatusCode::NOT_FOUND.into_response();
        }
    };

    let source_root = std::path::Path::new(&source_root_str);
    let full_path = source_root.join(&params.path);

    // Canonicalize both paths and confirm the file is still inside the root.
    let canonical_root = match source_root.canonicalize() {
        Ok(p) => p,
        Err(e) => {
            tracing::warn!(source = %params.source, root = %source_root_str, error = %e, "raw: failed to canonicalize source root");
            return StatusCode::NOT_FOUND.into_response();
        }
    };
    let canonical_full = match full_path.canonicalize() {
        Ok(p) => p,
        Err(e) => {
            tracing::warn!(source = %params.source, path = %params.path, error = %e, "raw: failed to canonicalize file path");
            return StatusCode::NOT_FOUND.into_response();
        }
    };
    if !canonical_full.starts_with(&canonical_root) {
        tracing::warn!(source = %params.source, path = %params.path, "raw: path escapes source root");
        return StatusCode::BAD_REQUEST.into_response();
    }

    // If convert=png is requested, decode the image and re-encode as PNG.
    // Build Content-Disposition with the real filename so browser PDF/image
    // viewers show the actual name rather than "raw" (the endpoint path).
    let display_filename = canonical_full
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("file");
    // For convert=png the extension changes, so use the stem + ".png".
    let png_filename = canonical_full
        .file_stem()
        .and_then(|n| n.to_str())
        .map(|stem| format!("{stem}.png"))
        .unwrap_or_else(|| "file.png".to_string());
    // Sanitize: strip any double-quotes to avoid breaking the header value.
    let safe_name = display_filename.replace('"', "");
    let safe_png_name = png_filename.replace('"', "");
    let disposition = format!("inline; filename=\"{safe_name}\"");
    let png_disposition = format!("inline; filename=\"{safe_png_name}\"");

    if params.convert.as_deref() == Some("png") {
        let bytes = match tokio::fs::read(&canonical_full).await {
            Ok(b) => b,
            Err(_) => return StatusCode::NOT_FOUND.into_response(),
        };
        let png_bytes = match tokio::task::spawn_blocking(move || -> Result<Vec<u8>, ()> {
            let img = image::load_from_memory(&bytes).map_err(|_| ())?;
            let mut out = Vec::new();
            img.write_to(
                &mut std::io::Cursor::new(&mut out),
                image::ImageFormat::Png,
            )
            .map_err(|_| ())?;
            Ok(out)
        })
        .await
        {
            Ok(Ok(b)) => b,
            _ => return StatusCode::UNPROCESSABLE_ENTITY.into_response(),
        };
        return Response::builder()
            .status(StatusCode::OK)
            .header(header::CONTENT_TYPE, "image/png")
            .header(header::CONTENT_DISPOSITION, png_disposition)
            .body(Body::from(png_bytes))
            .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response());
    }

    // Default: stream the file as-is.
    let file = match File::open(&canonical_full).await {
        Ok(f) => f,
        Err(_) => return StatusCode::NOT_FOUND.into_response(),
    };

    let mime = mime_guess::from_path(&canonical_full).first_or_octet_stream();
    let stream = ReaderStream::new(file);
    let body = Body::from_stream(stream);

    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, mime.essence_str())
        .header(header::CONTENT_DISPOSITION, disposition)
        .body(body)
        .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
}

/// Serve a member from a ZIP archive at `outer_path` within the source root.
/// `outer_path` is the path of the outer ZIP file (no `::`, no leading `/`).
/// `member_name` is the path of the member inside the ZIP (may be nested).
/// Only ZIP archives are supported; other formats return 422.
async fn serve_archive_member(
    state: &AppState,
    source: &str,
    outer_path: &str,
    member_name: &str,
    convert: Option<&str>,
) -> Response {
    // Validate outer path: no leading slash, no `..`.
    if outer_path.starts_with('/') || outer_path.starts_with('\\') {
        return StatusCode::BAD_REQUEST.into_response();
    }
    for component in std::path::Path::new(outer_path).components() {
        if matches!(
            component,
            std::path::Component::ParentDir
                | std::path::Component::RootDir
                | std::path::Component::Prefix(_)
        ) {
            return StatusCode::BAD_REQUEST.into_response();
        }
    }

    // Enforce configured nesting limit. depth = number of '::' separators in the full path + 1.
    // member_name carries everything after the first '::' split, so its '::' count = depth - 1.
    let member_depth = member_name.matches("::").count() + 1;
    if member_depth > state.config.server.download_zip_member_levels {
        return StatusCode::UNPROCESSABLE_ENTITY.into_response();
    }

    let source_root_str = match state.config.sources
        .get(source)
        .and_then(|sc| sc.path.as_deref())
    {
        Some(p) => p.to_owned(),
        None => {
            tracing::warn!(source = %source, outer = %outer_path, "raw(archive): source not configured or has no path");
            return StatusCode::NOT_FOUND.into_response();
        }
    };

    let outer_full = std::path::Path::new(&source_root_str).join(outer_path);
    let canonical_root = match std::path::Path::new(&source_root_str).canonicalize() {
        Ok(p) => p,
        Err(e) => {
            tracing::warn!(source = %source, root = %source_root_str, error = %e, "raw(archive): failed to canonicalize source root");
            return StatusCode::NOT_FOUND.into_response();
        }
    };
    let canonical_outer = match outer_full.canonicalize() {
        Ok(p) => p,
        Err(e) => {
            tracing::warn!(source = %source, outer = %outer_path, error = %e, "raw(archive): failed to canonicalize outer path");
            return StatusCode::NOT_FOUND.into_response();
        }
    };
    if !canonical_outer.starts_with(&canonical_root) {
        return StatusCode::BAD_REQUEST.into_response();
    }

    let member_name = member_name.to_owned();
    let convert = convert.map(|s| s.to_owned());

    tokio::task::spawn_blocking(move || -> Response {
        // Refuse to buffer very large members (>64 MB) to avoid OOM.
        const MAX_MEMBER_BYTES: u64 = 64 * 1024 * 1024;

        let file = match std::fs::File::open(&canonical_outer) {
            Ok(f) => f,
            Err(_) => return StatusCode::NOT_FOUND.into_response(),
        };
        let mut zip = match zip::ZipArchive::new(file) {
            Ok(z) => z,
            Err(_) => return StatusCode::UNPROCESSABLE_ENTITY.into_response(),
        };

        // Nested member: outer.zip::inner.zip::file — extract the intermediate
        // ZIP from the outer archive, then extract the target file from that.
        let (bytes, leaf_name) = if let Some((mid_path, leaf)) = split_composite(&member_name) {
            // Step 1: read the intermediate ZIP from the outer archive.
            let mid_bytes = {
                let mut mid_entry = match zip.by_name(mid_path) {
                    Ok(e) => e,
                    Err(_) => return StatusCode::NOT_FOUND.into_response(),
                };
                if mid_entry.size() > MAX_MEMBER_BYTES {
                    return StatusCode::PAYLOAD_TOO_LARGE.into_response();
                }
                let mut b = Vec::with_capacity(mid_entry.size() as usize);
                if mid_entry.read_to_end(&mut b).is_err() {
                    return StatusCode::INTERNAL_SERVER_ERROR.into_response();
                }
                b
            };
            // Step 2: open the intermediate ZIP and extract the target file.
            let cursor = std::io::Cursor::new(mid_bytes);
            let mut inner_zip = match zip::ZipArchive::new(cursor) {
                Ok(z) => z,
                Err(_) => return StatusCode::UNPROCESSABLE_ENTITY.into_response(),
            };
            let mut inner_entry = match inner_zip.by_name(leaf) {
                Ok(e) => e,
                Err(_) => return StatusCode::NOT_FOUND.into_response(),
            };
            if inner_entry.size() > MAX_MEMBER_BYTES {
                return StatusCode::PAYLOAD_TOO_LARGE.into_response();
            }
            let mut b = Vec::with_capacity(inner_entry.size() as usize);
            if inner_entry.read_to_end(&mut b).is_err() {
                return StatusCode::INTERNAL_SERVER_ERROR.into_response();
            }
            (b, leaf.to_owned())
        } else {
            // Single-level member.
            let mut entry = match zip.by_name(&member_name) {
                Ok(e) => e,
                Err(_) => return StatusCode::NOT_FOUND.into_response(),
            };
            if entry.size() > MAX_MEMBER_BYTES {
                return StatusCode::PAYLOAD_TOO_LARGE.into_response();
            }
            let mut b = Vec::with_capacity(entry.size() as usize);
            if entry.read_to_end(&mut b).is_err() {
                return StatusCode::INTERNAL_SERVER_ERROR.into_response();
            }
            (b, member_name.clone())
        };

        let filename = std::path::Path::new(&leaf_name)
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("file")
            .replace('"', "");

        if convert.as_deref() == Some("png") {
            let png_name = std::path::Path::new(&leaf_name)
                .file_stem()
                .and_then(|n| n.to_str())
                .map(|s| format!("{s}.png"))
                .unwrap_or_else(|| "file.png".to_string());
            let png_name = png_name.replace('"', "");
            let png_bytes = match (|| -> Result<Vec<u8>, ()> {
                let img = image::load_from_memory(&bytes).map_err(|_| ())?;
                let mut out = Vec::new();
                img.write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png)
                    .map_err(|_| ())?;
                Ok(out)
            })() {
                Ok(b) => b,
                Err(_) => return StatusCode::UNPROCESSABLE_ENTITY.into_response(),
            };
            return Response::builder()
                .status(StatusCode::OK)
                .header(header::CONTENT_TYPE, "image/png")
                .header(header::CONTENT_DISPOSITION, format!("inline; filename=\"{png_name}\""))
                .body(Body::from(png_bytes))
                .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response());
        }

        let mime = mime_guess::from_path(&leaf_name).first_or_octet_stream();
        Response::builder()
            .status(StatusCode::OK)
            .header(header::CONTENT_TYPE, mime.essence_str())
            .header(header::CONTENT_DISPOSITION, format!("inline; filename=\"{filename}\""))
            .body(Body::from(bytes))
            .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
    })
    .await
    .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
}
