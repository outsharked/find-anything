use serde::{Deserialize, Serialize};

/// Classify a file by its extension alone — no extractor lib deps.
/// Used by `find-watch` (subprocess mode) and `batch.rs` for archive member kinds.
///
/// Returns `"unknown"` for extensions not in any known category.  Callers that
/// also run content extraction (scan.rs, batch.rs) refine "unknown" to "text"
/// or "binary" based on the actual bytes.
pub fn detect_kind_from_ext(ext: &str) -> &'static str {
    match ext.to_lowercase().as_str() {
        "zip" | "tar" | "gz" | "bz2" | "xz" | "tgz" | "tbz2" | "txz" | "7z" => "archive",
        "pdf" => "pdf",
        "jpg" | "jpeg" | "png" | "gif" | "bmp" | "ico" | "webp" | "heic"
        | "tiff" | "tif" | "raw" | "cr2" | "nef" | "arw" => "image",
        "mp3" | "flac" | "ogg" | "m4a" | "aac" | "wav" | "wma" | "opus" => "audio",
        "mp4" | "mkv" | "avi" | "mov" | "wmv" | "webm" | "m4v" | "flv" => "video",
        "docx" | "xlsx" | "xls" | "xlsm" | "pptx" | "epub" => "document",
        // Known binary formats
        "exe" | "dll" | "so" | "dylib" | "sys" | "scr" | "efi"
        | "o" | "a" | "lib" | "obj" | "wasm"
        | "deb" | "rpm" | "pkg" | "msi" | "snap" | "flatpak"
        | "class" | "jar" | "pyc" | "pyd"
        | "bin" | "img" | "iso" | "dmg" | "vmdk" | "vhd" | "qcow2"
        | "db" | "sqlite" | "sqlite3" | "mdb"
        | "ttf" | "otf" | "woff" | "woff2"
        => "binary",
        // Known text formats — we are confident these are human-readable
        "rs" | "ts" | "js" | "mjs" | "cjs" | "jsx" | "tsx"
        | "py" | "rb" | "go" | "java" | "c" | "cpp" | "cc" | "cxx" | "h" | "hpp"
        | "cs" | "swift" | "kt" | "scala" | "r" | "m" | "pl"
        | "sh" | "bash" | "zsh" | "fish" | "ps1" | "bat" | "cmd" | "vbs" | "vba" | "bas" | "cls"
        | "lua" | "el" | "clj" | "hs" | "ml" | "fs" | "ex" | "erl"
        | "dart" | "jl" | "nim" | "zig" | "s" | "asm"
        | "html" | "htm" | "xhtml" | "xml" | "svg" | "css" | "scss" | "sass" | "less"
        | "json" | "yaml" | "yml" | "toml" | "ini" | "cfg" | "conf" | "env"
        | "properties" | "plist" | "nix" | "hcl" | "tf"
        | "csv" | "tsv" | "sql" | "graphql" | "gql" | "proto"
        | "md" | "markdown" | "rst" | "tex" | "adoc" | "org"
        | "txt" | "log" | "diff" | "patch" | "lock"
        | "gitignore" | "gitattributes" | "gitmodules" | "dockerignore"
        | "makefile" | "dockerfile" | "procfile" | "gemfile" | "rakefile"
        | "mod" | "sum" | "cabal" | "gradle" | "sln" | "csproj" | "vcxproj"
        => "text",
        // Everything else: don't guess — let content inspection decide
        _ => "unknown",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_detect_kind_archives() {
        for ext in &["zip", "tar", "gz", "bz2", "xz", "tgz", "tbz2", "txz", "7z"] {
            assert_eq!(detect_kind_from_ext(ext), "archive", "ext={ext}");
        }
    }

    #[test]
    fn test_detect_kind_pdf() {
        assert_eq!(detect_kind_from_ext("pdf"), "pdf");
    }

    #[test]
    fn test_detect_kind_images() {
        for ext in &["jpg", "jpeg", "png", "gif", "bmp", "ico", "webp", "heic",
                     "tiff", "tif", "raw", "cr2", "nef", "arw"] {
            assert_eq!(detect_kind_from_ext(ext), "image", "ext={ext}");
        }
    }

    #[test]
    fn test_detect_kind_audio() {
        for ext in &["mp3", "flac", "ogg", "m4a", "aac", "wav", "wma", "opus"] {
            assert_eq!(detect_kind_from_ext(ext), "audio", "ext={ext}");
        }
    }

    #[test]
    fn test_detect_kind_video() {
        for ext in &["mp4", "mkv", "avi", "mov", "wmv", "webm", "m4v", "flv"] {
            assert_eq!(detect_kind_from_ext(ext), "video", "ext={ext}");
        }
    }

    #[test]
    fn test_detect_kind_known_text_exts() {
        for ext in &["rs", "py", "toml", "md", "txt", "json"] {
            assert_eq!(detect_kind_from_ext(ext), "text", "ext={ext}");
        }
    }

    #[test]
    fn test_detect_kind_unknown_ext_returns_unknown() {
        for ext in &["", "unknown", "xyz", "foobar"] {
            assert_eq!(detect_kind_from_ext(ext), "unknown", "ext={ext}");
        }
    }

    #[test]
    fn test_detect_kind_documents() {
        for ext in &["docx", "xlsx", "xls", "xlsm", "pptx", "epub"] {
            assert_eq!(detect_kind_from_ext(ext), "document", "ext={ext}");
        }
    }

    #[test]
    fn test_detect_kind_case_insensitive() {
        assert_eq!(detect_kind_from_ext("PDF"), "pdf");
        assert_eq!(detect_kind_from_ext("ZIP"), "archive");
        assert_eq!(detect_kind_from_ext("JPG"), "image");
        assert_eq!(detect_kind_from_ext("MP3"), "audio");
        assert_eq!(detect_kind_from_ext("MP4"), "video");
        assert_eq!(detect_kind_from_ext("DOCX"), "document");
    }
}

