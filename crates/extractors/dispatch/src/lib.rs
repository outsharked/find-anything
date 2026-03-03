use std::path::Path;

use anyhow::Result;
use find_common::api::IndexLine;
use find_common::config::ExtractorConfig;
use tracing::warn;

/// Dispatch extraction from in-memory bytes.
///
/// Runs extractors in priority order:
///   PDF → media → HTML → office → EPUB → PE → text → MIME fallback
///
/// Returns content/metadata lines.  Does NOT include a filename line at
/// `line_number = 0` (the caller is responsible for that).  Does NOT set
/// `archive_path` on lines (the caller sets that for archive members).
pub fn dispatch_from_bytes(bytes: &[u8], name: &str, cfg: &ExtractorConfig) -> Vec<IndexLine> {
    let member_path = Path::new(name);

    // ── PDF ───────────────────────────────────────────────────────────────────
    if find_extract_pdf::accepts(member_path) {
        match find_extract_pdf::extract_from_bytes(bytes, name, cfg) {
            Ok(lines) => return lines,
            Err(e) => warn!("PDF extraction failed for '{}': {}", name, e),
        }
        return vec![];
    }

    // ── Media (image / audio / video) ─────────────────────────────────────────
    if find_extract_media::accepts(member_path) {
        match find_extract_media::extract_from_bytes(bytes, name, cfg) {
            Ok(lines) => return lines,
            Err(e) => warn!("media extraction failed for '{}': {}", name, e),
        }
        return vec![];
    }

    // ── HTML (before text — text accepts .html via extension list) ────────────
    if find_extract_html::accepts(member_path) {
        return find_extract_html::extract_from_bytes(bytes, name, cfg);
    }

    // ── Office documents ──────────────────────────────────────────────────────
    if find_extract_office::accepts(member_path) {
        match find_extract_office::extract_from_bytes(bytes, name, cfg) {
            Ok(lines) => return lines,
            Err(e) => warn!("office extraction failed for '{}': {}", name, e),
        }
        return vec![];
    }

    // ── EPUB ──────────────────────────────────────────────────────────────────
    if find_extract_epub::accepts(member_path) {
        match find_extract_epub::extract_from_bytes(bytes, name, cfg) {
            Ok(lines) => return lines,
            Err(e) => warn!("EPUB extraction failed for '{}': {}", name, e),
        }
        return vec![];
    }

    // ── PE executables ────────────────────────────────────────────────────────
    if find_extract_pe::accepts(member_path) {
        match find_extract_pe::extract_from_bytes(bytes, name, cfg) {
            Ok(lines) => return lines,
            Err(e) => warn!("PE extraction failed for '{}': {}", name, e),
        }
        return vec![];
    }

    // ── Text (most permissive — accepts many files by extension or content sniff) ──
    if find_extract_text::accepts_bytes(member_path, bytes) {
        tracing::debug!("text extraction for '{name}' ({} bytes)", bytes.len());
        match find_extract_text::extract_from_bytes(bytes, name, cfg) {
            Ok(lines) => {
                tracing::debug!("text extraction yielded {} lines for '{name}'", lines.len());
                return lines;
            }
            Err(e) => {
                warn!("text extraction failed for '{name}': {e}");
                return vec![];
            }
        }
    } else {
        tracing::debug!("no text extractor matched '{name}' ({} bytes)", bytes.len());
    }

    // ── MIME fallback ─────────────────────────────────────────────────────────
    // If we reached here, content_inspector already rejected the bytes as
    // non-text (accepts_bytes returned false), so the content is binary.
    // Emit [FILE:mime] so the caller can display a real kind rather than
    // "unknown".  Fall back to application/octet-stream when infer has no
    // specific type — this at least ensures we never store "unknown" for
    // content we know is not text.
    let mut lines = vec![];
    if !bytes.is_empty() {
        let mime = infer::get(bytes)
            .map(|t| t.mime_type())
            .unwrap_or("application/octet-stream");
        lines.push(IndexLine {
            archive_path: None,
            line_number: 0,
            content: format!("[FILE:mime] {}", mime),
        });
    }
    lines
}

/// Dispatch extraction from a file path.
///
/// Reads the file into memory and calls `dispatch_from_bytes`.
/// Does NOT handle archives — the caller is responsible for routing
/// archive files to `find-extract-archive` before calling this.
pub fn dispatch_from_path(path: &Path, cfg: &ExtractorConfig) -> Result<Vec<IndexLine>> {
    let bytes = std::fs::read(path)?;
    let name = path.to_string_lossy();
    Ok(dispatch_from_bytes(&bytes, &name, cfg))
}

/// Map a MIME type string to a file kind string.
///
/// This is the single source of truth — previously duplicated in
/// `find-client`'s `extract.rs` and `batch.rs`.
pub fn mime_to_kind(mime: &str) -> &'static str {
    if mime.starts_with("image/") { return "image"; }
    if mime.starts_with("audio/") { return "audio"; }
    if mime.starts_with("video/") { return "video"; }
    if mime.starts_with("text/")  { return "text"; }
    if mime == "application/pdf"  { return "pdf"; }
    if matches!(mime,
        "application/zip"
        | "application/x-tar"
        | "application/gzip"
        | "application/x-7z-compressed"
    ) { return "archive"; }
    if mime == "application/octet-stream" { return "binary"; }
    "binary"
}
