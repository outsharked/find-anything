use std::io::{Cursor, Read};
use std::path::Path;

use anyhow::{Context, Result};
use tracing::warn;

use find_extract_types::{IndexLine, LINE_CONTENT_START, LINE_METADATA};
use find_extract_types::ExtractorConfig;

use super::{CB, MemberBatch, extract_member_bytes};

/// True for Apple iWork extensions (.pages, .numbers, .key).
/// These are ZIP-based documents; only `preview.jpg` is worth extracting.
pub fn is_iwork_ext(ext: &str) -> bool {
    matches!(ext.to_lowercase().as_str(), "pages" | "numbers" | "key")
}

/// Decompress an IWA (iWork Archive) file using the IWA snappy framing.
///
/// Each chunk is: byte 0 = `0x00`, bytes 1–3 = 3-byte LE compressed length,
/// followed by raw snappy-compressed data.  Multiple chunks may follow.
fn iwa_decompress(data: &[u8]) -> Vec<u8> {
    let mut result = Vec::new();
    let mut pos = 0;
    while pos + 4 <= data.len() {
        if data[pos] != 0x00 {
            break;
        }
        let length = (data[pos + 1] as usize)
            | ((data[pos + 2] as usize) << 8)
            | ((data[pos + 3] as usize) << 16);
        pos += 4;
        if pos + length > data.len() {
            break;
        }
        if let Ok(dec) = snap::raw::Decoder::new().decompress_vec(&data[pos..pos + length]) {
            result.extend_from_slice(&dec);
        }
        pos += length;
    }
    result
}

/// Extract human-readable text runs from decompressed IWA protobuf bytes.
///
/// Rather than parsing the full protobuf schema (which requires generated
/// message types for each iWork version), we do a byte-level scan: replace
/// non-printable bytes with separators then collect runs that look like prose.
fn iwa_extract_text(data: &[u8]) -> Vec<String> {
    let text = String::from_utf8_lossy(data);
    let mut results = Vec::new();
    let mut current = String::new();
    for ch in text.chars() {
        if ch == '\u{FFFD}' || (ch.is_control() && ch != '\t' && ch != '\n' && ch != '\r') {
            let trimmed = current.trim().to_owned();
            if trimmed.len() >= 8 {
                let alpha = trimmed.chars().filter(|c| c.is_alphabetic()).count();
                if alpha as f64 / trimmed.chars().count() as f64 >= 0.4 {
                    results.push(trimmed);
                }
            }
            current.clear();
        } else {
            current.push(ch);
        }
    }
    let trimmed = current.trim().to_owned();
    if trimmed.len() >= 8 {
        let alpha = trimmed.chars().filter(|c| c.is_alphabetic()).count();
        if alpha as f64 / trimmed.chars().count() as f64 >= 0.4 {
            results.push(trimmed);
        }
    }
    results
}

/// Extract text from old-format iWork XML (index.apxl / index.xml).
///
/// Old-format iWork files (pre-2013) use ZIP + XML rather than ZIP + IWA.
/// This function strips XML tags and collects meaningful text runs from the
/// raw XML bytes, applying the same quality filter as `iwa_extract_text`.
fn iwork_xml_extract_text(data: &[u8]) -> Vec<String> {
    let text = String::from_utf8_lossy(data);
    let mut results = Vec::new();
    let mut current = String::new();
    let mut in_tag = false;
    for ch in text.chars() {
        if ch == '<' {
            // Flush any accumulated text before this tag.
            let trimmed = current.trim().to_owned();
            if trimmed.len() >= 4 {
                let alpha = trimmed.chars().filter(|c| c.is_alphabetic()).count();
                if alpha as f64 / trimmed.chars().count() as f64 >= 0.4 {
                    for line in trimmed.lines() {
                        let line = line.trim().to_owned();
                        if line.len() >= 4 {
                            results.push(line);
                        }
                    }
                }
            }
            current.clear();
            in_tag = true;
        } else if ch == '>' {
            in_tag = false;
        } else if !in_tag {
            current.push(ch);
        }
    }
    // Flush final run.
    let trimmed = current.trim().to_owned();
    if trimmed.len() >= 4 {
        let alpha = trimmed.chars().filter(|c| c.is_alphabetic()).count();
        if alpha as f64 / trimmed.chars().count() as f64 >= 0.4 {
            for line in trimmed.lines() {
                let line = line.trim().to_owned();
                if line.len() >= 4 {
                    results.push(line);
                }
            }
        }
    }
    results
}

/// Old-format iWork XML entry filenames (pre-2013 format, no .iwa files).
const IWORK_OLD_XML: &[&str] = &["index.apxl", "index.xml"];

