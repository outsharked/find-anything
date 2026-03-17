use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use tracing::warn;

// ── Built-in defaults (embedded from TOML at compile time) ───────────────────

/// Private structs used only for parsing the embedded defaults files.
/// They have NO serde(default) attributes to prevent circular calls back into
/// the default_* functions while the OnceLock is being initialised.
#[derive(Deserialize)]
struct ClientDefaults {
    scan: ScanDefaults,
    watch: WatchDefaults,
    log: LogDefaults,
}

#[derive(Deserialize)]
struct ScanDefaults {
    exclude: Vec<String>,
    max_content_size_mb: u64,
    max_line_length: usize,
    noindex_file: String,
    index_file: String,
    subprocess_timeout_secs: u64,
    batch_size: usize,
    batch_bytes: usize,
    batch_interval_secs: u64,
    archives: ArchiveDefaults,
}

#[derive(Deserialize)]
struct ArchiveDefaults {
    max_depth: usize,
    max_temp_file_mb: usize,
    max_7z_solid_block_mb: usize,
}

#[derive(Deserialize)]
struct WatchDefaults {
    batch_window_secs: f64,
    scan_interval_hours: f64,
}

#[derive(Deserialize)]
struct LogDefaults {
    ignore: Vec<String>,
}

#[derive(Deserialize)]
struct ServerDefaults {
    server: ServerSettingsDefaults,
    search: SearchDefaults,
    extraction: ExtractionDefaults,
    // log.ignore shares the same default_log_ignore() function as the client;
    // both files have identical values.  Parsed by serde but not stored here.
}

#[derive(Deserialize)]
struct ServerSettingsDefaults {
    bind: String,
    download_zip_member_levels: usize,
    log_batch_detail_limit: usize,
    archive_batch_size: usize,
    inbox_request_timeout_secs: u64,
    inline_threshold_bytes: u64,
    activity_log_max_entries: usize,
}

#[derive(Deserialize)]
struct SearchDefaults {
    default_limit: usize,
    max_limit: usize,
    fts_candidate_limit: usize,
    context_window: usize,
}

#[derive(Deserialize)]
struct ExtractionDefaults {
    max_content_size_mb: u64,
    max_line_length: usize,
    max_archive_depth: usize,
}

static CLIENT_DEFAULTS: OnceLock<ClientDefaults> = OnceLock::new();
static SERVER_DEFAULTS: OnceLock<ServerDefaults> = OnceLock::new();

fn client_defaults() -> &'static ClientDefaults {
    CLIENT_DEFAULTS.get_or_init(|| {
        toml::from_str(include_str!("defaults_client.toml"))
            .expect("built-in defaults_client.toml is invalid — this is a compile-time bug")
    })
}

