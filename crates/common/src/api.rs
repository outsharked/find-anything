use serde::{Deserialize, Serialize};

pub use find_extract_types::index_line::{detect_kind_from_ext, IndexLine, SCANNER_VERSION};

/// Typed representation of a file's kind — replaces the stringly-typed `kind: String`
/// pattern throughout the codebase.
///
/// `#[serde(rename_all = "lowercase")]` preserves the existing wire format exactly:
/// `"text"`, `"pdf"`, `"archive"`, etc.
///
/// `#[serde(other)]` on `Unknown` ensures any unrecognised string from an older or
/// third-party client deserialises to `Unknown` instead of returning an error.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FileKind {
    Text,
    Pdf,
    Archive,
    Image,
    Audio,
    Video,
    Document,
    Executable,
    Epub,
    #[serde(other)]
    Unknown,
}

impl FileKind {
    /// Re-derive kind from file extension.  Delegates to `detect_kind_from_ext`.
    pub fn from_extension(ext: &str) -> Self {
        Self::from(detect_kind_from_ext(ext))
    }

    /// True for kinds whose extracted lines are passed through the text normalizer.
    pub fn is_text_like(&self) -> bool {
        matches!(self, Self::Text | Self::Pdf)
    }
}

impl std::fmt::Display for FileKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Text       => "text",
            Self::Pdf        => "pdf",
            Self::Archive    => "archive",
            Self::Image      => "image",
            Self::Audio      => "audio",
            Self::Video      => "video",
            Self::Document   => "document",
            Self::Executable => "executable",
            Self::Epub       => "epub",
            Self::Unknown    => "unknown",
        })
    }
}

impl From<&str> for FileKind {
    fn from(s: &str) -> Self {
        match s {
            "text"       => Self::Text,
            "pdf"        => Self::Pdf,
            "archive"    => Self::Archive,
            "image"      => Self::Image,
            "audio"      => Self::Audio,
            "video"      => Self::Video,
            "document"   => Self::Document,
            "executable" => Self::Executable,
            "epub"       => Self::Epub,
            _            => Self::Unknown,
        }
    }
}

impl From<String> for FileKind {
    fn from(s: String) -> Self {
        Self::from(s.as_str())
    }
}

/// Search mode sent in `?mode=` query param.
///
/// `kebab-case` preserves the existing wire format exactly (`"fuzzy"`,
/// `"file-fuzzy"`, `"doc-exact"`, …).
///
/// `#[serde(other)]` on `Fuzzy` — any unrecognised mode string from a future
/// client deserialises to `Fuzzy` (safe fallback) instead of erroring.
/// `Fuzzy` is also the `Default`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum SearchMode {
    Exact,
    Regex,
    /// Fuzzy multi-term document mode: each term may appear on any line.
    Document,
    FileFuzzy,
    FileExact,
    FileRegex,
    DocExact,
    DocRegex,
    /// Default mode; also the catch-all for any unrecognised mode string.
    #[default]
    #[serde(other)]
    Fuzzy,
}

/// Action recorded in the activity log and broadcast on `GET /api/v1/recent`.
///
/// No `#[serde(other)]` — the server is the sole producer; an unknown value
/// here is a server bug, not a client compatibility issue.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum RecentAction {
    #[default]
    Added,
    Modified,
    Deleted,
    Renamed,
}

impl From<&str> for RecentAction {
    fn from(s: &str) -> Self {
        match s {
            "modified" => Self::Modified,
            "deleted"  => Self::Deleted,
            "renamed"  => Self::Renamed,
            _          => Self::Added,
        }
    }
}

/// Whether an inbox batch is in the pending or failed queue.
///
/// No `#[serde(other)]` — the server is the sole producer of this value.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum WorkerQueueSlot {
    Pending,
    Failed,
}

/// Minimum client version the server will accept.
/// Update this constant whenever a breaking API change is made (e.g. new
/// required request fields, removed endpoints, changed response shapes).
/// Clients older than this version will be refused with a clear error message.
pub const MIN_CLIENT_VERSION: &str = "0.6.0";

/// GET /api/v1/sources response entry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SourceInfo {
    pub name: String,
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
    pub kind: FileKind,
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
    /// True when the client knows this file did not previously exist in the index.
    /// Used by the server to log "added" vs "modified" in the activity log.
    /// Defaults to false (treated as a modify) when absent (older clients).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub is_new: bool,
}

