// crates/server/src/stats_cache.rs

use std::collections::HashMap;
use std::path::Path;

use find_common::api::{ExtStat, FileKind, KindStats};

/// In-memory cache of per-source stats.  Wrapped in Arc<RwLock<...>> in AppState.
#[derive(Default, Clone)]
pub struct SourceStatsCache {
    pub sources: Vec<CachedSourceStats>,
    /// Unix timestamp of the last full rebuild.
    pub rebuilt_at: Option<i64>,
}

#[derive(Clone, Default)]
pub struct CachedSourceStats {
    pub name: String,
    pub total_files: usize,
    pub total_size:  i64,
    pub by_kind:     HashMap<FileKind, KindStats>,
    /// Only populated on full rebuild.
    pub by_ext:      Vec<ExtStat>,
    /// Only populated on full rebuild.
    pub fts_row_count: i64,
    /// Files whose content hasn't been written to ZIP yet.
    pub files_pending_content: usize,
}

/// Run all expensive queries for every source DB and store results in `cache`.
/// Called at startup, daily, and on `?refresh=true`.
pub fn full_rebuild(data_dir: &Path, cache: &std::sync::RwLock<SourceStatsCache>) {
    let sources_dir = data_dir.join("sources");
    let mut sources: Vec<CachedSourceStats> = Vec::new();

    let rd = match std::fs::read_dir(&sources_dir) {
        Ok(rd) => rd,
        Err(e) => { tracing::warn!("stats_cache: cannot read sources dir: {e:#}"); return; }
    };

    for entry in rd.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("db") { continue; }
        let source_name = match path.file_stem().and_then(|s| s.to_str()) {
            Some(s) => s.to_string(),
            None => continue,
        };
        let conn = match crate::db::open_for_stats(&path) {
            Ok(c) => c,
            Err(e) => { tracing::debug!("stats_cache: skipping {source_name}: {e:#}"); continue; }
        };
        let (total_files, total_size, by_kind) = crate::db::get_stats(&conn).unwrap_or_default();
        let by_ext     = crate::db::get_stats_by_ext(&conn).unwrap_or_default();
        let fts_row_count = crate::db::get_fts_row_count(&conn).unwrap_or(0);
        let files_pending_content = crate::db::get_files_pending_content(&conn).unwrap_or(0);
        sources.push(CachedSourceStats { name: source_name, total_files, total_size, by_kind, by_ext, fts_row_count, files_pending_content });
    }

    sources.sort_by(|a, b| a.name.cmp(&b.name));

    let source_count = sources.len();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;

    if let Ok(mut guard) = cache.write() {
        guard.sources = sources;
        guard.rebuilt_at = Some(now);
    }
    tracing::debug!("stats_cache: full rebuild complete ({source_count} sources)");
}

/// Per-source incremental delta — applied after each worker batch.
#[derive(Default)]
pub struct SourceStatsDelta {
    pub source: String,
    pub files_delta: i64,
    pub size_delta:  i64,
    /// Positive = added, negative = removed.
    pub kind_deltas: HashMap<FileKind, (i64, i64)>, // kind → (count_delta, size_delta)
}

impl SourceStatsCache {
    pub fn apply_delta(&mut self, delta: &SourceStatsDelta) {
        // Find the source entry, creating one on first use so new sources become
        // visible immediately rather than waiting for the next full rebuild.
        if !self.sources.iter().any(|s| s.name == delta.source) {
            self.sources.push(CachedSourceStats {
                name: delta.source.clone(),
                ..Default::default()
            });
            self.sources.sort_by(|a, b| a.name.cmp(&b.name));
        }
        let s = self.sources.iter_mut().find(|s| s.name == delta.source).unwrap();
        s.total_files = (s.total_files as i64 + delta.files_delta).max(0) as usize;
        s.total_size  = (s.total_size  + delta.size_delta).max(0);
        for (kind, (count_d, size_d)) in &delta.kind_deltas {
            let e = s.by_kind.entry(kind.clone()).or_default();
            e.count = (e.count as i64 + count_d).max(0) as usize;
            e.size  = (e.size  + size_d).max(0);
        }
    }
}