fn server_defaults() -> &'static ServerDefaults {
    SERVER_DEFAULTS.get_or_init(|| {
        toml::from_str(include_str!("defaults_server.toml"))
            .expect("built-in defaults_server.toml is invalid — this is a compile-time bug")
    })
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClientConfig {
    pub server: ServerConfig,
    #[serde(default)]
    pub sources: Vec<SourceConfig>,
    #[serde(default)]
    pub scan: ScanConfig,
    #[serde(default)]
    pub watch: WatchConfig,
    #[serde(default)]
    pub log: LogConfig,
    #[serde(default)]
    pub tray: TrayConfig,
    #[serde(default)]
    pub cli: CliConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServerConfig {
    pub url: String,
    pub token: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SourceConfig {
    pub name: String,

    /// Root directory for this source. All indexed paths are relative to this.
    /// The server can map this to a filesystem path for raw file serving.
    pub path: String,

    /// Only index files whose relative path matches at least one of these glob
    /// patterns. Use forward slashes as separators (backslashes are normalised
    /// automatically). If empty, all files under the source root are indexed.
    ///
    /// Example:
    /// ```toml
    /// include = ["Users/alice/**", "data/**"]
    /// ```
    #[serde(default)]
    pub include: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScanConfig {
    #[serde(default = "default_excludes")]
    pub exclude: Vec<String>,

    /// Additional exclude patterns appended to `exclude` after parsing.
    ///
    /// Use this to extend the built-in defaults without replacing them.
    /// `exclude` alone **replaces** the defaults; `exclude_extra` always
    /// **adds to** whatever `exclude` contains.
    ///
    /// Example in client.toml:
    /// ```toml
    /// [scan]
    /// exclude_extra = ["**/my-build/**", "*.tmp"]
    /// ```
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub exclude_extra: Vec<String>,

    /// Maximum content size in MB to index per file.
    /// Content is truncated at this limit rather than the file being skipped.
    /// Accepts old key `max_file_size_mb` for backward compatibility.
    #[serde(default = "default_max_content_size_mb", alias = "max_file_size_mb")]
    pub max_content_size_mb: u64,

    #[serde(default)]
    pub follow_symlinks: bool,

    #[serde(default)]
    pub include_hidden: bool,

    #[serde(default)]
    pub archives: ArchiveConfig,

    /// Maximum line length (in characters) for PDF text extraction.
    /// Lines longer than this are split at word boundaries so that context
    /// retrieval returns meaningful snippets.
    /// Set to 0 to disable wrapping. Default: 120.
    #[serde(default = "default_max_line_length")]
    pub max_line_length: usize,

    /// Name of the marker file that signals a directory (and all descendants)
    /// should be excluded from indexing. Default: ".noindex".
    #[serde(default = "default_noindex_file")]
    pub noindex_file: String,

    /// Name of the per-directory config file that overrides scan settings for
    /// a subtree. Default: ".index".
    #[serde(default = "default_index_file")]
    pub index_file: String,

    /// Per-directory include filter set by a `.index` file at runtime.
    /// `Some((dir, patterns))` means: the `.index` file at `dir` declared
    /// `include = [...]`; files must match at least one pattern relative to
    /// `dir` to be indexed within this subtree.
    /// Not persisted to TOML — populated at runtime by the scanner.
    #[serde(skip)]
    pub dir_include: Option<(PathBuf, Vec<String>)>,

    /// Directory containing find-extract-* binaries.
    /// None = auto-detect (same dir as the executable, then PATH).
    #[serde(default)]
    pub extractor_dir: Option<String>,

    /// When true, files whose subprocess extractor exits non-zero are uploaded
    /// to the server for server-side extraction (requires server to have
    /// find-extract-* binaries available).  Default: false.
    #[serde(default)]
    pub server_fallback: bool,

    /// Maximum number of seconds to wait for a single file's extraction
    /// subprocess before killing it and recording a failure.
    /// Default: 300 (5 minutes).
    #[serde(default = "default_subprocess_timeout_secs")]
    pub subprocess_timeout_secs: u64,

    /// Maximum number of files in a single batch submitted to the server.
    /// Default: 200.
    #[serde(default = "default_batch_size")]
    pub batch_size: usize,

    /// Maximum total content bytes in a single batch submitted to the server.
    /// Default: 8388608 (8 MB).
    #[serde(default = "default_batch_bytes")]
    pub batch_bytes: usize,

    /// Submit the current batch if this many seconds have elapsed since the
    /// last submission, regardless of file count or byte size. Ensures the
    /// server receives data promptly when individual files are slow to extract.
    /// Default: 30.
    #[serde(default = "default_batch_interval_secs")]
    pub batch_interval_secs: u64,

    /// Extension → extractor override map. Key = lowercase extension (without dot).
    /// Set a value to `"builtin"` to use built-in routing, or provide an external tool config.
    #[serde(default)]
    pub extractors: std::collections::HashMap<String, ExtractorEntry>,
}

impl Default for ScanConfig {
    fn default() -> Self {
        Self {
            exclude: default_excludes(),
            exclude_extra: vec![],
            max_content_size_mb: default_max_content_size_mb(),
            follow_symlinks: false,
            include_hidden: false,
            archives: ArchiveConfig::default(),
            max_line_length: default_max_line_length(),
            noindex_file: default_noindex_file(),
            index_file: default_index_file(),
            dir_include: None,
            extractor_dir: None,
            server_fallback: false,
            subprocess_timeout_secs: default_subprocess_timeout_secs(),
            batch_size: default_batch_size(),
            batch_bytes: default_batch_bytes(),
            batch_interval_secs: default_batch_interval_secs(),
            extractors: std::collections::HashMap::new(),
        }
    }
}

impl ScanConfig {
    /// Produce a new `ScanConfig` by applying a per-directory override.
    ///
    /// - `exclude` is **additive**: patterns are appended to the parent list.
    /// - All other fields are **replacement**: the innermost value wins.
    /// - `noindex_file` and `index_file` are never overridden (global-only).
    ///
    /// Like `apply_override` but also applies `ov.include` using `dir` as the
    /// base directory for the patterns (absolute path of the `.index` file's
    /// directory). Call this instead of `apply_override` when loading `.index`
    /// files so that `dir_include` is populated correctly.
    pub fn apply_dir_override(&self, ov: &ScanOverride, dir: &Path) -> ScanConfig {
        let mut result = self.apply_override(ov);
        if let Some(patterns) = &ov.include {
            result.dir_include = Some((dir.to_path_buf(), patterns.clone()));
        }
        result
    }

    pub fn apply_override(&self, ov: &ScanOverride) -> ScanConfig {
        let mut result = self.clone();
        if let Some(extra) = &ov.exclude {
            result.exclude.extend(extra.iter().cloned());
        }
        if let Some(v) = ov.max_content_size_mb {
            result.max_content_size_mb = v;
        }
        if let Some(v) = ov.include_hidden {
            result.include_hidden = v;
        }
        if let Some(v) = ov.follow_symlinks {
            result.follow_symlinks = v;
        }
        if let Some(v) = ov.max_line_length {
            result.max_line_length = v;
        }
        if let Some(arch_ov) = &ov.archives {
            if let Some(v) = arch_ov.enabled {
                result.archives.enabled = v;
            }
            if let Some(v) = arch_ov.max_depth {
                result.archives.max_depth = v;
            }
        }
        result
    }
}

/// Partial scan config read from a per-directory `.index` file.
/// All fields are optional; `None` means "inherit from parent config".
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ScanOverride {
    /// Per-directory include filter. When set, only files matching these glob
    /// patterns (relative to the directory containing this `.index` file) are
    /// indexed within this subtree. A file must match at least one pattern.
    ///
    /// Replacement semantics: the innermost `.index` with `include` wins.
    /// Use instead of `.noindex` when you want to whitelist a specific
    /// subdirectory: place `.index` in the parent with `include = ["sub/**"]`.
    pub include: Option<Vec<String>>,
    /// Additional exclude patterns (appended to parent list, never removed).
    pub exclude: Option<Vec<String>>,
    /// Accepts old key `max_file_size_mb` for backward compatibility.
    #[serde(alias = "max_file_size_mb")]
    pub max_content_size_mb: Option<u64>,
    pub include_hidden: Option<bool>,
    pub follow_symlinks: Option<bool>,
    pub archives: Option<ArchiveOverride>,
    pub max_line_length: Option<usize>,
}

/// Archive-specific fields for a `ScanOverride`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ArchiveOverride {
    pub enabled: Option<bool>,
    pub max_depth: Option<usize>,
}

/// Load and parse a `.index` override file from `dir`.
/// Returns `None` if the file is absent, unreadable, or unparseable.
pub fn load_dir_override(dir: &Path, index_filename: &str) -> Option<ScanOverride> {
    let path = dir.join(index_filename);
    let content = std::fs::read_to_string(&path).ok()?;
    match toml::from_str::<ScanOverride>(&content) {
        Ok(ov) => Some(ov),
        Err(e) => {
            warn!("invalid {} file at {}: {e}", index_filename, path.display());
            None
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArchiveConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Maximum nesting depth for archives-within-archives.
    /// Prevents infinite recursion from malicious zip bombs.
    /// Default: 10. Set to 1 to only extract direct members (no nested archives).
    #[serde(default = "default_max_archive_depth")]
    pub max_depth: usize,
    /// Maximum size in MB of a temporary file created when extracting a nested
    /// 7z or large nested zip archive.  Guards against excessive disk use from
    /// deeply compressed or unusually large inner archives.  Default: 500 MB.
    #[serde(default = "default_max_archive_temp_file_mb")]
    pub max_temp_file_mb: usize,
    /// Maximum total uncompressed size in MB of a single 7z solid block.
    ///
    /// When decompressing a 7z solid block, the LZMA decoder allocates a
    /// dictionary buffer proportional to the block's total unpack size,
    /// regardless of how large any individual file within the block is.
    /// Blocks whose total unpack size exceeds this limit are skipped: all
    /// member files are indexed by filename only (no content extraction).
    ///
    /// Lower this on memory-constrained systems (NAS boxes, containers).
    /// Default: 256 MB.
    #[serde(default = "default_max_7z_solid_block_mb")]
    pub max_7z_solid_block_mb: usize,
}

impl Default for ArchiveConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            max_depth: default_max_archive_depth(),
            max_temp_file_mb: default_max_archive_temp_file_mb(),
            max_7z_solid_block_mb: default_max_7z_solid_block_mb(),
        }
    }
}

fn default_max_archive_depth() -> usize       { client_defaults().scan.archives.max_depth }
fn default_max_archive_temp_file_mb() -> usize { client_defaults().scan.archives.max_temp_file_mb }
fn default_max_7z_solid_block_mb() -> usize   { client_defaults().scan.archives.max_7z_solid_block_mb }

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WatchConfig {
    /// Seconds to buffer filesystem events before dispatching a batch to the server.
    /// The timer starts on the first event and is NOT reset by subsequent events,
    /// so events are always dispatched within this window regardless of how busy
    /// the filesystem is. If the batch reaches `scan.batch_size` files the batch
    /// is flushed immediately without waiting for the window to expire.
    /// Default: 5.0.
    #[serde(default = "default_batch_window_secs")]
    pub batch_window_secs: f64,

    /// Directory containing find-extract-* binaries.
    /// None = auto-detect (same dir as find-watch, then PATH).
    #[serde(default)]
    pub extractor_dir: Option<String>,

    /// How often to run a full `find-scan` in the background (hours).
    /// Set to 0.0 to disable scheduled scanning entirely.
    #[serde(default = "default_scan_interval_hours")]
    pub scan_interval_hours: f64,
}

impl Default for WatchConfig {
    fn default() -> Self {
        Self {
            batch_window_secs: default_batch_window_secs(),
            extractor_dir: None,
            scan_interval_hours: default_scan_interval_hours(),
        }
    }
}

/// Windows system tray configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrayConfig {
    /// Refresh interval while the recent-files popup is open (milliseconds).
    /// Default: 1000.
    #[serde(default = "default_tray_poll_interval_ms")]
    pub poll_interval_ms: u64,
}

