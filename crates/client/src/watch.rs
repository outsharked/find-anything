use std::collections::hash_map::Entry;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use anyhow::Result;
use globset::{Glob, GlobSet, GlobSetBuilder};
use notify::{Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use tokio::sync::mpsc;
use tracing::{info, warn};

use find_common::{
    api::{detect_kind_from_ext, BulkRequest},
    config::{load_dir_override, ClientConfig, ScanConfig, SourceConfig},
};

use crate::api::ApiClient;
use crate::batch::build_index_files;
use crate::subprocess;

/// (root_path, source_name, root_str, include_globset)
type SourceMap = Vec<(PathBuf, String, String, GlobSet)>;

/// What to do with a path after debounce.
#[derive(Debug)]
enum AccumulatedKind {
    Update,
    Delete,
}

pub async fn run_watch(config: &ClientConfig) -> Result<()> {
    let api = ApiClient::new(&config.server.url, &config.server.token);
    let source_map = build_source_map(&config.sources);

    if source_map.is_empty() {
        anyhow::bail!("no source paths configured");
    }

    info!("find-watch starting — watching {} source(s):", config.sources.len());
    for src in &config.sources {
        info!("  source {:?}: {:?}", src.name, src.path);
    }

    let debounce_ms = config.watch.debounce_ms;

    // Channel: notify (blocking thread) → tokio event loop.
    let (tx, mut rx) = mpsc::channel::<notify::Result<Event>>(1000);

    let mut watcher = RecommendedWatcher::new(
        move |res| {
            let _ = tx.blocking_send(res);
        },
        notify::Config::default(),
    )?;

    for (root, _, _, _) in &source_map {
        watcher.watch(root, RecursiveMode::Recursive)?;
        info!("watching {:?}", root);
    }

    // Debounce accumulator: path → what to do.
    let mut pending: HashMap<PathBuf, AccumulatedKind> = HashMap::new();

    loop {
        // Wait for the first event (or a timeout to flush pending).
        let timeout_dur = tokio::time::Duration::from_millis(debounce_ms);

        let got_event = if pending.is_empty() {
            // Nothing pending — block indefinitely.
            match rx.recv().await {
                Some(ev) => { accumulate(&mut pending, ev); true }
                None => break, // channel closed
            }
        } else {
            // Events pending — wait up to debounce_ms for another.
            match tokio::time::timeout(timeout_dur, rx.recv()).await {
                Ok(Some(ev)) => { accumulate(&mut pending, ev); true }
                Ok(None)     => break, // channel closed
                Err(_)       => false, // timeout — time to flush
            }
        };

        if got_event {
            // Drain any immediately-available events (non-blocking).
            while let Ok(ev) = rx.try_recv() {
                accumulate(&mut pending, ev);
            }
            // Reset debounce window: go back to the top of the loop.
            // The pending block will now wait debounce_ms again.
            continue;
        }

        // Flush accumulated events.
        let batch = std::mem::take(&mut pending);
        for (abs_path, kind) in batch {
            // Skip paths that contain '::' — those are archive member paths
            // managed server-side, not real filesystem paths.
            let path_str = abs_path.to_string_lossy();
            if path_str.contains("::") {
                continue;
            }

            // Find which source this file belongs to (also returns the source root).
            let Some((source_name, rel_path, source_root, source_includes)) =
                find_source(&abs_path, &source_map)
            else {
                continue;
            };

            // Apply source-level include filter.
            if !source_includes.is_empty() && !source_includes.is_match(&*rel_path) {
                continue;
            }

            // Resolve per-directory effective config: check for .noindex and .index files
            // on the ancestor chain. No caching is needed for watch (events are infrequent).
            let (eff_scan, skip) = resolve_watch_config(&abs_path, &source_root, &config.scan);
            if skip {
                tracing::debug!("skipping {} (in .noindex subtree)", abs_path.display());
                continue;
            }

            // Apply per-directory exclusion globs.
            let eff_excludes = match build_globset(&eff_scan.exclude) {
                Ok(gs) => gs,
                Err(e) => {
                    warn!("invalid exclude pattern for {}: {e:#}", abs_path.display());
                    continue;
                }
            };
            if is_excluded(&abs_path, &source_map, &eff_excludes) {
                continue;
            }

            match kind {
                AccumulatedKind::Update => {
                    // Only process if it exists and is a regular file.
                    if !abs_path.is_file() {
                        continue;
                    }
                    if let Err(e) = handle_update(
                        &api,
                        &source_name,
                        &abs_path,
                        &rel_path,
                        &eff_scan,
                        &config.watch.extractor_dir,
                    )
                    .await
                    {
                        warn!("update {}: {e:#}", abs_path.display());
                    }
                }
                AccumulatedKind::Delete => {
                    if let Err(e) = handle_delete(&api, &source_name, &rel_path).await {
                        warn!("delete {}: {e:#}", abs_path.display());
                    }
                }
            }
        }
    }

    Ok(())
}

// ── Source map ────────────────────────────────────────────────────────────────

fn build_source_map(sources: &[SourceConfig]) -> SourceMap {
    let mut map = Vec::new();
    for src in sources {
        let root_str = normalise_root(&src.path);
        let root = PathBuf::from(&root_str);
        let includes = build_globset(&src.include).unwrap_or_default();
        map.push((root, src.name.clone(), root_str, includes));
    }
    map
}

/// Return `(source_name, rel_path, source_root, include_globset)` for a given absolute path.
/// Picks the most-specific (longest) matching root.
fn find_source<'a>(path: &Path, map: &'a SourceMap) -> Option<(String, String, PathBuf, &'a GlobSet)> {
    let mut best: Option<(&PathBuf, &String, &String, &GlobSet)> = None;
    for (root, name, root_str, includes) in map {
        if path.starts_with(root)
            && best.is_none_or(|(b, _, _, _)| root.as_os_str().len() > b.as_os_str().len())
        {
            best = Some((root, name, root_str, includes));
        }
    }
    best.map(|(root, name, _, includes)| {
        let rel = normalise_path_sep(&path.strip_prefix(root).unwrap().to_string_lossy());
        (name.clone(), rel, root.clone(), includes)
    })
}

