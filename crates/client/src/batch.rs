#![allow(dead_code)] // functions are used by different binaries in this crate

use std::collections::HashMap;
use std::path::Path;

use anyhow::Result;
use find_common::api::{detect_kind_from_ext, BulkRequest, IndexFile, IndexingFailure, IndexLine, SCANNER_VERSION};

use crate::api::ApiClient;

/// Convert extracted lines for one filesystem file into one or more IndexFiles.
///
/// For non-archive files: one IndexFile with path = rel_path.
/// For archive files: one IndexFile per distinct archive member (archive_path on the lines),
/// each with a composite path "rel_path::member_path". The outer archive file itself also
/// gets its own IndexFile so it's searchable by name.
pub fn build_index_files(
    rel_path: String,
    mtime: i64,
    size: i64,
    kind: String,
    lines: Vec<IndexLine>,
) -> Vec<IndexFile> {
    let has_archive_members = lines.iter().any(|l| l.archive_path.is_some());

    if !has_archive_members {
        // Non-archive (or archive with no extractable text members): single IndexFile.
        let mut all_lines = lines;
        // Always index the relative path so the file is findable by name.
        all_lines.push(IndexLine {
            archive_path: None,
            line_number: 0,
            content: rel_path.clone(),
        });
        return vec![IndexFile { path: rel_path, mtime, size: Some(size), kind, lines: all_lines, extract_ms: None, content_hash: None, scanner_version: SCANNER_VERSION, is_new: false }];
    }

    // Group by archive_path.
    let mut member_groups: HashMap<String, Vec<IndexLine>> = HashMap::new();
    let mut outer_extra: Vec<IndexLine> = Vec::new();

    for line in lines {
        match line.archive_path.clone() {
            None => outer_extra.push(line),
            Some(member) => member_groups.entry(member).or_default().push(line),
        }
    }

    let mut result = Vec::new();

    // Outer file: searchable by path name.
    let mut outer_lines = outer_extra;
    outer_lines.push(IndexLine {
        archive_path: None,
        line_number: 0,
        content: rel_path.clone(),
    });
    result.push(IndexFile {
        path: rel_path.clone(),
        mtime,
        size: Some(size),
        kind: kind.clone(),
        lines: outer_lines,
        extract_ms: None,
        content_hash: None,
        scanner_version: SCANNER_VERSION,
        is_new: false,
    });

    // One IndexFile per archive member, with composite path "zip::member".
    for (member, mut content_lines) in member_groups {
        let composite_path = format!("{}::{}", rel_path, member);
        // Strip archive_path from individual lines (redundant now that path is composite).
        for l in &mut content_lines {
            l.archive_path = None;
        }
        // Remove the extractor's filename line (member name only); replace with composite path.
        content_lines.retain(|l| l.line_number != 0);
        // Add a line_number=0 entry so the member is findable by name.
        content_lines.push(IndexLine {
            archive_path: None,
            line_number: 0,
            content: composite_path.clone(),
        });
        // Detect the member's actual kind from its filename extension.
        let ext = Path::new(&member)
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("");
        let member_kind = detect_kind_from_ext(ext).to_string();
        result.push(IndexFile {
            path: composite_path,
            mtime,
            size: None, // individual archive member sizes are not available
            kind: member_kind,
            lines: content_lines,
            extract_ms: None,
            content_hash: None,
            scanner_version: SCANNER_VERSION,
            is_new: false,
        });
    }

    result
}

/// Convert one archive member's lines (from streaming extraction) into IndexFiles.
///
/// Unlike `build_index_files`, this is called once per top-level member callback
/// invocation and does NOT produce an IndexFile for the outer archive itself
/// (the caller emits that separately with the accurate `extract_ms`).
///
/// `member_lines` may contain lines spanning multiple archive-path keys when a
/// nested archive (e.g. `inner.zip`) has been recursively expanded — the lines
/// will have paths like `inner.zip` and `inner.zip::foo.txt`.
pub fn build_member_index_files(
    outer_path: &str,
    mtime: i64,
    _size: i64,
    member_lines: Vec<IndexLine>,
    content_hash: Option<String>,
) -> Vec<IndexFile> {
    let mut groups: std::collections::HashMap<String, Vec<IndexLine>> = std::collections::HashMap::new();
    for line in member_lines {
        if let Some(member) = line.archive_path.clone() {
            groups.entry(member).or_default().push(line);
        }
        // Lines without archive_path are ignored (archive extractor always sets it).
    }

    let mut result = Vec::new();
    for (member, mut lines) in groups {
        let composite_path = format!("{}::{}", outer_path, member);
        for l in &mut lines {
            l.archive_path = None;
        }
        // Remove the extractor's filename line (content == member name) but keep
        // metadata lines (EXIF, [FILE:mime], etc.) which also have line_number=0.
        lines.retain(|l| !(l.line_number == 0 && l.content == member));
        lines.push(IndexLine {
            archive_path: None,
            line_number: 0,
            content: composite_path.clone(),
        });
        let ext = Path::new(&member)
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("");
        let mut member_kind = detect_kind_from_ext(ext).to_string();
        // Refine "unknown" or "text" kind using extracted content:
        // - A [FILE:mime] line emitted by dispatch means binary → use mime_to_kind.
        // - Text content lines (line_number > 0) present → promote to "text".
        if member_kind == "text" || member_kind == "unknown" {
            if let Some(mime_line) = lines.iter().find(|l| l.line_number == 0 && l.content.starts_with("[FILE:mime] ")) {
                let mime = &mime_line.content["[FILE:mime] ".len()..];
                member_kind = find_extract_dispatch::mime_to_kind(mime).to_string();
            } else if lines.iter().any(|l| l.line_number > 0) {
                member_kind = "text".to_string();
            }
        }
        result.push(IndexFile {
            path: composite_path,
            mtime,
            size: None, // Individual member sizes are unknown; outer archive size is counted separately
            kind: member_kind,
            lines,
            extract_ms: None,
            content_hash: content_hash.clone(),
            scanner_version: SCANNER_VERSION,
            is_new: false,
        });
    }
    result
}