impl Default for TrayConfig {
    fn default() -> Self {
        Self {
            poll_interval_ms: default_tray_poll_interval_ms(),
        }
    }
}

fn default_tray_poll_interval_ms() -> u64 { 1000 }

/// CLI tool configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CliConfig {
    /// Poll interval for `--follow` / `--watch` modes (seconds). Default: 2.0.
    #[serde(default = "default_cli_poll_interval_secs")]
    pub poll_interval_secs: f64,
}

impl Default for CliConfig {
    fn default() -> Self {
        Self { poll_interval_secs: default_cli_poll_interval_secs() }
    }
}

fn default_cli_poll_interval_secs() -> f64 { 2.0 }

fn default_batch_window_secs() -> f64       { client_defaults().watch.batch_window_secs }
fn default_scan_interval_hours() -> f64     { client_defaults().watch.scan_interval_hours }
fn default_excludes() -> Vec<String>         { client_defaults().scan.exclude.clone() }
fn default_max_content_size_mb() -> u64      { client_defaults().scan.max_content_size_mb }
fn default_max_line_length() -> usize        { client_defaults().scan.max_line_length }
fn default_noindex_file() -> String          { client_defaults().scan.noindex_file.clone() }
fn default_index_file() -> String            { client_defaults().scan.index_file.clone() }
fn default_subprocess_timeout_secs() -> u64  { client_defaults().scan.subprocess_timeout_secs }
fn default_batch_size() -> usize             { client_defaults().scan.batch_size }
fn default_batch_bytes() -> usize            { client_defaults().scan.batch_bytes }
fn default_batch_interval_secs() -> u64      { client_defaults().scan.batch_interval_secs }
fn default_true() -> bool               { true }