// ── Exclusion ─────────────────────────────────────────────────────────────────

fn build_globset(patterns: &[String]) -> Result<GlobSet> {
    let mut builder = GlobSetBuilder::new();
    for pat in patterns {
        // Normalise backslashes so Windows-style patterns work correctly.
        let pat = pat.replace('\\', "/");
        builder.add(Glob::new(&pat)?);
        if let Some(dir_pat) = pat.strip_suffix("/**") {
            builder.add(Glob::new(dir_pat)?);
        }
    }
    Ok(builder.build()?)
}

fn is_excluded(abs_path: &Path, source_map: &SourceMap, excludes: &GlobSet) -> bool {
    // Find the root for this path and check relative path against excludes.
    for (root, _, _, _) in source_map {
        if let Ok(rel) = abs_path.strip_prefix(root) {
            let rel_normalised = normalise_path_sep(&rel.to_string_lossy());
            if excludes.is_match(&*rel_normalised) {
                return true;
            }
        }
    }
    false
}

/// On Windows, replace backslash separators with forward slashes so paths are
/// stored consistently. On Unix, backslash is a valid filename character.
#[cfg(windows)]
fn normalise_path_sep(s: &str) -> String {
    s.replace('\\', "/")
}

#[cfg(not(windows))]
fn normalise_path_sep(s: &str) -> String {
    s.to_string()
}

/// On Windows, normalise a bare drive letter like `"C:"` to `"C:/"` so that
/// `starts_with` and `strip_prefix` work correctly against absolute paths.
#[cfg(windows)]
fn normalise_root(s: &str) -> String {
    if s.len() == 2 && s.as_bytes()[1] == b':' {
        format!("{s}/")
    } else {
        s.to_string()
    }
}

#[cfg(not(windows))]
fn normalise_root(s: &str) -> String {
    s.to_string()
}

// ── Event accumulation ────────────────────────────────────────────────────────

