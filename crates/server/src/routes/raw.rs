use std::io::Read as _;
use std::sync::Arc;

use axum::{
    body::Body,
    extract::{Path as AxumPath, Query, State},
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
};
use serde::Deserialize;
use tokio::fs::File;
use tokio::io::{AsyncReadExt as _, AsyncSeekExt as _};
use tokio_util::io::ReaderStream;

/// Parse an HTTP `Range: bytes=<start>-[end]` header value.
/// Only single-range requests are supported; multi-range is not.
/// Returns `(start, end)` where `end` is `None` for open-ended ranges.
fn parse_byte_range(range: &str) -> Option<(u64, Option<u64>)> {
    let s = range.strip_prefix("bytes=")?;
    // Reject multi-range (contains comma).
    if s.contains(',') { return None; }
    let (start_str, end_str) = s.split_once('-')?;
    let start: u64 = start_str.trim().parse().ok()?;
    let end: Option<u64> = if end_str.trim().is_empty() { None } else { end_str.trim().parse().ok() };
    Some((start, end))
}

use find_common::path::split_composite;

use crate::AppState;

use super::{check_auth, check_link_code_auth};

/// Log a file-access failure with context that helps distinguish between
/// "mount not available" and "file genuinely missing on the client".
///
/// When `full_path` can't be resolved, stat the `source_root` directory:
/// - root inaccessible → mount probably failed or path is misconfigured
/// - root accessible   → the file was likely deleted from the client
fn log_file_not_found(
    source: &str,
    source_root: &std::path::Path,
    full_path: &std::path::Path,
    error: &std::io::Error,
    ctx: &str,
) {
    if source_root.metadata().is_err() {
        tracing::warn!(
            source,
            root = %source_root.display(),
            path = %full_path.display(),
            error = %error,
            "{ctx}: source root is not accessible — mount may have failed or path is misconfigured"
        );
    } else {
        tracing::warn!(
            source,
            path = %full_path.display(),
            error = %error,
            "{ctx}: file not found — may have been deleted from the client"
        );
    }
}

#[derive(Deserialize)]
pub struct RawParams {
    source: String,
    path: String,
    /// When `convert=png`, the server decodes the file with the `image` crate
    /// and re-encodes it as PNG. Useful for formats browsers cannot display
    /// natively (e.g. TIFF).
    convert: Option<String>,
    /// Optional share link code as an alternative to bearer authentication.
    link_code: Option<String>,
    /// When `download=1`, set Content-Disposition to `attachment` instead of `inline`.
    download: Option<String>,
}

