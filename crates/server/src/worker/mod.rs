mod archive_batch;
mod group;
mod pipeline;
mod request;

use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

use find_common::api::{RecentFile, WorkerStatus};
use find_common::config::{AlertsConfig, NormalizationSettings};
use find_content_store::ContentStore;


const POLL_INTERVAL: std::time::Duration = std::time::Duration::from_secs(1);

/// Configuration values for the inbox worker — plain scalars read from the
/// server config at startup. Bundled into a struct so function signatures stay
/// stable when new settings are added.
#[derive(Clone)]
pub struct WorkerConfig {
    pub request_timeout: std::time::Duration,
    pub archive_batch_size: usize,
    pub activity_log_max_entries: usize,
    pub normalization: NormalizationSettings,
    /// Number of consecutive timeouts before auto-pausing. 0 = disabled.
    pub consecutive_timeout_limit: u32,
    /// Alert notification configuration.
    pub alerts: AlertsConfig,
}

/// Log the start and finish of a labelled step at DEBUG level, including elapsed ms.
///
/// ```ignore
/// let value = timed!(tag, "parse gz", { expensive_call()? });
/// ```
macro_rules! timed {
    ($tag:expr, $label:expr, $body:expr) => {{
        tracing::debug!("{} → {}", $tag, $label);
        let __t = std::time::Instant::now();
        let __r = $body;
        tracing::debug!("{} ← {} ({:.1}ms)", $tag, $label, __t.elapsed().as_secs_f64() * 1000.0);
        __r
    }};
}
// Export the macro to sibling modules.
use timed;

/// Log a warning if `start` is older than `threshold_secs`.
pub(super) fn warn_slow(start: std::time::Instant, threshold_secs: u64, step: &str, context: &str) {
    let elapsed = start.elapsed();
    if elapsed.as_secs() >= threshold_secs {
        tracing::warn!(
            elapsed_secs = elapsed.as_secs(),
            step,
            context,
            "Slow step: {step} took {:.1}s for {context}",
            elapsed.as_secs_f64(),
        );
    }
}

type StatusHandle = std::sync::Arc<std::sync::Mutex<WorkerStatus>>;

/// Runtime handles passed to the inbox worker at startup.
/// Bundles the Arc channels and broadcast sender so `start_inbox_worker`
/// stays under the clippy argument-count limit.
pub struct WorkerHandles {
    pub status: StatusHandle,
    pub content_store: Arc<dyn ContentStore>,
    pub inbox_paused: Arc<AtomicBool>,
    /// Counts consecutive inbox request processing timeouts for the circuit breaker.
    pub consecutive_timeouts: Arc<AtomicU32>,
    /// Broadcast channel for live activity events sent to SSE subscribers.
    pub recent_tx: tokio::sync::broadcast::Sender<RecentFile>,
    /// Shared stats cache for incremental updates after each batch.
    pub source_stats_cache: Arc<std::sync::RwLock<crate::stats_cache::SourceStatsCache>>,
    /// Watch channel incremented after every stats cache update.
    pub stats_watch: Arc<tokio::sync::watch::Sender<u64>>,
}

/// Ensure inbox subdirectories exist on startup.
///
/// Files left in `inbox/` from a previous run are simply re-processed on the
/// next scan — no explicit recovery is needed because files are never moved out
/// of `inbox/` until processing completes.  Files in `inbox/to-archive/` are
/// left alone; the archive thread picks them up automatically.
pub async fn recover_stranded_requests(data_dir: &Path) -> anyhow::Result<()> {
    let inbox_dir = data_dir.join("inbox");
    tokio::fs::create_dir_all(&inbox_dir).await?;
    tokio::fs::create_dir_all(inbox_dir.join("failed")).await?;
    tokio::fs::create_dir_all(inbox_dir.join("to-archive")).await?;

    // One-time migration: if a `processing/` directory exists from an older
    // server version, move any stranded files back to `inbox/`.
    let processing_dir = inbox_dir.join("processing");
    if processing_dir.exists() {
        let mut stranded = tokio::fs::read_dir(&processing_dir).await?;
        while let Ok(Some(entry)) = stranded.next_entry().await {
            let src = entry.path();
            if src.extension() == Some(OsStr::new("gz")) {
                let dst = inbox_dir.join(entry.file_name());
                if let Err(e) = tokio::fs::rename(&src, &dst).await {
                    tracing::warn!("Failed to recover stranded request {}: {e}", src.display());
                } else {
                    tracing::info!("Recovered stranded request: {}", dst.display());
                }
            }
        }
    }
    Ok(())
}

