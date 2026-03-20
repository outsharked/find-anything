use std::collections::HashMap;

use anyhow::{Context, Result};
use rusqlite::{Connection, params};
use find_content_store::{ContentKey, ContentStore};

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

/// Count files whose content has not yet been written to the content store.
///
/// A file is "pending content" when it has a `content_hash` (i.e. content was
/// extracted) but neither an inline `file_content` row nor an entry in the
/// content store exists yet.  This is the DB-level view of archive backlog,
/// independent of how many `.gz` files remain in the `to-archive/` queue.
pub fn get_files_pending_content(conn: &Connection, content_store: &dyn ContentStore) -> Result<usize> {
    // Collect all distinct content_hash values that have no inline content.
    let hashes: Vec<String> = conn
        .prepare(
            "SELECT DISTINCT f.content_hash FROM files f
             WHERE f.content_hash IS NOT NULL
               AND NOT EXISTS (SELECT 1 FROM file_content fc WHERE fc.file_id = f.id)",
        )?
        .query_map([], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?;

    let mut pending = 0usize;
    for hash in hashes {
        let key = ContentKey::new(hash.as_str());
        if !content_store.contains(&key).unwrap_or(true) {
            pending += 1;
        }
    }
    Ok(pending)
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;
    use rusqlite::Connection;
    use find_content_store::{CompactResult, ContentKey};

    fn test_conn() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("PRAGMA foreign_keys = ON;").unwrap();
        conn.execute_batch(include_str!("../schema_v3.sql")).unwrap();
        crate::db::register_scalar_functions(&conn).unwrap();
        conn
    }

    /// A ContentStore stub that always reports no key is present.
    struct EmptyStore;

    impl find_content_store::ContentStore for EmptyStore {
        fn put(&self, _: &ContentKey, _: &str) -> anyhow::Result<bool> { Ok(false) }
        fn delete(&self, _: &ContentKey) -> anyhow::Result<()> { Ok(()) }
        fn get_lines(&self, _: &ContentKey, _: usize, _: usize) -> anyhow::Result<Option<Vec<(usize, String)>>> { Ok(None) }
        fn contains(&self, _: &ContentKey) -> anyhow::Result<bool> { Ok(false) }
        fn compact(&self, _: &HashSet<ContentKey>, _: bool) -> anyhow::Result<CompactResult> {
            Ok(CompactResult { units_scanned: 0, units_rewritten: 0, units_deleted: 0, chunks_removed: 0, bytes_freed: 0 })
        }
        fn storage_stats(&self) -> Option<(u64, u64)> { Some((0, 0)) }
    }

    #[test]
    fn test_get_files_pending_content_counts_unarchived() {
        let conn = test_conn();

        // Insert a file with a content_hash but no inline file_content row.
        conn.execute(
            "INSERT INTO files (path, mtime, kind, content_hash) VALUES ('file.txt', 1000, 'text', 'abc123')",
            [],
        ).unwrap();

        let store = EmptyStore;
        let pending = get_files_pending_content(&conn, &store).unwrap();
        assert_eq!(pending, 1, "file with content_hash but no stored content should be pending");

        // Now insert an inline file_content row — the file is no longer pending.
        let file_id: i64 = conn.query_row("SELECT id FROM files WHERE path = 'file.txt'", [], |r| r.get(0)).unwrap();
        conn.execute(
            "INSERT INTO file_content (file_id, content) VALUES (?1, 'hello')",
            rusqlite::params![file_id],
        ).unwrap();

        let pending_after = get_files_pending_content(&conn, &store).unwrap();
        assert_eq!(pending_after, 0, "file with inline content should not be pending");
    }

    #[test]
    fn test_get_stats_by_ext_excludes_archive_members() {
        let conn = test_conn();

        conn.execute(
            "INSERT INTO files (path, mtime, kind) VALUES ('script.js', 1000, 'text')",
            [],
        ).unwrap();
        // Archive member path contains '::' — should be excluded.
        conn.execute(
            "INSERT INTO files (path, mtime, kind) VALUES ('outer.zip::module.js', 1000, 'text')",
            [],
        ).unwrap();

        let by_ext = get_stats_by_ext(&conn).unwrap();
        let js_entry = by_ext.iter().find(|e| e.ext == "js");
        assert!(js_entry.is_some(), "js extension should appear");
        assert_eq!(js_entry.unwrap().count, 1, "archive member must be excluded; only 1 outer file");
    }

    #[test]
    fn test_upsert_indexing_errors_increments_count() {
        let conn = test_conn();

        let failure = IndexingFailure { path: "bad.txt".into(), error: "oops".into() };

        upsert_indexing_errors(&conn, &[failure.clone()], 1000).unwrap();
        upsert_indexing_errors(&conn, &[failure.clone()], 2000).unwrap();

        let count: i64 = conn.query_row(
            "SELECT count FROM indexing_errors WHERE path = 'bad.txt'",
            [],
            |r| r.get(0),
        ).unwrap();
        assert_eq!(count, 2, "calling upsert twice should increment count to 2");

        let last_seen: i64 = conn.query_row(
            "SELECT last_seen FROM indexing_errors WHERE path = 'bad.txt'",
            [],
            |r| r.get(0),
        ).unwrap();
        assert_eq!(last_seen, 2000, "last_seen should be updated to the second call's timestamp");
    }

    #[test]
    fn test_get_stats_counts_files_by_kind() {
        let conn = test_conn();

        conn.execute("INSERT INTO files (path, mtime, kind) VALUES ('a.txt', 1000, 'text')", []).unwrap();
        conn.execute("INSERT INTO files (path, mtime, kind) VALUES ('b.txt', 1000, 'text')", []).unwrap();
        conn.execute("INSERT INTO files (path, mtime, kind) VALUES ('img.png', 1000, 'image')", []).unwrap();

        let (total, _size, by_kind) = get_stats(&conn).unwrap();
        assert_eq!(total, 3);
        assert_eq!(by_kind[&FileKind::Text].count, 2);
        assert_eq!(by_kind[&FileKind::Image].count, 1);
    }

    #[test]
    fn test_upsert_indexing_errors_empty_is_noop() {
        let conn = test_conn();
        // Should not panic or error on empty input.
        upsert_indexing_errors(&conn, &[], 1000).unwrap();
        let count: i64 = conn.query_row("SELECT COUNT(*) FROM indexing_errors", [], |r| r.get(0)).unwrap();
        assert_eq!(count, 0);
    }
}
