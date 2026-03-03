use std::collections::{HashMap, HashSet};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::UNIX_EPOCH;

use anyhow::Result;
use globset::{Glob, GlobSet, GlobSetBuilder};
use tracing::{info, warn};
use walkdir::WalkDir;

use find_common::{
    api::{IndexFile, IndexLine, IndexingFailure},
    config::{load_dir_override, ScanConfig},
};

use crate::api::ApiClient;
use crate::batch::{build_index_files, build_member_index_files, submit_batch};
use crate::extract;
use crate::lazy_header;
use crate::subprocess;
use crate::upload;




/// Hash a file's contents using blake3 without reading the whole file into memory.
fn hash_file(path: &Path) -> Option<String> {
    let mut file = std::fs::File::open(path).ok()?;
    let mut hasher = blake3::Hasher::new();
    let mut buf = [0u8; 65536];
    loop {
        let n = file.read(&mut buf).ok()?;
        if n == 0 { break; }
        hasher.update(&buf[..n]);
    }
    Some(hasher.finalize().to_hex().to_string())
}
const MAX_FAILURES_PER_BATCH: usize = 100;
const MAX_ERROR_LEN: usize = 500;

pub async fn run_scan(
    api: &ApiClient,
    source_name: &str,
    paths: &[String],
    scan: &ScanConfig,
    base_url: Option<&str>,
    full: bool,
    quiet: bool,
) -> Result<()> {
    // Build global exclusion GlobSet for the walk phase.
    let excludes = build_globset(&scan.exclude)?;

    // Warn if the server inbox is not empty — the file list will reflect only
    // files the worker has already committed, so pending batches from a recent
    // scan will appear "new" again on this run.
    match api.inbox_status().await {
        Ok(status) if !status.pending.is_empty() => {
            warn!(
                "server inbox has {} pending batch(es) not yet processed; \
                 some files may appear as new even though they were recently indexed. \
                 Consider waiting for the inbox to drain before re-scanning.",
                status.pending.len()
            );
        }
        Ok(status) if !status.failed.is_empty() => {
            warn!(
                "server inbox has {} failed batch(es); run `find-admin inbox retry` \
                 or check /api/v1/admin/inbox for details.",
                status.failed.len()
            );
        }
        _ => {}
    }

    // Fetch what the server already knows about this source.
    // Only consider outer files (no "::" in path) for deletion/mtime comparison;
    // inner archive members are managed server-side.
    info!("fetching existing file list from server...");
    let server_files: HashMap<String, i64> = api
        .list_files(source_name)
        .await?
        .into_iter()
        .filter(|f| !f.path.contains("::"))
        .map(|f| (f.path, f.mtime))
        .collect();

    // Walk all configured paths and build the local file map.
    info!("walking filesystem...");
    let local_files = walk_paths(paths, scan, &excludes);
    info!("walk complete: {} files found", local_files.len());

    // Compute deletions (pure set diff — no I/O).
    let server_paths: HashSet<&str> = server_files.keys().map(|s| s.as_str()).collect();
    let local_paths: HashSet<&str> = local_files.keys().map(|s| s.as_str()).collect();

    let to_delete: Vec<String> = server_paths
        .difference(&local_paths)
        .map(|s| s.to_string())
        .collect();

    info!(
        "{} to delete; processing {} local files...",
        to_delete.len(),
        local_files.len(),
    );

    let mut ctx = ScanContext::new(api, source_name, paths, base_url, scan, quiet);

    let mut indexed: usize = 0;
    let mut skipped: usize = 0;
    let mut new_files: usize = 0;   // in local but absent from server DB
    let mut modified: usize = 0;    // mtime changed since last scan
    let log_interval = std::time::Duration::from_secs(5);
    let mut last_log = std::time::Instant::now();

    // Sort by relative path for deterministic, reproducible processing order.
    // HashMap iteration order is randomised per-process, so without this the
    // same crash would hit a different file each run and logs would differ.
    let mut local_entries: Vec<(&String, &PathBuf)> = local_files.iter().collect();
    local_entries.sort_unstable_by(|a, b| a.0.cmp(b.0));

    for (rel_path, abs_path) in local_entries {
        // Check mtime before any further work so unchanged files are skipped cheaply.
        let mtime = mtime_of(abs_path).unwrap_or(0);
        if !full {
            let server_mtime = server_files.get(rel_path.as_str()).copied();
            match server_mtime {
                None => { new_files += 1; }                  // new file — index it
                Some(sm) if mtime > sm => { modified += 1; } // modified — index it
                Some(_) => {
                    skipped += 1;
                    if last_log.elapsed() >= log_interval {
                        let total = indexed + skipped;
                        info!("processed {total} files ({skipped} unchanged, {new_files} new, {modified} modified) so far...");
                        last_log = std::time::Instant::now();
                    }
                    continue;
                }
            }
        }

        indexed += 1;
        process_file(&mut ctx, rel_path, abs_path, mtime).await?;
    }

    // Final batch: remaining files + all deletes.
    let deleted = to_delete.len();
    if deleted > 0 {
        info!("deleting {deleted} removed files");
    }
    ctx.submit(to_delete).await?;

    info!("scan complete — {indexed} indexed ({new_files} new, {modified} modified), {skipped} unchanged, {deleted} deleted");
    Ok(())
}