/// Version of the scanner/extraction logic. Stored with each indexed file so
/// that `find-scan --upgrade` can selectively re-index files that were indexed
/// by an older version of the client. Increment this when extraction logic
/// changes in a way that produces meaningfully different output.
pub const SCANNER_VERSION: u32 = 1;

/// GET /api/v1/sources response entry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SourceInfo {
    pub name: String,
}

/// A single extracted line sent from client → server.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IndexLine {
    /// NULL for regular files; inner path for archive entries; "page:N" for PDFs.
    pub archive_path: Option<String>,
    pub line_number: usize,
    pub content: String,
}

/// A file record sent from client → server.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IndexFile {
    /// Relative path within the source base_path.
    /// For inner archive members this is a composite path: "archive.zip::member.txt".
    /// Nesting is supported: "outer.zip::inner.tar.gz::file.txt".
    pub path: String,
    pub mtime: i64,
    /// Actual byte size of the file. `None` for archive members whose individual
    /// sizes are not available (only the outer archive's size is known).
    #[serde(default)]
    pub size: Option<i64>,
    /// "text" | "pdf" | "archive" | "image" | "audio"
    pub kind: String,
    pub lines: Vec<IndexLine>,
    /// Milliseconds taken to extract content for this file, measured by the client.
    /// Set on the outer file; None for inner archive members.
    #[serde(default)]
    pub extract_ms: Option<u64>,
    /// blake3 hex hash of the file's raw bytes, used for content deduplication.
    /// None for files that could not be hashed (too large, permission error, etc.).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_hash: Option<String>,
    /// Version of the scanner that indexed this file. Compared against
    /// `SCANNER_VERSION` by `find-scan --upgrade` to detect stale entries.
    #[serde(default)]
    pub scanner_version: u32,
}

/// One extraction failure reported by the client.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct IndexingFailure {
    /// Relative path of the file that failed extraction.
    pub path: String,
    /// Error message, truncated to MAX_ERROR_LEN characters.
    pub error: String,
}

/// POST /api/v1/bulk request body.
/// Combines upserts, deletes, and scan-complete into a single async operation.
#[derive(Debug, Serialize, Deserialize)]
pub struct BulkRequest {
    pub source: String,
    /// Files to upsert into the index.
    #[serde(default)]
    pub files: Vec<IndexFile>,
    /// Paths to remove from the index.
    #[serde(default)]
    pub delete_paths: Vec<String>,
    /// If present, update the last_scan timestamp for this source.
    #[serde(default)]
    pub scan_timestamp: Option<i64>,
    /// Extraction failures encountered during this scan batch.
    #[serde(default)]
    pub indexing_failures: Vec<IndexingFailure>,
}

/// One search result.
#[derive(Debug, Serialize, Deserialize)]
pub struct SearchResult {
    pub source: String,
    pub path: String,
    pub archive_path: Option<String>,
    pub line_number: usize,
    pub snippet: String,
    pub score: u32,
    /// File kind (e.g. "text", "pdf", "image").
    pub kind: String,
    /// Unix timestamp (seconds) of last modification.
    pub mtime: i64,
    /// File size in bytes. None for archive members whose individual sizes are unknown.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<i64>,
    /// Populated when ?context=N is passed to the search endpoint.
    #[serde(default)]
    pub context_lines: Vec<ContextLine>,
    /// Other paths with identical content (deduplication aliases).
    /// Empty when there are no duplicates; omitted from JSON.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub aliases: Vec<String>,
    /// Additional lines where query terms were found (document mode only).
    /// Each entry is the best matching line for a term not covered by `line_number`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub extra_matches: Vec<ContextLine>,
}

