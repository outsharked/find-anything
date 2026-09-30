// crates/server/src/stats_cache.rs

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use find_common::api::{ExtStat, FileKind, KindStats};
use find_content_store::ContentStore;

/// In-memory cache of per-source stats.  Wrapped in Arc<RwLock<...>> in AppState.
#[derive(Default, Clone)]
pub struct SourceStatsCache {
    pub sources: Vec<CachedSourceStats>,
    /// Unix timestamp of the last completed full rebuild.
    pub rebuilt_at: Option<i64>,
    /// Unix timestamp of the most recent full rebuild *start* (set even if it
    /// is still running or bailed out early), so the post-archive throttle
    /// also covers in-flight and failed rebuilds.
    pub rebuild_started_at: Option<i64>,
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

/// Minimum age of the cache before an archive-queue drain may trigger another
/// full rebuild.  A rebuild scans every source DB, so client pushes (which
/// drain the queue constantly) must not each cause one; the daily rebuild and
/// `?refresh=true` still refresh on their own schedule.
pub const POST_ARCHIVE_REBUILD_MIN_AGE_SECS: i64 = 3600;

impl SourceStatsCache {
    /// True if no rebuild has started or completed within the last
    /// `min_age_secs` before `now` (unix seconds).
    pub fn is_older_than(&self, min_age_secs: i64, now: i64) -> bool {
        match self.rebuilt_at.max(self.rebuild_started_at) {
            Some(at) => now - at >= min_age_secs,
            None => true,
        }
    }
}

/// Decide whether the archive worker should run a rebuild this tick.
///
/// `pending` is the worker's "stats are stale" flag: set whenever a batch was
/// archived, cleared only when a rebuild actually runs — so a rebuild skipped
/// by the throttle is made up on a later tick rather than waiting for the
/// daily rebuild.
pub fn post_archive_rebuild_step(pending: &mut bool, any_processed: bool, due: bool) -> bool {
    *pending |= any_processed;
    if *pending && due {
        *pending = false;
        true
    } else {
        false
    }
}

/// Run all expensive queries for every source DB and store results in `cache`.
/// Called at startup, daily, and on `?refresh=true`.
pub fn full_rebuild(
    data_dir: &Path,
    cache: &std::sync::RwLock<SourceStatsCache>,
    content_store: &Arc<dyn ContentStore>,
) {
    let started = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;
    if let Ok(mut guard) = cache.write() {
        guard.rebuild_started_at = Some(started);
    }

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
        let files_pending_content = crate::db::get_files_pending_content(&conn, content_store.as_ref()).unwrap_or(0);
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

#[cfg(test)]
mod tests {
    use super::*;
    use find_content_store::{CompactResult, ContentKey};
    use std::collections::HashSet;

    struct NoStore;
    impl ContentStore for NoStore {
        fn put(&self, _: &ContentKey, _: &str) -> anyhow::Result<bool> { Ok(false) }
        fn delete(&self, _: &ContentKey) -> anyhow::Result<()> { Ok(()) }
        fn get_lines(&self, _: &ContentKey, _: usize, _: usize) -> anyhow::Result<Option<Vec<(usize, String)>>> { Ok(None) }
        fn contains(&self, _: &ContentKey) -> anyhow::Result<bool> { Ok(false) }
        fn compact(&self, _: &HashSet<ContentKey>, _: bool) -> anyhow::Result<CompactResult> {
            Ok(CompactResult { units_scanned: 0, units_rewritten: 0, units_deleted: 0, chunks_removed: 0, bytes_freed: 0 })
        }
    }

    #[test]
    fn never_built_cache_is_always_due() {
        assert!(SourceStatsCache::default().is_older_than(3600, 1_000));
    }

    #[test]
    fn in_flight_or_failed_rebuild_also_throttles() {
        // Started 10s ago, never completed (still running, or bailed early).
        let c = SourceStatsCache { rebuild_started_at: Some(1_000), ..Default::default() };
        assert!(!c.is_older_than(3600, 1_010));
        assert!(c.is_older_than(3600, 1_000 + 3600));
    }

    #[test]
    fn full_rebuild_records_start_even_when_sources_dir_missing() {
        use std::sync::RwLock;
        let cache = RwLock::new(SourceStatsCache::default());
        let dir = std::env::temp_dir().join("find-anything-no-such-dir-xyz");
        let store: Arc<dyn ContentStore> = Arc::new(NoStore);
        full_rebuild(&dir, &cache, &store);
        let g = cache.read().unwrap();
        assert!(g.rebuild_started_at.is_some());
        assert!(g.rebuilt_at.is_none(), "early bail-out must not count as completed");
    }

    #[test]
    fn skipped_rebuild_is_made_up_later() {
        let mut pending = false;
        // Batch archived but throttled: no rebuild, flag stays set.
        assert!(!post_archive_rebuild_step(&mut pending, true, false));
        assert!(pending);
        // Idle tick, still throttled.
        assert!(!post_archive_rebuild_step(&mut pending, false, false));
        // Idle tick once the throttle window has passed: trailing rebuild runs.
        assert!(post_archive_rebuild_step(&mut pending, false, true));
        assert!(!pending);
        // Nothing new archived: no further rebuild.
        assert!(!post_archive_rebuild_step(&mut pending, false, true));
    }

    #[test]
    fn rebuild_throttled_until_min_age_elapses() {
        let c = SourceStatsCache { rebuilt_at: Some(1_000), ..Default::default() };
        assert!(!c.is_older_than(3600, 1_000 + 3599));
        assert!(c.is_older_than(3600, 1_000 + 3600));
    }
}