/// Shared state used by `process_file` so it can be called from both the
/// `run_scan` loop and the single-file entry point without threading a long
/// parameter list through every call.
struct ScanContext<'a> {
    api: &'a ApiClient,
    source_name: &'a str,
    paths: &'a [String],
    base_url: Option<&'a str>,
    quiet: bool,
    scan_start: i64,
    batch: Vec<IndexFile>,
    batch_bytes: usize,
    failures: Vec<IndexingFailure>,
    last_submit: std::time::Instant,
    batch_size: usize,
    batch_bytes_limit: usize,
    batch_interval: std::time::Duration,
    scan_arc: Arc<ScanConfig>,
    /// Keyed by raw Arc pointer — valid as long as the Arc lives in dir_scan_cache.
    dir_scan_cache: HashMap<PathBuf, Arc<ScanConfig>>,
    dir_excludes_cache: HashMap<*const ScanConfig, Arc<GlobSet>>,
}

impl<'a> ScanContext<'a> {
    fn new(
        api: &'a ApiClient,
        source_name: &'a str,
        paths: &'a [String],
        base_url: Option<&'a str>,
        scan: &ScanConfig,
        quiet: bool,
    ) -> Self {
        let scan_start = std::time::SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as i64;
        ScanContext {
            api,
            source_name,
            paths,
            base_url,
            quiet,
            scan_start,
            batch: Vec::with_capacity(scan.batch_size),
            batch_bytes: 0,
            failures: Vec::new(),
            last_submit: std::time::Instant::now(),
            batch_size: scan.batch_size,
            batch_bytes_limit: scan.batch_bytes,
            batch_interval: std::time::Duration::from_secs(scan.batch_interval_secs),
            scan_arc: Arc::new(scan.clone()),
            dir_scan_cache: HashMap::new(),
            dir_excludes_cache: HashMap::new(),
        }
    }

    async fn submit(&mut self, delete_paths: Vec<String>) -> Result<()> {
        submit_batch(
            self.api, self.source_name, self.base_url,
            &mut self.batch, &mut self.failures,
            delete_paths, Some(self.scan_start),
        ).await?;
        self.batch_bytes = 0;
        self.last_submit = std::time::Instant::now();
        Ok(())
    }
}