/// Start the two-phase inbox worker.
///
/// Phase 1 (indexing loop): a single worker processes inbox requests
/// sequentially, writing to SQLite only (no ZIP I/O). On success, moves
/// the .gz to `inbox/to-archive/` and signals the archive thread.
///
/// Phase 2 (archive loop): a single archive thread batches up to
/// `archive_batch_size` requests from `to-archive/`, coalesces work, rewrites
/// ZIPs, and updates line refs in SQLite.
pub async fn start_inbox_worker(
    data_dir: PathBuf,
    cfg: WorkerConfig,
    handles: WorkerHandles,
) -> anyhow::Result<()> {
    let WorkerHandles { status, content_store, inbox_paused, consecutive_timeouts, recent_tx, source_stats_cache, stats_watch } = handles;
    let stats_watch_archive = Arc::clone(&stats_watch);
    let source_stats_cache_archive = Arc::clone(&source_stats_cache);
    let inbox_dir = data_dir.join("inbox");
    let failed_dir = inbox_dir.join("failed");
    let to_archive_dir = inbox_dir.join("to-archive");

    tokio::fs::create_dir_all(&to_archive_dir).await?;

    tracing::info!(
        "Starting two-phase inbox worker: {}",
        inbox_dir.display()
    );

    let archive_notify = Arc::new(tokio::sync::Notify::new());

    // Channel from router → worker (capacity 1: at most one group buffered ahead).
    let (work_tx, mut work_rx) = tokio::sync::mpsc::channel::<Vec<PathBuf>>(1);
    // Channel from worker → router: signals that a path is no longer in-flight.
    let (done_tx, done_rx) = tokio::sync::mpsc::channel::<PathBuf>(64);

    // Spawn the single indexing worker task.
    {
        let data_dir = data_dir.clone();
        let failed_dir = failed_dir.clone();
        let to_archive_dir_clone = to_archive_dir.clone();
        let handles = group::IndexerHandles {
            phase1: group::Phase1Handles {
                status: status.clone(),
                cfg: cfg.clone(),
                recent_tx,
                stats_watch,
                content_store: Arc::clone(&content_store),
                source_stats_cache,
                archive_notify: Arc::clone(&archive_notify),
            },
            inbox_paused: Arc::clone(&inbox_paused),
            consecutive_timeouts: Arc::clone(&consecutive_timeouts),
        };

        tokio::spawn(async move {
            tracing::debug!("Indexing worker started");
            while let Some(paths) = work_rx.recv().await {
                let ctx = group::GroupContext {
                    data_dir: data_dir.clone(),
                    paths: paths.clone(),
                    failed_dir: failed_dir.clone(),
                    to_archive_dir: to_archive_dir_clone.clone(),
                };
                group::process_group_async(ctx, &handles).await;
                // Signal the router that these paths are done (whatever the
                // outcome: flushed, failed, or left in inbox for rediscovery).
                for path in paths {
                    let _ = done_tx.send(path).await;
                }
            }
            tracing::debug!("Indexing worker exited");
        });
    }

    // Spawn the archive loop (blocking, spawn_blocking wrapper).
    {
        let data_dir = data_dir.clone();
        let to_archive_dir = to_archive_dir.clone();
        let cs = Arc::clone(&content_store);
        let archive_notify = Arc::clone(&archive_notify);
        let stats_watch = stats_watch_archive;
        let source_stats_cache = source_stats_cache_archive;

        tokio::spawn(async move {
            tracing::debug!("Archive worker started");
            loop {
                // Wait for signal OR 60 s timeout.
                let _ = tokio::time::timeout(
                    std::time::Duration::from_secs(60),
                    archive_notify.notified(),
                )
                .await;

                // Sleep 5 s to allow accumulation before processing.
                tokio::time::sleep(std::time::Duration::from_secs(5)).await;

                let mut any_processed = false;

                // Drain queue in batches (blocking).
                loop {
                    let to_archive = to_archive_dir.clone();
                    let data = data_dir.clone();
                    let cs_batch = Arc::clone(&cs);
                    let cfg_clone = cfg.clone();
                    let batch_result = tokio::task::spawn_blocking(move || {
                        archive_batch::run_archive_batch(&data, &to_archive, cfg_clone, &cs_batch)
                    })
                    .await;

                    match batch_result {
                        Ok(Ok(processed)) => {
                            if processed > 0 {
                                any_processed = true;
                                // Notify the SSE stats stream so archive_queue count updates.
                                stats_watch.send_modify(|v| *v = v.wrapping_add(1));
                            }
                            if processed < cfg.archive_batch_size {
                                break; // queue drained
                            }
                            // else loop immediately for next batch
                        }
                        Ok(Err(e)) => {
                            tracing::error!("Archive batch failed: {e:#}");
                            break;
                        }
                        Err(e) => {
                            tracing::error!("Archive batch task error: {e}");
                            break;
                        }
                    }
                }

                // When the queue drains, rebuild stats so files_pending_content updates.
                if any_processed {
                    let cache = Arc::clone(&source_stats_cache);
                    let cs2 = Arc::clone(&cs);
                    let dd = data_dir.clone();
                    tokio::task::spawn_blocking(move || {
                        crate::stats_cache::full_rebuild(&dd, &cache, &cs2);
                    }).await.ok();
                    stats_watch.send_modify(|v| *v = v.wrapping_add(1));
                }
            }
        });
    }

    // Router loop: poll inbox, dispatch files not already in-flight to the worker.
    let mut interval = tokio::time::interval(POLL_INTERVAL);
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut in_flight: std::collections::HashSet<PathBuf> = std::collections::HashSet::new();
    let mut done_rx = done_rx;

    loop {
        tokio::select! {
            _ = interval.tick() => {}
            Some(done_path) = done_rx.recv() => {
                in_flight.remove(&done_path);
                while let Ok(p) = done_rx.try_recv() { in_flight.remove(&p); }
            }
        }

        while let Ok(done_path) = done_rx.try_recv() {
            in_flight.remove(&done_path);
        }

        let mut entries = match tokio::fs::read_dir(&inbox_dir).await {
            Ok(e) => e,
            Err(e) => {
                tracing::error!("Failed to read inbox dir: {e}");
                continue;
            }
        };

        let mut gz_files: Vec<(std::time::SystemTime, u64, PathBuf)> = Vec::new();
        while let Ok(Some(entry)) = entries.next_entry().await {
            let path = entry.path();
            if path.extension() == Some(OsStr::new("gz")) {
                let meta = entry.metadata().await.ok();
                let mtime = meta.as_ref()
                    .and_then(|m| m.modified().ok())
                    .unwrap_or(std::time::UNIX_EPOCH);
                let size = meta.map(|m| m.len()).unwrap_or(0);
                gz_files.push((mtime, size, path));
            }
        }
        gz_files.sort_unstable_by_key(|(mtime, _, _)| *mtime);

        if inbox_paused.load(Ordering::Relaxed) {
            continue;
        }

        let candidates: Vec<(u64, PathBuf)> = gz_files
            .into_iter()
            .filter(|(_, _, p)| !in_flight.contains(p))
            .map(|(_, size, p)| (size, p))
            .collect();

        let mut remaining = candidates.as_slice();
        while !remaining.is_empty() {
            let group_paths = form_group(remaining);
            let n = group_paths.len();
            match work_tx.try_send(group_paths.clone()) {
                Ok(()) => {
                    for p in group_paths {
                        in_flight.insert(p);
                    }
                    remaining = &remaining[n..];
                }
                Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => {
                    break;
                }
                Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => {
                    tracing::error!("Worker channel closed unexpectedly; stopping router");
                    return Ok(());
                }
            }
        }
    }
}

