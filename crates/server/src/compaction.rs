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
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::Result;

use find_common::api::CompactResponse;
use find_common::config::CompactionConfig;
use find_content_store::{ContentKey, ContentStore};

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

/// Collect all distinct `content_hash` values from every source DB.
/// These are the live keys that the content store must keep.
fn collect_live_keys(data_dir: &Path) -> HashSet<ContentKey> {
    let sources_dir = data_dir.join("sources");
    let mut keys = HashSet::new();
    let rd = match std::fs::read_dir(&sources_dir) {
        Ok(rd) => rd,
        Err(_) => return keys,
    };
    for entry in rd.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("db") {
            continue;
        }
        let conn = match db::open_for_stats(&path) {
            Ok(c) => c,
            Err(_) => continue,
        };
        let mut stmt = match conn.prepare(
            "SELECT DISTINCT content_hash FROM files WHERE content_hash IS NOT NULL",
        ) {
            Ok(s) => s,
            Err(_) => continue,
        };
        let _ = stmt
            .query_map([], |row| row.get::<_, String>(0))
            .map(|rows| rows.flatten().for_each(|h| { keys.insert(ContentKey::new(h)); }));
    }
    keys
}

/// Scan all ZIP archives and compute orphaned vs total compressed bytes.
/// Does not modify any files.
pub fn scan_wasted_space(
    data_dir: &Path,
    content_store: &dyn ContentStore,
) -> Result<CompactionStats> {
    let live_keys = collect_live_keys(data_dir);

    // Dry-run compact gives us orphaned bytes without touching any files.
    let result = content_store.compact(&live_keys, true /* dry_run */)?;
    let orphaned_bytes = result.bytes_freed;

    // Total bytes from the content store's incremental counter.
    let total_bytes = content_store.storage_stats().map(|(_, b)| b).unwrap_or(0);

    let scanned_at = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;

    Ok(CompactionStats { orphaned_bytes, total_bytes, scanned_at })
}

// ── Compaction ────────────────────────────────────────────────────────────────

