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
    api::{IndexFile, IndexLine, IndexingFailure, SCANNER_VERSION},
    config::{load_dir_override, ScanConfig},
};

use crate::api::ApiClient;
use crate::batch::{build_index_files, build_member_index_files, submit_batch};
use crate::extract;
use crate::lazy_header;
use crate::subprocess;
use crate::upload;




/// Hash a file's contents using blake3 without reading the whole file into memory.
/// Returns `None` for empty files so they are not deduped against each other
/// (the hash of 0 bytes is a fixed value, which would falsely mark all empty
/// files as duplicates).
fn hash_file(path: &Path) -> Option<String> {
    let mut file = std::fs::File::open(path).ok()?;
    let mut hasher = blake3::Hasher::new();
    let mut buf = [0u8; 65536];
    let mut total = 0usize;
    loop {
        let n = file.read(&mut buf).ok()?;
        if n == 0 { break; }
        hasher.update(&buf[..n]);
        total += n;
    }
    if total == 0 { return None; }
    Some(hasher.finalize().to_hex().to_string())
}
const MAX_FAILURES_PER_BATCH: usize = 100;
const MAX_ERROR_LEN: usize = 500;

/// Per-invocation options for `run_scan` and `scan_single_file`.
pub struct ScanOptions {
    /// Re-index files whose stored `scanner_version` is older than the current
    /// `SCANNER_VERSION`, even if their mtime has not changed. Naturally
    /// resumable: interrupted runs skip files already upgraded to the current version.
    pub upgrade: bool,
    pub quiet: bool,
    pub dry_run: bool,
}

/// Source-specific parameters for `run_scan` and `scan_single_file`.
pub struct ScanSource<'a> {
    pub name: &'a str,
    pub paths: &'a [String],
    /// Glob patterns from `[sources.xxx] include = [...]`. Empty = include all.
    pub include: &'a [String],
    /// If set, restrict the scan to this subdirectory (relative path within the
    /// source root, forward-slash normalised). The walk is scoped to this
    /// directory; only server files under this prefix are considered for
    /// deletion; mtime checking is skipped (all files are re-indexed).
    pub subdir: Option<String>,
}

