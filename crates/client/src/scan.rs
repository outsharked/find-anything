use std::collections::{HashMap, HashSet};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::UNIX_EPOCH;

use anyhow::Result;
use globset::GlobSet;
use tracing::{info, warn};

use find_common::{
    api::{FileKind, IndexFile, IndexLine, IndexingFailure, SCANNER_VERSION, LINE_METADATA, LINE_CONTENT_START},
    config::{extractor_config_from_scan, load_dir_override, ExternalExtractorMode, ScanConfig},
    path::is_composite,
};

use crate::api::ApiClient;
use crate::batch::{build_index_files, build_member_index_files, index_file_bytes, submit_batch};
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
    /// Force re-index of all files regardless of mtime or scanner version.
    /// Holds the Unix timestamp (seconds) at which the force run was started.
    /// Files with `indexed_at >= force_since` are skipped (already done in a
    /// prior partial run). Pass the same epoch to resume an interrupted run.
    pub force_since: Option<i64>,
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

/// Decide whether a local file needs to be (re-)indexed, given what the server
/// already knows about it.
///
/// `server_entry` is `Some((mtime, scanner_version, indexed_at))` if the server
/// has a record for this path, `None` if it is new.
///
/// Returns `(should_index, is_new)`.
pub(crate) fn needs_reindex(
    server_entry: Option<(i64, u32, Option<i64>)>,
    local_mtime: i64,
    upgrade: bool,
    force_since: Option<i64>,
) -> (bool, bool) {
    match server_entry {
        None                                                          => (true,  true),
        Some((sm, _, _))  if local_mtime > sm                        => (true,  false),
        Some((_, sv, _))  if upgrade && sv < SCANNER_VERSION         => (true,  false),
        Some((_, _, ia))  if force_since.is_some_and(|fs| ia.is_none_or(|t| t < fs))
                                                                      => (true,  false),
        Some(_)                                                       => (false, false),
    }
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
    let server_files: HashMap<String, (i64, u32, Option<i64>)> = api
        .list_files(source_name)
        .await?
        .into_iter()
        .filter(|f| !is_composite(&f.path))
        .filter(|f| match &source.subdir {
            None => true,
            Some(sub) => f.path == *sub || f.path.starts_with(&format!("{sub}/")),
        })
        .map(|f| (f.path, (f.mtime, f.scanner_version, f.indexed_at)))
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
    let mut excluded: usize = 0;    // went through process_file but excluded by filter/missing extractor
    let mut new_files: usize = 0;   // in local but absent from server DB
    let mut modified: usize = 0;    // mtime changed since last scan
    let mut upgraded: usize = 0;    // mtime unchanged but scanner_version outdated

    // Build the "N unchanged[, M new][, P modified][, Q upgraded]" summary,
    // omitting new/modified/upgraded when they are zero.
    let fmt_changes = |skipped: usize, new_files: usize, modified: usize, upgraded: usize, excluded: usize| -> String {
        let mut parts = vec![format!("{skipped} unchanged")];
        if new_files > 0 { parts.push(format!("{new_files} new")); }
        if modified  > 0 { parts.push(format!("{modified} modified")); }
        if upgraded  > 0 { parts.push(format!("{upgraded} upgraded")); }
        if excluded  > 0 { parts.push(format!("{excluded} excluded")); }
        parts.join(", ")
    };
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
        let mut is_new = false; // set inside the !subdir_rescan block when server_entry is known
        let mut is_upgraded_file = false;
        if !subdir_rescan {
            let server_entry = server_files.get(rel_path.as_str()).copied();
            let (should_index, file_is_new) = needs_reindex(server_entry, mtime, opts.upgrade, opts.force_since);
            if !should_index {
                skipped += 1;
                if last_log.elapsed() >= log_interval {
                    let total = indexed + skipped;
                    info!("processed {total} files ({}) so far...", fmt_changes(skipped, new_files, modified, upgraded, excluded));
                    last_log = std::time::Instant::now();
                }
                continue;
            }
            is_new = file_is_new;
            is_upgraded_file = !file_is_new && server_entry.is_some_and(|(_, sv, _)| opts.upgrade && sv < SCANNER_VERSION);
        }

        if !opts.dry_run {
            if process_file(&mut ctx, rel_path, abs_path, mtime, is_new).await? {
                indexed += 1;
                if is_new { new_files += 1; }
                else if is_upgraded_file { upgraded += 1; }
                else if !subdir_rescan { modified += 1; }
            } else {
                excluded += 1;
            }
        } else {
            indexed += 1;
            if is_new { new_files += 1; }
            else if is_upgraded_file { upgraded += 1; }
            else if !subdir_rescan { modified += 1; }
        }
        if last_log.elapsed() >= log_interval {
            let total = indexed + skipped;
            info!(
                "processed {total} files ({}) so far, {} in current batch...",
                fmt_changes(skipped, new_files, modified, upgraded, excluded),
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

    let excluded_msg = if excluded > 0 { format!(", {excluded} excluded by filter") } else { String::new() };
    info!("scan complete — {indexed} indexed ({new_files} new, {modified} modified, {upgraded} upgraded), {skipped} unchanged, {deleted} deleted{excluded_msg}");
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

    async fn maybe_flush(&mut self) -> Result<()> {
        if self.batch.len() >= self.batch_size
            || self.batch_bytes >= self.batch_bytes_limit
            || (!self.batch.is_empty() && self.last_submit.elapsed() >= self.batch_interval)
        {
            self.submit(vec![]).await?;
        }
        Ok(())
    }
}

/// Bundled parameters for `push_non_archive_files` — groups the per-file
/// extraction results so the function signature stays under the argument limit.
pub struct ExtractedFile {
    pub rel_path:   String,
    pub abs_path:   PathBuf,
    pub mtime:      i64,
    pub size:       i64,
    pub kind:       FileKind,
    pub lines:      Vec<IndexLine>,
    pub extract_ms: u64,
    pub is_new:     bool,
}

/// Shared post-processing for non-archive extraction (both builtin and external-stdout).
///
/// Applies kind refinement from `[FILE:mime]` lines, computes the content hash,
/// builds `IndexFile`s, and pushes them into the batch.
async fn push_non_archive_files(
    ctx: &mut ScanContext<'_>,
    file: &ExtractedFile,
) -> Result<()> {
    // Refine Unknown or Text kind using extracted content:
    // - A [FILE:mime] line emitted by dispatch means binary → use mime_to_kind.
    // - Text content lines (line_number > 0) present → promote to Text.
    // - Neither → keep as-is.
    let kind = if file.kind == FileKind::Text || file.kind == FileKind::Unknown {
        if let Some(mime_line) = file.lines.iter().find(|l| l.line_number == LINE_METADATA && l.content.starts_with("[FILE:mime] ")) {
            let mime = &mime_line.content["[FILE:mime] ".len()..];
            FileKind::from(find_extract_dispatch::mime_to_kind(mime))
        } else if file.lines.iter().any(|l| l.line_number >= LINE_CONTENT_START) {
            FileKind::Text
        } else {
            file.kind.clone()
        }
    } else {
        file.kind.clone()
    };
    // Hash raw file bytes for dedup (streaming to avoid OOM on large files).
    // Skip hashing known binary extensions that no specialist extractor handles:
    // opening these files can block indefinitely on Windows (e.g. live VHDX held by Hyper-V).
    let content_hash = if find_extract_dispatch::is_binary_ext_path(&file.abs_path) {
        None
    } else {
        hash_file(&file.abs_path)
    };
    let mut index_files = build_index_files(file.rel_path.clone(), file.mtime, file.size, kind, file.lines.clone());
    if let Some(f) = index_files.first_mut() {
        f.extract_ms = Some(file.extract_ms);
        f.content_hash = content_hash;
        f.is_new = file.is_new;
    }
    for f in index_files {
        ctx.batch_bytes += index_file_bytes(&f);
        ctx.batch.push(f);
        ctx.maybe_flush().await?;
    }
    Ok(())
}

/// Process one file: resolve its effective config, extract content via
const SCAN_INLINE_SET: &[subprocess::InlineKind] = &[
    subprocess::InlineKind::Text,
    subprocess::InlineKind::Html,
    subprocess::InlineKind::Media,
    subprocess::InlineKind::Office,
];

/// subprocess, handle OOM server-fallback, and accumulate the result in the
/// batch. Called from both the `run_scan` loop and `scan_single_file`.
/// Returns `true` if the file was actually submitted to the server, `false` if
/// it was excluded by a filter or skipped due to a missing extractor.
async fn process_file(ctx: &mut ScanContext<'_>, rel_path: &str, abs_path: &Path, mtime: i64, is_new: bool) -> Result<bool> {
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
                return Ok(false);
            }
        }
    }

    let size = size_of(abs_path).unwrap_or(0);
    let kind = FileKind::from(extract::detect_kind(abs_path));

    if !ctx.quiet {
        info!("Processing {rel_path}");
    }

    match subprocess::resolve_extractor(abs_path, &eff_scan, &eff_scan.extractor_dir, SCAN_INLINE_SET) {
        subprocess::ExtractorRoute::External(ref ext_cfg) => {
            match ext_cfg.mode {
                ExternalExtractorMode::Stdout => {
                    // ── External stdout extraction ────────────────────────────────
                    let t0 = std::time::Instant::now();
                    if ctx.quiet { lazy_header::set_pending(&abs_path.to_string_lossy()); }
                    let outcome = subprocess::run_external_stdout(abs_path, ext_cfg, &eff_scan).await;
                    if ctx.quiet { lazy_header::clear_pending(); }

                    let lines = match outcome {
                        subprocess::ExternalOutcome::Ok(lines) => lines,
                        subprocess::ExternalOutcome::OkMembers(_) => unreachable!("stdout mode always returns Ok"),
                        subprocess::ExternalOutcome::BinaryMissing => {
                            warn!("skipping {rel_path}: external extractor binary not found (file will be retried once configured correctly)");
                            return Ok(false);
                        }
                        subprocess::ExternalOutcome::Failed(e) => {
                            if eff_scan.server_fallback {
                                if let Err(upload_err) = upload::upload_file(ctx.api, abs_path, rel_path, mtime, ctx.source_name).await {
                                    warn!("server fallback upload failed for {rel_path}: {upload_err:#}");
                                } else {
                                    return Ok(true);
                                }
                            }
                            if ctx.failures.len() < MAX_FAILURES_PER_BATCH {
                                ctx.failures.push(IndexingFailure {
                                    path: rel_path.to_string(),
                                    error: truncate_error(&e, MAX_ERROR_LEN),
                                });
                            }
                            vec![]
                        }
                    };

                    let extract_ms = t0.elapsed().as_millis() as u64;
                    push_non_archive_files(ctx, &ExtractedFile {
                        rel_path: rel_path.to_string(),
                        abs_path: abs_path.to_path_buf(),
                        mtime,
                        size,
                        kind,
                        lines,
                        extract_ms,
                        is_new,
                    }).await?;
                }
                ExternalExtractorMode::TempDir => {
                    // ── External tempdir extraction ───────────────────────────────
                    if ctx.quiet {
                        info!("extracting {rel_path} via external extractor");
                    }

                    let outer_hash = hash_file(abs_path);

                    // Sentinel: mtime=0 signals server to delete stale members.
                    let outer_start = IndexFile {
                        path: rel_path.to_string(),
                        mtime: 0,
                        size: Some(size),
                        kind: FileKind::Archive,
                        lines: vec![IndexLine { archive_path: None, line_number: 0, content: format!("[PATH] {}", rel_path) }],
                        extract_ms: None,
                        content_hash: None,
                        scanner_version: SCANNER_VERSION,
                        is_new,
                    };
                    ctx.batch.push(outer_start);
                    ctx.submit(vec![]).await?;

                    if ctx.quiet { lazy_header::set_pending(&abs_path.to_string_lossy()); }
                    let ext_config = extractor_config_from_scan(&eff_scan);
                    let outcome = subprocess::run_external_tempdir(abs_path, ext_cfg, &eff_scan, &ext_config).await;
                    if ctx.quiet { lazy_header::clear_pending(); }

                    let member_batches = match outcome {
                        subprocess::ExternalOutcome::OkMembers(batches) => batches,
                        subprocess::ExternalOutcome::Ok(_) => unreachable!("tempdir always returns OkMembers"),
                        subprocess::ExternalOutcome::BinaryMissing => {
                            warn!("skipping {rel_path}: external extractor binary not found (file will be retried once configured correctly)");
                            return Ok(false);
                        }
                        subprocess::ExternalOutcome::Failed(e) => {
                            if ctx.failures.len() < MAX_FAILURES_PER_BATCH {
                                ctx.failures.push(IndexingFailure {
                                    path: rel_path.to_string(),
                                    error: truncate_error(&e, MAX_ERROR_LEN),
                                });
                            }
                            vec![]
                        }
                    };

                    let mut members_submitted: usize = 0;
                    for batch in member_batches {
                        for file in build_member_index_files(rel_path, mtime, batch.size, batch.lines, batch.content_hash) {
                            ctx.batch_bytes += index_file_bytes(&file);
                            members_submitted += 1;
                            ctx.batch.push(file);
                            ctx.maybe_flush().await?;
                        }
                    }

                    // Flush remaining members.
                    if !ctx.batch.is_empty() {
                        info!("submitting batch — extracting {rel_path} ({} members, {members_submitted} total)", ctx.batch.len());
                        ctx.submit(vec![]).await?;
                    }

                    // Completion upsert: real mtime so next scan skips re-indexing.
                    ctx.batch.push(IndexFile {
                        path: rel_path.to_string(),
                        mtime,
                        size: Some(size),
                        kind: FileKind::Archive,
                        lines: vec![IndexLine { archive_path: None, line_number: 0, content: format!("[PATH] {}", rel_path) }],
                        extract_ms: None,
                        content_hash: outer_hash,
                        scanner_version: SCANNER_VERSION,
                        is_new,
                    });
                }
            }
        }
        subprocess::ExtractorRoute::Archive => {
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
                    lines: vec![IndexLine { archive_path: None, line_number: 0, content: format!("[PATH] {}", rel_path) }],
                    extract_ms: None,
                    content_hash: None, // no hash on start sentinel — avoids premature dedup alias
                    scanner_version: SCANNER_VERSION,
                    is_new,
                };
                ctx.batch.push(outer_start);
                ctx.submit(vec![]).await?;

                if ctx.quiet { lazy_header::set_pending(&abs_path.to_string_lossy()); }
                let (mut member_rx, subprocess_task) = subprocess::start_archive_subprocess(
                    abs_path.to_path_buf(), &eff_scan, &subprocess::resolve_binary_for_archive(&eff_scan.extractor_dir));

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
                    // each "::" segment is checked so that e.g. node_modules/pkg.tgz::index.js
                    // is excluded when **/node_modules/** is in the exclude list.
                    if let Some(ap) = member_batch.lines.first().and_then(|l| l.archive_path.as_deref()) {
                        if ap.split("::").any(|seg| eff_excludes.is_match(seg)) {
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
                    for file in build_member_index_files(rel_path, member_mtime, member_batch.size, member_batch.lines, content_hash) {
                        ctx.batch_bytes += index_file_bytes(&file);
                        members_submitted += 1;
                        ctx.batch.push(file);
                        ctx.maybe_flush().await?;
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
                    lines: vec![IndexLine { archive_path: None, line_number: 0, content: format!("[PATH] {}", rel_path) }],
                    extract_ms: None,
                    content_hash: outer_hash,
                    scanner_version: SCANNER_VERSION,
                    is_new,
                });
        }
        subprocess::ExtractorRoute::Subprocess(ref binary) => {
            // ── Non-archive extraction ────────────────────────────────────────────
            // dispatch_from_path handles MIME detection internally: it emits a
            // [FILE:mime] line when no extractor matched the bytes, so we check
            // for that line below to update the kind accordingly.
            let t0 = std::time::Instant::now();
            if ctx.quiet { lazy_header::set_pending(&abs_path.to_string_lossy()); }
            let outcome = subprocess::extract_via_subprocess(
                abs_path, &eff_scan, binary).await;
            if ctx.quiet { lazy_header::clear_pending(); }

            let lines = match outcome {
                subprocess::SubprocessOutcome::Ok(lines) => lines,
                subprocess::SubprocessOutcome::BinaryMissing => {
                    // Extractor binary not installed — skip this file entirely so it
                    // is re-indexed (with content) once the binary is deployed.
                    warn!("skipping {rel_path}: extractor binary not found (file will be retried once the binary is installed)");
                    return Ok(false);
                }
                subprocess::SubprocessOutcome::Failed => {
                    if eff_scan.server_fallback {
                        if let Err(e) = upload::upload_file(ctx.api, abs_path, rel_path, mtime, ctx.source_name).await {
                            warn!("server fallback upload failed for {rel_path}: {e:#}");
                            // Fall through: index filename-only so file appears in search.
                        } else {
                            // Server will index it; skip local filename-only entry.
                            return Ok(true);
                        }
                    }
                    // Index filename-only so the file is at least findable by name.
                    vec![]
                }
            };

            let extract_ms = t0.elapsed().as_millis() as u64;
            push_non_archive_files(ctx, &ExtractedFile {
                rel_path: rel_path.to_string(),
                abs_path: abs_path.to_path_buf(),
                mtime,
                size,
                kind,
                lines,
                extract_ms,
                is_new,
            }).await?;
        }
        subprocess::ExtractorRoute::Inline(inline_kind) => {
            // `inline_kind` is the InlineKind enum variant (bound here to avoid shadowing
            // the outer `kind: String` computed from detect_kind on line 473).
            let t0 = std::time::Instant::now();
            if ctx.quiet { lazy_header::set_pending(&abs_path.to_string_lossy()); }
            let ext_config = extractor_config_from_scan(&eff_scan);
            let lines = subprocess::extract_inline(inline_kind, abs_path, &ext_config);
            if ctx.quiet { lazy_header::clear_pending(); }

            let extract_ms = t0.elapsed().as_millis() as u64;
            // `kind` here is the outer FileKind variable, not the InlineKind.
            push_non_archive_files(ctx, &ExtractedFile {
                rel_path: rel_path.to_string(),
                abs_path: abs_path.to_path_buf(),
                mtime,
                size,
                kind,
                lines,
                extract_ms,
                is_new,
            }).await?;
        }
    }

    ctx.maybe_flush().await?;
    Ok(true)
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
    process_file(&mut ctx, rel_path, abs_path, mtime, false).await?;
    ctx.submit(vec![]).await?;
    info!("done");
    Ok(())
}