/// IWA files that contain document prose.  Stylesheet, ViewState,
/// CalculationEngine, and Annotation files hold no useful text.
const IWA_SKIP_SUFFIXES: &[&str] = &[
    "Stylesheet.iwa",
    "ViewState.iwa",
    "CalculationEngine",
    "Annotation",
    "DocumentMetadata.iwa",
];
const IWA_TEXT_EXACT: &[&str] = &["Index/Document.iwa"];
const IWA_TEXT_PATTERNS: &[&str] = &["Slide", "Tables/", "Sections/"];

fn is_text_iwa(name: &str) -> bool {
    if !name.ends_with(".iwa") { return false; }
    if IWA_SKIP_SUFFIXES.iter().any(|s| name.contains(s)) { return false; }
    IWA_TEXT_EXACT.contains(&name)
        || IWA_TEXT_PATTERNS.iter().any(|p| name.contains(p))
}

/// Open an iWork file as a ZIP, emit the preview image as a member, and
/// extract text from the IWA protobuf archives natively (no Java/Tika needed).
pub(super) fn iwork_streaming(path: &Path, cfg: &ExtractorConfig, callback: CB<'_>) -> Result<()> {
    let file = std::fs::File::open(path)?;
    let mut archive = zip::ZipArchive::new(file).context("opening iwork file as zip")?;
    let display_prefix = path.to_str().unwrap_or("");

    // Collect IWA names first to avoid borrow conflicts.
    let iwa_names: Vec<String> = archive
        .file_names()
        .filter(|n| is_text_iwa(n))
        .map(|n| n.to_owned())
        .collect();

    // Extract text from each relevant IWA file.
    let mut text_lines: Vec<IndexLine> = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    // Prioritise exact matches.
    let mut ordered: Vec<String> = IWA_TEXT_EXACT
        .iter()
        .filter(|e| iwa_names.contains(&e.to_string()))
        .map(|e| e.to_string())
        .collect();
    for name in &iwa_names {
        if !ordered.contains(name) { ordered.push(name.clone()); }
    }
    for name in &ordered {
        let mut entry = match archive.by_name(name) { Ok(e) => e, Err(_) => continue };
        let mut raw = Vec::new();
        if entry.read_to_end(&mut raw).is_err() { continue; }
        let decompressed = iwa_decompress(&raw);
        for s in iwa_extract_text(&decompressed) {
            // Split on embedded newlines so each line gets its own IndexLine.
            // Without this, a multi-line text block stored as one IndexLine
            // gets truncated by get_lines(0, line_count) because line_count
            // only counts IndexLine objects, not \n-separated blob lines.
            for sub in s.lines() {
                let sub = sub.trim_end().to_string();
                if !sub.is_empty() && seen.insert(sub.clone()) {
                    text_lines.push(IndexLine {
                        archive_path: None,
                        line_number: text_lines.len() + 2, // 0=path, 1=metadata
                        content: sub,
                    });
                }
            }
        }
    }

    // Fallback for old-format iWork (pre-2013): no .iwa files, XML instead.
    if text_lines.is_empty() {
        for xml_name in IWORK_OLD_XML {
            if let Ok(mut entry) = archive.by_name(xml_name) {
                let mut raw = Vec::new();
                if entry.read_to_end(&mut raw).is_ok() {
                    for s in iwork_xml_extract_text(&raw) {
                        if seen.insert(s.clone()) {
                            text_lines.push(IndexLine {
                                archive_path: None,
                                line_number: text_lines.len() + 2,
                                content: s,
                            });
                        }
                    }
                }
                break;
            }
        }
    }

    // Build outer_lines: [IWORK_PREVIEW] metadata first (→ LINE_METADATA=1),
    // then any extracted text.  scan.rs re-numbers these starting at 1.
    let preview_name = if archive.by_name("preview.jpg").is_ok() {
        Some("preview.jpg")
    } else if archive.by_name("preview-web.jpg").is_ok() {
        Some("preview-web.jpg")
    } else {
        None
    };
    let mut outer: Vec<IndexLine> = Vec::new();
    if let Some(name) = preview_name {
        outer.push(IndexLine {
            archive_path: None,
            line_number: LINE_METADATA, // placeholder; scan.rs will renumber
            content: format!("[IWORK_PREVIEW] {name}"),
        });
    }
    outer.extend(text_lines);

    // Emit preview image as a member batch; carry outer_lines so they flow
    // to the outer archive file's own content entry in scan.rs.
    let mut emitted = false;
    extract_iwork_preview(&mut archive, display_prefix, cfg, &mut |mut batch: MemberBatch| {
        if !emitted {
            batch.outer_lines = outer.clone();
            emitted = true;
        }
        callback(batch);
    });

    // If there was no preview, still deliver metadata/text via outer_lines.
    if !emitted && !outer.is_empty() {
        callback(MemberBatch { lines: vec![], outer_lines: outer, file_hash: None, skip_reason: None, mtime: None, size: None, delegate_temp_path: None });
    }

    Ok(())
}

