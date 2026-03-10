//! Archive compaction: identify and remove orphaned chunks from ZIP archives.
//!
//! A chunk is "orphaned" when its `(chunk_archive, chunk_name)` pair no longer
//! appears in any `lines` row in any source database — for example, after the
//! server was killed between the DB commit and the subsequent ZIP rewrite.
//!
//! The scan is cheap: `ZipArchive::new()` reads only the Central Directory
//! (a compact index at the end of the file), and `by_index_raw(i).compressed_size()`
//! returns the cached size without decompressing any content.

use std::collections::HashSet;
use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::Result;
use zip::ZipArchive;

use find_common::api::CompactResponse;

use crate::archive::SharedArchiveState;
use crate::db;

// ── Persistent stats store ────────────────────────────────────────────────────

const SERVER_DB_NAME: &str = "server.db";
const KEY_ORPHANED_BYTES: &str = "compact_orphaned_bytes";
const KEY_TOTAL_BYTES:    &str = "compact_total_bytes";
const KEY_SCANNED_AT:     &str = "compact_scanned_at";

/// Cached compaction statistics.
#[derive(Debug, Clone, Copy)]
pub struct CompactionStats {
    /// Sum of compressed sizes of all orphaned chunks (bytes).
    pub orphaned_bytes: u64,
    /// Sum of compressed sizes of all chunks in all archives (bytes).
    pub total_bytes: u64,
    /// Unix timestamp when the scan was last completed.
    pub scanned_at: i64,
}

/// Open (or create) the server-wide metadata database.
fn open_server_db(data_dir: &Path) -> Result<rusqlite::Connection> {
    let path = data_dir.join(SERVER_DB_NAME);
    let conn = rusqlite::Connection::open(&path)?;
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);",
    )?;
    Ok(conn)
}

/// Load cached compaction statistics from the server meta database.
/// Returns `None` if no scan has been recorded yet.
pub fn load_cached_stats(data_dir: &Path) -> Option<CompactionStats> {
    let conn = open_server_db(data_dir).ok()?;
    let get = |key: &str| -> Option<i64> {
        conn.query_row(
            "SELECT value FROM meta WHERE key = ?1",
            rusqlite::params![key],
            |row| row.get::<_, String>(0),
        ).ok()?.parse().ok()
    };
    Some(CompactionStats {
        orphaned_bytes: get(KEY_ORPHANED_BYTES)? as u64,
        total_bytes:    get(KEY_TOTAL_BYTES)?    as u64,
        scanned_at:     get(KEY_SCANNED_AT)?,
    })
}

fn save_stats(data_dir: &Path, stats: &CompactionStats) -> Result<()> {
    let conn = open_server_db(data_dir)?;
    let upsert = |key: &str, val: i64| -> Result<()> {
        conn.execute(
            "INSERT INTO meta(key, value) VALUES(?1, ?2)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            rusqlite::params![key, val.to_string()],
        )?;
        Ok(())
    };
    upsert(KEY_ORPHANED_BYTES, stats.orphaned_bytes as i64)?;
    upsert(KEY_TOTAL_BYTES,    stats.total_bytes    as i64)?;
    upsert(KEY_SCANNED_AT,     stats.scanned_at)?;
    Ok(())
}

// ── Core scan ─────────────────────────────────────────────────────────────────

/// Build the set of every `(archive_name, chunk_name)` pair that is
/// referenced by at least one `lines` row across all source databases.
fn build_referenced_set(sources_dir: &Path) -> HashSet<(String, String)> {
    let mut set = HashSet::new();
    let rd = match std::fs::read_dir(sources_dir) {
        Ok(rd) => rd,
        Err(_) => return set,
    };
    for entry in rd.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("db") {
            continue;
        }
        let conn = match db::open(&path) {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!("compaction scan: skipping {}: {e:#}", path.display());
                continue;
            }
        };
        match db::collect_all_chunk_refs(&conn) {
            Ok(refs) => {
                for r in refs {
                    set.insert((r.archive_name, r.chunk_name));
                }
            }
            Err(e) => tracing::warn!("compaction scan: collect_all_chunk_refs failed for {}: {e:#}", path.display()),
        }
    }
    set
}