/// Process one file: resolve its effective config, extract content via
/// subprocess, handle OOM server-fallback, and accumulate the result in the
/// batch. Called from both the `run_scan` loop and `scan_single_file`.
async fn process_file(ctx: &mut ScanContext<'_>, rel_path: &str, abs_path: &Path, mtime: i64) -> Result<()> {
    // Resolve effective config for this file's directory (cached).
    let eff_scan = resolve_effective_scan(abs_path, ctx.paths, &ctx.scan_arc, &mut ctx.dir_scan_cache);

    // Get (or build) the GlobSet for this config. Directories that share the
    // same effective config share one compiled GlobSet (keyed by Arc pointer).
    let scan_ptr = Arc::as_ptr(&eff_scan);
    if let std::collections::hash_map::Entry::Vacant(e) = ctx.dir_excludes_cache.entry(scan_ptr) {
        e.insert(Arc::new(build_globset(&eff_scan.exclude)?));
    }
    let eff_excludes = Arc::clone(&ctx.dir_excludes_cache[&scan_ptr]);

    let size = size_of(abs_path).unwrap_or(0);
    let kind = extract::detect_kind(abs_path).to_string();

    if !ctx.quiet {
        info!("Processing {rel_path}");
    }

    if find_extract_archive::accepts(abs_path) {
        // ── Streaming archive extraction ─────────────────────────────────────
        // Members are processed one at a time via a bounded channel so that
        // lines are freed after each member is converted to an IndexFile,
        // rather than holding the entire archive's content in memory.
        if ctx.quiet {
            info!("extracting archive {rel_path}");
        }

        // Hash the outer archive file for dedup (streaming to avoid OOM on large archives).
        let outer_hash = hash_file(abs_path);

        // Submit the outer archive file first, before any member batches.
        // The server deletes stale inner members when it sees the outer file,
        // so it must arrive before member batches — not after them.
        let outer_file = IndexFile {
            path: rel_path.to_string(),
            mtime,
            size,
            kind,
            lines: vec![IndexLine { archive_path: None, line_number: 0, content: rel_path.to_string() }],
            extract_ms: None,
            content_hash: outer_hash,
        };
        ctx.batch.push(outer_file);
        ctx.submit(vec![]).await?;

        if ctx.quiet { lazy_header::set_pending(&abs_path.to_string_lossy()); }
        let (mut member_rx, subprocess_task) = subprocess::start_archive_subprocess(
            abs_path.to_path_buf(), &eff_scan, &eff_scan.extractor_dir);

        let mut members_submitted: usize = 0;
        while let Some(member_batch) = member_rx.recv().await {
            // A batch with empty lines and a skip_reason is a summary failure
            // that applies to the outer archive itself (e.g. 7z solid block too
            // large).  Record the failure on the outer archive path and move on.
            if member_batch.lines.is_empty() {
                if let Some(reason) = member_batch.skip_reason {
                    if ctx.failures.len() < MAX_FAILURES_PER_BATCH {
                        ctx.failures.push(IndexingFailure {
                            path: rel_path.to_string(),
                            error: truncate_error(&reason, MAX_ERROR_LEN),
                        });
                    }
                }
                continue;
            }

            // Apply effective exclude patterns to archive members.
            // archive_path may be "inner.zip::path/to/file.js" for nested archives;
            // take the last segment (actual file path) for glob matching.
            if let Some(ap) = member_batch.lines.first().and_then(|l| l.archive_path.as_deref()) {
                let file_path = ap.rsplit("::").next().unwrap_or(ap);
                if eff_excludes.is_match(file_path) {
                    continue;
                }
            }

            // Record a per-member skip reason as an indexing failure.
            if let Some(ref reason) = member_batch.skip_reason {
                if ctx.failures.len() < MAX_FAILURES_PER_BATCH {
                    if let Some(ap) = member_batch.lines.first().and_then(|l| l.archive_path.as_deref()) {
                        ctx.failures.push(IndexingFailure {
                            path: format!("{}::{}", rel_path, ap),
                            error: truncate_error(reason, MAX_ERROR_LEN),
                        });
                    }
                }
            }

            let content_hash = member_batch.content_hash;
            for file in build_member_index_files(rel_path, mtime, size, member_batch.lines, content_hash) {
                let file_bytes: usize = file.lines.iter().map(|l| l.content.len()).sum();
                ctx.batch_bytes += file_bytes;
                members_submitted += 1;
                ctx.batch.push(file);
                if ctx.batch.len() >= ctx.batch_size || ctx.batch_bytes >= ctx.batch_bytes_limit
                    || (!ctx.batch.is_empty() && ctx.last_submit.elapsed() >= ctx.batch_interval)
                {
                    info!("submitting batch — extracting {rel_path} ({} members, {} total)", ctx.batch.len(), members_submitted);
                    ctx.submit(vec![]).await?;
                }
            }
        }

        if ctx.quiet { lazy_header::clear_pending(); }

        // Check whether the subprocess exited successfully.
        if !subprocess_task.await.unwrap_or(false) && ctx.failures.len() < MAX_FAILURES_PER_BATCH {
            ctx.failures.push(IndexingFailure {
                path: rel_path.to_string(),
                error: "archive extraction subprocess failed".to_string(),
            });
        }

        // Flush any remaining archive members (partial final batch).
        if !ctx.batch.is_empty() {
            info!("submitting batch — extracting {rel_path} ({} members, {members_submitted} total)", ctx.batch.len());
            ctx.submit(vec![]).await?;
        }
    } else {
        // ── Non-archive extraction ────────────────────────────────────────────
        // dispatch_from_path handles MIME detection internally: it emits a
        // [FILE:mime] line when no extractor matched the bytes, so we check
        // for that line below to update the kind accordingly.
        let t0 = std::time::Instant::now();
        if ctx.quiet { lazy_header::set_pending(&abs_path.to_string_lossy()); }
        let outcome = subprocess::extract_via_subprocess(
            abs_path, &eff_scan, &eff_scan.extractor_dir).await;
        if ctx.quiet { lazy_header::clear_pending(); }

        let lines = match outcome {
            subprocess::SubprocessOutcome::Ok(lines) => lines,
            subprocess::SubprocessOutcome::Failed => {
                if eff_scan.server_fallback {
                    if let Err(e) = upload::upload_file(ctx.api, abs_path, rel_path, mtime, ctx.source_name).await {
                        warn!("server fallback upload failed for {rel_path}: {e:#}");
                        // Fall through: index filename-only so file appears in search.
                    } else {
                        // Server will index it; skip local filename-only entry.
                        return Ok(());
                    }
                }
                // Index filename-only so the file is at least findable by name.
                vec![]
            }
        };

        // Refine "unknown" or "text" kind using extracted content:
        // - A [FILE:mime] line emitted by dispatch means binary → use mime_to_kind.
        // - Text content lines (line_number > 0) present → promote to "text".
        // - Neither → keep as-is (archive members use "unknown" when unrecognised).
        let kind = if kind == "text" || kind == "unknown" {
            if let Some(mime_line) = lines.iter().find(|l| l.line_number == 0 && l.content.starts_with("[FILE:mime] ")) {
                let mime = &mime_line.content["[FILE:mime] ".len()..];
                find_extract_dispatch::mime_to_kind(mime).to_string()
            } else if lines.iter().any(|l| l.line_number > 0) {
                "text".to_string()
            } else {
                kind
            }
        } else {
            kind
        };
        let extract_ms = t0.elapsed().as_millis() as u64;
        // Hash raw file bytes for dedup (streaming to avoid OOM on large files).
        let content_hash = hash_file(abs_path);
        let mut index_files = build_index_files(rel_path.to_string(), mtime, size, kind, lines);
        if let Some(f) = index_files.first_mut() {
            f.extract_ms = Some(extract_ms);
            f.content_hash = content_hash;
        }
        for file in index_files {
            let file_bytes: usize = file.lines.iter().map(|l| l.content.len()).sum();
            ctx.batch_bytes += file_bytes;
            ctx.batch.push(file);
            if ctx.batch.len() >= ctx.batch_size || ctx.batch_bytes >= ctx.batch_bytes_limit
                || (!ctx.batch.is_empty() && ctx.last_submit.elapsed() >= ctx.batch_interval)
            {
                ctx.submit(vec![]).await?;
            }
        }
    }

    if ctx.batch.len() >= ctx.batch_size || ctx.batch_bytes >= ctx.batch_bytes_limit
        || (!ctx.batch.is_empty() && ctx.last_submit.elapsed() >= ctx.batch_interval)
    {
        ctx.submit(vec![]).await?;
    }

    Ok(())
}

