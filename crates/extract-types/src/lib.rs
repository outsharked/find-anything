pub mod extractor_config;
pub mod index_line;
pub mod mem;
pub mod run;

pub use extractor_config::ExtractorConfig;
pub use index_line::{detect_kind_from_ext, IndexLine, SCANNER_VERSION};

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