fn accumulate(pending: &mut HashMap<PathBuf, AccumulatedKind>, res: notify::Result<Event>) {
    let event = match res {
        Ok(e) => e,
        Err(e) => { warn!("watch error: {e:#}"); return; }
    };

    for path in event.paths {
        let new_kind = match &event.kind {
            EventKind::Create(_) => AccumulatedKind::Update,
            EventKind::Modify(notify::event::ModifyKind::Data(_)) => AccumulatedKind::Update,
            EventKind::Modify(notify::event::ModifyKind::Name(_)) => {
                // Renames: notify sends From path as Remove-like and To path as Create-like,
                // but both arrive as Modify(Name). We treat each independently:
                // if the file now exists → Update, otherwise → Delete.
                if path.exists() {
                    AccumulatedKind::Update
                } else {
                    AccumulatedKind::Delete
                }
            }
            EventKind::Remove(_) => AccumulatedKind::Delete,
            // Ignore access, metadata-only modify, other events.
            _ => continue,
        };

        match pending.entry(path) {
            Entry::Occupied(mut occ) => {
                // Collapse: Update→Delete = Delete, Delete→Update = Update.
                let existing = occ.get_mut();
                *existing = match (&*existing, &new_kind) {
                    (AccumulatedKind::Update, AccumulatedKind::Delete) => AccumulatedKind::Delete,
                    (AccumulatedKind::Delete, AccumulatedKind::Update) => AccumulatedKind::Update,
                    _ => new_kind,
                };
            }
            Entry::Vacant(vac) => {
                vac.insert(new_kind);
            }
        }
    }
}

// ── Per-directory config resolution ──────────────────────────────────────────

/// Walk from `file_path` up to `source_root`, applying `.index` overrides and
/// checking for `.noindex` markers.
///
/// Returns `(effective_scan_config, skip)`. If `skip` is true, the file is
/// inside a `.noindex` subtree and should be ignored. No cache is maintained
/// since watch events are infrequent (a few filesystem stat calls per event is
/// acceptable).
fn resolve_watch_config(
    file_path: &Path,
    source_root: &Path,
    global: &ScanConfig,
) -> (ScanConfig, bool) {
    // Collect ancestor directories from source_root down to file's parent.
    let start = match file_path.parent() {
        Some(p) if p.starts_with(source_root) => p,
        _ => return (global.clone(), false),
    };

    let mut ancestors: Vec<PathBuf> = Vec::new();
    let mut cur = start;
    loop {
        ancestors.push(cur.to_path_buf());
        if cur == source_root {
            break;
        }
        match cur.parent() {
            Some(p) => cur = p,
            None => break,
        }
    }
    ancestors.reverse(); // source_root → file's parent

    // Check for .noindex from root down: if any ancestor has it, skip.
    for dir in &ancestors {
        if dir.join(&global.noindex_file).exists() {
            return (global.clone(), true);
        }
    }

    // Apply .index overrides from root → file's parent.
    let mut eff = global.clone();
    for dir in &ancestors {
        if let Some(ov) = load_dir_override(dir, &global.index_file) {
            eff = eff.apply_override(&ov);
        }
    }

    (eff, false)
}

// ── File handling ─────────────────────────────────────────────────────────────

async fn handle_update(
    api: &ApiClient,
    source_name: &str,
    abs_path: &Path,
    rel_path: &str,
    eff_scan: &ScanConfig,
    extractor_dir: &Option<String>,
) -> Result<()> {
    info!("update: {}", rel_path);

    let lines = match subprocess::extract_via_subprocess(abs_path, eff_scan, extractor_dir).await {
        subprocess::SubprocessOutcome::Ok(lines) => lines,
        subprocess::SubprocessOutcome::BinaryMissing => return Ok(()),
        subprocess::SubprocessOutcome::Failed => vec![],
    };

    let mtime = mtime_of(abs_path).unwrap_or(0);
    let size = size_of(abs_path).unwrap_or(0);
    let ext = abs_path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("");
    let kind = detect_kind_from_ext(ext).to_string();

    let mut files = build_index_files(rel_path.to_string(), mtime, size, kind, lines);

    api.bulk(&BulkRequest {
        source: source_name.to_string(),
        files: std::mem::take(&mut files),
        delete_paths: vec![],
        scan_timestamp: None,
        indexing_failures: vec![],
    })
    .await
}

async fn handle_delete(
    api: &ApiClient,
    source_name: &str,
    rel_path: &str,
) -> Result<()> {
    info!("delete: {}", rel_path);

    api.bulk(&BulkRequest {
        source: source_name.to_string(),
        files: vec![],
        delete_paths: vec![rel_path.to_string()],
        scan_timestamp: None,
        indexing_failures: vec![],
    })
    .await
}

// ── Filesystem helpers ────────────────────────────────────────────────────────

fn mtime_of(path: &Path) -> Option<i64> {
    path.metadata()
        .ok()?
        .modified()
        .ok()?
        .duration_since(UNIX_EPOCH)
        .ok()
        .map(|d| d.as_secs() as i64)
}

fn size_of(path: &Path) -> Option<i64> {
    path.metadata().ok().map(|m| m.len() as i64)
}