pub use find_extract_types::ExtractorConfig;

/// Build an `ExtractorConfig` from the scan section of the client config.
pub fn extractor_config_from_scan(scan: &ScanConfig) -> ExtractorConfig {
    ExtractorConfig {
        max_content_kb: scan.max_content_size_mb as usize * 1024,
        max_depth: scan.archives.max_depth,
        max_line_length: scan.max_line_length,
        max_temp_file_mb: scan.archives.max_temp_file_mb,
        include_hidden: scan.include_hidden,
        max_7z_solid_block_mb: scan.archives.max_7z_solid_block_mb,
        exclude_patterns: scan.exclude.clone(),
    }
}

/// Build an `ExtractorConfig` from the server's extraction settings.
pub fn extractor_config_from_extraction(extraction: &ExtractionSettings) -> ExtractorConfig {
    ExtractorConfig {
        max_content_kb: extraction.max_content_size_mb as usize * 1024,
        max_depth: extraction.max_archive_depth,
        max_line_length: extraction.max_line_length,
        ..ExtractorConfig::default()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServerAppConfig {
    pub server: ServerAppSettings,
    #[serde(default)]
    pub search: SearchSettings,
    #[serde(default)]
    pub extraction: ExtractionSettings,
    #[serde(default)]
    pub normalization: NormalizationSettings,
    #[serde(default)]
    pub compaction: CompactionConfig,
    #[serde(default)]
    pub links: LinksConfig,
    #[serde(default)]
    pub log: LogConfig,
    /// Per-source server configuration (e.g. filesystem root for raw file serving).
    #[serde(default)]
    pub sources: std::collections::HashMap<String, ServerSourceConfig>,
}

/// Configuration for share link generation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LinksConfig {
    /// How long generated links stay valid, in seconds.
    /// Configured as a duration string like `"30d"`, `"7d"`, `"24h"`.
    /// Default: 30 days (2592000 seconds).
    #[serde(default = "default_links_ttl_secs", deserialize_with = "deserialize_links_ttl")]
    pub ttl_secs: u64,
}

impl Default for LinksConfig {
    fn default() -> Self {
        Self { ttl_secs: default_links_ttl_secs() }
    }
}

fn default_links_ttl_secs() -> u64 { 30 * 24 * 3600 }

fn deserialize_links_ttl<'de, D>(de: D) -> Result<u64, D::Error>
where
    D: serde::Deserializer<'de>,
{
    struct TtlVisitor;
    impl<'de> serde::de::Visitor<'de> for TtlVisitor {
        type Value = u64;
        fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
            write!(f, r#"a duration string like "30d" or "24h", or an integer of seconds"#)
        }
        fn visit_u64<E: serde::de::Error>(self, v: u64) -> Result<u64, E> { Ok(v) }
        fn visit_i64<E: serde::de::Error>(self, v: i64) -> Result<u64, E> {
            u64::try_from(v).map_err(|_| E::custom("TTL must be non-negative"))
        }
        fn visit_str<E: serde::de::Error>(self, v: &str) -> Result<u64, E> {
            parse_ttl(v).map_err(E::custom)
        }
    }
    de.deserialize_any(TtlVisitor)
}

/// Parse a TTL string like `"30d"`, `"7d"`, `"24h"`, `"1h"` into seconds.
pub fn parse_ttl(s: &str) -> Result<u64, String> {
    if let Some(days) = s.strip_suffix('d') {
        let d: u64 = days.parse().map_err(|_| format!("invalid TTL: {s:?}"))?;
        return Ok(d * 24 * 3600);
    }
    if let Some(hours) = s.strip_suffix('h') {
        let h: u64 = hours.parse().map_err(|_| format!("invalid TTL: {s:?}"))?;
        return Ok(h * 3600);
    }
    Err(format!("invalid TTL {s:?}: expected suffix 'd' (days) or 'h' (hours)"))
}

/// Configuration for automatic archive compaction.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompactionConfig {
    /// Minimum percentage of orphaned bytes required to trigger compaction.
    /// If the orphaned fraction is below this threshold, compaction is skipped.
    /// Set to 0.0 to always compact, or 100.0 to effectively disable.
    #[serde(default = "default_compaction_threshold_pct")]
    pub threshold_pct: f64,
    /// Local time (HH:MM, 24-hour) at which the daily compaction window runs.
    #[serde(default = "default_compaction_start_time")]
    pub start_time: String,
}

impl Default for CompactionConfig {
    fn default() -> Self {
        Self {
            threshold_pct: default_compaction_threshold_pct(),
            start_time: default_compaction_start_time(),
        }
    }
}

fn default_compaction_threshold_pct() -> f64 { 10.0 }
fn default_compaction_start_time() -> String { "02:00".to_string() }