pub async fn run_scan(
    api: &ApiClient,
    source: &ScanSource<'_>,
    scan: &ScanConfig,
    opts: &ScanOptions,
) -> Result<()> {
    let (source_name, paths) = (source.name, source.paths);
    // Build global exclusion GlobSet for the walk phase.
    let excludes = build_globset(&scan.exclude)?;
    // Build include GlobSet (empty = include everything).
    let includes = build_globset(source.include)?;
    let include_dirs = if source.include.is_empty() {
        None
    } else {
        include_dir_prefixes(source.include)
    };
    // When a subdir is set, always re-index all files (no mtime skip).
    let subdir_rescan = source.subdir.is_some();

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
                "server inbox has {} failed batch(es); run `find-admin inbox-retry` \
                 or check /api/v1/admin/inbox for details.",
                status.failed.len()
            );
        }
        _ => {}
    }

    // Fetch what the server already knows about this source.
    // Only consider outer files (no "::" in path) for deletion/mtime comparison;
    // inner archive members are managed server-side.
    // When scanning a subdir, restrict to files under that prefix only.
    info!("fetching existing file list from server...");
    let server_files: HashMap<String, (i64, u32)> = api
        .list_files(source_name)
        .await?
        .into_iter()
        .filter(|f| !f.path.contains("::"))
        .filter(|f| match &source.subdir {
            None => true,
            Some(sub) => f.path == *sub || f.path.starts_with(&format!("{sub}/")),
        })
        .map(|f| (f.path, (f.mtime, f.scanner_version)))
        .collect();

    // Walk all configured paths (or just the subdir) and build the local file map.
    info!("walking filesystem...");
    let local_files = walk_paths(paths, scan, &excludes, &includes, include_dirs.as_ref(), source.subdir.as_deref());
    info!("walk complete: {} files found", local_files.len());

    // Compute deletions (pure set diff — no I/O).
    let server_paths: HashSet<&str> = server_files.keys().map(|s| s.as_str()).collect();
    let local_paths: HashSet<&str> = local_files.keys().map(|s| s.as_str()).collect();

    let to_delete: Vec<String> = server_paths
        .difference(&local_paths)
        .map(|s| s.to_string())
        .collect();

    let deleted = to_delete.len();
    info!(
        "{} to delete; processing {} local files...",
        deleted,
        local_files.len(),
    );

    let mut ctx = ScanContext::new(api, source_name, paths, scan, opts.quiet, source.subdir.is_none());

    // Submit deletions immediately so removed files are gone before new/modified
    // files are indexed.  This also ensures renames (delete + add) don't leave a
    // stale entry visible while the new path is being indexed.
    if !opts.dry_run && deleted > 0 {
        info!("deleting {deleted} removed files");
        ctx.submit(to_delete).await?;
    }

    let mut indexed: usize = 0;
    let mut skipped: usize = 0;
    let mut new_files: usize = 0;   // in local but absent from server DB
    let mut modified: usize = 0;    // mtime changed since last scan
    let mut upgraded: usize = 0;    // mtime unchanged but scanner_version outdated
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
        if !subdir_rescan {
            let server_entry = server_files.get(rel_path.as_str()).copied();
            let needs_index = match server_entry {
                None => { new_files += 1; true }
                Some((sm, _)) if mtime > sm => { modified += 1; true }
                Some((_, sv)) if opts.upgrade && sv < SCANNER_VERSION => { upgraded += 1; true }
                Some(_) => {
                    skipped += 1;
                    false
                }
            };
            if !needs_index {
                if last_log.elapsed() >= log_interval {
                    let total = indexed + skipped;
                    info!("processed {total} files ({skipped} unchanged, {new_files} new, {modified} modified, {upgraded} upgraded) so far...");
                    last_log = std::time::Instant::now();
                }
                continue;
            }
        }

        indexed += 1;
        if !opts.dry_run {
            process_file(&mut ctx, rel_path, abs_path, mtime).await?;
        }
        if last_log.elapsed() >= log_interval {
            let total = indexed + skipped;
            info!(
                "processed {total} files ({skipped} unchanged, {new_files} new, {modified} modified, {upgraded} upgraded) so far, {} in current batch...",
                ctx.batch.len(),
            );
            last_log = std::time::Instant::now();
        }
    }

    if opts.dry_run {
        if subdir_rescan {
            info!(
                "dry-run complete — {} files found (all would be reindexed), {} to delete",
                local_files.len(),
                deleted
            );
        } else {
            info!(
                "dry-run complete — {} files found, {} new, {} modified, {} upgraded, {} unchanged, {} to delete",
                local_files.len(),
                new_files,
                modified,
                upgraded,
                skipped,
                deleted
            );
        }
        return Ok(());
    }

    // Final batch: flush any remaining indexed files.
    ctx.submit(vec![]).await?;

    info!("scan complete — {indexed} indexed ({new_files} new, {modified} modified, {upgraded} upgraded), {skipped} unchanged, {deleted} deleted");
    Ok(())
}

/// Shared state used by `process_file` so it can be called from both the
/// `run_scan` loop and the single-file entry point without threading a long
/// parameter list through every call.
struct ScanContext<'a> {
    api: &'a ApiClient,
    source_name: &'a str,
    paths: &'a [String],
    quiet: bool,
    scan_start: i64,
    /// Whether to include `scan_timestamp` in submitted batches. False for
    /// partial (subdir) rescans so the source's last-scanned time is not updated.
    emit_scan_timestamp: bool,
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
    dir_includes_cache: HashMap<*const ScanConfig, Arc<GlobSet>>,
}

impl<'a> ScanContext<'a> {
    fn new(
        api: &'a ApiClient,
        source_name: &'a str,
        paths: &'a [String],
        scan: &ScanConfig,
        quiet: bool,
        emit_scan_timestamp: bool,
    ) -> Self {
        let scan_start = std::time::SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as i64;
        ScanContext {
            api,
            source_name,
            paths,
            quiet,
            scan_start,
            emit_scan_timestamp,
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
            dir_includes_cache: HashMap::new(),
        }
    }

