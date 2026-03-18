use std::collections::HashMap;

use anyhow::{Context, Result};
use rusqlite::{Connection, params};

use find_common::api::{ExtStat, FileKind, IndexingError, IndexingFailure, KindStats, ScanHistoryPoint};

// ── Stats ─────────────────────────────────────────────────────────────────────

/// Returns (total_files, total_size, by_kind) aggregated from the files table.
pub fn get_stats(conn: &Connection) -> Result<(usize, i64, HashMap<FileKind, KindStats>)> {
    let mut stmt = conn.prepare(
        "SELECT kind, COUNT(*), COALESCE(SUM(size), 0), AVG(CAST(extract_ms AS REAL))
         FROM files GROUP BY kind",
    )?;

    let rows: Vec<(String, i64, i64, Option<f64>)> = stmt
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, Option<f64>>(3)?,
            ))
        })?
        .collect::<rusqlite::Result<_>>()?;

    let mut total_files = 0usize;
    let mut total_size = 0i64;
    let mut by_kind: HashMap<FileKind, KindStats> = HashMap::new();

    for (kind_str, count, size, avg_ms) in rows {
        total_files += count as usize;
        total_size += size;
        by_kind.insert(FileKind::from(kind_str.as_str()), KindStats { count: count as usize, size, avg_extract_ms: avg_ms });
    }

    Ok((total_files, total_size, by_kind))
}

/// Returns file counts by extension for outer files (no archive members),
/// sorted by count descending, limited to 100 rows.
///
/// Uses the `file_basename` and `file_ext` custom scalar functions registered
/// in [`super::register_scalar_functions`].  Files without an extension are omitted.
pub fn get_stats_by_ext(conn: &Connection) -> Result<Vec<ExtStat>> {
    let mut stmt = conn.prepare(
        "SELECT
             file_ext(file_basename(path)) AS ext,
             COUNT(*)                      AS cnt,
             COALESCE(SUM(size), 0)        AS total_size
         FROM files
         WHERE path NOT LIKE '%::%'
           AND file_ext(file_basename(path)) != ''
         GROUP BY ext
         ORDER BY cnt DESC
         LIMIT 100",
    )?;

    let rows = stmt.query_map([], |row| {
        Ok(ExtStat {
            ext:   row.get::<_, String>(0)?,
            count: row.get::<_, i64>(1)? as usize,
            size:  row.get::<_, i64>(2)?,
        })
    })?
    .collect::<rusqlite::Result<_>>()?;

    Ok(rows)
}

/// Snapshot the current totals into the scan_history table.
pub fn append_scan_history(conn: &Connection, scanned_at: i64) -> Result<()> {
    let (total_files, total_size, by_kind) = get_stats(conn)?;
    let by_kind_json = serde_json::to_string(&by_kind).context("serialising by_kind")?;
    conn.execute(
        "INSERT INTO scan_history (scanned_at, total_files, total_size, by_kind)
         VALUES (?1, ?2, ?3, ?4)",
        params![scanned_at, total_files as i64, total_size, by_kind_json],
    )?;
    Ok(())
}

/// Count files whose content has not yet been written to a ZIP archive.
///
/// A file is "pending content" when it has a `content_hash` (i.e. content was
/// extracted) but neither an inline `file_content` row nor any `content_chunks`
/// row for its block exists yet.  This is the DB-level view of archive backlog,
/// independent of how many `.gz` files remain in the `to-archive/` queue.
pub fn get_files_pending_content(conn: &Connection) -> Result<usize> {
    let n: i64 = conn.query_row(
        "SELECT COUNT(*) FROM files f
         WHERE f.content_hash IS NOT NULL
           AND NOT EXISTS (SELECT 1 FROM file_content fc WHERE fc.file_id = f.id)
           AND NOT EXISTS (
               SELECT 1 FROM content_blocks cb
               JOIN content_chunks cc ON cc.block_id = cb.id
               WHERE cb.content_hash = f.content_hash
           )",
        [],
        |r| r.get(0),
    )?;
    Ok(n as usize)
}

/// Return up to `limit` scan history points, oldest first.
pub fn get_scan_history(conn: &Connection, limit: usize) -> Result<Vec<ScanHistoryPoint>> {
    let mut stmt = conn.prepare(
        "SELECT scanned_at, total_files, total_size
         FROM scan_history ORDER BY scanned_at ASC LIMIT ?1",
    )?;
    let rows = stmt
        .query_map(params![limit as i64], |row| {
            Ok(ScanHistoryPoint {
                scanned_at:  row.get(0)?,
                total_files: row.get::<_, i64>(1)? as usize,
                total_size:  row.get(2)?,
            })
        })?
        .collect::<rusqlite::Result<_>>()?;
    Ok(rows)
}

// ── Indexing errors ───────────────────────────────────────────────────────────

/// Insert or update indexing errors. On conflict (same path), updates the error
/// message, `last_seen`, and increments `count`.
pub fn upsert_indexing_errors(
    conn: &Connection,
    failures: &[IndexingFailure],
    now: i64,
) -> Result<()> {
    if failures.is_empty() {
        return Ok(());
    }
    let tx = conn.unchecked_transaction()?;
    {
        let mut stmt = tx.prepare_cached(
            "INSERT INTO indexing_errors (path, error, first_seen, last_seen, count)
             VALUES (?1, ?2, ?3, ?3, 1)
             ON CONFLICT(path) DO UPDATE SET
               error     = excluded.error,
               last_seen = excluded.last_seen,
               count     = count + 1",
        )?;
        for f in failures {
            stmt.execute(params![f.path, f.error, now])?;
        }
    }
    tx.commit()?;
    Ok(())
}