/// Server-side configuration for a named source.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ServerSourceConfig {
    /// Filesystem root for this source. When set, the server can serve
    /// original files via GET /api/v1/raw.
    pub path: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServerAppSettings {
    #[serde(default = "default_bind")]
    pub bind: String,
    pub data_dir: String,
    pub token: String,
    /// Directory containing find-extract-* binaries for server-side extraction.
    /// None = auto-detect (same dir as the executable, then PATH).
    #[serde(default)]
    pub extractor_dir: Option<String>,
    /// Maximum number of ZIP nesting levels supported for member download/inline view.
    /// 1 = only direct members (outer.zip::file).
    /// 2 = one level of nesting (outer.zip::inner.zip::file). Default: 2.
    #[serde(default = "default_download_zip_member_levels")]
    pub download_zip_member_levels: usize,
    /// When the inbox worker processes a batch, log each file path individually
    /// if the batch contains at most this many files. For larger batches, log
    /// only the count. Default: 5.
    #[serde(default = "default_log_batch_detail_limit")]
    pub log_batch_detail_limit: usize,
    /// Maximum seconds a single inbox request may run before the worker
    /// abandons it and moves the file to `failed/`. The blocking thread
    /// cannot be cancelled and continues in the background, but the worker
    /// slot is freed for new work. Default: 1800 (30 minutes).
    #[serde(default = "default_inbox_request_timeout_secs")]
    pub inbox_request_timeout_secs: u64,
    /// Number of `to-archive/` requests processed per archive-thread batch.
    /// The archive thread coalesces work across all files in a batch before
    /// doing ZIP rewrites, so larger batches reduce the number of rewrite passes.
    /// Default: 200.
    #[serde(default = "default_archive_batch_size")]
    pub archive_batch_size: usize,
    /// Files whose total extracted content is at or below this size (bytes) are
    /// stored directly in SQLite (`file_content` table) rather than in ZIP archives.
    /// This eliminates ZIP overhead for small files (config files, dotfiles, etc.)
    /// and speeds up reads for them.  Set to 0 to disable inline storage.
    /// Default: 256.
    #[serde(default = "default_inline_threshold_bytes")]
    pub inline_threshold_bytes: u64,
    /// Maximum number of activity-log entries retained per source database.
    /// When a batch pushes the count over this limit, the oldest entries are
    /// pruned.  Set to 0 to disable the activity log entirely.
    /// Default: 10000.
    #[serde(default = "default_activity_log_max_entries")]
    pub activity_log_max_entries: usize,
    /// Maximum markdown file size (in KB) that the UI will render as formatted
    /// HTML. Files larger than this threshold are shown as plain text.
    /// Default: 512.
    #[serde(default = "default_max_markdown_render_kb")]
    pub max_markdown_render_kb: usize,
    /// Maximum number of content lines returned per /api/v1/file request.
    /// When the file exceeds this threshold the UI enters paged mode and
    /// loads additional lines on scroll. 0 disables pagination (legacy).
    /// Default: 2000.
    #[serde(default = "default_file_view_page_size")]
    pub file_view_page_size: usize,
}

fn default_max_markdown_render_kb() -> usize { 512 }
fn default_file_view_page_size() -> usize { 2000 }
fn default_bind() -> String { server_defaults().server.bind.clone() }
fn default_download_zip_member_levels() -> usize { server_defaults().server.download_zip_member_levels }
fn default_log_batch_detail_limit() -> usize     { server_defaults().server.log_batch_detail_limit }
fn default_inbox_request_timeout_secs() -> u64   { server_defaults().server.inbox_request_timeout_secs }
fn default_archive_batch_size() -> usize         { server_defaults().server.archive_batch_size }
fn default_inline_threshold_bytes() -> u64       { server_defaults().server.inline_threshold_bytes }
fn default_activity_log_max_entries() -> usize   { server_defaults().server.activity_log_max_entries }

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchSettings {
    #[serde(default = "default_search_limit")]
    pub default_limit: usize,
    #[serde(default = "default_max_limit")]
    pub max_limit: usize,
    #[serde(default = "default_fts_candidate_limit")]
    pub fts_candidate_limit: usize,
    /// Number of lines shown before and after each match in search result cards.
    /// Total lines displayed = 2 × context_window + 1. Default: 1 (3 lines total).
    #[serde(default = "default_context_window")]
    pub context_window: usize,
}

impl Default for SearchSettings {
    fn default() -> Self {
        Self {
            default_limit: default_search_limit(),
            max_limit: default_max_limit(),
            fts_candidate_limit: default_fts_candidate_limit(),
            context_window: default_context_window(),
        }
    }
}

fn default_search_limit() -> usize    { server_defaults().search.default_limit }
fn default_max_limit() -> usize       { server_defaults().search.max_limit }
fn default_fts_candidate_limit() -> usize { server_defaults().search.fts_candidate_limit }
fn default_context_window() -> usize  { server_defaults().search.context_window }

/// Extraction settings for the server (used for server-side file indexing).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExtractionSettings {
    /// Maximum content size in MB to index per file. Default: 10.
    #[serde(default = "default_extraction_max_content_size_mb")]
    pub max_content_size_mb: u64,
    /// Maximum line length in characters for PDF extraction. Default: 120.
    #[serde(default = "default_extraction_max_line_length")]
    pub max_line_length: usize,
    /// Maximum archive nesting depth. Default: 10.
    #[serde(default = "default_extraction_max_archive_depth")]
    pub max_archive_depth: usize,
}

impl Default for ExtractionSettings {
    fn default() -> Self {
        Self {
            max_content_size_mb: default_extraction_max_content_size_mb(),
            max_line_length: default_extraction_max_line_length(),
            max_archive_depth: default_extraction_max_archive_depth(),
        }
    }
}

fn default_extraction_max_content_size_mb() -> u64 { server_defaults().extraction.max_content_size_mb }
fn default_extraction_max_line_length() -> usize   { server_defaults().extraction.max_line_length }
fn default_extraction_max_archive_depth() -> usize { server_defaults().extraction.max_archive_depth }

// ── Normalization settings ─────────────────────────────────────────────────────