/// One extraction failure reported by the client.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct IndexingFailure {
    /// Relative path of the file that failed extraction.
    pub path: String,
    /// Error message, truncated to MAX_ERROR_LEN characters.
    pub error: String,
}

/// A file rename — old path to new path within the same source.
/// Sent by the watcher when a file or directory is renamed. The server
/// updates `files.path` without re-extracting content or touching ZIP archives.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PathRename {
    pub old_path: String,
    pub new_path: String,
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
    /// Path renames detected by the watcher. The server updates file paths in
    /// the index without re-extracting content. Processed after deletes and
    /// before upserts.
    #[serde(default)]
    pub rename_paths: Vec<PathRename>,
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
    pub kind: FileKind,
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
    pub kind: FileKind,
}

/// GET /api/v1/file response.
#[derive(Debug, Serialize, Deserialize)]
pub struct FileResponse {
    /// Content lines in line-number order (line_number > 0). Plain strings;
    /// the display line number is `index + 1` when lines are sequential after
    /// normalisation, or the corresponding entry in `line_offsets` when present.
    pub lines: Vec<String>,
    /// Actual 1-based line numbers for each entry in `lines`, only present
    /// when lines are not a contiguous 1-based sequence (e.g. sparse PDFs).
    /// Clients should fall back to `index + 1` when this field is absent.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub line_offsets: Vec<usize>,
    /// Line-number-0 entries: the file's own path, EXIF/audio metadata strings,
    /// and dedup-alias paths. Clients filter these to determine what to display.
    pub metadata: Vec<String>,
    pub file_kind: FileKind,
    pub total_lines: usize,
    pub mtime: Option<i64>,
    pub size: Option<i64>,
    /// Extraction error message for this file, if one was recorded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub indexing_error: Option<String>,
    /// True when the file has been indexed (metadata available) but its content
    /// has not yet been written to the ZIP archive by the background worker.
    /// Clients should show "content not yet available" rather than empty lines.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub content_unavailable: bool,
}

/// GET /api/v1/files response entry (for deletion detection / Ctrl+P).
#[derive(Debug, Serialize, Deserialize)]
pub struct FileRecord {
    pub path: String,
    pub mtime: i64,
    pub kind: FileKind,
    /// Scanner version stored when the file was last indexed. Used by
    /// `find-scan --upgrade` to detect entries that need re-extraction.
    #[serde(default)]
    pub scanner_version: u32,
    /// Unix timestamp (seconds) when the server last processed this file.
    /// Used by `find-scan --force` to skip files already re-indexed in a
    /// prior interrupted run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub indexed_at: Option<i64>,
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
    pub kind: Option<FileKind>,
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
    pub kind: FileKind,
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
    /// Minimum client version this server accepts.
    /// Clients should compare their own version against this and refuse to
    /// proceed if they are older. Defaults to empty string (no minimum) for
    /// backwards compatibility with older servers that don't send this field.
    #[serde(default)]
    pub min_client_version: String,
    /// Maximum markdown file size (KB) the UI will render as formatted HTML.
    /// Files larger than this are shown as plain text.
    /// Defaults to 512 for backwards compatibility with older servers.
    #[serde(default = "default_max_markdown_render_kb")]
    pub max_markdown_render_kb: usize,
    /// Maximum content lines returned per /api/v1/file request.
    /// 0 = no limit (older servers). Default: 2000.
    #[serde(default)]
    pub file_view_page_size: usize,
}

fn default_max_markdown_render_kb() -> usize { 512 }

// ── Stats types ───────────────────────────────────────────────────────────────

/// Per-kind breakdown entry in `SourceStats`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
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
    pub by_kind: std::collections::HashMap<FileKind, KindStats>,
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
    /// Number of requests waiting for the archive thread to write ZIP content.
    #[serde(default)]
    pub archive_queue: usize,
    pub total_archives: usize,
    /// Total on-disk size of all SQLite source databases (bytes).
    pub db_size_bytes: u64,
    /// Total on-disk size of all ZIP content archives (bytes).
    pub archive_size_bytes: u64,
    /// Current state of the inbox worker.
    pub worker_status: WorkerStatus,
    /// True when inbox processing has been paused via `POST /api/v1/admin/inbox/pause`.
    #[serde(default)]
    pub inbox_paused: bool,
    /// Total compressed size of orphaned chunks across all archives (bytes).
    /// `None` if the background scanner has not yet run.
    #[serde(default)]
    pub orphaned_bytes: Option<u64>,
    /// Seconds since the orphaned-chunk stats were last computed.
    /// `None` if the background scanner has not yet run.
    #[serde(default)]
    pub orphaned_stats_age_secs: Option<u64>,
}