/// Extract the iWork preview metadata and IWA text from `bytes` and append to `lines`.
///
/// `entry_name` is the iWork document filename (e.g. `"doc.pages"`).
///
/// Appends a `[IWORK_PREVIEW] <name>` line at LINE_METADATA (if a preview image is found)
/// and IWA text lines at LINE_CONTENT_START+ (if IWA protobuf data is present).
/// The preview is served on demand by the view endpoint; it is not indexed as a separate
/// file entry.  This ensures nested iWork files use the same extraction logic as top-level
/// ones (single code path).
pub(super) fn iwork_extract_preview_into_lines(
    bytes: &[u8],
    entry_name: &str,
    lines: &mut Vec<IndexLine>,
) {
    let cursor = Cursor::new(bytes);
    let mut inner_archive = match zip::ZipArchive::new(cursor) {
        Ok(a) => a,
        Err(_) => return,
    };

    // Collect IWA names and extract text (same logic as iwork_streaming).
    let iwa_names: Vec<String> = inner_archive
        .file_names()
        .filter(|n| is_text_iwa(n))
        .map(|n| n.to_owned())
        .collect();
    let mut text_strings: Vec<String> = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut ordered: Vec<String> = IWA_TEXT_EXACT
        .iter()
        .filter(|e| iwa_names.contains(&e.to_string()))
        .map(|e| e.to_string())
        .collect();
    for name in &iwa_names {
        if !ordered.contains(name) { ordered.push(name.clone()); }
    }
    for name in &ordered {
        let mut entry = match inner_archive.by_name(name) { Ok(e) => e, Err(_) => continue };
        let mut raw = Vec::new();
        if entry.read_to_end(&mut raw).is_err() { continue; }
        let decompressed = iwa_decompress(&raw);
        for s in iwa_extract_text(&decompressed) {
            for sub in s.lines() {
                let sub = sub.trim_end().to_string();
                if !sub.is_empty() && seen.insert(sub.clone()) {
                    text_strings.push(sub);
                }
            }
        }
    }

    // Fallback for old-format iWork (pre-2013): no .iwa files, XML instead.
    if text_strings.is_empty() {
        for xml_name in IWORK_OLD_XML {
            if let Ok(mut entry) = inner_archive.by_name(xml_name) {
                let mut raw = Vec::new();
                if entry.read_to_end(&mut raw).is_ok() {
                    for s in iwork_xml_extract_text(&raw) {
                        if seen.insert(s.clone()) {
                            text_strings.push(s);
                        }
                    }
                }
                break;
            }
        }
    }

    // Detect preview.
    let preview_name = if inner_archive.by_name("preview.jpg").is_ok() {
        Some("preview.jpg")
    } else if inner_archive.by_name("preview-web.jpg").is_ok() {
        Some("preview-web.jpg")
    } else {
        None
    };

    if let Some(pname) = preview_name {
        lines.push(IndexLine {
            archive_path: Some(entry_name.to_string()),
            line_number: LINE_METADATA,
            content: format!("[IWORK_PREVIEW] {pname}"),
        });
    }
    for (i, s) in text_strings.into_iter().enumerate() {
        lines.push(IndexLine {
            archive_path: Some(entry_name.to_string()),
            line_number: LINE_CONTENT_START + i,
            content: s,
        });
    }
}

/// Find `preview.jpg` (or `preview-web.jpg`) inside an iWork ZIP and emit it
/// as a `MemberBatch`.  Called for both top-level files and nested members.
fn extract_iwork_preview<R: Read + std::io::Seek>(
    archive: &mut zip::ZipArchive<R>,
    display_prefix: &str,
    cfg: &ExtractorConfig,
    callback: CB<'_>,
) {
    let preview_name = if archive.by_name("preview.jpg").is_ok() {
        "preview.jpg"
    } else if archive.by_name("preview-web.jpg").is_ok() {
        "preview-web.jpg"
    } else {
        return; // no preview available
    };

    let mut entry = match archive.by_name(preview_name) {
        Ok(e) => e,
        Err(e) => { warn!("iwork: failed to open {preview_name} in {display_prefix}: {e:#}"); return; }
    };

    let size_limit = cfg.max_content_kb * 1024;
    let member_size = Some(entry.size());
    let mut bytes = Vec::new();
    if let Err(e) = (&mut entry as &mut dyn Read).take(size_limit as u64).read_to_end(&mut bytes) {
        warn!("iwork: failed to read {preview_name} in {display_prefix}: {e:#}");
        return;
    }
    let file_hash = find_extract_types::content_hash(&bytes);
    let lines = extract_member_bytes(bytes, preview_name, display_prefix, cfg);
    callback(MemberBatch { lines, file_hash, skip_reason: None, mtime: None, size: member_size, delegate_temp_path: None, outer_lines: vec![] });
}