/// Controls how an external extractor is invoked.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ExternalExtractorMode {
    /// Tool writes extracted content to stdout.
    Stdout,
    /// Tool extracts files into a temp directory.
    TempDir,
}

/// A single external extractor tool.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExternalExtractorConfig {
    /// How to invoke the tool.
    pub mode: ExternalExtractorMode,
    /// Absolute or PATH-relative path to the binary.
    pub bin: String,
    /// Args with {file}, {name}, {dir} placeholders.
    pub args: Vec<String>,
}

/// Value in the [scan.extractors] table.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ExtractorEntry {
    /// Use built-in routing. The string value is conventionally "builtin".
    Builtin(String),
    /// Use an external command.
    External(ExternalExtractorConfig),
}

/// Configuration for a single external formatter.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FormatterConfig {
    /// Absolute path to the formatter binary.
    pub path: String,
    /// File extensions this formatter handles (without leading dot, lowercase).
    pub extensions: Vec<String>,
    /// Command-line arguments. Use `{name}` as a placeholder for the filename
    /// (used by tools like biome/prettier to detect the file type).
    /// Example: `["format", "--stdin-filepath", "{name}", "-"]`
    pub args: Vec<String>,
}

/// Server-side text normalization settings.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NormalizationSettings {
    /// Maximum line length before word-wrap is applied. 0 = disabled.
    /// Default: 120.
    #[serde(default = "default_norm_max_line_length")]
    pub max_line_length: usize,

    /// External formatters tried in order. First matching extension that exits
    /// successfully wins. Empty list = word-wrap only.
    #[serde(default)]
    pub formatters: Vec<FormatterConfig>,
}

fn default_norm_max_line_length() -> usize { 120 }

impl Default for NormalizationSettings {
    fn default() -> Self {
        Self {
            max_line_length: default_norm_max_line_length(),
            formatters: Vec::new(),
        }
    }
}

// ── Log config ────────────────────────────────────────────────────────────────

/// Logging configuration shared by client and server.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct LogConfig {
    /// Regular-expression patterns for log messages to suppress.
    /// Any event whose message contains a match for one of these patterns is
    /// silently dropped before it reaches the output formatter.
    /// Default: suppresses "unknown glyph name" noise from pdf-extract.
    #[serde(default = "default_log_ignore")]
    pub ignore: Vec<String>,
    /// Omit the timestamp and module-path target from each log line.
    ///
    /// Set to `true` when running under systemd/journald, which already
    /// captures the timestamp and process name from OS metadata — keeping them
    /// in the log message itself is redundant.  Equivalent to the
    /// `--compact-log` CLI flag; the flag takes precedence.
    ///
    /// Default: false.
    #[serde(default)]
    pub compact: bool,
}

fn default_log_ignore() -> Vec<String> { client_defaults().log.ignore.clone() }

/// Resolves the server config path using the following priority:
///
/// 1. `FIND_ANYTHING_SERVER_CONFIG` environment variable (if set)
/// 2. `$XDG_CONFIG_HOME/find-anything/server.toml` (if `XDG_CONFIG_HOME` is set)
/// 3. `/etc/find-anything/server.toml` if running as root (uid 0) — typical for system services
/// 4. `~/.config/find-anything/server.toml` otherwise
pub fn default_server_config_path() -> String {
    if let Ok(p) = std::env::var("FIND_ANYTHING_SERVER_CONFIG") {
        return p;
    }
    if let Ok(xdg) = std::env::var("XDG_CONFIG_HOME") {
        return format!("{xdg}/find-anything/server.toml");
    }
    // Running as root → system-wide config location used by service units.
    #[cfg(unix)]
    if unsafe { libc::getuid() } == 0 {
        return "/etc/find-anything/server.toml".into();
    }
    let home = std::env::var("HOME").unwrap_or_default();
    format!("{home}/.config/find-anything/server.toml")
}

/// Resolves the client config path using the following priority:
///
/// 1. `FIND_ANYTHING_CONFIG` environment variable (if set)
/// 2. `$XDG_CONFIG_HOME/find-anything/client.toml` (if `XDG_CONFIG_HOME` is set)
/// 3. `/etc/find-anything/client.toml` (when running as root, e.g. system service) [Unix only]
/// 4. `%USERPROFILE%\.config\FindAnything\client.toml` [Windows]
/// 5. `~/.config/find-anything/client.toml` [Unix default]
pub fn default_config_path() -> String {
    if let Ok(p) = std::env::var("FIND_ANYTHING_CONFIG") {
        return p;
    }
    if let Ok(xdg) = std::env::var("XDG_CONFIG_HOME") {
        return format!("{xdg}/find-anything/client.toml");
    }
    // Running as root → system-wide config location used by service units.
    #[cfg(unix)]
    if unsafe { libc::getuid() } == 0 {
        return "/etc/find-anything/client.toml".into();
    }
    // On Windows use %USERPROFILE%\.config\FindAnything\client.toml
    #[cfg(windows)]
    if let Ok(profile) = std::env::var("USERPROFILE") {
        return format!("{profile}\\.config\\FindAnything\\client.toml");
    }
    let home = std::env::var("HOME").unwrap_or_default();
    format!("{home}/.config/find-anything/client.toml")
}

// ── Config loaders with unknown-field warnings ─────────────────────────────

