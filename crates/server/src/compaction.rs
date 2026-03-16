//! Archive compaction: identify and remove orphaned chunks from ZIP archives.
//!
//! A chunk is "orphaned" when its `(chunk_archive, chunk_name)` pair no longer
//! appears in any `lines` row in any source database — because the file was
//! deleted or re-indexed since it was written.
//!
//! The scan is cheap: `ZipArchive::new()` reads only the Central Directory
//! (a compact index at the end of the file), and `by_index_raw(i).compressed_size()`
//! returns the cached size without decompressing any content.

use std::collections::HashSet;
use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::Result;
use zip::ZipArchive;

use find_common::api::CompactResponse;
use find_common::config::CompactionConfig;

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
            archives_rewritten += 1;

            if dry_run { continue; }

            let lock = shared.rewrite_lock_for(&path);
            let _guard = lock.lock().unwrap();

            if let Err(e) = rewrite_without(&path, &orphaned) {
                tracing::error!("compaction: failed to rewrite {}: {e:#}", path.display());
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

// ── Background scanner / scheduler ───────────────────────────────────────────

/// Parse an "HH:MM" string into (hours, minutes). Returns `None` on bad input.
fn parse_hhmm(s: &str) -> Option<(u32, u32)> {
    let (h, m) = s.split_once(':')?;
    let h: u32 = h.trim().parse().ok()?;
    let m: u32 = m.trim().parse().ok()?;
    if h > 23 || m > 59 { return None; }
    Some((h, m))
}

/// Compute the duration until the next occurrence of `(hour, minute)` in local
/// time, using `chrono`. Returns at least 1 second (never zero/negative).
fn duration_until_next(hour: u32, minute: u32) -> std::time::Duration {
    use chrono::{Local, Timelike};

    let now = Local::now();
    let today_target = now
        .with_hour(hour).unwrap()
        .with_minute(minute).unwrap()
        .with_second(0).unwrap()
        .with_nanosecond(0).unwrap();

    let target = if today_target > now {
        today_target
    } else {
        today_target + chrono::Duration::days(1)
    };

    let secs = (target - now).num_seconds().max(1) as u64;
    std::time::Duration::from_secs(secs)
}

/// Spawn the background compaction scheduler.
///
/// Behaviour:
/// - Runs a wasted-space scan at startup (after a 30 s settle delay) and logs
///   the result with elapsed timing.
/// - Daily at `cfg.start_time` (local time, HH:MM): runs the scan, then
///   compacts if orphaned bytes ≥ `cfg.threshold_pct` percent of total.
///   If `start_time` cannot be parsed, falls back to 02:00.
pub fn start_compaction_scanner(
    data_dir: PathBuf,
    stats_slot: Arc<std::sync::RwLock<Option<CompactionStats>>>,
    shared: Arc<SharedArchiveState>,
    cfg: CompactionConfig,
) {
    let (hour, minute) = parse_hhmm(&cfg.start_time).unwrap_or_else(|| {
        tracing::warn!(
            "compaction: invalid start_time {:?} — falling back to 02:00",
            cfg.start_time
        );
        (2, 0)
    });

    tokio::spawn(async move {
        // Initial startup scan: let the server settle first.
        tokio::time::sleep(std::time::Duration::from_secs(30)).await;
        run_scan_and_log(&data_dir, &stats_slot).await;

        // Daily loop: wait until the configured time, scan, then compact if needed.
        loop {
            let wait = duration_until_next(hour, minute);
            tracing::debug!(
                "compaction: next run in {:.0}h {:.0}m",
                wait.as_secs() / 3600,
                (wait.as_secs() % 3600) / 60,
            );
            tokio::time::sleep(wait).await;

            let stats = run_scan_and_log(&data_dir, &stats_slot).await;

            if let Some(stats) = stats {
                let pct = if stats.total_bytes > 0 {
                    stats.orphaned_bytes as f64 / stats.total_bytes as f64 * 100.0
                } else {
                    0.0
                };

                if pct >= cfg.threshold_pct {
                    tracing::info!(
                        "compaction: {:.1}% orphaned ≥ threshold {:.1}% — starting compaction",
                        pct, cfg.threshold_pct,
                    );
                    let data = data_dir.clone();
                    let sh = Arc::clone(&shared);
                    let t0 = std::time::Instant::now();
                    let result = tokio::task::spawn_blocking(move || {
                        compact_archives(&data, &sh, false)
                    }).await;
                    match result {
                        Ok(Ok(resp)) => tracing::info!(
                            "compaction: done in {:.1}s — {} archives rewritten, {} chunks removed, {} bytes freed",
                            t0.elapsed().as_secs_f64(),
                            resp.archives_rewritten,
                            resp.chunks_removed,
                            resp.bytes_freed,
                        ),
                        Ok(Err(e)) => tracing::error!("compaction: failed: {e:#}"),
                        Err(e)     => tracing::error!("compaction: task panicked: {e}"),
                    }
                } else {
                    tracing::info!(
                        "compaction: {:.1}% orphaned < threshold {:.1}% — skipping",
                        pct, cfg.threshold_pct,
                    );
                }
            }
        }
    });
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use find_common::config::CompactionConfig;

    // ── parse_hhmm ────────────────────────────────────────────────────────────

    #[test]
    fn parse_hhmm_valid() {
        assert_eq!(parse_hhmm("02:00"), Some((2, 0)));
        assert_eq!(parse_hhmm("00:00"), Some((0, 0)));
        assert_eq!(parse_hhmm("23:59"), Some((23, 59)));
        assert_eq!(parse_hhmm("12:30"), Some((12, 30)));
        // leading/trailing whitespace around components is accepted
        assert_eq!(parse_hhmm(" 3 : 5 "), Some((3, 5)));
    }

    #[test]
    fn parse_hhmm_invalid() {
        assert_eq!(parse_hhmm(""),        None); // empty
        assert_eq!(parse_hhmm("0200"),    None); // no colon
        assert_eq!(parse_hhmm("24:00"),   None); // hour out of range
        assert_eq!(parse_hhmm("23:60"),   None); // minute out of range
        assert_eq!(parse_hhmm("abc:00"),  None); // non-numeric hour
        assert_eq!(parse_hhmm("00:xyz"),  None); // non-numeric minute
        assert_eq!(parse_hhmm(":30"),     None); // missing hour
        assert_eq!(parse_hhmm("10:"),     None); // missing minute
    }

    // ── CompactionConfig defaults ─────────────────────────────────────────────

    #[test]
    fn compaction_config_defaults() {
        let cfg = CompactionConfig::default();
        assert_eq!(cfg.threshold_pct, 10.0);
        assert_eq!(cfg.start_time, "02:00");
    }

    #[test]
    fn compaction_config_parses_from_toml() {
        let toml = r#"
            threshold_pct = 25.0
            start_time = "03:30"
        "#;
        let cfg: CompactionConfig = toml::from_str(toml).unwrap();
        assert_eq!(cfg.threshold_pct, 25.0);
        assert_eq!(cfg.start_time, "03:30");
    }

    #[test]
    fn compaction_config_partial_toml_uses_defaults() {
        // Only override one field — the other should come from serde defaults.
        let toml = r#"threshold_pct = 5.0"#;
        let cfg: CompactionConfig = toml::from_str(toml).unwrap();
        assert_eq!(cfg.threshold_pct, 5.0);
        assert_eq!(cfg.start_time, "02:00"); // default
    }

    // ── duration_until_next ───────────────────────────────────────────────────

    #[test]
    fn duration_until_next_is_positive_and_at_most_one_day() {
        // For any valid (hour, minute) the result must be in (0, 24h].
        for &(h, m) in &[(0u32, 0u32), (2, 0), (12, 30), (23, 59)] {
            let d = duration_until_next(h, m);
            assert!(d.as_secs() >= 1,        "duration should be ≥ 1 s for {h}:{m:02}");
            assert!(d.as_secs() <= 24 * 3600, "duration should be ≤ 24 h for {h}:{m:02}");
        }
    }

    // ── scan_wasted_space ─────────────────────────────────────────────────────

    fn write_test_zip(path: &std::path::Path, entries: &[(&str, &[u8])]) {
        use std::io::Write;
        use zip::ZipWriter;
        use zip::write::SimpleFileOptions;

        let file = std::fs::File::create(path).unwrap();
        let mut zip = ZipWriter::new(file);
        let options = SimpleFileOptions::default();
        for (name, data) in entries {
            zip.start_file(*name, options).unwrap();
            zip.write_all(data).unwrap();
        }
        zip.finish().unwrap();
    }

    fn seed_db_with_chunk_ref(
        conn: &rusqlite::Connection,
        chunk_archive: &str,
        chunk_name: &str,
    ) {
        conn.execute(
            "INSERT INTO files (path, mtime, size, kind, indexed_at) \
             VALUES ('test.txt', 1000, 100, 'text', 0)",
            [],
        )
        .unwrap();
        let file_id: i64 = conn.last_insert_rowid();
        conn.execute(
            "INSERT INTO lines (file_id, line_number, chunk_archive, chunk_name, \
             line_offset_in_chunk) VALUES (?1, 1, ?2, ?3, 0)",
            rusqlite::params![file_id, chunk_archive, chunk_name],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO lines_fts(rowid, content) VALUES (last_insert_rowid(), 'hello')",
            [],
        )
        .unwrap();
    }

    #[test]
    fn all_chunks_referenced_no_orphans() {
        let tmp = tempfile::TempDir::new().unwrap();
        let data_dir = tmp.path();
        std::fs::create_dir_all(data_dir.join("sources/content/0000")).unwrap();

        // Create a ZIP archive with one chunk entry.
        let zip_path = data_dir.join("sources/content/0000/content_00001.zip");
        write_test_zip(&zip_path, &[("test.txt.chunk0.txt", b"hello world")]);

        // Open source DB and insert a lines row referencing that chunk.
        let db_path = data_dir.join("sources/test_source.db");
        let conn = crate::db::open(&db_path).unwrap();
        seed_db_with_chunk_ref(&conn, "content_00001.zip", "test.txt.chunk0.txt");

        let stats = scan_wasted_space(data_dir).unwrap();
        assert!(stats.total_bytes > 0, "expected total_bytes > 0");
        assert_eq!(stats.orphaned_bytes, 0, "expected no orphaned bytes");
    }

    #[test]
    fn unreferenced_chunks_counted_as_orphaned() {
        let tmp = tempfile::TempDir::new().unwrap();
        let data_dir = tmp.path();
        std::fs::create_dir_all(data_dir.join("sources/content/0000")).unwrap();

        // Create a ZIP archive with one chunk entry.
        let zip_path = data_dir.join("sources/content/0000/content_00001.zip");
        write_test_zip(&zip_path, &[("test.txt.chunk0.txt", b"hello world")]);

        // No DB entries referencing those chunks — everything should be orphaned.

        let stats = scan_wasted_space(data_dir).unwrap();
        assert!(stats.total_bytes > 0, "expected total_bytes > 0");
        assert_eq!(
            stats.orphaned_bytes, stats.total_bytes,
            "all bytes should be orphaned when no DB references exist"
        );
    }

    #[test]
    fn empty_content_dir_returns_zero() {
        let tmp = tempfile::TempDir::new().unwrap();
        let data_dir = tmp.path();
        std::fs::create_dir_all(data_dir.join("sources/content")).unwrap();

        let stats = scan_wasted_space(data_dir).unwrap();
        assert_eq!(stats.total_bytes, 0, "expected total_bytes == 0 for empty content dir");
        assert_eq!(stats.orphaned_bytes, 0, "expected orphaned_bytes == 0 for empty content dir");
    }
}

/// Run a wasted-space scan, log the result with timing, update `stats_slot`,
/// persist to disk, and return the stats.
async fn run_scan_and_log(
    data_dir: &Path,
    stats_slot: &Arc<std::sync::RwLock<Option<CompactionStats>>>,
) -> Option<CompactionStats> {
    let data_dir2 = data_dir.to_path_buf();
    let t0 = std::time::Instant::now();
    let result = tokio::task::spawn_blocking(move || scan_wasted_space(&data_dir2)).await;
    let elapsed = t0.elapsed();
    match result {
        Ok(Ok(stats)) => {
            let pct = if stats.total_bytes > 0 {
                stats.orphaned_bytes as f64 / stats.total_bytes as f64 * 100.0
            } else {
                0.0
            };
            tracing::info!(
                "compaction scan: {}/{} bytes orphaned ({:.1}%) in {:.1}s",
                stats.orphaned_bytes, stats.total_bytes, pct,
                elapsed.as_secs_f64(),
            );
            if let Ok(mut slot) = stats_slot.write() {
                *slot = Some(stats);
            }
            let _ = save_stats(data_dir, &stats);
            Some(stats)
        }
        Ok(Err(e)) => { tracing::warn!("compaction scan failed: {e:#}"); None }
        Err(e)     => { tracing::warn!("compaction scan task panicked: {e}"); None }
    }
}
