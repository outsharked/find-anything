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

// ── Minimal protobuf primitives ──────────────────────────────────────────────

/// Read a protobuf varint from `data` starting at `*pos`, advancing `*pos`.
fn pb_varint(data: &[u8], pos: &mut usize) -> Option<u64> {
    let mut result: u64 = 0;
    let mut shift = 0u32;
    loop {
        if *pos >= data.len() { return None; }
        let byte = data[*pos];
        *pos += 1;
        result |= ((byte & 0x7F) as u64) << shift;
        if byte & 0x80 == 0 { return Some(result); }
        shift += 7;
        if shift >= 64 { return None; }
    }
}

/// Skip one protobuf field value of the given `wire_type`.
fn pb_skip(wire_type: u64, data: &[u8], pos: &mut usize) -> Option<()> {
    match wire_type {
        0 => { pb_varint(data, pos)?; }
        1 => { *pos = pos.checked_add(8)?; if *pos > data.len() { return None; } }
        2 => { let n = pb_varint(data, pos)? as usize; *pos = pos.checked_add(n)?; if *pos > data.len() { return None; } }
        5 => { *pos = pos.checked_add(4)?; if *pos > data.len() { return None; } }
        _ => return None,
    }
    Some(())
}

// ── IWA record stream parser ─────────────────────────────────────────────────

/// Type ID of `TSWP.StorageArchive` in the iWork type registry.
const STORAGE_ARCHIVE_TYPE: u32 = 2001;

/// Parse a `TSP.ArchiveInfo.MessageInfo` sub-message and return (type_id, payload_length).
///
/// MessageInfo fields (TSPArchiveMessages.proto):
///   1 = type (uint32), 2 = version (packed uint32), 3 = length (uint32)
fn parse_message_info(data: &[u8]) -> Option<(u32, u32)> {
    let mut pos = 0;
    let mut type_id: Option<u32> = None;
    let mut length: Option<u32> = None;
    while pos < data.len() {
        let key = pb_varint(data, &mut pos)?;
        let (field, wire_type) = ((key >> 3) as u32, key & 7);
        match field {
            1 if wire_type == 0 => { type_id  = Some(pb_varint(data, &mut pos)? as u32); }
            3 if wire_type == 0 => { length   = Some(pb_varint(data, &mut pos)? as u32); }
            _ => { pb_skip(wire_type, data, &mut pos)?; }
        }
    }
    Some((type_id?, length?))
}

/// Parse a `TSP.ArchiveInfo` header and return the list of (type_id, payload_length) pairs.
///
/// ArchiveInfo fields: 1 = identifier (uint64), 2 = message_infos (repeated MessageInfo).
fn parse_archive_info(data: &[u8]) -> Vec<(u32, u32)> {
    let mut pos = 0;
    let mut infos = Vec::new();
    while pos < data.len() {
        let Some(key) = pb_varint(data, &mut pos) else { break };
        let (field, wire_type) = ((key >> 3) as u32, key & 7);
        if field == 2 && wire_type == 2 {
            let Some(n) = pb_varint(data, &mut pos) else { break };
            let n = n as usize;
            if pos + n > data.len() { break; }
            if let Some(mi) = parse_message_info(&data[pos..pos + n]) { infos.push(mi); }
            pos += n;
        } else if pb_skip(wire_type, data, &mut pos).is_none() {
            break;
        }
    }
    infos
}

/// Extract `repeated string text` (field 3) from a `TSWP.StorageArchive` payload.
fn extract_storage_archive_text(payload: &[u8]) -> Vec<String> {
    let mut pos = 0;
    let mut out = Vec::new();
    while pos < payload.len() {
        let Some(key) = pb_varint(payload, &mut pos) else { break };
        let (field, wire_type) = ((key >> 3) as u32, key & 7);
        if field == 3 && wire_type == 2 {
            let Some(n) = pb_varint(payload, &mut pos) else { break };
            let n = n as usize;
            if pos + n > payload.len() { break; }
            if let Ok(s) = std::str::from_utf8(&payload[pos..pos + n]) {
                let s = s.trim_end_matches('\n');
                if !s.is_empty() { out.push(s.to_owned()); }
            }
            pos += n;
        } else if pb_skip(wire_type, payload, &mut pos).is_none() {
            break;
        }
    }
    out
}