/// Snapshot sent via `GET /api/v1/stats/stream` (SSE).
/// Cache-only — no DB opens. Omits last_scan / history / indexing_error_count
/// which do not change during active indexing.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StatsStreamEvent {
    pub sources: Vec<SourceStreamSnapshot>,
    pub inbox_pending: usize,
    pub failed_requests: usize,
    #[serde(default)]
    pub archive_queue: usize,
    pub total_archives: usize,
    pub db_size_bytes: u64,
    pub archive_size_bytes: u64,
    pub worker_status: WorkerStatus,
    #[serde(default)]
    pub inbox_paused: bool,
    #[serde(default)]
    pub orphaned_bytes: Option<u64>,
    #[serde(default)]
    pub orphaned_stats_age_secs: Option<u64>,
}

/// Per-source snapshot for SSE streaming.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SourceStreamSnapshot {
    pub name: String,
    pub total_files: usize,
    pub total_size: i64,
    pub by_kind: std::collections::HashMap<FileKind, KindStats>,
    pub fts_row_count: i64,
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
    /// True when inbox processing has been paused via `POST /api/v1/admin/inbox/pause`.
    #[serde(default)]
    pub paused: bool,
    /// Number of requests that have been indexed into SQLite but are waiting
    /// for the archive thread to write their content to ZIP archives.
    #[serde(default)]
    pub archive_queue: usize,
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

/// `POST /api/v1/admin/inbox/pause` response.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InboxPauseResponse {
    /// Number of in-flight jobs returned to the inbox from `inbox/processing/`.
    pub returned: usize,
}

/// `POST /api/v1/admin/inbox/resume` response.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InboxResumeResponse {}

/// `POST /api/v1/admin/compact` response.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompactResponse {
    pub archives_scanned: usize,
    pub archives_rewritten: usize,
    pub chunks_removed: usize,
    pub bytes_freed: u64,
    pub dry_run: bool,
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
    pub kind: FileKind,
    /// Number of content lines (line_number > 0).
    pub content_lines: usize,
}

/// `GET /api/v1/admin/inbox/show` response.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InboxShowResponse {
    pub queue: WorkerQueueSlot,
    pub source: String,
    pub files: Vec<InboxShowFile>,
    pub delete_paths: Vec<String>,
    pub failures: Vec<IndexingFailure>,
    pub scan_timestamp: Option<i64>,
}

// ── Self-update types ─────────────────────────────────────────────────────────

/// `GET /api/v1/admin/update/check` response.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpdateCheckResponse {
    /// Currently running server version.
    pub current: String,
    /// Latest version available on GitHub.
    pub latest: String,
    /// True if `latest` > `current` and a matching asset exists for this platform.
    pub update_available: bool,
    /// True if the server is running under systemd and can restart itself.
    pub restart_supported: bool,
    /// Human-readable reason when `restart_supported` is false.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub restart_unsupported_reason: Option<String>,
}

/// `POST /api/v1/admin/update/apply` response.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpdateApplyResponse {
    pub ok: bool,
    pub message: String,
}

// ── Recent files types ─────────────────────────────────────────────────────────

/// One entry in a `GET /api/v1/recent` response.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecentFile {
    pub source: String,
    /// Relative path within the source (outer files only; no `::` members).
    /// For `action = "renamed"` this is the old (pre-rename) path.
    pub path: String,
    /// Unix timestamp (seconds) when this event was recorded.
    pub indexed_at: i64,
    /// What happened. Defaults to `Added` when reading from older servers that
    /// don't populate this field.
    #[serde(default)]
    pub action: RecentAction,
    /// For `action = "renamed"`: the new (post-rename) path.  `None` for all other actions.
    #[serde(default)]
    pub new_path: Option<String>,
}


/// `GET /api/v1/recent` response.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecentResponse {
    pub files: Vec<RecentFile>,
}

// ── Link sharing types ────────────────────────────────────────────────────────

/// `POST /api/v1/links` request body.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateLinkRequest {
    pub source: String,
    pub path: String,
    pub archive_path: Option<String>,
}

/// `POST /api/v1/links` response.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateLinkResponse {
    /// 6-character URL-safe code.
    pub code: String,
    /// Relative URL for the direct view page, e.g. `/v/aB3mZx`.
    pub url: String,
    /// Unix timestamp (seconds) when this link expires.
    pub expires_at: i64,
}