/// Parse a client `client.toml` string.
///
/// Returns `(config, warnings)` where `warnings` is a list of human-readable
/// warning strings for unknown or deprecated keys.  Callers are responsible for
/// displaying these to the user (e.g. `eprintln!`) rather than routing them
/// through the tracing subscriber.
pub fn parse_client_config(toml_str: &str) -> Result<(ClientConfig, Vec<String>)> {
    let value: toml::Value = toml::from_str(toml_str).context("invalid TOML")?;
    let mut warnings = Vec::new();
    // Detect deprecated key before deserialisation.
    if let Some(scan) = value.get("scan") {
        if scan.get("max_file_size_mb").is_some() {
            warnings.push(
                "max_file_size_mb is deprecated; rename to max_content_size_mb in your client.toml"
                    .to_string(),
            );
        }
    }
    let mut unknown = Vec::new();
    let mut cfg: ClientConfig = serde_ignored::deserialize(value, |path| {
        unknown.push(path.to_string());
    })
    .context("parsing client config")?;
    for key in &unknown {
        warnings.push(format!("unknown config key: \"{key}\""));
    }
    // Merge exclude_extra into exclude so the rest of the codebase only
    // needs to look at one field.
    cfg.scan.exclude.extend(std::mem::take(&mut cfg.scan.exclude_extra));
    Ok((cfg, warnings))
}