/// GET /api/v1/search response.
#[derive(Debug, Serialize, Deserialize)]
pub struct SearchResponse {
    pub results: Vec<SearchResult>,
    pub total: usize,
}

/// One line in a context window.
#[derive(Debug, Serialize, Deserialize)]
pub struct ContextLine {
    pub line_number: usize,
    pub content: String,
}

/// GET /api/v1/context response.
#[derive(Debug, Serialize, Deserialize)]
pub struct ContextResponse {
    /// Line number of the first element in `lines`. Client computes each
    /// line's number as `start + index` (approximate — gaps exist in sparse
    /// files like PDFs where empty lines are not stored).
    pub start: usize,
    /// Index within `lines` of the matched line, or null if the center line
    /// was not found in the returned window (e.g. it fell in a gap).
    pub match_index: Option<usize>,
    pub lines: Vec<String>,
    pub kind: String,
}

/// GET /api/v1/file response.
#[derive(Debug, Serialize, Deserialize)]
pub struct FileResponse {
    pub lines: Vec<ContextLine>,
    pub file_kind: String,
    pub total_lines: usize,
    pub mtime: Option<i64>,
    pub size: Option<i64>,
    /// Extraction error message for this file, if one was recorded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub indexing_error: Option<String>,
}

/// GET /api/v1/files response entry (for deletion detection / Ctrl+P).
#[derive(Debug, Serialize, Deserialize)]
pub struct FileRecord {
    pub path: String,
    pub mtime: i64,
    pub kind: String,
    /// Scanner version stored when the file was last indexed. Used by
    /// `find-scan --upgrade` to detect entries that need re-extraction.
    #[serde(default)]
    pub scanner_version: u32,
}

/// One entry in a directory listing.
#[derive(Debug, Serialize, Deserialize)]
pub struct DirEntry {
    /// Last path component (file or directory name).
    pub name: String,
    /// Full relative path within the source, including `::` for archive members.
    pub path: String,
    /// `"dir"` or `"file"`. Archive files have `kind = "archive"` and can be expanded.
    pub entry_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mtime: Option<i64>,
}

/// GET /api/v1/tree response.
#[derive(Debug, Serialize, Deserialize)]
pub struct TreeResponse {
    pub entries: Vec<DirEntry>,
}

/// One item in a POST /api/v1/context-batch request.
#[derive(Debug, Serialize, Deserialize)]
pub struct ContextBatchItem {
    pub source: String,
    pub path: String,
    #[serde(default)]
    pub archive_path: Option<String>,
    pub line: usize,
    #[serde(default = "default_context_window")]
    pub window: usize,
}

fn default_context_window() -> usize { 5 }

/// POST /api/v1/context-batch request body.
#[derive(Debug, Serialize, Deserialize)]
pub struct ContextBatchRequest {
    pub requests: Vec<ContextBatchItem>,
}

/// One result within a POST /api/v1/context-batch response.
#[derive(Debug, Serialize, Deserialize)]
pub struct ContextBatchResult {
    pub source: String,
    pub path: String,
    pub line: usize,
    pub start: usize,
    pub match_index: Option<usize>,
    pub lines: Vec<String>,
    pub kind: String,
}

/// POST /api/v1/context-batch response.
#[derive(Debug, Serialize, Deserialize)]
pub struct ContextBatchResponse {
    pub results: Vec<ContextBatchResult>,
}

/// GET /api/v1/settings response — display configuration for the web UI.
#[derive(Debug, Serialize, Deserialize)]
pub struct AppSettingsResponse {
    /// Lines shown before and after each match in search result cards.
    /// Total lines = 2 × context_window + 1.
    pub context_window: usize,
    /// Server version string (from Cargo.toml).
    pub version: String,
    /// SQLite schema version for all source databases.
    pub schema_version: i64,
    /// Short git commit hash baked in at compile time.
    pub git_hash: String,
}

// ── Stats types ───────────────────────────────────────────────────────────────

/// Per-kind breakdown entry in `SourceStats`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KindStats {
    pub count: usize,
    pub size: i64,
    pub avg_extract_ms: Option<f64>,
}

/// Per-extension breakdown entry in `SourceStats`.
/// Sorted by count descending; covers outer files only (no archive members).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExtStat {
    pub ext: String,
    pub count: usize,
    pub size: i64,
}