/// Remove orphaned chunks from all archives via the content store.
///
/// Collects live keys from all source DBs, then delegates to
/// `content_store.compact()`.  If `dry_run` is `true`, reports what would be
/// freed without modifying any files.
pub fn compact_archives(
    data_dir: &Path,
    content_store: &Arc<dyn ContentStore>,
    dry_run: bool,
) -> Result<CompactResponse> {
    let live_keys = collect_live_keys(data_dir);
    let r = content_store.compact(&live_keys, dry_run)?;
    Ok(CompactResponse {
        units_scanned:   r.units_scanned,
        units_rewritten: r.units_rewritten,
        units_deleted:   r.units_deleted,
        chunks_removed:     r.chunks_removed,
        bytes_freed:        r.bytes_freed,
        dry_run,
    })
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
    content_store: Arc<dyn ContentStore>,
    cfg: CompactionConfig,
    source_stats_cache: Arc<std::sync::RwLock<crate::stats_cache::SourceStatsCache>>,
    stats_watch: Arc<tokio::sync::watch::Sender<u64>>,
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
        run_scan_and_log(&data_dir, &stats_slot, &content_store).await;

        // Daily loop: wait until the configured time, scan, then compact if needed.
        loop {
            let wait = duration_until_next(hour, minute);
            tracing::debug!(
                "compaction: next run in {:.0}h {:.0}m",
                wait.as_secs() / 3600,
                (wait.as_secs() % 3600) / 60,
            );
            tokio::time::sleep(wait).await;

            let stats = run_scan_and_log(&data_dir, &stats_slot, &content_store).await;

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
                    let cs = Arc::clone(&content_store);
                    let t0 = std::time::Instant::now();
                    let result = tokio::task::spawn_blocking(move || {
                        compact_archives(&data, &cs, false)
                    }).await;
                    match result {
                        Ok(Ok(resp)) => tracing::info!(
                            "compaction: done in {:.1}s — {} storage units rewritten, {} chunks removed, {} freed",
                            t0.elapsed().as_secs_f64(),
                            resp.units_rewritten,
                            resp.chunks_removed,
                            find_common::mem::fmt_bytes(resp.bytes_freed),
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

            // Daily stats cache rebuild after compaction completes.
            {
                let cache = Arc::clone(&source_stats_cache);
                let cs    = Arc::clone(&content_store);
                let dd    = data_dir.clone();
                tokio::task::spawn_blocking(move || {
                    crate::stats_cache::full_rebuild(&dd, &cache, &cs);
                }).await.ok();
                tracing::debug!("stats_cache: daily full rebuild complete");
                stats_watch.send_modify(|v| *v = v.wrapping_add(1));
            }
        }
    });
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use find_common::config::CompactionConfig;
    use find_content_store::ContentStore;

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

    // ── scan_wasted_space / compact_archives ─────────────────────────────────

    fn open_store(data_dir: &std::path::Path) -> std::sync::Arc<dyn ContentStore> {
        std::sync::Arc::new(find_content_store::SqliteContentStore::open(data_dir, None, None, None).unwrap())
    }

    fn seed_source_db(data_dir: &std::path::Path, source: &str, hash: &str) {
        let db_path = data_dir.join("sources").join(format!("{source}.db"));
        std::fs::create_dir_all(db_path.parent().unwrap()).unwrap();
        let conn = crate::db::open(&db_path).unwrap();
        conn.execute(
            "INSERT INTO files (path, mtime, size, kind, indexed_at, content_hash, line_count)
             VALUES ('test.txt', 1000, 100, 'text', 0, ?1, 1)",
            rusqlite::params![hash],
        ).unwrap();
    }

    #[test]
    fn all_chunks_referenced_no_orphans() {
        let tmp = tempfile::TempDir::new().unwrap();
        let data_dir = tmp.path();
        let cs = open_store(data_dir);

        // Put a blob and record its hash in a source DB.
        let hash = "aabbccddeeff00112233445566778899";
        cs.put(&find_content_store::ContentKey::new(hash), "hello world").unwrap();
        seed_source_db(data_dir, "src", hash);

        let stats = scan_wasted_space(data_dir, cs.as_ref()).unwrap();
        assert!(stats.total_bytes > 0, "expected total_bytes > 0");
        assert_eq!(stats.orphaned_bytes, 0, "live content should not be orphaned");
    }

    #[test]
    fn unreferenced_content_counted_as_orphaned() {
        let tmp = tempfile::TempDir::new().unwrap();
        let data_dir = tmp.path();
        let cs = open_store(data_dir);

        // Put a blob but DON'T add its hash to any source DB.
        let hash = "aabbccddeeff00112233445566778899";
        cs.put(&find_content_store::ContentKey::new(hash), "hello world").unwrap();

        // No source DB → no live keys → everything is orphaned.
        let stats = scan_wasted_space(data_dir, cs.as_ref()).unwrap();
        // orphaned_bytes counts compressed chunk bytes; total_bytes is the full
        // archive on-disk size (includes ZIP headers). orphaned_bytes > 0 is
        // sufficient to confirm the content is considered orphaned.
        assert!(stats.orphaned_bytes > 0, "unreferenced content should contribute orphaned bytes");
    }

    #[test]
    fn orphaned_content_is_removed_by_compact() {
        let tmp = tempfile::TempDir::new().unwrap();
        let data_dir = tmp.path();
        let cs = open_store(data_dir);

        let hash_live   = "aabbccddeeff00112233445566778899";
        let hash_orphan = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

        cs.put(&find_content_store::ContentKey::new(hash_live),   "live content").unwrap();
        cs.put(&find_content_store::ContentKey::new(hash_orphan), "orphan content").unwrap();

        // Only live hash recorded in a source DB.
        seed_source_db(data_dir, "src", hash_live);

        let resp = compact_archives(data_dir, &cs, false).unwrap();
        assert!(
            resp.chunks_removed > 0 || resp.units_deleted > 0 || resp.units_rewritten > 0,
            "expected at least some compaction work"
        );
        // Orphan should be gone.
        assert!(!cs.contains(&find_content_store::ContentKey::new(hash_orphan)).unwrap());
        // Live content should remain.
        assert!(cs.contains(&find_content_store::ContentKey::new(hash_live)).unwrap());
    }

    #[test]
    fn empty_content_dir_returns_zero() {
        let tmp = tempfile::TempDir::new().unwrap();
        let data_dir = tmp.path();
        let cs = open_store(data_dir);

        let stats = scan_wasted_space(data_dir, cs.as_ref()).unwrap();
        assert_eq!(stats.orphaned_bytes, 0, "expected orphaned_bytes == 0 for empty content dir");
    }
}

/// Write `stats` into `slot`, replacing any previously cached value,
/// and persist to `server.db` so the value survives a server restart.
pub fn save_stats_to_slot(
    slot: &Arc<std::sync::RwLock<Option<CompactionStats>>>,
    data_dir: &Path,
    stats: CompactionStats,
) {
    let _ = save_stats(data_dir, &stats);
    if let Ok(mut g) = slot.write() {
        *g = Some(stats);
    }
}

/// Run a wasted-space scan, log the result with timing, update `stats_slot`,
/// persist to disk, and return the stats.
async fn run_scan_and_log(
    data_dir: &Path,
    stats_slot: &Arc<std::sync::RwLock<Option<CompactionStats>>>,
    content_store: &Arc<dyn ContentStore>,
) -> Option<CompactionStats> {
    let data_dir2 = data_dir.to_path_buf();
    let cs = Arc::clone(content_store);
    let t0 = std::time::Instant::now();
    let result = tokio::task::spawn_blocking(move || scan_wasted_space(&data_dir2, cs.as_ref())).await;
    let elapsed = t0.elapsed();
    match result {
        Ok(Ok(stats)) => {
            let pct = if stats.total_bytes > 0 {
                stats.orphaned_bytes as f64 / stats.total_bytes as f64 * 100.0
            } else {
                0.0
            };
            tracing::info!(
                "compaction scan: {}/{} orphaned ({:.1}%) in {:.1}s",
                find_common::mem::fmt_bytes(stats.orphaned_bytes), find_common::mem::fmt_bytes(stats.total_bytes), pct,
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