    async fn submit(&mut self, delete_paths: Vec<String>) -> Result<()> {
        if !self.batch.is_empty() || !delete_paths.is_empty() {
            info!(
                "submitting batch — {} files, {} deletes",
                self.batch.len(),
                delete_paths.len(),
            );
        }
        let scan_ts = self.emit_scan_timestamp.then_some(self.scan_start);
        submit_batch(
            self.api, self.source_name,
            &mut self.batch, &mut self.failures,
            delete_paths, scan_ts,
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

    // Check per-directory include filter from a .index file. The filter uses
    // patterns relative to the directory that declared it; we strip that prefix
    // from abs_path before matching. Non-matching files are skipped entirely
    // (not indexed, so they will be picked up on the next scan if the .index
    // include is removed or broadened).
    if let Some((dir_path, patterns)) = &eff_scan.dir_include {
        if let std::collections::hash_map::Entry::Vacant(e) = ctx.dir_includes_cache.entry(scan_ptr) {
            e.insert(Arc::new(build_globset(patterns)?));
        }
        let dir_includes = Arc::clone(&ctx.dir_includes_cache[&scan_ptr]);
        if !dir_includes.is_empty() {
            let rel_to_dir = abs_path
                .strip_prefix(dir_path)
                .map(|p| normalise_path_sep(&p.to_string_lossy()))
                .unwrap_or_default();
            if !dir_includes.is_match(&*rel_to_dir) {
                return Ok(());
            }
        }
    }

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

        // Submit the outer archive file with mtime=0 (sentinel: members not yet indexed).
        // The server deletes stale inner members when it receives mtime=0 for an outer
        // archive, so this must arrive before member batches.  Using mtime=0 means that
        // if indexing is interrupted before the completion upsert below, the next scan
        // will see a mtime mismatch (any real mtime > 0) and re-index the archive.
        let outer_start = IndexFile {
            path: rel_path.to_string(),
            mtime: 0,
            size: Some(size),
            kind: kind.clone(),
            lines: vec![IndexLine { archive_path: None, line_number: 0, content: rel_path.to_string() }],
            extract_ms: None,
            content_hash: None, // no hash on start sentinel — avoids premature dedup alias
            scanner_version: SCANNER_VERSION,
        };
        ctx.batch.push(outer_start);
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
            let member_mtime = member_batch.mtime.unwrap_or(mtime);
            for file in build_member_index_files(rel_path, member_mtime, size, member_batch.lines, content_hash) {
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

        // Completion upsert: update the outer file with its real mtime now that
        // all members have been submitted.  The server only deletes inner members
        // when it receives mtime=0, so this upsert simply updates the mtime field
        // without disturbing any member rows.  If indexing was interrupted before
        // this point the outer file retains mtime=0, causing the next scan to
        // re-index the archive from scratch.
        ctx.batch.push(IndexFile {
            path: rel_path.to_string(),
            mtime,
            size: Some(size),
            kind,
            lines: vec![IndexLine { archive_path: None, line_number: 0, content: rel_path.to_string() }],
            extract_ms: None,
            content_hash: outer_hash,
            scanner_version: SCANNER_VERSION,
        });
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
            subprocess::SubprocessOutcome::BinaryMissing => {
                // Extractor binary not installed — skip this file entirely so it
                // is re-indexed (with content) once the binary is deployed.
                return Ok(());
            }
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
        // Skip hashing known binary extensions that no specialist extractor handles:
        // opening these files can block indefinitely on Windows (e.g. live VHDX held by Hyper-V).
        let content_hash = if find_extract_dispatch::is_binary_ext_path(abs_path) {
            None
        } else {
            hash_file(abs_path)
        };
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
pub async fn scan_single_file(
    api: &ApiClient,
    source: &ScanSource<'_>,
    rel_path: &str,
    abs_path: &Path,
    scan: &ScanConfig,
    opts: &ScanOptions,
) -> Result<()> {
    let mtime = mtime_of(abs_path).unwrap_or(0);
    let mut ctx = ScanContext::new(api, source.name, source.paths, scan, opts.quiet, true);
    process_file(&mut ctx, rel_path, abs_path, mtime).await?;
    ctx.submit(vec![]).await?;
    info!("done");
    Ok(())
}

// ── Helpers ───────────────────────────────────────────────────────────────────

fn build_globset(patterns: &[String]) -> Result<GlobSet> {
    let mut builder = GlobSetBuilder::new();
    for pat in patterns {
        // Always use forward slashes — normalise any backslashes from Windows configs.
        let pat = pat.replace('\\', "/");
        builder.add(Glob::new(&pat)?);
        // For patterns like **/node_modules/**, also add **/node_modules so that
        // the directory entry itself is excluded and walkdir won't descend into it.
        if let Some(dir_pat) = pat.strip_suffix("/**") {
            builder.add(Glob::new(dir_pat)?);
        }
    }
    Ok(builder.build()?)
}

/// Given a list of include glob patterns, return the set of **terminal**
/// directory prefixes — the deepest safe literal directory path before any
/// wildcard character in each pattern.
///
/// The set is used in `filter_entry` with a three-way check for a directory
/// at relative path `d`:
/// - `t == d`                    — `d` is itself a terminal (enter it)
/// - `t.starts_with(d + "/")`   — `d` is an ancestor of a terminal (pass through)
/// - `d.starts_with(t + "/")`   — `d` is inside a terminal (already matching)
///
/// Returns `None` if no useful pruning can be determined — any pattern that
/// could match from the root (e.g. `**/*.rs`) requires traversing everything.
///
/// The key correctness rule: the terminal for a pattern is taken from the last
/// `/` **before** the first wildcard character (`*`, `?`, `[`, `{`). This
/// ensures we never cut a directory name in half (e.g. `Users/Administrat?r/**`
/// gives terminal `Users`, not `Users/Administrat`). Patterns starting with a
/// wildcard, negations (`!`), or those whose wildcard falls in the first path
/// component cause the whole function to return `None` (fail-open: traverse
/// everything).
///
/// Example: `["Users/alice/**", "data/**"]` → `{"Users/alice", "data"}`
fn include_dir_prefixes(patterns: &[String]) -> Option<std::collections::HashSet<String>> {
    let mut terminals = std::collections::HashSet::new();
    for pat in patterns {
        let pat = pat.replace('\\', "/");

        // Negation patterns (e.g. `!some/path/**`) — fall back to no pruning.
        if pat.starts_with('!') {
            return None;
        }

        // Find the first wildcard. Include `{` for alternations like {a,b}/**.
        let wildcard_pos = pat.find(['*', '?', '[', '{']);

        // Determine the safe literal directory prefix: everything before the
        // last `/` that precedes the first wildcard. This prevents cutting a
        // directory component in half (e.g. `Users/Administrat?r` → `Users`).
        let literal = match wildcard_pos {
            None => pat.as_str(),       // no wildcard — whole pattern is literal
            Some(0) => return None,     // wildcard at root — can't prune anything
            Some(i) => {
                let before = &pat[..i]; // e.g. "Users/Administrat" or "Users/"
                match before.rfind('/') {
                    None => return None, // wildcard in the first component — can't prune
                    Some(slash) => &pat[..slash], // safe: last complete dir before wildcard
                }
            }
        };

        let literal = literal.trim_end_matches('/');
        if literal.is_empty() {
            return None;
        }

        terminals.insert(literal.to_string());
    }
    Some(terminals)
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
            let r = normalise_root(r);
            let rp = PathBuf::from(&r);
            if dir.starts_with(&rp) {
                Some(rp)
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
            eff = Arc::new(eff.apply_dir_override(&ov, ancestor));
        }
        dir_cache.insert(ancestor.clone(), Arc::clone(&eff));
    }

    eff
}

/// Returns a map of relative_path → absolute_path for all files under `paths`.
///
/// `includes` is empty when no include filter is configured (all files pass).
/// `include_dirs` is the terminal set from `include_dir_prefixes`; if `None`,
/// no directory pruning is applied (patterns like `**/*.rs` can match anywhere).
fn walk_paths(
    paths: &[String],
    scan: &ScanConfig,
    excludes: &GlobSet,
    includes: &GlobSet,
    include_dirs: Option<&std::collections::HashSet<String>>,
    subdir: Option<&str>,
) -> HashMap<String, PathBuf> {
    let mut map = HashMap::new();
    let log_interval = std::time::Duration::from_secs(5);
    let mut last_log = std::time::Instant::now();

    for root_str in paths {
        let root_str = normalise_root(root_str);
        let root_str = root_str.as_str();
        let root = Path::new(root_str);
        // When scanning a subdir, walk from root/subdir but compute rel-paths
        // relative to root so they match what the server already stores.
        let walk_start = match subdir {
            Some(sub) => {
                let mut p = PathBuf::from(root_str);
                p.push(sub);
                p
            }
            None => PathBuf::from(root_str),
        };
        for entry in WalkDir::new(&walk_start)
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
                    // If include patterns have extractable directory prefixes, prune
                    // directories that can't contain any matching files.
                    if e.depth() > 0 {
                        if let Some(terminals) = include_dirs {
                            if let Ok(rel) = e.path().strip_prefix(root) {
                                let rel_str = normalise_path_sep(&rel.to_string_lossy());
                                // Allow the directory if it is a terminal, an ancestor of a
                                // terminal (navigating toward the ** portion), or already
                                // inside a terminal (under the ** portion).
                                let allowed = terminals.iter().any(|t| {
                                    t == &rel_str
                                        || t.starts_with(&format!("{rel_str}/"))
                                        || rel_str.starts_with(&format!("{t}/"))
                                });
                                if !allowed {
                                    return false;
                                }
                            }
                        }
                    }
                }
                // Exclusion globs (match relative to root, forward-slash normalised).
                if let Ok(rel) = e.path().strip_prefix(root) {
                    let rel_str = normalise_path_sep(&rel.to_string_lossy());
                    if excludes.is_match(&*rel_str) {
                        return false;
                    }
                }
                true
            })
        {
            let entry = match entry {
                Ok(e) => e,
                Err(e) => {
                    // Access-denied errors are expected on Windows for protected
                    // directories (e.g. C:\Users\Administrator). Log at debug so
                    // they don't spam the output; the same applies to paths that
                    // match an exclude glob but whose OS error surfaced before
                    // filter_entry could prevent the descent.
                    let access_denied = e.io_error()
                        .map(|io| io.kind() == std::io::ErrorKind::PermissionDenied)
                        .unwrap_or(false);
                    let excluded = e.path()
                        .and_then(|p| p.strip_prefix(root).ok())
                        .map(|rel| excludes.is_match(&*normalise_path_sep(&rel.to_string_lossy())))
                        .unwrap_or(false);
                    if access_denied || excluded {
                        tracing::debug!("skipping inaccessible path: {e}");
                    } else {
                        warn!("walk error: {e:#}");
                    }
                    continue;
                }
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
            // Apply include filter (empty GlobSet = no filter = include all).
            if !includes.is_empty() && !includes.is_match(&*rel) {
                continue;
            }
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
        let root = normalise_root(root);
        if let Ok(rel) = abs.strip_prefix(&root) {
            return normalise_path_sep(&rel.to_string_lossy());
        }
    }
    normalise_path_sep(&abs.to_string_lossy())
}

/// On Windows, replace backslash separators with forward slashes so that
/// paths are stored consistently regardless of platform. On Unix, backslash
/// is a valid filename character and must not be replaced.
#[cfg(windows)]
pub fn normalise_path_sep(s: &str) -> String {
    s.replace('\\', "/")
}

#[cfg(not(windows))]
pub fn normalise_path_sep(s: &str) -> String {
    s.to_string()
}

/// On Windows, normalise a bare drive letter like `"C:"` to `"C:/"` so that
/// WalkDir walks the drive root (not the drive's current directory) and
/// `strip_prefix` returns clean relative paths without a leading separator.
/// On non-Windows this is a no-op.
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

#[cfg(test)]
mod tests {
    use super::*;

    // Helper: returns true if the three-way filter_entry check would allow `dir`
    // given a terminal set built from `patterns`.
    fn dir_allowed(patterns: &[&str], dir: &str) -> bool {
        let owned: Vec<String> = patterns.iter().map(|s| s.to_string()).collect();
        let terminals = match include_dir_prefixes(&owned) {
            Some(t) => t,
            // None means no pruning — everything is allowed.
            None => return true,
        };
        terminals.iter().any(|t| {
            t == dir
                || t.starts_with(&format!("{dir}/"))
                || dir.starts_with(&format!("{t}/"))
        })
    }

    // Helper: returns None when include_dir_prefixes returns None (no pruning).
    fn terminals(patterns: &[&str]) -> Option<Vec<String>> {
        let owned: Vec<String> = patterns.iter().map(|s| s.to_string()).collect();
        include_dir_prefixes(&owned).map(|t| {
            let mut v: Vec<String> = t.into_iter().collect();
            v.sort();
            v
        })
    }

    // ── include_dir_prefixes extraction ────────────────────────────────────────

    #[test]
    fn simple_patterns_extract_terminals() {
        assert_eq!(
            terminals(&["Users/alice/**", "data/**"]),
            Some(vec!["Users/alice".into(), "data".into()])
        );
    }

    #[test]
    fn wildcard_in_first_component_returns_none() {
        // e.g. "*/foo/**" — wildcard before the first slash, can't prune
        assert_eq!(terminals(&["*/foo/**"]), None);
    }

    #[test]
    fn double_star_prefix_returns_none() {
        assert_eq!(terminals(&["**/*.rs"]), None);
    }

    #[test]
    fn negation_pattern_returns_none() {
        assert_eq!(terminals(&["Users/alice/**", "!secret/**"]), None);
    }

    #[test]
    fn alternation_in_first_component_returns_none() {
        // "{a,b}/**" has `{` at position 0 — should return None
        assert_eq!(terminals(&["{a,b}/**"]), None);
    }

    #[test]
    fn wildcard_in_dir_name_uses_safe_prefix() {
        // "Users/Administrat?r/**" — wildcard inside dir name, safe prefix is "Users"
        assert_eq!(
            terminals(&["Users/Administrat?r/**"]),
            Some(vec!["Users".into()])
        );
    }

    #[test]
    fn no_wildcard_pattern_is_literal_terminal() {
        assert_eq!(
            terminals(&["docs/api"]),
            Some(vec!["docs/api".into()])
        );
    }

    // ── filter_entry allow/deny ────────────────────────────────────────────────

    #[test]
    fn exact_terminal_is_allowed() {
        assert!(dir_allowed(&["Users/alice/**", "data/**"], "Users/alice"));
        assert!(dir_allowed(&["Users/alice/**", "data/**"], "data"));
    }

    #[test]
    fn ancestor_of_terminal_is_allowed() {
        // "Users" is an ancestor of the terminal "Users/alice"
        assert!(dir_allowed(&["Users/alice/**"], "Users"));
    }

    #[test]
    fn descendant_of_terminal_is_allowed() {
        assert!(dir_allowed(&["Users/alice/**"], "Users/alice/documents"));
        assert!(dir_allowed(&["Users/alice/**"], "Users/alice/documents/2024"));
    }

    #[test]
    fn sibling_of_terminal_is_denied() {
        // "Users/bob" is a sibling of "Users/alice", should be pruned
        assert!(!dir_allowed(&["Users/alice/**"], "Users/bob"));
    }

    #[test]
    fn sibling_with_shared_prefix_is_denied() {
        // "datafiles" shares the prefix "data" but is not under "data/"
        assert!(!dir_allowed(&["data/**"], "datafiles"));
    }

    #[test]
    fn unrelated_dir_is_denied() {
        assert!(!dir_allowed(&["Users/alice/**"], "tmp"));
        assert!(!dir_allowed(&["Users/alice/**", "data/**"], "var"));
    }

    #[test]
    fn no_pruning_when_none_allows_everything() {
        // **/*.rs returns None → no pruning → all dirs allowed
        assert!(dir_allowed(&["**/*.rs"], "anything"));
        assert!(dir_allowed(&["**/*.rs"], "Users/Administrator"));
    }

    #[test]
    fn windows_path_separators_normalised() {
        // Backslash-separated patterns (Windows config) should work the same way
        assert_eq!(
            terminals(&["Users\\alice\\**"]),
            Some(vec!["Users/alice".into()])
        );
        assert!(dir_allowed(&["Users\\alice\\**"], "Users/alice/docs"));
        assert!(!dir_allowed(&["Users\\alice\\**"], "Users/bob"));
    }
}
