/// Configuration passed to extractor functions.
///
/// Bundles all per-extraction settings into one struct so that adding new
/// options in the future only requires updating this struct and its
/// construction site — not every function signature in the call chain.
///
/// Construction sites that don't care about a particular field can use
/// `..ExtractorConfig::default()` to forward-compatibly inherit the defaults.
#[derive(Debug, Clone)]
pub struct ExtractorConfig {
    /// Maximum content size in KB; content is truncated at this limit.
    pub max_content_kb: usize,
    /// Maximum archive nesting depth; prevents zip-bomb recursion.
    pub max_depth: usize,
    /// Maximum line length in characters for PDF extraction.
    /// Long lines are wrapped at word boundaries. 0 = no wrapping.
    pub max_line_length: usize,
    /// Maximum size in MB of a temporary file used when extracting nested 7z
    /// archives (which require a seekable file path) or oversized nested zips.
    /// Guards against excessive disk use. Default: 500 MB.
    pub max_temp_file_mb: usize,
    /// When false (default), archive members whose path contains a dot-prefixed
    /// component (e.g. `.terraform/`, `.git/`) are skipped entirely, consistent
    /// with the filesystem walk's `include_hidden = false` behaviour.
    pub include_hidden: bool,
    /// Maximum total uncompressed size in MB of a 7z solid block before
    /// falling back to filename-only extraction.  Maps to
    /// `scan.archives.max_7z_solid_block_mb`.  Default: 256 MB.
    pub max_7z_solid_block_mb: usize,
    /// Glob patterns (same syntax as `scan.exclude`) applied to archive member
    /// paths.  Members whose path matches any pattern are skipped entirely —
    /// not indexed by filename, not recursed into.  Empty = no filtering.
    pub exclude_patterns: Vec<String>,
}

impl Default for ExtractorConfig {
    fn default() -> Self {
        Self {
            max_content_kb: 10 * 1024,
            max_depth: 10,
            max_line_length: 120,
            max_temp_file_mb: 500,
            include_hidden: false,
            max_7z_solid_block_mb: 256,
            exclude_patterns: vec![],
        }
    }
}