/// Delete all error rows for the given paths.
pub fn clear_errors_for_paths(conn: &Connection, paths: &[String]) -> Result<()> {
    if paths.is_empty() {
        return Ok(());
    }
    // SQLite doesn't support parameterised IN lists easily; use one DELETE per path.
    let tx = conn.unchecked_transaction()?;
    {
        let mut stmt =
            tx.prepare_cached("DELETE FROM indexing_errors WHERE path = ?1")?;
        for p in paths {
            stmt.execute(params![p])?;
        }
    }
    tx.commit()?;
    Ok(())
}

/// Consolidate all post-write cleanup into a **single** transaction to avoid
/// the WAL POSIX lock deadlock on WSL / network mounts.
///
/// On these mount types issuing a second `BEGIN` on the same connection after a
/// previous write transaction commits can hang indefinitely because the POSIX
/// advisory lock left by the first commit is not reliably visible to the same
/// file descriptor.  Merging all cleanup into one transaction means at most one
/// extra write per request on the connection used for file/delete writes.
///
/// - `clear_paths`: indexing-error rows to delete (successfully indexed files).
///   Deleted-path errors are handled inside `delete_files` and must not be
///   passed here to avoid duplicating the DELETE.
/// - `indexing_failures`: client- and server-side failures to record.
/// - `scan_timestamp`: if set, update `last_scan` and snapshot scan history.
pub fn do_cleanup_writes(
    conn: &Connection,
    clear_paths: &[String],
    indexing_failures: &[IndexingFailure],
    now: i64,
    scan_timestamp: Option<i64>,
) -> Result<()> {
    let has_work = !clear_paths.is_empty()
        || !indexing_failures.is_empty()
        || scan_timestamp.is_some();
    if !has_work {
        return Ok(());
    }

    let tx = conn.unchecked_transaction()?;

    if !clear_paths.is_empty() {
        let mut stmt = tx.prepare_cached("DELETE FROM indexing_errors WHERE path = ?1")?;
        for path in clear_paths {
            stmt.execute(params![path])?;
        }
    }

    if !indexing_failures.is_empty() {
        let mut stmt = tx.prepare_cached(
            "INSERT INTO indexing_errors (path, error, first_seen, last_seen, count)
             VALUES (?1, ?2, ?3, ?3, 1)
             ON CONFLICT(path) DO UPDATE SET
               error     = excluded.error,
               last_seen = excluded.last_seen,
               count     = count + 1",
        )?;
        for f in indexing_failures {
            stmt.execute(params![f.path, f.error, now])?;
        }
    }

    if let Some(ts) = scan_timestamp {
        tx.execute(
            "INSERT INTO meta (key, value) VALUES ('last_scan', ?1)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![ts.to_string()],
        )?;

        // Snapshot current stats into scan_history within the same transaction.
        // Reading inside an open write transaction is valid in WAL mode.
        let (total_files, total_size, by_kind) = get_stats(&tx)?;
        let by_kind_json = serde_json::to_string(&by_kind).context("serialising by_kind")?;
        tx.execute(
            "INSERT INTO scan_history (scanned_at, total_files, total_size, by_kind)
             VALUES (?1, ?2, ?3, ?4)",
            params![ts, total_files as i64, total_size, by_kind_json],
        )?;
    }

    tx.commit()?;
    Ok(())
}

/// Return a page of indexing errors ordered by `last_seen` descending.
pub fn get_indexing_errors(
    conn: &Connection,
    limit: usize,
    offset: usize,
) -> Result<Vec<IndexingError>> {
    let mut stmt = conn.prepare(
        "SELECT path, error, first_seen, last_seen, count
         FROM indexing_errors
         ORDER BY last_seen DESC
         LIMIT ?1 OFFSET ?2",
    )?;
    let rows = stmt
        .query_map(params![limit as i64, offset as i64], |row| {
            Ok(IndexingError {
                path:       row.get(0)?,
                error:      row.get(1)?,
                first_seen: row.get(2)?,
                last_seen:  row.get(3)?,
                count:      row.get(4)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

/// Return the total number of rows in `indexing_errors`.
pub fn get_indexing_error_count(conn: &Connection) -> Result<usize> {
    let count: i64 =
        conn.query_row("SELECT COUNT(*) FROM indexing_errors", [], |r| r.get(0))?;
    Ok(count as usize)
}

/// Return the total number of rows in the FTS5 index.
/// Includes stale entries from re-indexed files; useful for diagnosing
/// whether the index is being populated at all.
pub fn get_fts_row_count(conn: &Connection) -> Result<i64> {
    conn.query_row("SELECT COUNT(*) FROM lines_fts", [], |r| r.get(0))
        .map_err(Into::into)
}

/// Return the error message for a single path, if one exists.
pub fn get_indexing_error(conn: &Connection, path: &str) -> Result<Option<String>> {
    let result = conn.query_row(
        "SELECT error FROM indexing_errors WHERE path = ?1",
        params![path],
        |row| row.get(0),
    );
    match result {
        Ok(s) => Ok(Some(s)),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(e) => Err(e.into()),
    }
}