/// Scan a single file and submit it to the server. The file must belong to one
/// of the source's configured paths. Processes the file identically to a file
/// discovered during a full scan — subprocess extraction, OOM server-fallback,
/// archive streaming — but skips the walk, mtime check, and deletion step.
#[allow(clippy::too_many_arguments)]
pub async fn scan_single_file(
    api: &ApiClient,
    source_name: &str,
    paths: &[String],
    rel_path: &str,
    abs_path: &Path,
    scan: &ScanConfig,
    base_url: Option<&str>,
    quiet: bool,
) -> Result<()> {
    let mtime = mtime_of(abs_path).unwrap_or(0);
    let mut ctx = ScanContext::new(api, source_name, paths, base_url, scan, quiet);
    process_file(&mut ctx, rel_path, abs_path, mtime).await?;
    ctx.submit(vec![]).await?;
    info!("done");
    Ok(())
}

// ── Helpers ───────────────────────────────────────────────────────────────────

fn build_globset(patterns: &[String]) -> Result<GlobSet> {
    let mut builder = GlobSetBuilder::new();
    for pat in patterns {
        builder.add(Glob::new(pat)?);
        // For patterns like **/node_modules/**, also add **/node_modules so that
        // the directory entry itself is excluded and walkdir won't descend into it.
        if let Some(dir_pat) = pat.strip_suffix("/**") {
            builder.add(Glob::new(dir_pat)?);
        }
    }
    Ok(builder.build()?)
}