// ── Numbers spreadsheet cell extraction ──────────────────────────────────────

/// Type ID of `TST.Tile` — the Numbers cell storage container.
const TILE_TYPE: u32 = 6002;

/// Extract the string value from a single `TST.Cell` protobuf payload.
///
/// Cell fields (TSTArchives.proto):
///   2 = valueType (varint): 2 = stringCellValueType
///   6 = stringValue (string)
///
/// Returns `Some(s)` only when valueType == 2 (string cell).
fn extract_cell_string(data: &[u8]) -> Option<String> {
    let mut pos = 0;
    let mut value_type: Option<u32> = None;
    let mut string_value: Option<String> = None;
    while pos < data.len() {
        let Some(key) = pb_varint(data, &mut pos) else { break };
        let (field, wire_type) = ((key >> 3) as u32, key & 7);
        match field {
            2 if wire_type == 0 => { value_type = Some(pb_varint(data, &mut pos)? as u32); }
            6 if wire_type == 2 => {
                let Some(n) = pb_varint(data, &mut pos) else { break };
                let n = n as usize;
                if pos + n > data.len() { break; }
                if let Ok(s) = std::str::from_utf8(&data[pos..pos + n]) {
                    let s = s.trim().to_owned();
                    if !s.is_empty() { string_value = Some(s); }
                }
                pos += n;
            }
            _ => { if pb_skip(wire_type, data, &mut pos).is_none() { break; } }
        }
    }
    if value_type == Some(2) { string_value } else { None }
}

/// Extract string cell values from a single `TST.TileRowInfo` protobuf payload.
///
/// TileRowInfo fields:
///   3 = cellStorageBuffer (bytes) — packed Cell protobuf payloads
///   4 = cellOffsets (bytes)       — uint16 LE byte offsets into cellStorageBuffer
fn extract_tile_row_strings(data: &[u8]) -> Vec<String> {
    let mut pos = 0;
    let mut cell_buf: &[u8] = &[];
    let mut offsets_raw: &[u8] = &[];
    while pos < data.len() {
        let Some(key) = pb_varint(data, &mut pos) else { break };
        let (field, wire_type) = ((key >> 3) as u32, key & 7);
        if wire_type == 2 {
            let Some(n) = pb_varint(data, &mut pos) else { break };
            let n = n as usize;
            if pos + n > data.len() { break; }
            match field {
                3 => cell_buf   = &data[pos..pos + n],
                4 => offsets_raw = &data[pos..pos + n],
                _ => {}
            }
            pos += n;
        } else if pb_skip(wire_type, data, &mut pos).is_none() {
            break;
        }
    }
    if cell_buf.is_empty() || !offsets_raw.len().is_multiple_of(2) { return Vec::new(); }
    // cellOffsets is a tightly-packed array of uint16 LE byte offsets.
    let offsets: Vec<usize> = offsets_raw.as_chunks::<2>().0.iter()
        .map(|c| u16::from_le_bytes(*c) as usize)
        .collect();
    let mut out = Vec::new();
    for (i, &start) in offsets.iter().enumerate() {
        let end = if i + 1 < offsets.len() { offsets[i + 1] } else { cell_buf.len() };
        if start < end && end <= cell_buf.len() {
            if let Some(s) = extract_cell_string(&cell_buf[start..end]) {
                out.push(s);
            }
        }
    }
    out
}

