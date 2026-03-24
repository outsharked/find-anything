use std::path::Path;

use anyhow::Result;
use find_extract_types::{IndexLine, LINE_METADATA};
use find_extract_types::ExtractorConfig;
use tracing::warn;

/// Dispatch extraction from in-memory bytes.
///
/// Runs extractors in priority order:
///   PDF → DICOM → media → HTML → office → EPUB → PE → text → MIME fallback
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

    // ── DICOM (before media — extensionless DICOM must be caught by magic bytes) ─
    if find_extract_dicom::accepts(member_path) || find_extract_dicom::accepts_bytes(bytes) {
        match find_extract_dicom::extract_from_bytes(bytes, name, cfg) {
            Ok(lines) => return lines,
            Err(e) => warn!("DICOM extraction failed for '{}': {}", name, e),
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
            line_number: LINE_METADATA,
            content: format!("[FILE:mime] {}", mime),
        });
    }
    lines
}

/// Dispatch extraction from a file path.
///
/// Does NOT handle archives — the caller is responsible for routing
/// archive files to `find-extract-archive` before calling this.
///
/// Reading strategy:
/// - Specialised extractors (PDF, media, office, etc.) need the full content,
///   so those files are read up to `cfg.max_content_kb`.
/// - Everything else: read 512 bytes first and sniff.  Only read the rest
///   if the content looks like text; binary files stop at the sniff buffer.
pub fn dispatch_from_path(path: &Path, cfg: &ExtractorConfig) -> Result<Vec<IndexLine>> {
    use std::io::Read;

    let name = path.to_string_lossy();
    let limit = (cfg.max_content_kb as u64 * 1024).max(8192);

    let claimed_by_specialist = find_extract_pdf::accepts(path)
        || find_extract_dicom::accepts(path)
        || find_extract_media::accepts(path)
        || find_extract_html::accepts(path)
        || find_extract_office::accepts(path)
        || find_extract_epub::accepts(path)
        || find_extract_pe::accepts(path);

    macro_rules! open {
        ($p:expr) => {
            match std::fs::File::open($p) {
                Ok(f) => f,
                Err(e) => { warn!("skipping {}: {e}", $p.display()); return Ok(vec![]); }
            }
        };
    }

    let bytes = if claimed_by_specialist {
        // Specialist extractor needs the full content.
        let mut buf = Vec::new();
        if let Err(e) = open!(path).take(limit).read_to_end(&mut buf) {
            warn!("skipping {} (read error): {e}", path.display());
            return Ok(vec![]);
        }
        buf
    } else if find_extract_text::is_binary_ext_path(path) {
        // Known binary extension with no specialist extractor (e.g. .vhdx, .iso,
        // .vmdk).  Opening these files on Windows can block if they are in use
        // (e.g. the live WSL2 ext4.vhdx held open by Hyper-V).  There is no
        // content to extract anyway, so skip file I/O entirely.
        return Ok(vec![]);
    } else {
        // Unknown extension: sniff the first 512 bytes to decide whether to read more.
        let mut f = open!(path);
        let mut sniff = vec![0u8; 512];
        let n = match f.read(&mut sniff) {
            Ok(n) => n,
            Err(e) => { warn!("skipping {} (read error): {e}", path.display()); return Ok(vec![]); }
        };
        sniff.truncate(n);

        // DICOM magic at offset 128 — re-read full file before dispatching.
        if find_extract_dicom::accepts_bytes(&sniff) {
            let mut buf = Vec::new();
            if let Err(e) = open!(path).take(limit).read_to_end(&mut buf) {
                warn!("skipping {} (read error): {e}", path.display());
                return Ok(vec![]);
            }
            return Ok(dispatch_from_bytes(&buf, &name, cfg));
        }

        if find_extract_text::accepts_bytes(path, &sniff) {
            // Looks like text — read the rest up to the limit.
            let remaining = limit.saturating_sub(sniff.len() as u64);
            let _ = f.take(remaining).read_to_end(&mut sniff); // partial read is fine
        }
        sniff
    };

    Ok(dispatch_from_bytes(&bytes, &name, cfg))
}

/// Returns `true` if `path` has a known binary extension that no specialist
/// extractor handles (e.g. `.vhdx`, `.iso`, `.vmdk`).
///
/// Use this before any `File::open` call to avoid blocking on locked files
/// (e.g. the live WSL2 `ext4.vhdx` held open by Hyper-V).
pub fn is_binary_ext_path(path: &Path) -> bool {
    find_extract_text::is_binary_ext_path(path)
}

/// Returns `true` if `path` has an extension that is known to block
/// `File::open` indefinitely on Windows (live disk images held open by Hyper-V).
///
/// Use this as the guard before `hash_file` — narrower than `is_binary_ext_path`
/// so that media files (jpg, mp3, etc.) are hashed even though they are binary.
pub fn is_open_blocking_ext_path(path: &Path) -> bool {
    find_extract_text::is_open_blocking_ext_path(path)
}

/// Sniff the kind of a file from its raw bytes using magic byte detection.
///
/// Checks DICOM magic first (requires 132 bytes), then falls back to the
/// `infer` crate. Returns `""` (empty string) if the content is unrecognised
/// or empty — callers should treat `""` as Unknown.
pub fn sniff_kind_from_bytes(bytes: &[u8]) -> &'static str {
    if find_extract_dicom::accepts_bytes(bytes) {
        return "dicom";
    }
    if let Some(t) = infer::get(bytes) {
        let kind = mime_to_kind(t.mime_type());
        if kind != "binary" {
            return kind;
        }
    }
    ""
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
    if mime == "application/pdf"   { return "pdf"; }
    if mime == "application/dicom" { return "dicom"; }
    if matches!(mime,
        "application/zip"
        | "application/x-tar"
        | "application/gzip"
        | "application/x-7z-compressed"
    ) { return "archive"; }
    if mime == "application/octet-stream" { return "binary"; }
    "binary"
}
