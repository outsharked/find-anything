pub mod extractor_config;
pub mod index_line;
pub mod mem;
pub mod run;

pub use extractor_config::{
    ExtractorConfig, ExternalDispatchMode, ExternalMemberDispatch,
};
pub use index_line::{
    detect_kind_from_ext, IndexLine, SCANNER_VERSION,
    LINE_PATH, LINE_METADATA, LINE_CONTENT_START,
};

/// Compute the content-store key for raw file `bytes`.
///
/// Mixes [`SCANNER_VERSION`] into the hash so that upgrading the extraction
/// logic produces a new key for every file.  Old blobs (under the previous
/// version's keys) become orphaned and are removed by the next compaction run,
/// while the fresh blobs carry the updated extracted content.
///
/// Returns `None` for empty slices to avoid deduplicating all empty files
/// under a single key.
pub fn content_hash(bytes: &[u8]) -> Option<String> {
    if bytes.is_empty() { return None; }
    let mut hasher = blake3::Hasher::new();
    hasher.update(bytes);
    hasher.update(&SCANNER_VERSION.to_le_bytes());
    Some(hasher.finalize().to_hex().to_string())
}

/// Build a [`globset::GlobSet`] from a list of glob patterns.
///
/// Backslashes are normalised to forward slashes. For patterns ending in
/// `/**`, the directory entry itself is also matched (e.g. `**/node_modules/**`
/// adds `**/node_modules`) so that filesystem `filter_entry` can prune a
/// directory before descending into it.
///
/// Used by both the filesystem walker (client) and the archive extractor so
/// that the same exclude patterns apply inside archives as on the filesystem.
pub fn build_globset(patterns: &[String]) -> anyhow::Result<globset::GlobSet> {
    let mut builder = globset::GlobSetBuilder::new();
    for pat in patterns {
        let pat = pat.replace('\\', "/");
        builder.add(globset::Glob::new(&pat)?);
        if let Some(dir_pat) = pat.strip_suffix("/**") {
            builder.add(globset::Glob::new(dir_pat)?);
        }
    }
    Ok(builder.build()?)
}