/// Walk ancestor directories from `file_path` up to the nearest source root,
/// applying any `.index` override files found. Returns the effective `ScanConfig`
/// for this file. Results are cached by directory so each directory's `.index`
/// is parsed at most once per scan session.
fn resolve_effective_scan(
    file_path: &Path,
    roots: &[String],
    global: &Arc<ScanConfig>,
    dir_cache: &mut HashMap<PathBuf, Arc<ScanConfig>>,
) -> Arc<ScanConfig> {
    let dir = match file_path.parent() {
        Some(d) => d.to_path_buf(),
        None => return Arc::clone(global),
    };

    if let Some(cached) = dir_cache.get(&dir) {
        return Arc::clone(cached);
    }

    // Find which root this file lives under (pick the longest matching root).
    let root = roots
        .iter()
        .filter_map(|r| {
            let rp = Path::new(r);
            if dir.starts_with(rp) {
                Some(rp.to_path_buf())
            } else {
                None
            }
        })
        .max_by_key(|rp| rp.as_os_str().len());

    let Some(root) = root else {
        return Arc::clone(global);
    };

    // Collect ancestors from root down to dir (inclusive), root first.
    let mut ancestors: Vec<PathBuf> = Vec::new();
    let mut cur = dir.as_path();
    loop {
        ancestors.push(cur.to_path_buf());
        if cur == root {
            break;
        }
        match cur.parent() {
            Some(p) => cur = p,
            None => break,
        }
    }
    ancestors.reverse(); // root → dir order

    // Find the deepest ancestor already in the cache to avoid redundant work.
    // Using Arc::clone here is O(1) — no ScanConfig data is copied.
    let mut eff: Arc<ScanConfig> = Arc::clone(global);
    let mut start_idx = 0;
    for (i, ancestor) in ancestors.iter().enumerate().rev() {
        if let Some(cached) = dir_cache.get(ancestor) {
            eff = Arc::clone(cached);
            start_idx = i + 1;
            break;
        }
    }

    // Apply .index overrides for uncached ancestors and populate the cache.
    // When no override is found for an ancestor, Arc::clone shares the existing
    // allocation — no ScanConfig clone occurs.  A new Arc is only allocated when
    // an actual .index override changes the config for that subtree.
    for ancestor in &ancestors[start_idx..] {
        if let Some(ov) = load_dir_override(ancestor, &global.index_file) {
            eff = Arc::new(eff.apply_override(&ov));
        }
        dir_cache.insert(ancestor.clone(), Arc::clone(&eff));
    }

    eff
}

/// Returns a map of relative_path → absolute_path for all files under `paths`.
fn walk_paths(
    paths: &[String],
    scan: &ScanConfig,
    excludes: &GlobSet,
) -> HashMap<String, PathBuf> {
    let mut map = HashMap::new();
    let log_interval = std::time::Duration::from_secs(5);
    let mut last_log = std::time::Instant::now();

    for root_str in paths {
        let root = Path::new(root_str);
        for entry in WalkDir::new(root)
            .follow_links(scan.follow_symlinks)
            .into_iter()
            .filter_entry(|e| {
                let name = e.file_name().to_str().unwrap_or("");

                if e.file_type().is_dir() {
                    // Skip hidden directories (avoid descending into .git etc.).
                    // Hidden FILES are handled in the loop body so that control files
                    // (.index) are always visible regardless of include_hidden.
                    if !scan.include_hidden && name.starts_with('.') && e.depth() > 0 {
                        return false;
                    }
                    // Don't descend into directories that contain a .noindex marker.
                    // This is checked inline so the progress count is accurate and
                    // WalkDir never collects files from excluded subtrees.
                    if e.path().join(&scan.noindex_file).exists() {
                        tracing::debug!("skipping {} (.noindex present)", e.path().display());
                        return false;
                    }
                }
                // Exclusion globs (match relative to root)
                if let Ok(rel) = e.path().strip_prefix(root) {
                    if excludes.is_match(rel) {
                        return false;
                    }
                }
                true
            })
        {
            let entry = match entry {
                Ok(e) => e,
                Err(e) => { warn!("walk error: {e:#}"); continue; }
            };
            let name = entry.file_name().to_str().unwrap_or("");

            // Skip the .index control file (not a content file).
            if name == scan.index_file {
                continue;
            }

            if !entry.file_type().is_file() {
                continue;
            }

            // Hidden files (hidden directories already pruned in filter_entry).
            if !scan.include_hidden && name.starts_with('.') && entry.depth() > 0 {
                continue;
            }

            let abs = entry.path().to_path_buf();
            let rel = relative_path(&abs, paths);
            map.insert(rel, abs);

            if last_log.elapsed() >= log_interval {
                info!("walking filesystem... {} files found so far", map.len());
                last_log = std::time::Instant::now();
            }
        }
    }

    map
}

fn relative_path(abs: &Path, roots: &[String]) -> String {
    for root in roots {
        if let Ok(rel) = abs.strip_prefix(root) {
            return rel.to_string_lossy().to_string();
        }
    }
    abs.to_string_lossy().to_string()
}

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

/// Truncate `s` to at most `max` bytes at a UTF-8 char boundary, appending `…` if truncated.
fn truncate_error(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    // Walk back from `max` to find a valid char boundary.
    let mut end = max;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &s[..end])
}

