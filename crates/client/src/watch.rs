use std::collections::hash_map::Entry;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::{Duration, UNIX_EPOCH};

use anyhow::Result;
use globset::GlobSet;
use notify::{Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use tokio::sync::mpsc;
use tracing::{info, warn};

use find_common::{
    api::{detect_kind_from_ext, BulkRequest, IndexFile, PathRename},
    config::{extractor_config_from_scan, load_dir_override, ClientConfig, ExternalExtractorMode, ScanConfig, SourceConfig},
    path::is_composite,
};

use walkdir::WalkDir;
use crate::api::ApiClient;
use crate::batch::build_index_files;
use crate::subprocess;

/// Options passed to `run_watch` from the CLI entry point.
pub struct WatchOptions {
    /// Path to the client config file; forwarded to scheduled `find-scan` invocations.
    pub config_path: String,
    /// If true, run one `find-scan` immediately at startup before the interval begins.
    pub scan_now: bool,
}

/// (root_path, source_name, root_str, include_globset, include_dir_terminals)
///
/// `include_dir_terminals` is `None` when no pruning can be determined (e.g.
/// `**/*.rs` patterns), meaning all directories must be traversed.  When
/// `Some`, only directories inside the terminal set need to be watched.
type SourceMap = Vec<(PathBuf, String, String, GlobSet, Option<std::collections::HashSet<String>>)>;

/// What to do with a path after debounce.
#[derive(Debug, Clone, PartialEq, Eq)]
enum AccumulatedKind {
    Create, // OS create event (file definitely new)
    Update, // OS modify event (or create→modify collapse)
    Delete,
}

/// Collapse an existing accumulated event with a newly arrived event.
///
/// Transition table:
/// - `Create + Modify  → Create`  (still a new file; modifier doesn't change that)
/// - `Create + Delete  → Delete`  (created then immediately deleted)
/// - `Delete + Create  → Create`  (deleted then re-created)
/// - `Update + Delete  → Delete`
/// - `Delete + Update  → Update`  (unknown history — treat as a modify)
/// - anything else    → `new`     (last event wins)
fn collapse(existing: &AccumulatedKind, new: &AccumulatedKind) -> AccumulatedKind {
    match (existing, new) {
        (AccumulatedKind::Create, AccumulatedKind::Update) => AccumulatedKind::Create,
        (AccumulatedKind::Create, AccumulatedKind::Delete) => AccumulatedKind::Delete,
        (AccumulatedKind::Delete, AccumulatedKind::Create) => AccumulatedKind::Create,
        (AccumulatedKind::Update, AccumulatedKind::Delete) => AccumulatedKind::Delete,
        (AccumulatedKind::Delete, AccumulatedKind::Update) => AccumulatedKind::Update,
        _                                                  => new.clone(),
    }
}

pub async fn run_watch(config: &ClientConfig, opts: &WatchOptions) -> Result<()> {
    // Spawn the periodic find-scan scheduler as a background task.
    {
        let config_path = opts.config_path.clone();
        let scan_now = opts.scan_now;
        let interval_hours = config.watch.scan_interval_hours;
        tokio::spawn(async move {
            run_scan_scheduler(interval_hours, &config_path, scan_now).await;
        });
    }

    let api = ApiClient::new(&config.server.url, &config.server.token);
    let source_map = build_source_map(&config.sources);

    if source_map.is_empty() {
        anyhow::bail!("no source paths configured");
    }

    info!("find-watch starting — watching {} source(s):", config.sources.len());
    for src in &config.sources {
        info!("  source {:?}: {:?}", src.name, src.path);
    }

    let batch_window = std::time::Duration::from_secs_f64(config.watch.batch_window_secs);
    let batch_limit  = config.scan.batch_size;

    // Channel: notify (blocking thread) → tokio event loop.
    let (tx, mut rx) = mpsc::channel::<notify::Result<Event>>(1000);

    let mut watcher = RecommendedWatcher::new(
        move |res| {
            let _ = tx.blocking_send(res);
        },
        notify::Config::default(),
    )?;

    let global_excludes = build_globset(&config.scan.exclude).unwrap_or_default();
    for (root, name, _, _, terminals) in &source_map {
        tracing::debug!(
            "source {:?}: root={:?} terminals={:?}",
            name, root, terminals
        );
        let n = watch_tree(&mut watcher, root, terminals.as_ref(), &global_excludes, &config.scan);
        info!("watching {:?} ({n} directories registered)", root);
    }

    // Accumulator: path → what to do.
    let mut pending: HashMap<PathBuf, AccumulatedKind> = HashMap::new();
    // Paths whose very first event in this window was a Create (i.e. never previously indexed).
    // Used by rename detection to avoid sending a rename when the old path was ephemeral.
    let mut first_seen_creates: HashSet<PathBuf> = HashSet::new();
    // When the batch window opened (i.e. when the first event in this batch arrived).
    let mut window_start: Option<tokio::time::Instant> = None;

    loop {
        // Decide whether to flush before waiting for the next event.
        let flush = if pending.is_empty() {
            // Nothing pending — block indefinitely waiting for the first event.
            match rx.recv().await {
                Some(ev) => {
                    accumulate(&mut pending, &mut first_seen_creates, ev);
                    window_start = Some(tokio::time::Instant::now());
                    false
                }
                None => break, // channel closed
            }
        } else {
            // Events are buffered. Compute how much of the window remains.
            let elapsed = window_start.map(|s| s.elapsed()).unwrap_or(batch_window);
            let remaining = batch_window.saturating_sub(elapsed);

            if remaining.is_zero() {
                true // window expired — flush now
            } else {
                // Wait for either a new event or the window to expire.
                match tokio::time::timeout(remaining, rx.recv()).await {
                    Ok(Some(ev)) => { accumulate(&mut pending, &mut first_seen_creates, ev); false }
                    Ok(None)     => break, // channel closed
                    Err(_)       => true,  // window expired
                }
            }
        };

        // Drain any immediately-available events before deciding to flush.
        while let Ok(ev) = rx.try_recv() {
            accumulate(&mut pending, &mut first_seen_creates, ev);
        }

        // Flush if the window expired or the batch has hit its size limit.
        if !flush && pending.len() < batch_limit {
            continue;
        }

        if pending.is_empty() {
            window_start = None;
            continue;
        }

        window_start = None;

        // Flush accumulated events.
        let mut batch = std::mem::take(&mut pending);
        let fresh_creates = std::mem::take(&mut first_seen_creates);

        // Detect rename pairs and process them; removes paired entries from batch.
        process_renames(&mut batch, &source_map, &config.scan, &config.watch.extractor_dir, &api, &fresh_creates).await;

        for (abs_path, kind) in batch {
            // Skip paths that contain '::' — those are archive member paths
            // managed server-side, not real filesystem paths.
            let path_str = abs_path.to_string_lossy();
            if is_composite(&path_str) {
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
                tracing::debug!("filtered (not in source includes): {}", rel_path);
                continue;
            }

            // Resolve per-directory effective config: check for .noindex and .index files
            // on the ancestor chain. No caching is needed for watch (events are infrequent).
            let (eff_scan, skip) = resolve_watch_config(&abs_path, &source_root, &config.scan);
            if skip {
                tracing::debug!("skipping {} (in .noindex subtree)", abs_path.display());
                continue;
            }

            // Apply per-directory include filter from a .index file.
            if let Some((dir_path, patterns)) = &eff_scan.dir_include {
                match build_globset(patterns) {
                    Ok(dir_includes) if !dir_includes.is_empty() => {
                        let rel_to_dir = abs_path
                            .strip_prefix(dir_path)
                            .map(|p| p.to_string_lossy().replace('\\', "/"))
                            .unwrap_or_default();
                        if !dir_includes.is_match(&*rel_to_dir) {
                            tracing::debug!("skipping {} (not in .index include)", abs_path.display());
                            continue;
                        }
                    }
                    _ => {}
                }
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
                AccumulatedKind::Create | AccumulatedKind::Update => {
                    // When a new directory is created, register watches for it and
                    // any subdirectories it already contains (e.g. a copied tree).
                    // Pass None for terminals: the dir is already inside the included
                    // area (we got an event for it), so no further path pruning is needed.
                    if abs_path.is_dir() {
                        watch_tree(&mut watcher, &abs_path, None, &global_excludes, &config.scan);
                        continue;
                    }
                    // Only process if it exists and is a regular file.
                    if !abs_path.is_file() {
                        continue;
                    }
                    let is_new = matches!(kind, AccumulatedKind::Create);
                    if let Err(e) = handle_update(
                        &api,
                        &source_name,
                        &abs_path,
                        &rel_path,
                        &eff_scan,
                        &config.watch.extractor_dir,
                        is_new,
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
        let terminals = crate::path_util::include_dir_prefixes(&src.include);
        map.push((root, src.name.clone(), root_str, includes, terminals));
    }
    map
}

/// Return `(source_name, rel_path, source_root, include_globset)` for a given absolute path.
/// Picks the most-specific (longest) matching root.
fn find_source<'a>(path: &Path, map: &'a SourceMap) -> Option<(String, String, PathBuf, &'a GlobSet)> {
    let mut best: Option<(&PathBuf, &String, &String, &GlobSet)> = None;
    for (root, name, root_str, includes, _terminals) in map {
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

use crate::walk::build_globset;

fn is_excluded(abs_path: &Path, source_map: &SourceMap, excludes: &GlobSet) -> bool {
    // Find the root for this path and check relative path against excludes.
    for (root, _, _, _, _) in source_map {
        if let Ok(rel) = abs_path.strip_prefix(root) {
            let rel_normalised = normalise_path_sep(&rel.to_string_lossy());
            if excludes.is_match(&*rel_normalised) {
                return true;
            }
        }
    }
    false
}

use crate::path_util::{normalise_path_sep, normalise_root};

/// Walk `root` registering a `NonRecursive` inotify watch for every accessible,
/// non-excluded directory.  Returns the number of directories registered.
///
/// Delegates all filtering to [`crate::walk::walk_source_tree`] so the rules
/// are identical to `find-scan`.  Per-directory `NonRecursive` watches mean
/// individual inaccessible subdirectories don't abort the entire setup.  New
/// directories are handled dynamically when the event loop sees a `Create`
/// event on a directory path.
fn watch_tree(
    watcher: &mut RecommendedWatcher,
    root: &Path,
    terminals: Option<&std::collections::HashSet<String>>,
    excludes: &GlobSet,
    scan: &find_common::config::ScanConfig,
) -> usize {
    let mut count = 0usize;
    crate::walk::walk_source_tree(root, root, scan, excludes, terminals, |item| {
        let crate::walk::WalkItem::Dir(dir_path) = item else { return; };
        match watcher.watch(&dir_path, RecursiveMode::NonRecursive) {
            Ok(()) => {
                tracing::debug!("watch: registered {:?}", dir_path);
                count += 1;
            }
            Err(e) => {
                let is_denied = matches!(
                    &e.kind,
                    notify::ErrorKind::Io(io) if io.kind() == std::io::ErrorKind::PermissionDenied
                );
                if is_denied {
                    warn!("watch: skipping inaccessible directory {:?}", dir_path);
                } else {
                    warn!("watch: could not register {:?}: {e:#}", dir_path);
                }
            }
        }
    });
    count
}


// ── Event accumulation ────────────────────────────────────────────────────────

fn accumulate(
    pending: &mut HashMap<PathBuf, AccumulatedKind>,
    first_seen_creates: &mut HashSet<PathBuf>,
    res: notify::Result<Event>,
) {
    let event = match res {
        Ok(e) => e,
        Err(e) => { warn!("watch error: {e:#}"); return; }
    };

    tracing::debug!("watch event: {:?} paths={:?}", event.kind, event.paths);
    for path in event.paths {
        let new_kind = match &event.kind {
            EventKind::Create(_) => AccumulatedKind::Create,
            // Data(_): inotify/kqueue — distinguishes data writes from metadata changes.
            // Any:     ReadDirectoryChangesW (Windows) — FILE_ACTION_MODIFIED maps here;
            //          Windows does not distinguish data vs metadata in this API.
            EventKind::Modify(notify::event::ModifyKind::Data(_))
            | EventKind::Modify(notify::event::ModifyKind::Any) => AccumulatedKind::Update,
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

        match pending.entry(path.clone()) {
            Entry::Occupied(mut occ) => {
                let existing = occ.get_mut();
                *existing = collapse(existing, &new_kind);
            }
            Entry::Vacant(vac) => {
                // Track paths whose first event was a Create — these were never previously
                // indexed, so a rename of such a path should index the destination rather
                // than sending a path-rename that the server has no record to rename.
                if new_kind == AccumulatedKind::Create {
                    first_seen_creates.insert(path);
                }
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
            eff = eff.apply_dir_override(&ov, dir);
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
    is_new: bool,
) -> Result<()> {
    info!("update: {}", rel_path);

    let lines = match subprocess::resolve_extractor(abs_path, eff_scan) {
        subprocess::ExtractorChoice::External(ref ext_cfg) => match ext_cfg.mode {
            ExternalExtractorMode::Stdout => {
                match subprocess::run_external_stdout(abs_path, ext_cfg, eff_scan).await {
                    subprocess::ExternalOutcome::Ok(lines) => lines,
                    subprocess::ExternalOutcome::BinaryMissing => return Ok(()),
                    subprocess::ExternalOutcome::Failed(_) => vec![],
                }
            }
            ExternalExtractorMode::TempDir => {
                let ext_config = extractor_config_from_scan(eff_scan);
                match subprocess::run_external_tempdir(abs_path, ext_cfg, eff_scan, &ext_config).await {
                    subprocess::ExternalOutcome::Ok(lines) => lines,
                    subprocess::ExternalOutcome::BinaryMissing => return Ok(()),
                    subprocess::ExternalOutcome::Failed(_) => vec![],
                }
            }
        },
        subprocess::ExtractorChoice::Builtin => {
            match subprocess::extract_via_subprocess(abs_path, eff_scan, extractor_dir).await {
                subprocess::SubprocessOutcome::Ok(lines) => lines,
                subprocess::SubprocessOutcome::BinaryMissing => return Ok(()),
                subprocess::SubprocessOutcome::Failed => vec![],
            }
        }
    };

    let mtime = mtime_of(abs_path).unwrap_or(0);
    let size = size_of(abs_path).unwrap_or(0);
    let ext = abs_path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("");
    // If the extractor returned archive members, treat the outer file as "archive"
    // regardless of extension (e.g. .rar mapped to an external tempdir extractor).
    let kind = if lines.iter().any(|l| l.archive_path.is_some()) {
        "archive".to_string()
    } else {
        detect_kind_from_ext(ext).to_string()
    };

    let mut files = build_index_files(rel_path.to_string(), mtime, size, kind, lines);
    if let Some(f) = files.first_mut() {
        f.is_new = is_new;
    }

    api.bulk(&BulkRequest {
        source: source_name.to_string(),
        files: std::mem::take(&mut files),
        delete_paths: vec![],
        scan_timestamp: None,
        indexing_failures: vec![],
        rename_paths: vec![],
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
        rename_paths: vec![],
    })
    .await
}

// ── Rename detection ─────────────────────────────────────────────────────────

/// Detect rename pairs in a debounce-flush batch and send the appropriate
/// `BulkRequest`s. Removes handled entries from `batch` so the caller's
/// fallback loop can process remaining events with standard delete/update logic.
async fn process_renames(
    batch: &mut HashMap<PathBuf, AccumulatedKind>,
    source_map: &SourceMap,
    global_scan: &ScanConfig,
    extractor_dir: &Option<String>,
    api: &ApiClient,
    first_seen_creates: &HashSet<PathBuf>,
) {
    // --- Directory rename detection ---
    // Look for a Delete path paired with an Update path that is a directory,
    // in the same parent directory and the same source (1:1 per parent).

    let dir_updates: Vec<PathBuf> = batch
        .iter()
        .filter(|(p, k)| matches!(k, AccumulatedKind::Create | AccumulatedKind::Update) && p.is_dir())
        .map(|(p, _)| p.clone())
        .collect();

    let deletes: Vec<PathBuf> = batch
        .iter()
        .filter(|(_, k)| matches!(k, AccumulatedKind::Delete))
        .map(|(p, _)| p.clone())
        .collect();

    // Index directory updates by parent.
    let mut dir_upd_by_parent: HashMap<PathBuf, Vec<PathBuf>> = HashMap::new();
    for p in &dir_updates {
        if let Some(parent) = p.parent() {
            dir_upd_by_parent.entry(parent.to_path_buf()).or_default().push(p.clone());
        }
    }

    // Index deletes by parent.
    let mut del_by_parent: HashMap<PathBuf, Vec<PathBuf>> = HashMap::new();
    for p in &deletes {
        if let Some(parent) = p.parent() {
            del_by_parent.entry(parent.to_path_buf()).or_default().push(p.clone());
        }
    }

    let mut handled: HashSet<PathBuf> = HashSet::new();

    for (parent, del_paths) in &del_by_parent {
        let Some(upd_dirs) = dir_upd_by_parent.get(parent) else { continue };
        // Only pair when exactly 1 delete and 1 directory update share this parent.
        if del_paths.len() != 1 || upd_dirs.len() != 1 {
            continue;
        }
        let old_dir = &del_paths[0];
        let new_dir = &upd_dirs[0];

        let Some((source_name, old_rel_dir, source_root, source_includes)) =
            find_source(old_dir, source_map)
        else {
            continue;
        };
        let Some((new_source_name, new_rel_dir, _, _)) = find_source(new_dir, source_map) else {
            continue;
        };
        if source_name != new_source_name {
            continue;
        }

        if let Err(e) = handle_dir_rename(
            api,
            &source_name,
            old_dir,
            new_dir,
            &old_rel_dir,
            &new_rel_dir,
            &source_root,
            source_includes,
            global_scan,
            extractor_dir,
        )
        .await
        {
            warn!("dir rename {} → {}: {e:#}", old_dir.display(), new_dir.display());
        }

        // Mark the directory entries and all child events as handled.
        handled.insert(old_dir.clone());
        handled.insert(new_dir.clone());
        let keys: Vec<PathBuf> = batch.keys().cloned().collect();
        for path in keys {
            if path.starts_with(old_dir) || path.starts_with(new_dir) {
                handled.insert(path);
            }
        }
    }

    for p in &handled {
        batch.remove(p);
    }

    // --- Single file rename detection ---
    // Group remaining Delete+Update pairs by (source_name, parent_dir).
    // Pairs with exactly 1 delete and 1 file-update per group are treated as renames.

    type GroupKey = (String, PathBuf);
    let mut file_del_groups: HashMap<GroupKey, Vec<PathBuf>> = HashMap::new();
    let mut file_upd_groups: HashMap<GroupKey, Vec<PathBuf>> = HashMap::new();

    for (path, kind) in batch.iter() {
        if is_composite(&path.to_string_lossy()) {
            continue;
        }
        let Some((src, _, _, _)) = find_source(path, source_map) else { continue };
        let Some(parent) = path.parent() else { continue };
        let key = (src, parent.to_path_buf());
        match kind {
            AccumulatedKind::Delete if !path.exists() => {
                file_del_groups.entry(key).or_default().push(path.clone());
            }
            AccumulatedKind::Create | AccumulatedKind::Update if path.is_file() => {
                file_upd_groups.entry(key).or_default().push(path.clone());
            }
            _ => {}
        }
    }

    let mut file_handled: HashSet<PathBuf> = HashSet::new();

    for (key, del_paths) in &file_del_groups {
        if del_paths.len() != 1 {
            continue;
        }
        let Some(upd_paths) = file_upd_groups.get(key) else { continue };
        if upd_paths.len() != 1 {
            continue;
        }
        let old_path = &del_paths[0];
        let new_path = &upd_paths[0];

        let Some((source_name, old_rel, source_root, source_includes)) =
            find_source(old_path, source_map)
        else {
            continue;
        };
        let Some((_, new_rel, _, _)) = find_source(new_path, source_map) else { continue };

        // Check source-level include for new path.
        if !source_includes.is_empty() && !source_includes.is_match(&*new_rel) {
            continue; // excluded by source include — fall back to plain delete
        }

        // Check per-directory config for new path.
        let (eff_scan, skip) = resolve_watch_config(new_path, &source_root, global_scan);
        if skip {
            continue; // .noindex subtree — fall back to plain delete
        }
        let eff_excludes = match build_globset(&eff_scan.exclude) {
            Ok(gs) => gs,
            Err(_) => continue,
        };
        if is_excluded(new_path, source_map, &eff_excludes) {
            continue; // excluded by per-dir glob — fall back to plain delete
        }

        // If the old path was first seen as a Create in this window, it was never
        // previously indexed (e.g. a browser's temporary download file that was
        // renamed to its final name before the batch flushed). Sending a rename
        // would be a no-op on the server. Instead, upgrade the new path's
        // accumulated kind to Create so the main loop indexes it as a new file.
        if first_seen_creates.contains(old_path) {
            info!("create+rename: indexing new file {}", new_rel);
            file_handled.insert(old_path.clone());
            if let Some(kind) = batch.get_mut(new_path) {
                *kind = AccumulatedKind::Create;
            }
            continue;
        }

        info!("rename: {} → {}", old_rel, new_rel);
        if let Err(e) = api
            .bulk(&BulkRequest {
                source: source_name,
                files: vec![],
                delete_paths: vec![],
                rename_paths: vec![PathRename { old_path: old_rel, new_path: new_rel }],
                scan_timestamp: None,
                indexing_failures: vec![],
            })
            .await
        {
            warn!("rename {}: {e:#}", old_path.display());
            continue; // leave in batch — fall back to plain delete + re-index
        }

        file_handled.insert(old_path.clone());
        file_handled.insert(new_path.clone());
    }

    for p in &file_handled {
        batch.remove(p);
    }
}

/// Walk `new_dir` on disk, re-evaluate include/exclude rules for each file,
/// and emit a single `BulkRequest` covering renames, deletes, and new upserts.
#[allow(clippy::too_many_arguments)]
async fn handle_dir_rename(
    api: &ApiClient,
    source_name: &str,
    _old_dir: &Path,
    new_dir: &Path,
    old_rel_dir: &str,
    new_rel_dir: &str,
    source_root: &Path,
    source_includes: &GlobSet,
    global_scan: &ScanConfig,
    extractor_dir: &Option<String>,
) -> Result<()> {
    let mut rename_paths: Vec<PathRename> = Vec::new();
    let mut delete_paths: Vec<String> = Vec::new();
    let mut new_files: Vec<IndexFile> = Vec::new();

    for entry in WalkDir::new(new_dir).follow_links(false).into_iter().filter_map(|e| e.ok()) {
        if !entry.file_type().is_file() {
            continue;
        }
        let new_abs = entry.path();
        let sub_path = match new_abs.strip_prefix(new_dir) {
            Ok(p) => p,
            Err(_) => continue,
        };
        let sub_str = normalise_path_sep(&sub_path.to_string_lossy());
        let new_rel = if new_rel_dir.is_empty() {
            sub_str.clone()
        } else {
            format!("{}/{}", new_rel_dir, sub_str)
        };
        let old_rel = if old_rel_dir.is_empty() {
            sub_str.clone()
        } else {
            format!("{}/{}", old_rel_dir, sub_str)
        };

        // Evaluate inclusion for the new path.
        let new_source_included =
            source_includes.is_empty() || source_includes.is_match(&*new_rel);
        let (new_eff_scan, new_skip) = resolve_watch_config(new_abs, source_root, global_scan);
        let new_eff_excludes = build_globset(&new_eff_scan.exclude).unwrap_or_default();
        let new_included =
            new_source_included && !new_skip && !new_eff_excludes.is_match(&*new_rel);

        // Evaluate source-level inclusion for the old path (old dir is gone; only
        // source-glob check is possible — .noindex/.index files move with the rename).
        let old_source_included =
            source_includes.is_empty() || source_includes.is_match(&*old_rel);

        if new_included && old_source_included {
            // Was included, still included → rename.
            rename_paths.push(PathRename { old_path: old_rel, new_path: new_rel });
        } else if !new_included && old_source_included {
            // Was included, now excluded → delete from index.
            delete_paths.push(old_rel);
        } else if new_included && !old_source_included {
            // Was excluded, now included → full re-extraction.
            let lines = match subprocess::resolve_extractor(new_abs, &new_eff_scan) {
                subprocess::ExtractorChoice::External(ref ext_cfg) => match ext_cfg.mode {
                    ExternalExtractorMode::Stdout => {
                        match subprocess::run_external_stdout(new_abs, ext_cfg, &new_eff_scan).await {
                            subprocess::ExternalOutcome::Ok(lines) => lines,
                            subprocess::ExternalOutcome::BinaryMissing
                            | subprocess::ExternalOutcome::Failed(_) => vec![],
                        }
                    }
                    ExternalExtractorMode::TempDir => {
                        let ext_config = extractor_config_from_scan(&new_eff_scan);
                        match subprocess::run_external_tempdir(new_abs, ext_cfg, &new_eff_scan, &ext_config).await {
                            subprocess::ExternalOutcome::Ok(lines) => lines,
                            subprocess::ExternalOutcome::BinaryMissing
                            | subprocess::ExternalOutcome::Failed(_) => vec![],
                        }
                    }
                },
                subprocess::ExtractorChoice::Builtin => {
                    match subprocess::extract_via_subprocess(new_abs, &new_eff_scan, extractor_dir).await {
                        subprocess::SubprocessOutcome::Ok(lines) => lines,
                        subprocess::SubprocessOutcome::BinaryMissing
                        | subprocess::SubprocessOutcome::Failed => vec![],
                    }
                }
            };
            let mtime = mtime_of(new_abs).unwrap_or(0);
            let size = size_of(new_abs).unwrap_or(0);
            let ext = new_abs.extension().and_then(|e| e.to_str()).unwrap_or("");
            let kind = if lines.iter().any(|l| l.archive_path.is_some()) {
                "archive".to_string()
            } else {
                detect_kind_from_ext(ext).to_string()
            };
            let mut built = build_index_files(new_rel, mtime, size, kind, lines);
            new_files.append(&mut built);
        }
        // else: was excluded, still excluded — nothing to do.
    }

    if rename_paths.is_empty() && delete_paths.is_empty() && new_files.is_empty() {
        return Ok(());
    }

    api.bulk(&BulkRequest {
        source: source_name.to_string(),
        files: new_files,
        delete_paths,
        rename_paths,
        scan_timestamp: None,
        indexing_failures: vec![],
    })
    .await
}

// ── Scheduled scan ────────────────────────────────────────────────────────────

/// Background task that spawns `find-scan --config <path>` on a fixed interval.
///
/// - `interval_hours == 0.0` → disabled, returns immediately.
/// - `scan_now == true` → one scan is spawned immediately before the interval starts.
/// - Overlap: if the previous scan is still running when the next tick fires,
///   that tick is skipped and a warning is logged.
async fn run_scan_scheduler(interval_hours: f64, config_path: &str, scan_now: bool) {
    if interval_hours <= 0.0 {
        return;
    }

    let mut child: Option<tokio::process::Child> = None;

    if scan_now {
        child = spawn_scan(config_path);
    }

    let dur = Duration::from_secs_f64(interval_hours * 3600.0);
    let mut ticker = tokio::time::interval(dur);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    ticker.tick().await; // consume the initial immediate tick

    loop {
        ticker.tick().await;

        // Check whether the previous scan is still running.
        let still_running = match child.as_mut().map(|c| c.try_wait()) {
            Some(Ok(None)) => true,  // child exists and has not exited yet
            Some(Err(e)) => {
                tracing::warn!("scheduled scan: error checking child process: {e:#}");
                false
            }
            _ => false, // no child, or child has exited
        };

        if still_running {
            tracing::warn!("scheduled scan: previous scan still running, skipping tick");
            continue;
        }

        child = spawn_scan(config_path);
    }
}

/// Spawn `find-scan --config <config_path>` and return the child handle.
fn spawn_scan(config_path: &str) -> Option<tokio::process::Child> {
    let binary = find_scan_binary();
    match tokio::process::Command::new(&binary)
        .arg("--config")
        .arg(config_path)
        .spawn()
    {
        Ok(c) => {
            tracing::info!("scheduled scan: started {}", binary.display());
            Some(c)
        }
        Err(e) => {
            tracing::warn!("scheduled scan: failed to spawn {}: {e:#}", binary.display());
            None
        }
    }
}

/// Resolve the `find-scan` binary path: look next to the current executable first,
/// then fall back to relying on PATH.
fn find_scan_binary() -> PathBuf {
    let name = if cfg!(windows) { "find-scan.exe" } else { "find-scan" };
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.join(name)))
        .unwrap_or_else(|| PathBuf::from(name))
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

#[cfg(test)]
mod tests {
    use super::*;

    fn c() -> AccumulatedKind { AccumulatedKind::Create }
    fn u() -> AccumulatedKind { AccumulatedKind::Update }
    fn d() -> AccumulatedKind { AccumulatedKind::Delete }

    #[test]
    fn create_then_modify_stays_create() {
        assert_eq!(collapse(&c(), &u()), c());
    }

    #[test]
    fn create_then_delete_becomes_delete() {
        assert_eq!(collapse(&c(), &d()), d());
    }

    #[test]
    fn delete_then_create_becomes_create() {
        // File was deleted then re-created in the debounce window → new file.
        assert_eq!(collapse(&d(), &c()), c());
    }

    #[test]
    fn update_then_delete_becomes_delete() {
        assert_eq!(collapse(&u(), &d()), d());
    }

    #[test]
    fn delete_then_update_becomes_update() {
        // Unknown history after delete+update — treat as a modify.
        assert_eq!(collapse(&d(), &u()), u());
    }

    #[test]
    fn update_then_update_stays_update() {
        // Same event repeated — last wins (Update).
        assert_eq!(collapse(&u(), &u()), u());
    }

    #[test]
    fn create_then_create_stays_create() {
        assert_eq!(collapse(&c(), &c()), c());
    }

    #[test]
    fn delete_then_delete_stays_delete() {
        assert_eq!(collapse(&d(), &d()), d());
    }

    #[test]
    fn multi_step_sequence() {
        // Simulate: Create → Modify → Modify → Delete
        let mut k = c();
        k = collapse(&k, &u());
        k = collapse(&k, &u());
        k = collapse(&k, &d());
        assert_eq!(k, d());
    }

    #[test]
    fn delete_recreate_modify_sequence() {
        // File is deleted, then re-created, then modified — should end as Create.
        let mut k = d();
        k = collapse(&k, &c());
        k = collapse(&k, &u());
        assert_eq!(k, c());
    }
}