pub async fn submit_batch(
    api: &ApiClient,
    source_name: &str,
    batch: &mut Vec<IndexFile>,
    failures: &mut Vec<IndexingFailure>,
    delete_paths: Vec<String>,
    scan_timestamp: Option<i64>,
) -> Result<()> {
    let files = std::mem::take(batch);
    let indexing_failures = std::mem::take(failures);
    if files.is_empty() && delete_paths.is_empty() && indexing_failures.is_empty() {
        return Ok(());
    }
    api.bulk(&BulkRequest {
        source: source_name.to_string(),
        files,
        delete_paths,
        scan_timestamp,
        indexing_failures,
        rename_paths: vec![],
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(archive_path: Option<&str>, line_number: usize, content: &str) -> IndexLine {
        IndexLine {
            archive_path: archive_path.map(|s| s.to_string()),
            line_number,
            content: content.to_string(),
        }
    }

    // ── Non-archive files ──────────────────────────────────────────────────

    #[test]
    fn non_archive_no_content_lines() {
        let files = build_index_files("readme.md".into(), 100, 200, "text".into(), vec![]);
        assert_eq!(files.len(), 1);
        let f = &files[0];
        assert_eq!(f.path, "readme.md");
        assert_eq!(f.kind, "text");
        assert_eq!(f.mtime, 100);
        assert_eq!(f.size, Some(200));
        // Should have exactly the path line.
        assert_eq!(f.lines.len(), 1);
        assert_eq!(f.lines[0].line_number, 0);
        assert_eq!(f.lines[0].content, "readme.md");
        assert!(f.lines[0].archive_path.is_none());
    }

    #[test]
    fn non_archive_with_content_lines() {
        let lines = vec![
            line(None, 1, "hello"),
            line(None, 2, "world"),
        ];
        let files = build_index_files("src/main.rs".into(), 0, 0, "text".into(), lines);
        assert_eq!(files.len(), 1);
        let f = &files[0];
        // Content lines + the path line appended at the end.
        assert_eq!(f.lines.len(), 3);
        assert!(f.lines.iter().any(|l| l.line_number == 0 && l.content == "src/main.rs"));
        assert!(f.lines.iter().any(|l| l.line_number == 1 && l.content == "hello"));
        assert!(f.lines.iter().any(|l| l.line_number == 2 && l.content == "world"));
    }

    // ── Archive files ──────────────────────────────────────────────────────

    #[test]
    fn archive_produces_outer_and_member_files() {
        let lines = vec![
            line(Some("report.txt"), 1, "quarterly"),
            line(Some("report.txt"), 2, "results"),
            line(Some("photo.jpg"), 1, "exif data"),
        ];
        let files = build_index_files("data.zip".into(), 10, 20, "archive".into(), lines);

        // Outer file + one per distinct member.
        assert_eq!(files.len(), 3);

        let outer = files.iter().find(|f| f.path == "data.zip").unwrap();
        assert_eq!(outer.kind, "archive");
        assert!(outer.lines.iter().any(|l| l.line_number == 0 && l.content == "data.zip"));

        let report = files.iter().find(|f| f.path == "data.zip::report.txt").unwrap();
        assert_eq!(report.kind, "text");
        assert_eq!(report.mtime, 10);
        assert_eq!(report.size, None); // archive member sizes are not available
        // archive_path stripped from member lines.
        assert!(report.lines.iter().all(|l| l.archive_path.is_none()));
        // Path line present.
        assert!(report.lines.iter().any(|l| l.line_number == 0 && l.content == "data.zip::report.txt"));
        // Content lines present.
        assert!(report.lines.iter().any(|l| l.content == "quarterly"));
        assert!(report.lines.iter().any(|l| l.content == "results"));

        let photo = files.iter().find(|f| f.path == "data.zip::photo.jpg").unwrap();
        assert_eq!(photo.kind, "image");
        assert!(photo.lines.iter().any(|l| l.line_number == 0 && l.content == "data.zip::photo.jpg"));
    }

    #[test]
    fn archive_member_kinds_detected_from_extension() {
        let cases = [
            ("doc.pdf",  "pdf"),
            ("song.mp3", "audio"),
            ("clip.mp4", "video"),
            ("inner.zip","archive"),
            ("data.rs",  "text"),
        ];
        for (member_name, expected_kind) in &cases {
            let lines = vec![line(Some(member_name), 1, "content")];
            let files = build_index_files("outer.zip".into(), 0, 0, "archive".into(), lines);
            let member = files
                .iter()
                .find(|f| f.path == format!("outer.zip::{member_name}"))
                .unwrap_or_else(|| panic!("member {member_name} not found"));
            assert_eq!(&member.kind, expected_kind, "member={member_name}");
        }
    }

    #[test]
    fn archive_outer_lines_not_in_members() {
        // Lines without an archive_path belong to the outer file, not any member.
        let lines = vec![
            line(None, 1, "outer-only content"),
            line(Some("inner.txt"), 1, "inner content"),
        ];
        let files = build_index_files("pkg.tar.gz".into(), 0, 0, "archive".into(), lines);
        let outer = files.iter().find(|f| f.path == "pkg.tar.gz").unwrap();
        assert!(outer.lines.iter().any(|l| l.content == "outer-only content"));
        let inner = files.iter().find(|f| f.path == "pkg.tar.gz::inner.txt").unwrap();
        assert!(inner.lines.iter().all(|l| l.content != "outer-only content"));
    }
}