/// Scan all ZIP archives and compute orphaned vs total compressed bytes.
/// Does not modify any files.
pub fn scan_wasted_space(data_dir: &Path) -> Result<CompactionStats> {
    let sources_dir = data_dir.join("sources");
    let content_dir = sources_dir.join("content");

    let referenced = build_referenced_set(&sources_dir);

    let mut total_bytes:    u64 = 0;
    let mut orphaned_bytes: u64 = 0;

    for subdir_entry in std::fs::read_dir(&content_dir).into_iter().flatten().flatten() {
        let subdir = subdir_entry.path();
        if !subdir.is_dir() { continue; }
        for file_entry in std::fs::read_dir(&subdir).into_iter().flatten().flatten() {
            let path = file_entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("zip") { continue; }
            let archive_name = match path.file_name().and_then(|n| n.to_str()) {
                Some(n) => n.to_string(),
                None => continue,
            };
            let file = match File::open(&path) {
                Ok(f) => f,
                Err(_) => continue,
            };
            let mut zip = match ZipArchive::new(file) {
                Ok(z) => z,
                Err(_) => continue,
            };
            for i in 0..zip.len() {
                // by_index seeks to the local file header (tiny) and exposes
                // compressed_size() from the cached Central Directory — no
                // content is ever decompressed.
                if let Ok(entry) = zip.by_index_raw(i) {
                    let size = entry.compressed_size();
                    total_bytes += size;
                    if !referenced.contains(&(archive_name.clone(), entry.name().to_string())) {
                        orphaned_bytes += size;
                    }
                }
            }
        }
    }

    let scanned_at = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;

    Ok(CompactionStats { orphaned_bytes, total_bytes, scanned_at })
}

// ── Compaction ────────────────────────────────────────────────────────────────

/// Remove orphaned chunks from all archives.
///
/// Acquires the per-archive `rewrite_lock` before each rewrite, so compaction
/// is safe to run while the inbox worker pool is processing requests.
/// If `dry_run` is `true`, reports what would be freed without modifying files.
pub fn compact_archives(
    data_dir: &Path,
    shared: &Arc<SharedArchiveState>,
    dry_run: bool,
) -> Result<CompactResponse> {
    let sources_dir = data_dir.join("sources");
    let content_dir = sources_dir.join("content");

    let referenced = build_referenced_set(&sources_dir);

    let mut archives_scanned:  usize = 0;
    let mut archives_rewritten: usize = 0;
    let mut chunks_removed:    usize = 0;
    let mut bytes_freed:       u64   = 0;

    for subdir_entry in std::fs::read_dir(&content_dir).into_iter().flatten().flatten() {
        let subdir = subdir_entry.path();
        if !subdir.is_dir() { continue; }
        for file_entry in std::fs::read_dir(&subdir).into_iter().flatten().flatten() {
            let path = file_entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("zip") { continue; }
            let archive_name = match path.file_name().and_then(|n| n.to_str()) {
                Some(n) => n.to_string(),
                None => continue,
            };
            archives_scanned += 1;

            // Catalog scan: identify orphaned entries and their sizes.
            let mut orphaned: HashSet<String> = HashSet::new();
            let mut orphaned_size: u64 = 0;
            {
                let file = match File::open(&path) { Ok(f) => f, Err(_) => continue };
                let mut zip = match ZipArchive::new(file) { Ok(z) => z, Err(_) => continue };
                for i in 0..zip.len() {
                    if let Ok(entry) = zip.by_index_raw(i) {
                        let name = entry.name().to_string();
                        if !referenced.contains(&(archive_name.clone(), name.clone())) {
                            orphaned_size += entry.compressed_size();
                            orphaned.insert(name);
                        }
                    }
                }
            }

            if orphaned.is_empty() { continue; }

            chunks_removed += orphaned.len();
            bytes_freed    += orphaned_size;
            archives_rewritten += 1; // counts archives *affected* (would be rewritten or were rewritten)

            if dry_run { continue; }

            // Acquire the per-archive rewrite lock and rewrite.
            let lock = shared.rewrite_lock_for(&path);
            let _guard = lock.lock().unwrap();

            // Build the archive's chunk→size map for logging and rewriting.
            if let Err(e) = rewrite_without(&path, &orphaned) {
                tracing::error!("compaction: failed to rewrite {}: {e:#}", path.display());
                // Undo counts for this archive on error.
                archives_rewritten -= 1;
                chunks_removed -= orphaned.len();
                bytes_freed    -= orphaned_size;
            } else {
                tracing::info!(
                    "compaction: rewrote {} — removed {} orphaned chunks ({} bytes)",
                    archive_name, orphaned.len(), orphaned_size,
                );
            }
        }
    }

    Ok(CompactResponse { archives_scanned, archives_rewritten, chunks_removed, bytes_freed, dry_run })
}