/// Parse a server `server.toml` string.
///
/// Returns `(config, warnings)` where `warnings` is a list of human-readable
/// warning strings for unknown keys.  The server passes these through `warn!`
/// since it runs as a daemon; CLI tools should print them to stderr directly.
pub fn parse_server_config(toml_str: &str) -> Result<(ServerAppConfig, Vec<String>)> {
    let value: toml::Value = toml::from_str(toml_str).context("invalid TOML")?;
    let mut unknown = Vec::new();
    let cfg = serde_ignored::deserialize(value, |path| {
        unknown.push(path.to_string());
    })
    .context("parsing server config")?;
    let warnings = unknown
        .into_iter()
        .map(|key| format!("unknown config key: \"{key}\""))
        .collect();
    Ok((cfg, warnings))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Verify that both embedded defaults files parse without error.
    /// Catches TOML syntax mistakes and missing required fields at test time.
    #[test]
    fn embedded_defaults_parse() {
        let _c = client_defaults();
        let _s = server_defaults();
    }

    #[test]
    fn watch_config_default_values() {
        let w = WatchConfig::default();
        assert_eq!(w.batch_window_secs, 5.0);
        assert!(w.extractor_dir.is_none());
    }

    #[test]
    fn watch_config_serde_missing_fields_use_defaults() {
        // A config with no [watch] section should deserialise to defaults.
        let w: WatchConfig = serde_json::from_str("{}").unwrap();
        assert_eq!(w.batch_window_secs, 5.0);
        assert!(w.extractor_dir.is_none());
    }

    #[test]
    fn watch_config_serde_explicit_values() {
        let w: WatchConfig =
            serde_json::from_str(r#"{"batch_window_secs":10.0,"extractor_dir":"/usr/local/bin"}"#)
                .unwrap();
        assert_eq!(w.batch_window_secs, 10.0);
        assert_eq!(w.extractor_dir.as_deref(), Some("/usr/local/bin"));
    }

    #[test]
    fn scan_config_default_control_file_names() {
        let s = ScanConfig::default();
        assert_eq!(s.noindex_file, ".noindex");
        assert_eq!(s.index_file, ".index");
    }

    #[test]
    fn scan_override_exclude_is_additive() {
        let base = ScanConfig {
            exclude: vec!["**/.git/**".into()],
            ..ScanConfig::default()
        };
        let ov = ScanOverride {
            exclude: Some(vec!["*.log".into()]),
            ..ScanOverride::default()
        };
        let result = base.apply_override(&ov);
        assert_eq!(result.exclude, vec!["**/.git/**", "*.log"]);
    }

    #[test]
    fn scan_override_replaces_scalar_fields() {
        let base = ScanConfig::default();
        let ov = ScanOverride {
            include_hidden: Some(true),
            max_content_size_mb: Some(99),
            ..ScanOverride::default()
        };
        let result = base.apply_override(&ov);
        assert!(result.include_hidden);
        assert_eq!(result.max_content_size_mb, 99);
        // noindex_file/index_file are inherited unchanged
        assert_eq!(result.noindex_file, ".noindex");
    }

    #[test]
    fn scan_override_archive_fields() {
        let base = ScanConfig::default();
        let ov = ScanOverride {
            archives: Some(ArchiveOverride { enabled: Some(false), max_depth: Some(2) }),
            ..ScanOverride::default()
        };
        let result = base.apply_override(&ov);
        assert!(!result.archives.enabled);
        assert_eq!(result.archives.max_depth, 2);
        assert_eq!(result.archives.max_temp_file_mb, 500); // unchanged
    }

    #[test]
    fn scan_override_toml_parses() {
        let toml = r#"
include_hidden = true
exclude = ["*.tmp"]

[archives]
enabled = false
"#;
        let ov: ScanOverride = toml::from_str(toml).unwrap();
        assert_eq!(ov.include_hidden, Some(true));
        assert_eq!(ov.exclude, Some(vec!["*.tmp".into()]));
        assert_eq!(ov.archives.as_ref().unwrap().enabled, Some(false));
    }

    #[test]
    fn exclude_extra_appends_to_defaults() {
        let toml = r#"
[server]
url = "http://localhost:8080"
token = "t"

[scan]
exclude_extra = ["**/my-build/**", "*.tmp"]
"#;
        let (cfg, _) = parse_client_config(toml).unwrap();
        // exclude_extra is merged into exclude; the built-in defaults come first
        let defaults = default_excludes();
        assert!(cfg.scan.exclude.starts_with(&defaults));
        assert!(cfg.scan.exclude.contains(&"**/my-build/**".to_string()));
        assert!(cfg.scan.exclude.contains(&"*.tmp".to_string()));
        // exclude_extra itself is empty after merging
        assert!(cfg.scan.exclude_extra.is_empty());
    }

    #[test]
    fn exclude_replaces_defaults() {
        let toml = r#"
[server]
url = "http://localhost:8080"
token = "t"

[scan]
exclude = ["*.only"]
"#;
        let (cfg, _) = parse_client_config(toml).unwrap();
        assert_eq!(cfg.scan.exclude, vec!["*.only"]);
    }

    #[test]
    fn client_config_watch_field_defaults_when_absent() {
        // Simulate a client.toml that has no [watch] section.
        let json = r#"{
            "server": {"url": "http://localhost:8080", "token": "t"},
            "sources": []
        }"#;
        let cfg: ClientConfig = serde_json::from_str(json).unwrap();
        assert_eq!(cfg.watch.batch_window_secs, 5.0);
        assert!(cfg.watch.extractor_dir.is_none());
    }

    #[test]
    fn deprecated_max_file_size_mb_is_accepted() {
        // Old key should still parse (serde alias).
        let toml = r#"
[server]
url = "http://localhost:8080"
token = "t"

[scan]
max_file_size_mb = 50
"#;
        let (cfg, _) = parse_client_config(toml).unwrap();
        assert_eq!(cfg.scan.max_content_size_mb, 50);
    }

    // ── .index include field tests ────────────────────────────────────────────

    #[test]
    fn scan_override_include_toml_round_trip() {
        let toml = r#"include = ["myfolder/**", "*.md"]"#;
        let ov: ScanOverride = toml::from_str(toml).unwrap();
        assert_eq!(
            ov.include,
            Some(vec!["myfolder/**".to_string(), "*.md".to_string()])
        );
        // All other fields absent → None
        assert!(ov.exclude.is_none());
        assert!(ov.include_hidden.is_none());
    }

    #[test]
    fn scan_override_include_absent_is_none() {
        let toml = r#"exclude = ["*.log"]"#;
        let ov: ScanOverride = toml::from_str(toml).unwrap();
        assert!(ov.include.is_none());
    }

    #[test]
    fn apply_dir_override_sets_dir_include() {
        let base = ScanConfig::default();
        let dir = Path::new("/data/backups");
        let ov = ScanOverride {
            include: Some(vec!["myfolder/**".to_string()]),
            ..ScanOverride::default()
        };
        let result = base.apply_dir_override(&ov, dir);
        let (stored_dir, stored_patterns) = result.dir_include.as_ref().unwrap();
        assert_eq!(stored_dir, dir);
        assert_eq!(stored_patterns, &vec!["myfolder/**".to_string()]);
    }

    #[test]
    fn apply_dir_override_no_include_leaves_dir_include_none() {
        let base = ScanConfig::default();
        let dir = Path::new("/data/backups");
        let ov = ScanOverride {
            exclude: Some(vec!["*.tmp".into()]),
            ..ScanOverride::default()
        };
        let result = base.apply_dir_override(&ov, dir);
        assert!(result.dir_include.is_none());
    }

    #[test]
    fn apply_dir_override_inherits_parent_dir_include() {
        // If the parent already has a dir_include and the child override has no include,
        // the child inherits the parent's dir_include (via clone in apply_override).
        let parent_dir = Path::new("/data");
        let child_dir = Path::new("/data/sub");
        let base = ScanConfig {
            dir_include: Some((parent_dir.to_path_buf(), vec!["*.rs".to_string()])),
            ..ScanConfig::default()
        };
        let ov = ScanOverride {
            exclude: Some(vec!["*.log".into()]),
            ..ScanOverride::default()
        };
        let result = base.apply_dir_override(&ov, child_dir);
        // dir_include should be the parent's, not replaced
        let (stored_dir, stored_patterns) = result.dir_include.as_ref().unwrap();
        assert_eq!(stored_dir, parent_dir);
        assert_eq!(stored_patterns, &vec!["*.rs".to_string()]);
    }

    #[test]
    fn apply_dir_override_inner_include_replaces_outer() {
        // Replacement semantics: the innermost .index with include wins.
        let outer_dir = Path::new("/data");
        let inner_dir = Path::new("/data/sub");
        let base = ScanConfig {
            dir_include: Some((outer_dir.to_path_buf(), vec!["allowed/**".to_string()])),
            ..ScanConfig::default()
        };
        let ov = ScanOverride {
            include: Some(vec!["narrower/**".to_string()]),
            ..ScanOverride::default()
        };
        let result = base.apply_dir_override(&ov, inner_dir);
        let (stored_dir, stored_patterns) = result.dir_include.as_ref().unwrap();
        assert_eq!(stored_dir, inner_dir);
        assert_eq!(stored_patterns, &vec!["narrower/**".to_string()]);
    }

    #[test]
    fn dir_include_is_serde_skip() {
        // dir_include must not be serialised to TOML (it's runtime-only).
        let cfg = ScanConfig {
            dir_include: Some((
                PathBuf::from("/some/dir"),
                vec!["*.rs".to_string()],
            )),
            ..ScanConfig::default()
        };
        let serialised = toml::to_string(&cfg).unwrap();
        assert!(!serialised.contains("dir_include"));
    }
}
