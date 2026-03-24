#![allow(dead_code)] // functions are used by different binaries in this crate

use std::collections::HashMap;
use std::io::Read;
use std::path::Path;

use anyhow::Result;
use find_common::api::{BulkRequest, FileKind, IndexFile, IndexingFailure, IndexLine, SCANNER_VERSION, LINE_PATH, LINE_METADATA, LINE_CONTENT_START};

use crate::api::ApiClient;

/// Content-store key for a file: blake3 of its raw bytes mixed with
/// [`SCANNER_VERSION`].  Including the scanner version ensures that upgrading
/// extraction logic produces a new key, so old blobs become orphaned and
/// compaction can remove them while fresh content gets stored.
/// Returns `None` for empty files.
pub(crate) fn hash_file(path: &Path) -> Option<String> {
    let mut file = std::fs::File::open(path).ok()?;
    let mut hasher = blake3::Hasher::new();
    let mut buf = [0u8; 65536];
    let mut total = 0usize;
    loop {
        let n = file.read(&mut buf).ok()?;
        if n == 0 { break; }
        hasher.update(&buf[..n]);
        total += n;
    }
    if total == 0 { return None; }
    hasher.update(&SCANNER_VERSION.to_le_bytes());
    Some(hasher.finalize().to_hex().to_string())
}

/// Ensure the metadata slot (line 1) is present, inserting an empty placeholder if needed.
///
/// The server stores inline content as a `'\n'`-joined string indexed by position:
/// line 0 = path, line 1 = metadata, line 2+ = content.  A gap at line 1 would
/// shift all subsequent content lines down by one and corrupt positional reads.
/// Normalise a member's line list for submission:
/// - clears `archive_path` on every line (redundant once the path is composite),
/// - replaces the extractor's `LINE_PATH` marker with one using `composite_path`,
/// - ensures the `LINE_METADATA` slot is present.
fn prepare_member_lines(lines: &mut Vec<IndexLine>, composite_path: &str) {
    for l in lines.iter_mut() {
        l.archive_path = None;
    }
    lines.retain(|l| l.line_number != LINE_PATH);
    lines.push(IndexLine {
        archive_path: None,
        line_number: LINE_PATH,
        content: format!("[PATH] {}", composite_path),
    });
    ensure_metadata_slot(lines);
}