/// Rewrite `archive_path` omitting the named entries.
fn rewrite_without(archive_path: &Path, to_remove: &HashSet<String>) -> Result<()> {
    use zip::write::{FullFileOptions, SimpleFileOptions};
    use zip::{CompressionMethod, ZipWriter};

    let temp_path = archive_path.with_extension("zip.tmp");
    {
        let src_file = File::open(archive_path)?;
        let mut old_zip = ZipArchive::new(src_file)?;
        let tmp_file = File::create(&temp_path)?;
        let mut new_zip = ZipWriter::new(tmp_file);
        let base_opts: FullFileOptions<'_> = SimpleFileOptions::default()
            .compression_method(CompressionMethod::Deflated)
            .compression_level(Some(6))
            .into_full_options();
        for i in 0..old_zip.len() {
            let mut entry = old_zip.by_index(i)?;
            if !to_remove.contains(entry.name()) {
                let comment = entry.comment().to_string();
                let entry_opts = base_opts.clone().with_file_comment(comment.as_str());
                new_zip.start_file(entry.name(), entry_opts)?;
                std::io::copy(&mut entry, &mut new_zip)?;
            }
        }
        new_zip.finish()?;
    }
    std::fs::rename(&temp_path, archive_path)?;
    Ok(())
}

// ── Background scanner ────────────────────────────────────────────────────────

/// Spawn a background task that computes wasted-space statistics and caches
/// the results in `data_dir/server.db` and in `stats_slot`.
///
/// Scans run at two points:
/// - Once at startup (after a 30 s settle delay).
/// - 60 s after the last delete operation, provided no new delete has started
///   in that window.  Every time the worker calls `notify_one()` the 60 s
///   timer resets, so rapid back-to-back deletes coalesce into a single scan.
///
/// After each scan `deleted_since_scan` is reset to zero.  Between scans,
/// callers can estimate current wasted bytes as
/// `last_scan.orphaned_bytes + deleted_since_scan`.
pub fn start_compaction_scanner(
    data_dir: PathBuf,
    stats_slot: Arc<std::sync::RwLock<Option<CompactionStats>>>,
    deleted_since_scan: Arc<AtomicU64>,
    delete_notify: Arc<tokio::sync::Notify>,
) {
    tokio::spawn(async move {
        // Initial delay: let the server fully start before the first scan.
        tokio::time::sleep(std::time::Duration::from_secs(30)).await;
        run_and_save_scan(&data_dir, &stats_slot, &deleted_since_scan).await;

        // Event-driven: scan 60 s after the last delete, deferring on each
        // new delete that arrives within the window.
        loop {
            delete_notify.notified().await;

            while tokio::time::timeout(
                std::time::Duration::from_secs(60),
                delete_notify.notified(),
            ).await.is_ok() {
                // another delete arrived — reset the 60 s timer
            }
            // 60 s of silence — scan now

            run_and_save_scan(&data_dir, &stats_slot, &deleted_since_scan).await;
        }
    });
}

async fn run_and_save_scan(
    data_dir: &Path,
    stats_slot: &Arc<std::sync::RwLock<Option<CompactionStats>>>,
    deleted_since_scan: &Arc<AtomicU64>,
) {
    let data_dir2 = data_dir.to_path_buf();
    let result = tokio::task::spawn_blocking(move || scan_wasted_space(&data_dir2)).await;
    match result {
        Ok(Ok(stats)) => {
            tracing::info!(
                "compaction scan: {}/{} bytes orphaned ({:.1}%)",
                stats.orphaned_bytes, stats.total_bytes,
                if stats.total_bytes > 0 {
                    stats.orphaned_bytes as f64 / stats.total_bytes as f64 * 100.0
                } else { 0.0 },
            );
            deleted_since_scan.store(0, Ordering::Relaxed);
            if let Ok(mut slot) = stats_slot.write() {
                *slot = Some(stats);
            }
            let _ = save_stats(data_dir, &stats);
        }
        Ok(Err(e)) => tracing::warn!("compaction scan failed: {e:#}"),
        Err(e)     => tracing::warn!("compaction scan task panicked: {e}"),
    }
}