/// GET /api/v1/raw?source=<name>&path=<relative_path>[&convert=png][&link_code=C][&download=1]
///
/// Streams the original file from the source's configured filesystem root.
/// Requires the source to have a `path` configured in `[sources.<name>]`.
/// Auth: bearer/cookie, or a valid `link_code` that matches source+path.
pub async fn get_raw(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(params): Query<RawParams>,
) -> Response {
    if let Some(code) = &params.link_code {
        // Authenticate via link code (blocking DB lookup).
        let data_dir = state.data_dir.clone();
        let code = code.clone();
        let source = params.source.clone();
        let path = params.path.clone();
        let auth = tokio::task::spawn_blocking(move || {
            check_link_code_auth(&data_dir, &code, &source, &path)
        })
        .await
        .unwrap_or(Err(StatusCode::INTERNAL_SERVER_ERROR));
        if let Err(s) = auth {
            return s.into_response();
        }
    } else if let Err(s) = check_auth(&state, &headers) {
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
            tracing::warn!(source = %params.source, root = %source_root_str, error = %e,
                "raw: source root not accessible — mount may have failed or path is misconfigured");
            return StatusCode::NOT_FOUND.into_response();
        }
    };
    let canonical_full = match full_path.canonicalize() {
        Ok(p) => p,
        Err(e) => {
            log_file_not_found(&params.source, source_root, &full_path, &e, "raw");
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
    let disp_kind = if params.download.as_deref() == Some("1") { "attachment" } else { "inline" };
    let disposition = format!("{disp_kind}; filename=\"{safe_name}\"");
    let png_disposition = format!("{disp_kind}; filename=\"{safe_png_name}\"");

    if params.convert.as_deref() == Some("png") {
        let bytes = match tokio::fs::read(&canonical_full).await {
            Ok(b) => b,
            Err(_) => return StatusCode::NOT_FOUND.into_response(),
        };
        // Fast path: if the file's true content is already a browser-native format
        // (detected by magic bytes), serve it directly with the correct MIME type
        // rather than decoding and re-encoding to PNG.
        if let Some((mime, ext)) = crate::image_util::sniff_browser_format(&bytes) {
            let stem = canonical_full.file_stem().and_then(|n| n.to_str()).unwrap_or("file").replace('"', "");
            let native_disp = format!("{disp_kind}; filename=\"{stem}.{ext}\"");
            return Response::builder()
                .status(StatusCode::OK)
                .header(header::CONTENT_TYPE, mime)
                .header(header::CONTENT_DISPOSITION, native_disp)
                .body(Body::from(bytes))
                .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response());
        }
        let png_bytes = match tokio::task::spawn_blocking(move || -> Result<Vec<u8>, ()> {
            let img = crate::image_util::load_image(&bytes).map_err(|_| ())?;
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

    // Default: stream the file, with byte-range support for audio/video seeking.
    let file_size = match tokio::fs::metadata(&canonical_full).await {
        Ok(m) => m.len(),
        Err(_) => return StatusCode::NOT_FOUND.into_response(),
    };

    let mime = mime_guess::from_path(&canonical_full).first_or_octet_stream();

    // Parse Range header if present.
    let range = headers
        .get(header::RANGE)
        .and_then(|v| v.to_str().ok())
        .and_then(parse_byte_range);

    if let Some((start, end_opt)) = range {
        let end = end_opt.unwrap_or(file_size.saturating_sub(1)).min(file_size.saturating_sub(1));
        if start > end || start >= file_size {
            return (
                StatusCode::RANGE_NOT_SATISFIABLE,
                [(header::CONTENT_RANGE, format!("bytes */{file_size}"))],
            )
                .into_response();
        }
        let length = end - start + 1;
        let mut file = match File::open(&canonical_full).await {
            Ok(f) => f,
            Err(_) => return StatusCode::NOT_FOUND.into_response(),
        };
        if file.seek(std::io::SeekFrom::Start(start)).await.is_err() {
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
        let stream = ReaderStream::new(file.take(length));
        let body = Body::from_stream(stream);
        Response::builder()
            .status(StatusCode::PARTIAL_CONTENT)
            .header(header::CONTENT_TYPE, mime.essence_str())
            .header(header::CONTENT_RANGE, format!("bytes {start}-{end}/{file_size}"))
            .header(header::CONTENT_LENGTH, length.to_string())
            .header(header::ACCEPT_RANGES, "bytes")
            .header(header::CONTENT_DISPOSITION, disposition)
            .body(body)
            .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
    } else {
        let file = match File::open(&canonical_full).await {
            Ok(f) => f,
            Err(_) => return StatusCode::NOT_FOUND.into_response(),
        };
        let stream = ReaderStream::new(file);
        let body = Body::from_stream(stream);
        Response::builder()
            .status(StatusCode::OK)
            .header(header::CONTENT_TYPE, mime.essence_str())
            .header(header::CONTENT_LENGTH, file_size.to_string())
            .header(header::ACCEPT_RANGES, "bytes")
            .header(header::CONTENT_DISPOSITION, disposition)
            .body(body)
            .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
    }
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
    let archive_root = std::path::Path::new(&source_root_str);
    let canonical_root = match archive_root.canonicalize() {
        Ok(p) => p,
        Err(e) => {
            tracing::warn!(source = %source, root = %source_root_str, error = %e,
                "raw(archive): source root not accessible — mount may have failed or path is misconfigured");
            return StatusCode::NOT_FOUND.into_response();
        }
    };
    let canonical_outer = match outer_full.canonicalize() {
        Ok(p) => p,
        Err(e) => {
            log_file_not_found(source, archive_root, &outer_full, &e, "raw(archive)");
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
            let stem = std::path::Path::new(&leaf_name)
                .file_stem()
                .and_then(|n| n.to_str())
                .unwrap_or("file")
                .replace('"', "");
            // Fast path: serve browser-native formats directly by detected type.
            if let Some((mime, ext)) = crate::image_util::sniff_browser_format(&bytes) {
                return Response::builder()
                    .status(StatusCode::OK)
                    .header(header::CONTENT_TYPE, mime)
                    .header(header::CONTENT_DISPOSITION, format!("inline; filename=\"{stem}.{ext}\""))
                    .body(Body::from(bytes))
                    .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response());
            }
            let png_bytes = match (|| -> Result<Vec<u8>, ()> {
                let img = crate::image_util::load_image(&bytes).map_err(|_| ())?;
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
                .header(header::CONTENT_DISPOSITION, format!("inline; filename=\"{stem}.png\""))
                .body(Body::from(png_bytes))
                .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response());
        }

        let mime = mime_guess::from_path(&leaf_name).first_or_octet_stream();
        let content_length = bytes.len();
        Response::builder()
            .status(StatusCode::OK)
            .header(header::CONTENT_TYPE, mime.essence_str())
            .header(header::CONTENT_LENGTH, content_length.to_string())
            .header(header::ACCEPT_RANGES, "none")
            .header(header::CONTENT_DISPOSITION, format!("inline; filename=\"{filename}\""))
            .body(Body::from(bytes))
            .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
    })
    .await
    .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
}

/// GET /api/v1/raw/{source}/{*path}
///
/// Path-based variant of get_raw. Source and file path are URL path segments
/// rather than query parameters, so the browser resolves relative URLs in HTML
/// documents (images, CSS, etc.) to sibling paths on the same endpoint.
/// Auth: bearer/cookie only (no link_code support).
pub async fn get_raw_path(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    AxumPath((source, path)): AxumPath<(String, String)>,
) -> Response {
    if let Err(s) = check_auth(&state, &headers) {
        return s.into_response();
    }

    let (_, canonical_full) = match super::resolve_source_path(&state, &source, &path) {
        Ok(p) => p,
        Err(s) => return s.into_response(),
    };

    let file_size = match tokio::fs::metadata(&canonical_full).await {
        Ok(m) => m.len(),
        Err(_) => return StatusCode::NOT_FOUND.into_response(),
    };

    let mime = mime_guess::from_path(&canonical_full).first_or_octet_stream();
    let display_filename = canonical_full
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("file")
        .replace('"', "");

    let file = match File::open(&canonical_full).await {
        Ok(f) => f,
        Err(_) => return StatusCode::NOT_FOUND.into_response(),
    };
    let stream = ReaderStream::new(file);
    let body = Body::from_stream(stream);
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, mime.essence_str())
        .header(header::CONTENT_LENGTH, file_size.to_string())
        .header(header::CONTENT_DISPOSITION, format!("inline; filename=\"{display_filename}\""))
        .body(body)
        .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
}

#[cfg(test)]
mod tests {
    use super::parse_byte_range;

    #[test]
    fn parse_byte_range_valid_closed() {
        assert_eq!(parse_byte_range("bytes=0-99"), Some((0, Some(99))));
        assert_eq!(parse_byte_range("bytes=100-199"), Some((100, Some(199))));
    }

    #[test]
    fn parse_byte_range_valid_open_ended() {
        assert_eq!(parse_byte_range("bytes=500-"), Some((500, None)));
        assert_eq!(parse_byte_range("bytes=0-"), Some((0, None)));
    }

    #[test]
    fn parse_byte_range_multi_range_rejected() {
        assert_eq!(parse_byte_range("bytes=0-99,200-299"), None);
    }

    #[test]
    fn parse_byte_range_missing_prefix_rejected() {
        assert_eq!(parse_byte_range("0-99"), None);
        assert_eq!(parse_byte_range("0-"), None);
    }

    #[test]
    fn parse_byte_range_invalid_numbers_rejected() {
        assert_eq!(parse_byte_range("bytes=abc-def"), None);
        assert_eq!(parse_byte_range("bytes=-100"), None);
    }
}