/// One point in the scan history time series.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScanHistoryPoint {
    pub scanned_at: i64,
    pub total_files: usize,
    pub total_size: i64,
}

/// One row from the server's `indexing_errors` table.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IndexingError {
    pub path: String,
    pub error: String,
    /// Unix timestamp (seconds) when this error was first seen.
    pub first_seen: i64,
    /// Unix timestamp (seconds) when this error was last seen.
    pub last_seen: i64,
    /// How many scans have reported this error.
    pub count: i64,
}

/// `GET /api/v1/errors` response.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ErrorsResponse {
    pub errors: Vec<IndexingError>,
    /// Total number of error rows (for pagination).
    pub total: usize,
}

/// Stats for one source, returned by `GET /api/v1/stats`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SourceStats {
    pub name: String,
    pub last_scan: Option<i64>,
    pub total_files: usize,
    pub total_size: i64,
    pub by_kind: std::collections::HashMap<String, KindStats>,
    /// File counts by extension, sorted by count descending (outer files only).
    #[serde(default)]
    pub by_ext: Vec<ExtStat>,
    pub history: Vec<ScanHistoryPoint>,
    /// Number of files with recorded indexing errors.
    #[serde(default)]
    pub indexing_error_count: usize,
    /// Number of rows in the FTS5 index (includes stale entries from re-indexed
    /// files; useful for diagnosing whether the index is populated).
    #[serde(default)]
    pub fts_row_count: i64,
}

/// Current processing state of the inbox worker.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum WorkerStatus {
    /// Worker is idle — no requests in flight.
    Idle,
    /// Worker is actively indexing a file.
    Processing {
        /// Source name being indexed.
        source: String,
        /// Relative path of the file currently being processed.
        file: String,
    },
}


/// `GET /api/v1/stats` response.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StatsResponse {
    pub sources: Vec<SourceStats>,
    pub inbox_pending: usize,
    pub failed_requests: usize,
    pub total_archives: usize,
    /// Total on-disk size of all SQLite source databases (bytes).
    pub db_size_bytes: u64,
    /// Total on-disk size of all ZIP content archives (bytes).
    pub archive_size_bytes: u64,
    /// Current state of the inbox worker.
    pub worker_status: WorkerStatus,
}

// ── Inbox admin types ─────────────────────────────────────────────────────────

/// One item in the inbox (pending or failed), returned by `GET /api/v1/admin/inbox`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InboxItem {
    pub filename: String,
    pub size_bytes: u64,
    pub age_secs: u64,
}

/// `GET /api/v1/admin/inbox` response.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InboxStatusResponse {
    pub pending: Vec<InboxItem>,
    pub failed: Vec<InboxItem>,
}

/// `DELETE /api/v1/admin/inbox` response.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InboxDeleteResponse {
    pub deleted: usize,
}

/// `POST /api/v1/admin/inbox/retry` response.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InboxRetryResponse {
    pub retried: usize,
}

/// `DELETE /api/v1/admin/source` response.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SourceDeleteResponse {
    pub files_deleted: usize,
    pub chunks_removed: usize,
}

/// Summary of one file within an inbox batch, returned by `GET /api/v1/admin/inbox/show`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InboxShowFile {
    pub path: String,
    pub kind: String,
    /// Number of content lines (line_number > 0).
    pub content_lines: usize,
}

/// `GET /api/v1/admin/inbox/show` response.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InboxShowResponse {
    /// "pending" or "failed"
    pub queue: String,
    pub source: String,
    pub files: Vec<InboxShowFile>,
    pub delete_paths: Vec<String>,
    pub failures: Vec<IndexingFailure>,
    pub scan_timestamp: Option<i64>,
}

// ── Upload API types ───────────────────────────────────────────────────────────

/// `POST /api/v1/upload` request body — initiates a resumable file upload.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UploadInitRequest {
    /// Source name to index the file under.
    pub source: String,
    /// Relative path of the file within the source.
    pub rel_path: String,
    /// File modification time (Unix seconds).
    pub mtime: i64,
    /// Total file size in bytes.
    pub size: u64,
}

/// `POST /api/v1/upload` response.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UploadInitResponse {
    /// Opaque identifier for this upload session.
    pub upload_id: String,
}

/// `HEAD /api/v1/upload/{id}` response — current upload progress.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UploadStatusResponse {
    /// Bytes received so far.
    pub received: u64,
    /// Total expected bytes (from the init request).
    pub total: u64,
}

/// `PATCH /api/v1/upload/{id}` response.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UploadPatchResponse {
    /// Total bytes received after this patch.
    pub received: u64,
}