/// Extract string cell values from a `TST.Tile` (type 6002) payload.
///
/// Tile fields: 5 = row_infos (repeated TileRowInfo)
fn extract_tile_text(payload: &[u8]) -> Vec<String> {
    let mut pos = 0;
    let mut out = Vec::new();
    while pos < payload.len() {
        let Some(key) = pb_varint(payload, &mut pos) else { break };
        let (field, wire_type) = ((key >> 3) as u32, key & 7);
        if field == 5 && wire_type == 2 {
            let Some(n) = pb_varint(payload, &mut pos) else { break };
            let n = n as usize;
            if pos + n > payload.len() { break; }
            out.extend(extract_tile_row_strings(&payload[pos..pos + n]));
            pos += n;
        } else if pb_skip(wire_type, payload, &mut pos).is_none() {
            break;
        }
    }
    out
}

// ── IWA record stream dispatcher ─────────────────────────────────────────────

/// Extract text from decompressed IWA bytes by parsing the protobuf record stream.
///
/// IWA record framing (after Snappy decompression):
///   [varint header_len] [ArchiveInfo bytes] [payload₀] [payload₁] …
///
/// Handles two record types:
///   2001 — TSWP.StorageArchive: Pages/Keynote document text (field 3, repeated string)
///   6002 — TST.Tile: Numbers spreadsheet string cell values (via TileRowInfo cell buffers)
fn iwa_extract_text(data: &[u8]) -> Vec<String> {
    let mut pos = 0;
    let mut results = Vec::new();
    while pos < data.len() {
        let header_len = match pb_varint(data, &mut pos) {
            Some(l) => l as usize,
            None => break,
        };
        if pos + header_len > data.len() { break; }
        let infos = parse_archive_info(&data[pos..pos + header_len]);
        pos += header_len;
        for (type_id, payload_len) in infos {
            let n = payload_len as usize;
            if pos + n > data.len() { break; }
            match type_id {
                STORAGE_ARCHIVE_TYPE => results.extend(extract_storage_archive_text(&data[pos..pos + n])),
                TILE_TYPE            => results.extend(extract_tile_text(&data[pos..pos + n])),
                _                    => {}
            }
            pos += n;
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

fn is_iwa(name: &str) -> bool {
    name.ends_with(".iwa")
}

/// Open an iWork file as a ZIP, emit the preview image as a member, and
/// extract text from the IWA protobuf archives natively (no Java/Tika needed).
pub(super) fn iwork_streaming(path: &Path, cfg: &ExtractorConfig, callback: CB<'_>) -> Result<()> {
    let file = std::fs::File::open(path)?;
    let mut archive = zip::ZipArchive::new(file).context("opening iwork file as zip")?;
    let display_prefix = path.to_str().unwrap_or("");

    // Collect all .iwa filenames first to avoid borrow conflicts.
    // Type-based filtering (StorageArchive type 2001) means we don't need to
    // pre-filter by filename — non-text .iwa files simply yield no records.
    let iwa_names: Vec<String> = archive
        .file_names()
        .filter(|n| is_iwa(n))
        .map(|n| n.to_owned())
        .collect();

    // Extract text from every .iwa file; iwa_extract_text only reads
    // TSWP.StorageArchive (type 2001) records, so metadata/style noise is
    // filtered at the record level rather than at the filename level.
    let mut text_lines: Vec<IndexLine> = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    for name in &iwa_names {
        let mut entry = match archive.by_name(name) { Ok(e) => e, Err(_) => continue };
        let mut raw = Vec::new();
        if entry.read_to_end(&mut raw).is_err() { continue; }
        let decompressed = iwa_decompress(&raw);
        for s in iwa_extract_text(&decompressed) {
            // StorageArchive.text strings use '\n' as paragraph separator; split
            // each string so every paragraph becomes its own IndexLine.
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

    let iwa_names: Vec<String> = inner_archive
        .file_names()
        .filter(|n| is_iwa(n))
        .map(|n| n.to_owned())
        .collect();
    let mut text_strings: Vec<String> = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    for name in &iwa_names {
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
