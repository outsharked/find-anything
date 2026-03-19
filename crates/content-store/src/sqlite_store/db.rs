//! blobs.db schema and SQL helpers for SqliteContentStore.
//!
//! The database lives at `data_dir/blobs.db` and is owned entirely by
//! `SqliteContentStore`.  No other crate reads or writes it.

use std::path::Path;

use anyhow::{Context, Result};
use rusqlite::Connection;

pub const SCHEMA_SQL: &str = "
PRAGMA journal_mode = WAL;
PRAGMA synchronous = NORMAL;

-- One row per chunk per blob. Data is stored uncompressed.
-- Chunk positions are 0-based line indices into the original blob
-- (position 0 = first line, i.e. the file path itself).
CREATE TABLE IF NOT EXISTS blobs (
    key        TEXT    NOT NULL,   -- blake3 hex hash
    chunk_num  INTEGER NOT NULL,   -- 0-based chunk index
    start_line INTEGER NOT NULL,   -- first line position in this chunk
    end_line   INTEGER NOT NULL,   -- last line position in this chunk (inclusive)
    data       BLOB    NOT NULL,   -- raw chunk bytes: plain UTF-8 or gzip-compressed UTF-8
    PRIMARY KEY (key, chunk_num)
);

CREATE INDEX IF NOT EXISTS idx_blobs_key_start ON blobs(key, start_line);
";

/// Open `blobs.db` read-only with a 1 s busy timeout.
pub fn open_read_only(data_dir: &Path) -> Result<Connection> {
    let path = data_dir.join("blobs.db");
    let conn = Connection::open_with_flags(
        &path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .with_context(|| format!("opening {} (read-only)", path.display()))?;
    conn.busy_timeout(std::time::Duration::from_secs(1))?;
    Ok(conn)
}

/// Open (or create) `blobs.db` with WAL mode.
pub fn open_write(data_dir: &Path) -> Result<Connection> {
    let path = data_dir.join("blobs.db");
    let conn = Connection::open(&path)
        .with_context(|| format!("opening {}", path.display()))?;
    conn.busy_timeout(std::time::Duration::from_secs(30))?;
    conn.execute_batch(SCHEMA_SQL).context("applying blobs.db schema")?;
    Ok(conn)
}

/// Check whether any chunk exists for `key`.
pub fn blob_exists(conn: &Connection, key: &str) -> Result<bool> {
    let n: i64 = conn.query_row(
        "SELECT COUNT(*) FROM blobs WHERE key = ?1 LIMIT 1",
        rusqlite::params![key],
        |r| r.get(0),
    )?;
    Ok(n > 0)
}

/// Insert a single chunk row. Ignores conflicts (idempotent).
/// `data` is the raw bytes to store — either plain UTF-8 or gzip-compressed.
pub fn insert_chunk(
    tx: &rusqlite::Transaction,
    key: &str,
    chunk_num: usize,
    start_line: usize,
    end_line: usize,
    data: &[u8],
) -> Result<()> {
    tx.execute(
        "INSERT OR IGNORE INTO blobs(key, chunk_num, start_line, end_line, data)
         VALUES(?1, ?2, ?3, ?4, ?5)",
        rusqlite::params![key, chunk_num as i64, start_line as i64, end_line as i64, data],
    )?;
    Ok(())
}

/// Delete all chunks for `key`.
pub fn delete_blob(conn: &Connection, key: &str) -> Result<()> {
    conn.execute("DELETE FROM blobs WHERE key = ?1", rusqlite::params![key])?;
    Ok(())
}

/// A chunk row returned by a range query.
pub struct ChunkRow {
    pub start_line: i64,
    pub data: Vec<u8>,
}

/// Return all chunks for `key` whose line range overlaps `[lo, hi]`.
pub fn query_chunks_for_range(
    conn: &Connection,
    key: &str,
    lo: usize,
    hi: usize,
) -> Result<Vec<ChunkRow>> {
    let mut stmt = conn.prepare_cached(
        "SELECT start_line, data
         FROM blobs
         WHERE key = ?1 AND start_line <= ?2 AND end_line >= ?3
         ORDER BY chunk_num",
    )?;
    let rows = stmt
        .query_map(
            rusqlite::params![key, hi as i64, lo as i64],
            |row| Ok(ChunkRow { start_line: row.get(0)?, data: row.get(1)? }),
        )?
        .collect::<rusqlite::Result<_>>()?;
    Ok(rows)
}

/// Delete all blobs not in `live_keys`. Returns the number of rows deleted.
/// Uses a temp table to handle large key sets efficiently.
pub fn delete_orphan_blobs(conn: &Connection, live_keys: &[&str]) -> Result<usize> {
    conn.execute_batch("CREATE TEMP TABLE IF NOT EXISTS _live_keys (key TEXT PRIMARY KEY)")?;
    conn.execute_batch("DELETE FROM _live_keys")?;

    {
        let mut stmt = conn.prepare("INSERT OR IGNORE INTO _live_keys(key) VALUES(?1)")?;
        for key in live_keys {
            stmt.execute(rusqlite::params![key])?;
        }
    }

    let deleted = conn.execute(
        "DELETE FROM blobs WHERE key NOT IN (SELECT key FROM _live_keys)",
        [],
    )?;

    conn.execute_batch("DROP TABLE IF EXISTS _live_keys")?;
    Ok(deleted)
}

/// Return the total number of rows in the blobs table.
pub fn row_count(conn: &Connection) -> Result<u64> {
    let n: i64 = conn.query_row("SELECT COUNT(*) FROM blobs", [], |r| r.get(0))?;
    Ok(n as u64)
}

/// Return the total bytes of data stored for blobs whose key is not in `live_keys`.
/// Used for dry-run compaction to report orphaned bytes without deleting anything.
pub fn orphaned_data_bytes(conn: &Connection, live_keys: &[&str]) -> Result<u64> {
    conn.execute_batch("CREATE TEMP TABLE IF NOT EXISTS _live_keys2 (key TEXT PRIMARY KEY)")?;
    conn.execute_batch("DELETE FROM _live_keys2")?;
    {
        let mut stmt = conn.prepare("INSERT OR IGNORE INTO _live_keys2(key) VALUES(?1)")?;
        for key in live_keys {
            stmt.execute(rusqlite::params![key])?;
        }
    }
    let bytes: i64 = conn.query_row(
        "SELECT COALESCE(SUM(LENGTH(data)), 0) FROM blobs WHERE key NOT IN (SELECT key FROM _live_keys2)",
        [],
        |r| r.get(0),
    )?;
    conn.execute_batch("DROP TABLE IF EXISTS _live_keys2")?;
    Ok(bytes as u64)
}

/// Return the on-disk size of `blobs.db` in bytes.
pub fn db_size_bytes(data_dir: &Path) -> u64 {
    std::fs::metadata(data_dir.join("blobs.db"))
        .map(|m| m.len())
        .unwrap_or(0)
}