/// Take a bounded prefix of the pending inbox files (mtime order) as one
/// dispatch group. Caps: `MAX_GROUP_REQUESTS` files or `MAX_GROUP_GZ_BYTES`
/// of compressed input, whichever comes first — but always at least one
/// file, so an oversized request degrades to a group of one (today's
/// one-at-a-time behavior).
fn form_group(candidates: &[(u64, PathBuf)]) -> Vec<PathBuf> {
    let mut group = Vec::new();
    let mut bytes: u64 = 0;
    for (size, path) in candidates {
        if !group.is_empty()
            && (group.len() >= group::MAX_GROUP_REQUESTS || bytes + size > group::MAX_GROUP_GZ_BYTES)
        {
            break;
        }
        group.push(path.clone());
        bytes += size;
    }
    group
}

#[cfg(test)]
mod group_dispatch_tests {
    use super::*;

    fn p(name: &str) -> PathBuf {
        PathBuf::from(format!("/inbox/{name}.gz"))
    }

    #[test]
    fn form_group_takes_all_when_under_caps() {
        let candidates = vec![(100u64, p("a")), (100, p("b")), (100, p("c"))];
        assert_eq!(form_group(&candidates).len(), 3);
    }

    #[test]
    fn form_group_caps_request_count() {
        let candidates: Vec<(u64, PathBuf)> =
            (0..group::MAX_GROUP_REQUESTS + 10).map(|i| (10u64, p(&format!("f{i}")))).collect();
        assert_eq!(form_group(&candidates).len(), group::MAX_GROUP_REQUESTS);
    }

