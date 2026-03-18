mod archive_batch;
mod pipeline;
mod request;

use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use find_common::api::{RecentFile, WorkerStatus};
use find_common::config::NormalizationSettings;

use crate::archive::SharedArchiveState;


const POLL_INTERVAL: std::time::Duration = std::time::Duration::from_secs(1);

/// Configuration values for the inbox worker — plain scalars read from the
/// server config at startup. Bundled into a struct so function signatures stay
/// stable when new settings are added.
#[derive(Clone)]
pub struct WorkerConfig {
    pub request_timeout: std::time::Duration,
    pub inline_threshold_bytes: u64,
    pub archive_batch_size: usize,
    pub activity_log_max_entries: usize,
    pub normalization: NormalizationSettings,
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
    pub archive_state: Arc<SharedArchiveState>,
    pub inbox_paused: Arc<AtomicBool>,
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
    let WorkerHandles { status, archive_state: shared_archive_state, inbox_paused, recent_tx, source_stats_cache, stats_watch } = handles;
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

    // Channel from router → worker (capacity 1: at most one file buffered ahead).
    let (work_tx, mut work_rx) = tokio::sync::mpsc::channel::<PathBuf>(1);
    // Channel from worker → router: signals that a path is no longer in-flight.
    let (done_tx, done_rx) = tokio::sync::mpsc::channel::<PathBuf>(64);

    // Spawn the single indexing worker task.
    {
        let data_dir = data_dir.clone();
        let failed_dir = failed_dir.clone();
        let to_archive_dir_clone = to_archive_dir.clone();
        let status = status.clone();
        let archive_notify = Arc::clone(&archive_notify);
        let shared = Arc::clone(&shared_archive_state);
        let cfg_index = cfg.clone();

        tokio::spawn(async move {
            tracing::debug!("Indexing worker started");
            let handles = request::IndexerHandles {
                status,
                cfg: cfg_index,
                archive_notify,
                shared_archive: shared,
                recent_tx,
                source_stats_cache,
                stats_watch,
            };
            while let Some(path) = work_rx.recv().await {
                let ctx = request::RequestContext {
                    data_dir: data_dir.clone(),
                    request_path: path.clone(),
                    failed_dir: failed_dir.clone(),
                    to_archive_dir: to_archive_dir_clone.clone(),
                };
                request::process_request_async(&ctx, &handles).await;
                // Signal the router that this path is done (success or failure).
                let _ = done_tx.send(path).await;
            }
            tracing::debug!("Indexing worker exited");
        });
    }

    // Spawn the archive loop (blocking, spawn_blocking wrapper).
    {
        let data_dir = data_dir.clone();
        let to_archive_dir = to_archive_dir.clone();
        let shared = Arc::clone(&shared_archive_state);
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
                    let sh = Arc::clone(&shared);
                    let cfg_clone = cfg.clone();
                    let batch_result = tokio::task::spawn_blocking(move || {
                        archive_batch::run_archive_batch(&data, &to_archive, cfg_clone, &sh)
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
                    let dd = data_dir.clone();
                    tokio::task::spawn_blocking(move || {
                        crate::stats_cache::full_rebuild(&dd, &cache);
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

        let mut gz_files: Vec<(std::time::SystemTime, PathBuf)> = Vec::new();
        while let Ok(Some(entry)) = entries.next_entry().await {
            let path = entry.path();
            if path.extension() == Some(OsStr::new("gz")) {
                let mtime = entry.metadata().await
                    .ok()
                    .and_then(|m| m.modified().ok())
                    .unwrap_or(std::time::UNIX_EPOCH);
                gz_files.push((mtime, path));
            }
        }
        gz_files.sort_unstable_by_key(|(mtime, _)| *mtime);

        if inbox_paused.load(Ordering::Relaxed) {
            continue;
        }

        for (_, inbox_path) in gz_files {
            if in_flight.contains(&inbox_path) {
                continue;
            }
            match work_tx.try_send(inbox_path.clone()) {
                Ok(()) => {
                    in_flight.insert(inbox_path);
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
            content_hash: None,
            scanner_version: 0,
            is_new: false,
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
    fn filename_only_has_single_path_line() {
        let f = make_file("docs/report.pdf", FileKind::Pdf);
        let fallback = filename_only_file(&f);
        assert_eq!(fallback.lines.len(), 1);
        assert_eq!(fallback.lines[0].line_number, 0);
        assert_eq!(fallback.lines[0].content, "docs/report.pdf");
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
    fn outer_archive_stub_has_single_path_line() {
        let f = make_file("backups/big.tar.gz", FileKind::Archive);
        let stub = outer_archive_stub(&f);
        assert_eq!(stub.lines.len(), 1);
        assert_eq!(stub.lines[0].line_number, 0);
        assert_eq!(stub.lines[0].content, "backups/big.tar.gz");
    }
}