/// `GET /api/v1/links/:code` response.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResolveLinkResponse {
    pub source: String,
    /// Outer file path (no `::` suffix).
    pub path: String,
    /// Inner archive member path, if this is a composite path.
    pub archive_path: Option<String>,
    pub kind: FileKind,
    /// Basename of the file (last path component).
    pub filename: String,
    /// Unix timestamp (seconds) of last modification.
    pub mtime: i64,
    /// Unix timestamp (seconds) when this link expires.
    pub expires_at: i64,
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

#[cfg(test)]
mod file_kind_tests {
    use super::*;

    #[test]
    fn file_kind_serde_round_trip() {
        for (variant, wire) in [
            (FileKind::Text,       "\"text\""),
            (FileKind::Pdf,        "\"pdf\""),
            (FileKind::Archive,    "\"archive\""),
            (FileKind::Image,      "\"image\""),
            (FileKind::Audio,      "\"audio\""),
            (FileKind::Video,      "\"video\""),
            (FileKind::Document,   "\"document\""),
            (FileKind::Executable, "\"executable\""),
            (FileKind::Epub,       "\"epub\""),
            (FileKind::Unknown,    "\"unknown\""),
        ] {
            let serialized = serde_json::to_string(&variant).unwrap();
            assert_eq!(serialized, wire, "serialize {variant}");
            let deserialized: FileKind = serde_json::from_str(&serialized).unwrap();
            assert_eq!(deserialized, variant, "deserialize {wire}");
        }
    }

    #[test]
    fn file_kind_unknown_string_deserializes_to_unknown() {
        let result: FileKind = serde_json::from_str("\"binary\"").unwrap();
        assert_eq!(result, FileKind::Unknown);
        let result2: FileKind = serde_json::from_str("\"spreadsheet\"").unwrap();
        assert_eq!(result2, FileKind::Unknown);
    }

    #[test]
    fn file_kind_from_str_known_values() {
        assert_eq!(FileKind::from("text"),     FileKind::Text);
        assert_eq!(FileKind::from("pdf"),      FileKind::Pdf);
        assert_eq!(FileKind::from("archive"),  FileKind::Archive);
        assert_eq!(FileKind::from("image"),    FileKind::Image);
        assert_eq!(FileKind::from("audio"),    FileKind::Audio);
        assert_eq!(FileKind::from("video"),    FileKind::Video);
        assert_eq!(FileKind::from("document"), FileKind::Document);
        assert_eq!(FileKind::from("unknown"),  FileKind::Unknown);
    }

    #[test]
    fn file_kind_from_str_unrecognised_returns_unknown() {
        assert_eq!(FileKind::from("binary"),      FileKind::Unknown);
        assert_eq!(FileKind::from(""),            FileKind::Unknown);
        assert_eq!(FileKind::from("spreadsheet"), FileKind::Unknown);
    }

    #[test]
    fn file_kind_from_extension_covers_known_exts() {
        assert_eq!(FileKind::from_extension("pdf"),  FileKind::Pdf);
        assert_eq!(FileKind::from_extension("zip"),  FileKind::Archive);
        assert_eq!(FileKind::from_extension("jpg"),  FileKind::Image);
        assert_eq!(FileKind::from_extension("mp3"),  FileKind::Audio);
        assert_eq!(FileKind::from_extension("mp4"),  FileKind::Video);
        assert_eq!(FileKind::from_extension("docx"), FileKind::Document);
        assert_eq!(FileKind::from_extension("rs"),   FileKind::Text);
        assert_eq!(FileKind::from_extension("txt"),  FileKind::Text);
        assert_eq!(FileKind::from_extension(""),     FileKind::Unknown);
    }

    #[test]
    fn file_kind_display_matches_wire_format() {
        assert_eq!(FileKind::Text.to_string(),       "text");
        assert_eq!(FileKind::Pdf.to_string(),        "pdf");
        assert_eq!(FileKind::Archive.to_string(),    "archive");
        assert_eq!(FileKind::Image.to_string(),      "image");
        assert_eq!(FileKind::Audio.to_string(),      "audio");
        assert_eq!(FileKind::Video.to_string(),      "video");
        assert_eq!(FileKind::Document.to_string(),   "document");
        assert_eq!(FileKind::Unknown.to_string(),    "unknown");
    }