    #[test]
    fn form_group_caps_total_bytes() {
        let half = group::MAX_GROUP_GZ_BYTES / 2 + 1;
        let candidates = vec![(half, p("a")), (half, p("b")), (half, p("c"))];
        assert_eq!(form_group(&candidates).len(), 1, "second file would exceed the byte cap");
    }

    #[test]
    fn form_group_always_takes_at_least_one() {
        let candidates = vec![(group::MAX_GROUP_GZ_BYTES * 10, p("huge"))];
        assert_eq!(form_group(&candidates).len(), 1);
    }

    #[test]
    fn form_group_preserves_input_order() {
        let candidates = vec![(1u64, p("first")), (1, p("second")), (1, p("third"))];
        let group = form_group(&candidates);
        assert_eq!(group, vec![p("first"), p("second"), p("third")]);
    }

    #[test]
    fn form_group_empty_input() {
        assert!(form_group(&[]).is_empty());
    }
}

#[cfg(test)]
mod tests {
    use super::pipeline::{filename_only_file, is_outer_archive, outer_archive_stub};
    use find_common::api::{FileKind, IndexFile, IndexLine};

    fn make_file(path: &str, kind: FileKind) -> IndexFile {
        IndexFile {
            path: path.to_string(),
            mtime: 1000,
            size: Some(100),
            kind,
            lines: vec![IndexLine {
                archive_path: None,
                line_number: 0,
                content: path.to_string(),
            }],
            extract_ms: None,
            file_hash: None,
            scanner_version: 0,
            is_new: false,
            force: false,
        }
    }

    #[test]
    fn outer_archive_detected() {
        assert!(is_outer_archive("data.zip", &FileKind::Archive));
    }

    #[test]
    fn archive_member_not_outer() {
        assert!(!is_outer_archive("data.zip::inner.txt", &FileKind::Archive));
    }

    #[test]
    fn non_archive_kind_not_outer() {
        assert!(!is_outer_archive("data.zip", &FileKind::Text));
    }

    #[test]
    fn filename_only_converts_archive_kind_to_unknown() {
        let f = make_file("data.zip", FileKind::Archive);
        let fallback = filename_only_file(&f);
        assert_eq!(fallback.kind, FileKind::Unknown);
    }

    #[test]
    fn filename_only_keeps_non_archive_kind() {
        let f = make_file("notes.md", FileKind::Text);
        let fallback = filename_only_file(&f);
        assert_eq!(fallback.kind, FileKind::Text);
    }

    #[test]
    fn filename_only_has_path_and_metadata_lines() {
        let f = make_file("docs/report.pdf", FileKind::Pdf);
        let fallback = filename_only_file(&f);
        assert_eq!(fallback.lines.len(), 2);
        assert_eq!(fallback.lines[0].line_number, 0);
        assert_eq!(fallback.lines[0].content, "docs/report.pdf");
        assert_eq!(fallback.lines[1].line_number, 1);
        assert!(fallback.lines[1].content.is_empty());
    }

    #[test]
    fn filename_only_preserves_mtime_and_size() {
        let f = make_file("file.txt", FileKind::Text);
        let fallback = filename_only_file(&f);
        assert_eq!(fallback.mtime, f.mtime);
        assert_eq!(fallback.size, f.size);
    }

    #[test]
    fn outer_archive_stub_preserves_archive_kind() {
        let f = make_file("backup.7z", FileKind::Archive);
        let stub = outer_archive_stub(&f);
        assert_eq!(stub.kind, FileKind::Archive);
    }

    #[test]
    fn outer_archive_stub_uses_zero_mtime() {
        let f = make_file("backup.7z", FileKind::Archive);
        let stub = outer_archive_stub(&f);
        assert_eq!(stub.mtime, 0);
    }

    #[test]
    fn outer_archive_stub_has_path_and_metadata_lines() {
        let f = make_file("backups/big.tar.gz", FileKind::Archive);
        let stub = outer_archive_stub(&f);
        assert_eq!(stub.lines.len(), 2);
        assert_eq!(stub.lines[0].line_number, 0);
        assert_eq!(stub.lines[0].content, "backups/big.tar.gz");
        assert_eq!(stub.lines[1].line_number, 1);
        assert!(stub.lines[1].content.is_empty());
    }
}