// ── Helpers ───────────────────────────────────────────────────────────────────

use crate::walk::build_globset;

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
pub(crate) use crate::path_util::include_dir_prefixes;

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
        let root = PathBuf::from(&root_str);
        // When scanning a subdir, walk from root/subdir but compute rel-paths
        // relative to root so they match what the server already stores.
        let walk_start = match subdir {
            Some(sub) => { let mut p = root.clone(); p.push(sub); p }
            None => root.clone(),
        };

        crate::walk::walk_source_tree(
            &walk_start,
            &root,
            scan,
            excludes,
            include_dirs,
            |item| {
                let crate::walk::WalkItem::File { abs, rel, name, depth } = item else { return; };
                // Hidden files (hidden directories already pruned in walk_source_tree).
                if !scan.include_hidden && name.starts_with('.') && depth > 0 {
                    return;
                }
                // Apply source-level include filter.
                if !includes.is_empty() && !includes.is_match(&*rel) {
                    return;
                }
                map.insert(rel, abs);
                if last_log.elapsed() >= log_interval {
                    info!("walking filesystem... {} files found so far", map.len());
                    last_log = std::time::Instant::now();
                }
            },
        );
    }

    map
}


use crate::path_util::{normalise_path_sep, normalise_root};

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

    // ── needs_reindex ─────────────────────────────────────────────────────────

    #[test]
    fn needs_reindex_new_file() {
        // File not on server → should index, is_new = true
        let (idx, is_new) = needs_reindex(None, 1000, false, None);
        assert!(idx);
        assert!(is_new);
    }

    #[test]
    fn needs_reindex_mtime_newer() {
        // Local mtime is newer than server mtime → re-index, not new
        let (idx, is_new) = needs_reindex(Some((500, 1, None)), 1000, false, None);
        assert!(idx);
        assert!(!is_new);
    }

    #[test]
    fn needs_reindex_mtime_equal() {
        // Same mtime → skip
        let (idx, _) = needs_reindex(Some((1000, 1, None)), 1000, false, None);
        assert!(!idx);
    }

    #[test]
    fn needs_reindex_mtime_older() {
        // Local mtime is older than server (clock skew / rollback) → skip
        let (idx, _) = needs_reindex(Some((2000, 1, None)), 1000, false, None);
        assert!(!idx);
    }

    #[test]
    fn needs_reindex_upgrade_outdated_scanner() {
        // upgrade=true and server has an older scanner version → re-index
        let (idx, is_new) = needs_reindex(Some((1000, 0, None)), 1000, true, None);
        assert!(idx);
        assert!(!is_new);
    }

    #[test]
    fn needs_reindex_upgrade_current_scanner() {
        // upgrade=true but scanner version is current → skip
        let (idx, _) = needs_reindex(Some((1000, SCANNER_VERSION, None)), 1000, true, None);
        assert!(!idx);
    }

    #[test]
    fn needs_reindex_no_upgrade_flag_ignores_scanner_version() {
        // upgrade=false → scanner version difference is ignored
        let (idx, _) = needs_reindex(Some((1000, 0, None)), 1000, false, None);
        assert!(!idx);
    }

    #[test]
    fn needs_reindex_composite_path_not_special_cased() {
        // Composite paths (archive members) are never in server_files (filtered
        // out before building the map), so they would always arrive as None.
        // Confirm that needs_reindex treats them as new files.
        let (idx, is_new) = needs_reindex(None, 500, false, None);
        assert!(idx);
        assert!(is_new);
    }

    #[test]
    fn needs_reindex_force_since_no_indexed_at() {
        // force_since set, file has no indexed_at (never force-indexed) → re-index
        let (idx, is_new) = needs_reindex(Some((1000, SCANNER_VERSION, None)), 1000, false, Some(1_000_000));
        assert!(idx);
        assert!(!is_new);
    }

    #[test]
    fn needs_reindex_force_since_already_done() {
        // force_since set, indexed_at >= force_since → skip (already done this run)
        let (idx, _) = needs_reindex(Some((1000, SCANNER_VERSION, Some(1_000_001))), 1000, false, Some(1_000_000));
        assert!(!idx);
    }

    #[test]
    fn needs_reindex_force_since_not_yet_done() {
        // force_since set, indexed_at < force_since → re-index
        let (idx, is_new) = needs_reindex(Some((1000, SCANNER_VERSION, Some(999_999))), 1000, false, Some(1_000_000));
        assert!(idx);
        assert!(!is_new);
    }

    #[test]
    fn needs_reindex_force_none_does_not_force() {
        // force_since = None → indexed_at is irrelevant, mtime-equal file skipped
        let (idx, _) = needs_reindex(Some((1000, SCANNER_VERSION, Some(1))), 1000, false, None);
        assert!(!idx);
    }
}