    #[test]
    fn file_kind_is_text_like() {
        assert!(FileKind::Text.is_text_like());
        assert!(FileKind::Pdf.is_text_like());
        assert!(!FileKind::Image.is_text_like());
        assert!(!FileKind::Archive.is_text_like());
        assert!(!FileKind::Unknown.is_text_like());
    }
}

#[cfg(test)]
mod search_mode_tests {
    use super::*;

    #[test]
    fn search_mode_serde_round_trip() {
        for (variant, wire) in [
            (SearchMode::Fuzzy,     "\"fuzzy\""),
            (SearchMode::Exact,     "\"exact\""),
            (SearchMode::Regex,     "\"regex\""),
            (SearchMode::Document,  "\"document\""),
            (SearchMode::FileFuzzy, "\"file-fuzzy\""),
            (SearchMode::FileExact, "\"file-exact\""),
            (SearchMode::FileRegex, "\"file-regex\""),
            (SearchMode::DocExact,  "\"doc-exact\""),
            (SearchMode::DocRegex,  "\"doc-regex\""),
        ] {
            let serialized = serde_json::to_string(&variant).unwrap();
            assert_eq!(serialized, wire, "serialize {variant:?}");
            let deserialized: SearchMode = serde_json::from_str(&serialized).unwrap();
            assert_eq!(deserialized, variant, "deserialize {wire}");
        }
    }

    #[test]
    fn search_mode_unknown_string_deserializes_to_fuzzy() {
        let result: SearchMode = serde_json::from_str("\"word-search\"").unwrap();
        assert_eq!(result, SearchMode::Fuzzy);
        let result2: SearchMode = serde_json::from_str("\"unknown-mode\"").unwrap();
        assert_eq!(result2, SearchMode::Fuzzy);
    }

    #[test]
    fn search_mode_default_is_fuzzy() {
        assert_eq!(SearchMode::default(), SearchMode::Fuzzy);
    }
}

#[cfg(test)]
mod recent_action_tests {
    use super::*;

    #[test]
    fn recent_action_serde_round_trip() {
        for (variant, wire) in [
            (RecentAction::Added,    "\"added\""),
            (RecentAction::Modified, "\"modified\""),
            (RecentAction::Deleted,  "\"deleted\""),
            (RecentAction::Renamed,  "\"renamed\""),
        ] {
            let serialized = serde_json::to_string(&variant).unwrap();
            assert_eq!(serialized, wire, "serialize {variant:?}");
            let deserialized: RecentAction = serde_json::from_str(&serialized).unwrap();
            assert_eq!(deserialized, variant, "deserialize {wire}");
        }
    }

    #[test]
    fn recent_action_unknown_string_is_deserialization_error() {
        let result = serde_json::from_str::<RecentAction>("\"created\"");
        assert!(result.is_err(), "unknown action should fail deserialization");
    }

    #[test]
    fn recent_action_from_str_known() {
        assert_eq!(RecentAction::from("added"),    RecentAction::Added);
        assert_eq!(RecentAction::from("modified"), RecentAction::Modified);
        assert_eq!(RecentAction::from("deleted"),  RecentAction::Deleted);
        assert_eq!(RecentAction::from("renamed"),  RecentAction::Renamed);
    }

    #[test]
    fn recent_action_from_str_unknown_defaults_to_added() {
        assert_eq!(RecentAction::from("created"), RecentAction::Added);
        assert_eq!(RecentAction::from(""),        RecentAction::Added);
    }

    #[test]
    fn recent_action_default_is_added() {
        assert_eq!(RecentAction::default(), RecentAction::Added);
    }
}

#[cfg(test)]
mod worker_queue_slot_tests {
    use super::*;

    #[test]
    fn worker_queue_slot_serde_round_trip() {
        for (variant, wire) in [
            (WorkerQueueSlot::Pending, "\"pending\""),
            (WorkerQueueSlot::Failed,  "\"failed\""),
        ] {
            let serialized = serde_json::to_string(&variant).unwrap();
            assert_eq!(serialized, wire, "serialize {variant:?}");
            let deserialized: WorkerQueueSlot = serde_json::from_str(&serialized).unwrap();
            assert_eq!(deserialized, variant, "deserialize {wire}");
        }
    }

    #[test]
    fn worker_queue_slot_unknown_string_is_deserialization_error() {
        let result = serde_json::from_str::<WorkerQueueSlot>("\"queued\"");
        assert!(result.is_err(), "unknown slot should fail deserialization");
    }
}