fn ensure_metadata_slot(lines: &mut Vec<IndexLine>) {
    if !lines.iter().any(|l| l.line_number == LINE_METADATA) {
        lines.push(IndexLine {
            archive_path: None,
            line_number: LINE_METADATA,
            content: String::new(),
        });
    }
}

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
    kind: FileKind,
    lines: Vec<IndexLine>,
) -> Vec<IndexFile> {
    let has_archive_members = lines.iter().any(|l| l.archive_path.is_some());

    if !has_archive_members {
        // Non-archive (or archive with no extractable text members): single IndexFile.
        let mut all_lines = lines;
        // Always index the relative path so the file is findable by name.
        all_lines.push(IndexLine {
            archive_path: None,
            line_number: LINE_PATH,
            content: format!("[PATH] {}", rel_path),
        });
        ensure_metadata_slot(&mut all_lines);
        return vec![IndexFile { path: rel_path, mtime, size: Some(size), kind, lines: all_lines, extract_ms: None, file_hash: None, scanner_version: SCANNER_VERSION, is_new: false }];
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
        line_number: LINE_PATH,
        content: format!("[PATH] {}", rel_path),
    });
    ensure_metadata_slot(&mut outer_lines);
    result.push(IndexFile {
        path: rel_path.clone(),
        mtime,
        size: Some(size),
        kind: kind.clone(),
        lines: outer_lines,
        extract_ms: None,
        file_hash: None,
        scanner_version: SCANNER_VERSION,
        is_new: false,
    });

    // One IndexFile per archive member, with composite path "zip::member".
    for (member, mut content_lines) in member_groups {
        let composite_path = format!("{}::{}", rel_path, member);
        prepare_member_lines(&mut content_lines, &composite_path);
        // Detect the member's actual kind from its filename extension.
        let ext = Path::new(&member)
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("");
        let member_kind = FileKind::from_extension(ext);
        result.push(IndexFile {
            path: composite_path,
            mtime,
            size: None, // individual archive member sizes are not available
            kind: member_kind,
            lines: content_lines,
            extract_ms: None,
            file_hash: None,
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
    member_size: Option<u64>,
    member_lines: Vec<IndexLine>,
    file_hash: Option<String>,
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
        prepare_member_lines(&mut lines, &composite_path);
        let ext = Path::new(&member)
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("");
        let mut member_kind = FileKind::from_extension(ext);
        // Refine Unknown or Text kind using extracted content:
        // - A [FILE:mime] line emitted by dispatch (at LINE_METADATA) means binary → use mime_to_kind.
        // - Text content lines (line_number >= LINE_CONTENT_START) present → promote to Text.
        if member_kind == FileKind::Text || member_kind == FileKind::Unknown {
            if let Some(mime_line) = lines.iter().find(|l| l.line_number == LINE_METADATA && l.content.starts_with("[FILE:mime] ")) {
                let mime = &mime_line.content["[FILE:mime] ".len()..];
                member_kind = FileKind::from(find_extract_dispatch::mime_to_kind(mime));
            } else if lines.iter().any(|l| l.line_number >= LINE_CONTENT_START) {
                member_kind = FileKind::Text;
            }
        }
        result.push(IndexFile {
            path: composite_path,
            mtime,
            size: member_size.map(|s| s as i64),
            kind: member_kind,
            lines,
            extract_ms: None,
            file_hash: file_hash.clone(),
            scanner_version: SCANNER_VERSION,
            is_new: false,
        });
    }
    result
}

/// Returns the total byte size of all content lines in an `IndexFile`.
///
/// Used by the scan loop to enforce the byte-budget flush threshold
/// (`scan.batch_bytes`). Counts raw string bytes — not compressed size —
/// but serves as a reliable upper-bound proxy for payload size.
pub fn index_file_bytes(file: &IndexFile) -> usize {
    file.lines.iter().map(|l| l.content.len()).sum()
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
        let files = build_index_files("readme.md".into(), 100, 200, FileKind::Text, vec![]);
        assert_eq!(files.len(), 1);
        let f = &files[0];
        assert_eq!(f.path, "readme.md");
        assert_eq!(f.kind, FileKind::Text);
        assert_eq!(f.mtime, 100);
        assert_eq!(f.size, Some(200));
        // Should have the path line (0) and empty metadata line (1).
        assert_eq!(f.lines.len(), 2);
        assert!(f.lines.iter().any(|l| l.line_number == LINE_PATH && l.content == "[PATH] readme.md"));
        assert!(f.lines.iter().any(|l| l.line_number == LINE_METADATA && l.content.is_empty()));
    }

    #[test]
    fn non_archive_with_content_lines() {
        let lines = vec![
            line(None, LINE_CONTENT_START, "hello"),
            line(None, LINE_CONTENT_START + 1, "world"),
        ];
        let files = build_index_files("src/main.rs".into(), 0, 0, FileKind::Text, lines);
        assert_eq!(files.len(), 1);
        let f = &files[0];
        // Content lines + path line + empty metadata line.
        assert_eq!(f.lines.len(), 4);
        assert!(f.lines.iter().any(|l| l.line_number == LINE_PATH && l.content == "[PATH] src/main.rs"));
        assert!(f.lines.iter().any(|l| l.line_number == LINE_METADATA && l.content.is_empty()));
        assert!(f.lines.iter().any(|l| l.line_number == LINE_CONTENT_START && l.content == "hello"));
        assert!(f.lines.iter().any(|l| l.line_number == LINE_CONTENT_START + 1 && l.content == "world"));
    }

    // ── Archive files ──────────────────────────────────────────────────────

    #[test]
    fn archive_produces_outer_and_member_files() {
        let lines = vec![
            line(Some("report.txt"), LINE_CONTENT_START, "quarterly"),
            line(Some("report.txt"), LINE_CONTENT_START + 1, "results"),
            line(Some("photo.jpg"), LINE_CONTENT_START, "exif data"),
        ];
        let files = build_index_files("data.zip".into(), 10, 20, FileKind::Archive, lines);

        // Outer file + one per distinct member.
        assert_eq!(files.len(), 3);

        let outer = files.iter().find(|f| f.path == "data.zip").unwrap();
        assert_eq!(outer.kind, FileKind::Archive);
        assert!(outer.lines.iter().any(|l| l.line_number == LINE_PATH && l.content == "[PATH] data.zip"));

        let report = files.iter().find(|f| f.path == "data.zip::report.txt").unwrap();
        assert_eq!(report.kind, FileKind::Text);
        assert_eq!(report.mtime, 10);
        // archive_path stripped from member lines.
        assert!(report.lines.iter().all(|l| l.archive_path.is_none()));
        // Path line present.
        assert!(report.lines.iter().any(|l| l.line_number == LINE_PATH && l.content == "[PATH] data.zip::report.txt"));
        // Content lines present.
        assert!(report.lines.iter().any(|l| l.content == "quarterly"));
        assert!(report.lines.iter().any(|l| l.content == "results"));

        let photo = files.iter().find(|f| f.path == "data.zip::photo.jpg").unwrap();
        assert_eq!(photo.kind, FileKind::Image);
        assert!(photo.lines.iter().any(|l| l.line_number == LINE_PATH && l.content == "[PATH] data.zip::photo.jpg"));
    }

    #[test]
    fn archive_member_kinds_detected_from_extension() {
        let cases = [
            ("doc.pdf",   FileKind::Pdf),
            ("song.mp3",  FileKind::Audio),
            ("clip.mp4",  FileKind::Video),
            ("inner.zip", FileKind::Archive),
            ("data.rs",   FileKind::Code),
        ];
        for (member_name, expected_kind) in &cases {
            let lines = vec![line(Some(member_name), LINE_CONTENT_START, "content")];
            let files = build_index_files("outer.zip".into(), 0, 0, FileKind::Archive, lines);
            let member = files
                .iter()
                .find(|f| f.path == format!("outer.zip::{member_name}"))
                .unwrap_or_else(|| panic!("member {member_name} not found"));
            assert_eq!(&member.kind, expected_kind, "member={member_name}");
        }
    }

    // ── Byte budget ────────────────────────────────────────────────────────

    #[test]
    fn index_file_bytes_counts_all_content() {
        let lines = vec![
            line(None, LINE_METADATA, "hello world"),  // 11 bytes (metadata)
            line(None, LINE_CONTENT_START, "foo"),     // 3 bytes (content)
        ];
        let files = build_index_files("src/main.rs".into(), 0, 0, FileKind::Text, lines);
        // build_index_files appends [PATH] line (18 bytes); LINE_METADATA is present so no empty added.
        let total = super::index_file_bytes(&files[0]);
        // "hello world"=11, "foo"=3, "[PATH] src/main.rs"=18 → 32
        assert_eq!(total, 32);
    }

    #[test]
    fn index_file_bytes_large_file_exceeds_budget() {
        // Synthesise a file whose content exceeds an 8 KB budget even with only 2 lines.
        let big_line = "x".repeat(5_000);
        let lines = vec![
            line(None, LINE_CONTENT_START, &big_line),
            line(None, LINE_CONTENT_START + 1, &big_line),
        ];
        let files = build_index_files("big.txt".into(), 0, 0, FileKind::Text, lines);
        let bytes = super::index_file_bytes(&files[0]);
        // 5000 + 5000 + len("big.txt") = 10007 — exceeds an 8192-byte budget.
        assert!(bytes > 8_192, "expected bytes > 8192, got {bytes}");
    }

    #[test]
    fn index_file_bytes_empty_file() {
        let files = build_index_files("empty.txt".into(), 0, 0, FileKind::Text, vec![]);
        let bytes = super::index_file_bytes(&files[0]);
        // Only the path line: len("[PATH] empty.txt") = 16.
        assert_eq!(bytes, 16);
    }

    // ── build_member_index_files ───────────────────────────────────────────

    #[test]
    fn member_size_propagates_to_index_file() {
        let lines = vec![line(Some("notes.txt"), LINE_CONTENT_START, "hello")];
        let files = build_member_index_files("data.zip", 1000, Some(4096), lines, None);
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].path, "data.zip::notes.txt");
        assert_eq!(files[0].size, Some(4096));
    }

    #[test]
    fn member_size_none_when_not_available() {
        let lines = vec![line(Some("notes.txt"), LINE_CONTENT_START, "hello")];
        let files = build_member_index_files("data.zip", 1000, None, lines, None);
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].size, None);
    }

    #[test]
    fn archive_outer_lines_not_in_members() {
        // Lines without an archive_path belong to the outer file, not any member.
        let lines = vec![
            line(None, LINE_CONTENT_START, "outer-only content"),
            line(Some("inner.txt"), LINE_CONTENT_START, "inner content"),
        ];
        let files = build_index_files("pkg.tar.gz".into(), 0, 0, FileKind::Archive, lines);
        let outer = files.iter().find(|f| f.path == "pkg.tar.gz").unwrap();
        assert!(outer.lines.iter().any(|l| l.content == "outer-only content"));
        let inner = files.iter().find(|f| f.path == "pkg.tar.gz::inner.txt").unwrap();
        assert!(inner.lines.iter().all(|l| l.content != "outer-only content"));
    }
}
