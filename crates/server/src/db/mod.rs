#![allow(dead_code)] // some helpers reserved for future endpoints

use std::collections::HashMap;
use std::path::Path;

use anyhow::{Context, Result};
use rusqlite::{Connection, OptionalExtension, functions::FunctionFlags, params};

use find_common::api::{ContextLine, FileKind, FileRecord, IndexFile, PathRename, LINE_CONTENT_START};
use find_common::path::{composite_like_prefix, is_composite};

use find_content_store::{ContentKey, ContentStore};

pub mod constants;
pub mod links;
pub mod search;
pub mod stats;
pub mod tree;

#[allow(unused_imports)]
pub use constants::{
    decode_fts_rowid, encode_fts_rowid,
    MAX_LINES_PER_FILE, SQL_FTS_FILE_ID, SQL_FTS_FILENAME_ONLY, SQL_FTS_LINE_NUMBER,
};
pub use search::{
    document_candidates, fetch_duplicates_for_file_ids, fts_candidates, DateFilter,
};
pub use stats::{
    do_cleanup_writes, get_files_pending_content, get_fts_row_count, get_indexing_error,
    get_indexing_error_count, get_indexing_errors, get_scan_history, get_stats, get_stats_by_ext,
};
pub use tree::{expand_tree, list_dir, split_composite_path};

// ── Schema ────────────────────────────────────────────────────────────────────

/// The current schema version. Stored in SQLite's built-in `user_version` pragma.
/// Increment this whenever the schema changes incompatibly.
/// v13: content_blocks / content_archives / content_chunks removed from source
///      DBs; chunk metadata now lives in data_dir/content.db (find-content-store).
/// v14: Drop file_content table; rename content_hash → file_hash in files and
///      duplicates tables.
pub const SCHEMA_VERSION: i64 = 14;

pub fn open(db_path: &Path) -> Result<Connection> {
    let conn = Connection::open(db_path)
        .with_context(|| format!("opening {}", db_path.display()))?;
    // Wait up to 30 s for a write lock rather than failing immediately with
    // SQLITE_BUSY.  Multiple workers share one DB per source, so brief
    // contention is normal and should not be treated as an error.
    conn.busy_timeout(std::time::Duration::from_secs(30))?;
    // WAL mode allows concurrent reads during writes and avoids exclusive locks
    // for the full duration of large write transactions.  synchronous=NORMAL is
    // safe with WAL (data is never lost on crash) and much faster than the
    // default FULL mode (syncs at WAL checkpoints rather than every commit).
    conn.execute_batch("PRAGMA journal_mode = WAL; PRAGMA synchronous = NORMAL;")?;
    // foreign_keys must be re-enabled on every connection; PRAGMA in schema SQL
    // only runs once at creation time and does not persist across connections.
    conn.execute_batch("PRAGMA foreign_keys = ON;")?;
    register_scalar_functions(&conn)?;

    let version: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    if version == 0 {
        // Brand-new database — initialise the full current schema and stamp the version.
        conn.execute_batch(include_str!("../schema_v4.sql"))
            .context("initialising schema")?;
        conn.execute_batch(&format!("PRAGMA user_version = {SCHEMA_VERSION};"))
            .context("stamping schema version")?;
    } else if version == 13 {
        // v13 → v14: drop file_content, rename content_hash → file_hash.
        conn.execute_batch(
            "DROP TABLE IF EXISTS file_content;
             ALTER TABLE files RENAME COLUMN content_hash TO file_hash;
             DROP INDEX IF EXISTS files_content_hash;
             CREATE INDEX IF NOT EXISTS files_file_hash ON files(file_hash) WHERE file_hash IS NOT NULL;
             ALTER TABLE duplicates RENAME COLUMN content_hash TO file_hash;
             CREATE INDEX IF NOT EXISTS idx_files_mtime ON files(mtime);
             CREATE INDEX IF NOT EXISTS idx_duplicates_file_id ON duplicates(file_id);",
        ).context("migrating schema v13 → v14")?;
        conn.execute_batch(&format!("PRAGMA user_version = {SCHEMA_VERSION};"))
            .context("stamping schema version")?;
    } else if version != SCHEMA_VERSION {
        anyhow::bail!(
            "database schema is v{version} but this server requires v{SCHEMA_VERSION}. \
             Delete {} and re-run find-scan to rebuild.",
            db_path.display()
        );
    }

    Ok(conn)
}

/// Open a source DB for **read-only stats queries** with a short (1 s) busy
/// timeout.  If the DB is locked by a worker, the stats background task will
/// just skip it and return stale / zero values rather than blocking.
pub fn open_for_stats(db_path: &Path) -> Result<Connection> {
    let conn = Connection::open(db_path)
        .with_context(|| format!("opening {}", db_path.display()))?;
    conn.busy_timeout(std::time::Duration::from_secs(1))?;
    register_scalar_functions(&conn)?;
    Ok(conn)
}

/// Register custom scalar functions that SQLite does not provide built-in.
pub fn register_scalar_functions(conn: &Connection) -> Result<()> {
    let flags = FunctionFlags::SQLITE_UTF8 | FunctionFlags::SQLITE_DETERMINISTIC;

    // file_basename("foo/bar/baz.txt") → "baz.txt"
    conn.create_scalar_function("file_basename", 1, flags, |ctx| {
        let path: String = ctx.get(0)?;
        Ok(path.rsplit('/').next().unwrap_or(&path).to_string())
    })?;

    // file_ext("baz.txt") → "txt"   file_ext("baz") → ""
    conn.create_scalar_function("file_ext", 1, flags, |ctx| {
        let name: String = ctx.get(0)?;
        if let Some(pos) = name.rfind('.') {
            let ext = &name[pos + 1..];
            Ok(ext.to_lowercase())
        } else {
            Ok(String::new())
        }
    })?;

    Ok(())
}


/// Open and migrate all existing source databases in `sources_dir` at startup.
/// This ensures any pending schema migrations are applied eagerly rather than
/// lazily on the first request, and catches truly incompatible databases early.
pub fn check_all_sources(sources_dir: &Path) -> Result<()> {
    let read_dir = match std::fs::read_dir(sources_dir) {
        Ok(rd) => rd,
        Err(_) => return Ok(()), // sources dir doesn't exist yet — nothing to check
    };
    for entry in read_dir.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("db") {
            continue;
        }
        // open() applies any pending migrations and errors on truly incompatible versions.
        let conn = open(&path).with_context(|| format!("migrating {}", path.display()))?;
        // Idempotent index additions for existing databases.  These run once at
        // startup under no concurrency pressure.  Keeping DDL out of the per-request
        // open() path avoids write-lock contention when many threads open the same DB
        // simultaneously, which could otherwise deadlock via SQLite's WAL mutex.
        conn.execute_batch(
            "CREATE INDEX IF NOT EXISTS idx_files_mtime ON files(mtime);
             CREATE INDEX IF NOT EXISTS idx_duplicates_file_id ON duplicates(file_id);"
        ).with_context(|| format!("ensuring indexes on {}", path.display()))?;
    }
    Ok(())
}

// ── Chunk-read helpers ────────────────────────────────────────────────────────

/// Read a single line's content for a file via `ContentStore`.
///
/// Looks up `files.file_hash` and calls `content_store.get_lines`.
///
/// Returns `None` if the file or line cannot be found (stale FTS, pending archive).
pub fn read_chunk_for_file(
    conn: &Connection,
    content_store: &dyn ContentStore,
    file_id: i64,
    line_number: i64,
) -> Option<String> {
    let hash: String = conn.query_row(
        "SELECT file_hash FROM files WHERE id = ?1",
        params![file_id],
        |r| r.get(0),
    ).optional().ok().flatten()?;

    let key = ContentKey::new(hash.as_str());
    let lo = line_number as usize;
    let lines = content_store.get_lines(&key, lo, lo).ok()??;
    lines.into_iter().find(|(pos, _)| *pos == lo).map(|(_, c)| c)
}

/// Batch-resolve line content for multiple `(file_id, line_number)` pairs
/// using `ContentStore`.
///
/// 1. Batch-fetch `file_hash` for all file IDs.
/// 2. For each unique hash, call `content_store.get_lines` with the full range.
pub fn read_content_batch(
    conn: &Connection,
    content_store: &dyn ContentStore,
    pairs: &[(i64, i64)], // (file_id, line_number)
) -> HashMap<(i64, i64), String> {
    if pairs.is_empty() {
        return HashMap::new();
    }

    // Deduplicate file_ids.
    let mut seen = std::collections::HashSet::new();
    let file_ids: Vec<i64> = pairs
        .iter()
        .map(|(id, _)| *id)
        .filter(|id| seen.insert(*id))
        .collect();

    // ── 1. file_hash for all files ────────────────────────────────────────
    let ph: String = (1..=file_ids.len()).map(|i| format!("?{i}")).collect::<Vec<_>>().join(",");
    let sql = format!(
        "SELECT id, file_hash FROM files WHERE id IN ({ph}) AND file_hash IS NOT NULL"
    );
    let params_refs: Vec<&dyn rusqlite::ToSql> =
        file_ids.iter().map(|id| id as &dyn rusqlite::ToSql).collect();
    let mut hash_map: HashMap<i64, String> = HashMap::new();
    if let Ok(mut stmt) = conn.prepare(&sql) {
        let _ = stmt
            .query_map(params_refs.as_slice(), |row| {
                Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
            })
            .map(|rows| {
                rows.flatten().for_each(|(fid, hash)| { hash_map.insert(fid, hash); })
            });
    }

    // ── 2. Group needed line positions by hash ────────────────────────────
    let mut by_hash: HashMap<String, Vec<(i64, i64)>> = HashMap::new();
    for &(file_id, line_number) in pairs {
        if let Some(hash) = hash_map.get(&file_id) {
            by_hash.entry(hash.clone()).or_default().push((file_id, line_number));
        }
    }

    // ── 3. Fetch lines from ContentStore, one call per hash ───────────────
    let mut content_cache: HashMap<String, Vec<(usize, String)>> = HashMap::new();
    for (hash, pairs_for_hash) in &by_hash {
        let lo = pairs_for_hash.iter().map(|(_, ln)| *ln as usize).min().unwrap_or(0);
        let hi = pairs_for_hash.iter().map(|(_, ln)| *ln as usize).max().unwrap_or(0);
        let key = ContentKey::new(hash.as_str());
        if let Ok(Some(lines)) = content_store.get_lines(&key, lo, hi) {
            content_cache.insert(hash.clone(), lines);
        }
    }

    // ── 4. Resolve each pair ──────────────────────────────────────────────
    let mut result = HashMap::new();
    for &(file_id, line_number) in pairs {
        if let Some(hash) = hash_map.get(&file_id) {
            if let Some(lines) = content_cache.get(hash) {
                if let Some((_, content)) = lines.iter().find(|(pos, _)| *pos == line_number as usize) {
                    if !content.is_empty() {
                        result.insert((file_id, line_number), content.clone());
                    }
                }
            }
        }
    }
    result
}

/// Read all content lines for a file and return them as a single joined string.
/// Used by DocRegex mode to test a regex against the full document.
/// Returns an empty string if content is not available.
pub fn read_file_document(conn: &Connection, content_store: &dyn ContentStore, file_id: i64) -> String {
    let hash: Option<String> = conn.query_row(
        "SELECT file_hash FROM files WHERE id = ?1 AND file_hash IS NOT NULL",
        params![file_id],
        |r| r.get(0),
    ).optional().ok().flatten();
    let Some(hash) = hash else { return String::new(); };

    let key = ContentKey::new(hash.as_str());
    // i64::MAX as usize avoids the usize::MAX → -1 cast that would break the SQL range query.
    match content_store.get_lines(&key, 0, i64::MAX as usize) {
        Ok(Some(lines)) => lines.into_iter().map(|(_, c)| c).collect::<Vec<_>>().join("\n"),
        _ => String::new(),
    }
}

// ── Source-level helpers ──────────────────────────────────────────────────────

/// Delete singleton entries from the `duplicates` table.
/// A singleton is a file_hash that appears only once — meaning the file
/// is no longer a duplicate after another file with the same hash was deleted.
pub fn cleanup_singleton_duplicates(conn: &Connection) -> Result<()> {
    conn.execute(
        "DELETE FROM duplicates
         WHERE file_hash IN (
             SELECT file_hash FROM duplicates
             GROUP BY file_hash HAVING COUNT(*) = 1
         )",
        [],
    )?;
    Ok(())
}

/// Count all files in this source database.
pub fn count_files(conn: &Connection) -> Result<usize> {
    let n: i64 = conn.query_row("SELECT COUNT(*) FROM files", [], |r| r.get(0))?;
    Ok(n as usize)
}

/// Return the `limit` most recently indexed outer files (no `::` in path).
/// `sort_by_mtime = false` orders by `COALESCE(indexed_at, mtime)` (recently indexed);
/// `sort_by_mtime = true` orders by raw `mtime` (recently modified on disk).
/// Returns `(path, sort_ts)` pairs.
pub fn recent_files(conn: &Connection, limit: usize, sort_by_mtime: bool) -> Result<Vec<(String, i64)>> {
    let sql = if sort_by_mtime {
        "SELECT path, mtime FROM files \
         WHERE path NOT LIKE '%::%' \
         ORDER BY mtime DESC LIMIT ?1"
    } else {
        "SELECT path, COALESCE(indexed_at, mtime) FROM files \
         WHERE path NOT LIKE '%::%' \
         ORDER BY COALESCE(indexed_at, mtime) DESC LIMIT ?1"
    };
    let mut stmt = conn.prepare(sql)?;
    let rows = stmt
        .query_map(params![limit as i64], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

// ── Activity log ─────────────────────────────────────────────────────────────

/// Append activity-log entries for a batch of events and prune the log to
/// `max_entries` total rows (oldest rows are deleted first).
///
/// Only outer-file paths (no `::`) are logged; composite archive-member paths
/// are silently skipped.
pub fn log_activity(
    conn: &Connection,
    now: i64,
    added:    &[String],
    modified: &[String],
    deleted:  &[String],
    renamed:  &[(String, String)], // (old_path, new_path)
    max_entries: usize,
) -> Result<()> {
    if added.is_empty() && modified.is_empty() && deleted.is_empty() && renamed.is_empty() {
        return Ok(());
    }
    let tx = conn.unchecked_transaction()?;
    {
        let mut stmt = tx.prepare_cached(
            "INSERT INTO activity_log (occurred_at, action, path, new_path) VALUES (?1, ?2, ?3, ?4)"
        )?;
        for path in added    { if !is_composite(path) { stmt.execute(params![now, "added",    path, None::<&str>])?; } }
        for path in modified { if !is_composite(path) { stmt.execute(params![now, "modified", path, None::<&str>])?; } }
        for path in deleted  { if !is_composite(path) { stmt.execute(params![now, "deleted",  path, None::<&str>])?; } }
        for (old, new) in renamed {
            if !is_composite(old) && !is_composite(new) {
                stmt.execute(params![now, "renamed", old, Some(new.as_str())])?;
            }
        }
    }
    // Prune to max_entries, keeping the most recent rows.  Since IDs are
    // monotonically increasing with a single worker, ORDER BY id ≈ ORDER BY occurred_at.
    if max_entries > 0 {
        tx.execute(
            "DELETE FROM activity_log WHERE id NOT IN \
             (SELECT id FROM activity_log ORDER BY id DESC LIMIT ?1)",
            params![max_entries as i64],
        )?;
    }
    tx.commit()?;
    Ok(())
}

/// One row from the activity log: `(action, path, new_path, occurred_at)`.
pub type ActivityRow = (String, String, Option<String>, i64);

/// Return the `limit` most recent activity-log entries across outer files.
pub fn recent_activity(
    conn: &Connection,
    limit: usize,
) -> Result<Vec<ActivityRow>> {
    let mut stmt = conn.prepare(
        "SELECT action, path, new_path, occurred_at FROM activity_log \
         ORDER BY occurred_at DESC LIMIT ?1",
    )?;
    let rows = stmt
        .query_map(params![limit as i64], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, Option<String>>(2)?,
                row.get::<_, i64>(3)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

// ── File listing (for deletion detection) ────────────────────────────────────

pub fn list_files(conn: &Connection) -> Result<Vec<FileRecord>> {
    let mut stmt = conn.prepare(
        "SELECT path, mtime, kind, scanner_version, indexed_at FROM files ORDER BY path"
    )?;
    let rows = stmt
        .query_map([], |row| {
            let kind_str: String = row.get(2)?;
            Ok(FileRecord {
                path: row.get(0)?,
                mtime: row.get(1)?,
                kind: FileKind::from(kind_str.as_str()),
                scanner_version: row.get::<_, u32>(3).unwrap_or(0),
                indexed_at: row.get(4)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

// ── File search (for Ctrl+P palette) ─────────────────────────────────────────

pub fn search_files(conn: &Connection, q: &str, limit: usize) -> Result<Vec<FileRecord>> {
    if q.is_empty() {
        // No query: return most recently indexed files.
        let mut stmt = conn.prepare(
            "SELECT path, kind FROM files ORDER BY indexed_at DESC LIMIT ?"
        )?;
        let rows = stmt.query_map(params![limit as i64], |row| {
            let kind_str: String = row.get(1)?;
            Ok(FileRecord { path: row.get(0)?, mtime: 0, kind: FileKind::from(kind_str.as_str()),
                scanner_version: 0, indexed_at: Some(0) })
        })?.collect::<rusqlite::Result<Vec<_>>>()?;
        return Ok(rows);
    }
    let pattern = format!("%{}%", q);
    let mut stmt = conn.prepare(
        "SELECT path, kind FROM files WHERE lower(path) LIKE lower(?) ORDER BY path LIMIT ?"
    )?;
    let rows = stmt.query_map(params![pattern, limit as i64], |row| {
        let kind_str: String = row.get(1)?;
        Ok(FileRecord { path: row.get(0)?, mtime: 0, kind: FileKind::from(kind_str.as_str()),
            scanner_version: 0, indexed_at: Some(0) })
    })?.collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

// ── Upsert ────────────────────────────────────────────────────────────────────

pub fn upsert_files(conn: &Connection, files: &[IndexFile]) -> Result<()> {
    let tx = conn.unchecked_transaction()?;

    for file in files {
        tx.execute(
            "INSERT INTO files (path, mtime, size, kind, scanner_version)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(path) DO UPDATE SET
               mtime           = excluded.mtime,
               size            = excluded.size,
               kind            = excluded.kind,
               scanner_version = excluded.scanner_version",
            params![file.path, file.mtime, file.size.as_ref().map(|&s| s), file.kind.to_string(), file.scanner_version],
        )?;
    }

    tx.commit()?;
    Ok(())
}

// ── Delete ────────────────────────────────────────────────────────────────────

/// Delete `paths` from the database.
/// ZIP chunks are cleaned up by periodic compaction (not immediately).
///
/// Indexing errors for the deleted paths (and their inner archive members) are
/// cleared in the **same** transaction.  This is deliberate: on WAL-mode
/// SQLite over WSL / network mounts POSIX advisory locking is unreliable and a
/// second write transaction on the same connection can hang indefinitely after
/// the first one commits.  Keeping error-clearing inside this transaction means
/// delete-only requests never need a second write on the same connection.
pub fn delete_files(
    conn: &Connection,
    paths: &[String],
) -> Result<()> {
    let tx = conn.unchecked_transaction()?;

    for path in paths {
        delete_one_path_simple(&tx, path)?;
        // Clear indexing errors for the outer path and all inner archive members
        // in the same transaction to avoid a second write on this connection.
        tx.execute("DELETE FROM indexing_errors WHERE path = ?1", params![path])?;
        tx.execute(
            "DELETE FROM indexing_errors WHERE path LIKE ?1",
            params![composite_like_prefix(path)],
        )?;
    }

    // Clean up singleton duplicates left by the deletions.
    cleanup_singleton_duplicates_tx(&tx)?;

    tx.commit()?;
    // Orphaned blobs in the content store are reclaimed by the next compaction pass.
    Ok(())
}

/// Delete one path (outer + inner archive members). No canonical promotion in v3.
fn delete_one_path_simple(tx: &rusqlite::Transaction, path: &str) -> Result<()> {
    let outer_id: Option<i64> = tx.query_row(
        "SELECT id FROM files WHERE path = ?1",
        params![path],
        |row| row.get(0),
    ).optional()?;

    let Some(outer_id) = outer_id else { return Ok(()); };

    // Delete inner archive members first.
    tx.execute(
        "DELETE FROM files WHERE path LIKE ?1",
        params![composite_like_prefix(path)],
    )?;

    // Delete the outer file (CASCADE removes duplicates entries).
    tx.execute("DELETE FROM files WHERE id = ?1", params![outer_id])?;
    Ok(())
}

/// Run singleton-duplicate cleanup inside an existing transaction.
fn cleanup_singleton_duplicates_tx(tx: &rusqlite::Transaction) -> Result<()> {
    tx.execute(
        "DELETE FROM duplicates
         WHERE file_hash IN (
             SELECT file_hash FROM duplicates
             GROUP BY file_hash HAVING COUNT(*) = 1
         )",
        [],
    )?;
    Ok(())
}

// ── Delete (phase 1) ─────────────────────────────────────────────────────────

/// Stats captured from files that are about to be deleted (for incremental cache update).
pub struct DeleteDelta {
    pub files_removed: i64,
    pub size_removed:  i64,
    pub by_kind: HashMap<FileKind, (i64, i64)>, // kind → (count, size)
}

/// Delete files from SQLite. Orphaned ZIP chunks are cleaned up by periodic
/// compaction rather than immediately rewriting archives.
///
/// - Clears indexing errors for the deleted paths in the same transaction.
/// - Returns a `DeleteDelta` capturing the stats of all deleted outer files
///   (composite archive-member paths are excluded from the delta).
pub fn delete_files_phase1(conn: &Connection, paths: &[String]) -> Result<DeleteDelta> {
    let mut delta = DeleteDelta { files_removed: 0, size_removed: 0, by_kind: HashMap::new() };

    let tx = conn.unchecked_transaction()?;

    for path in paths {
        // Composite paths (archive members) don't appear in outer-file stats.
        if !is_composite(path) {
            let row: Option<(i64, String)> = tx.query_row(
                "SELECT COALESCE(size,0), kind FROM files WHERE path = ?1",
                params![path],
                |r| Ok((r.get(0)?, r.get(1)?)),
            ).optional()?;
            if let Some((size, kind_str)) = row {
                let kind = FileKind::from(kind_str.as_str());
                delta.files_removed += 1;
                delta.size_removed  += size;
                let e = delta.by_kind.entry(kind).or_insert((0, 0));
                e.0 += 1;
                e.1 += size;
            }
        }
        delete_one_path_simple(&tx, path)?;
        tx.execute("DELETE FROM indexing_errors WHERE path = ?1", params![path])?;
        tx.execute(
            "DELETE FROM indexing_errors WHERE path LIKE ?1",
            params![format!("{}::%", path)],
        )?;
    }

    // Clean up singleton duplicates.
    cleanup_singleton_duplicates_tx(&tx)?;

    tx.commit()?;
    Ok(delta)
}

// ── Rename ────────────────────────────────────────────────────────────────────

/// Rename files in the index. Updates `files.path` and archive member paths.
/// Also updates FTS line_number=0 entries (filename search lines).
/// No ZIP operations needed — chunk names are now {block_id}.{N} and are path-independent.
pub fn rename_files(conn: &Connection, renames: &[PathRename]) -> Result<()> {
    let tx = conn.unchecked_transaction()?;
    for rename in renames {
        // Check old path exists
        let file_id: Option<i64> = tx.query_row(
            "SELECT id FROM files WHERE path = ?1",
            params![rename.old_path],
            |r| r.get(0),
        ).optional()?;
        let Some(file_id) = file_id else { continue; };

        // Skip if new path already exists (race with periodic scan)
        let new_exists: bool = tx.query_row(
            "SELECT COUNT(*) FROM files WHERE path = ?1",
            params![rename.new_path],
            |r| r.get::<_, i64>(0),
        ).unwrap_or(0) > 0;
        if new_exists {
            tracing::debug!("rename: {} → {} skipped (new path already indexed)", rename.old_path, rename.new_path);
            continue;
        }

        // Update the file's path
        tx.execute(
            "UPDATE files SET path = ?1 WHERE path = ?2",
            params![rename.new_path, rename.old_path],
        )?;

        // Update archive member paths: old_path::member → new_path::member
        let old_prefix = format!("{}::", rename.old_path);
        let new_prefix = format!("{}::", rename.new_path);
        tx.execute(
            "UPDATE files SET path = ?1 || substr(path, length(?2) + 1) WHERE path LIKE ?3",
            params![new_prefix, old_prefix, format!("{}%", old_prefix)],
        )?;

        // Update FTS entry for line_number=0 (filename search line).
        // In v3, the FTS rowid for line_number=0 is encode_fts_rowid(file_id, 0).
        let rowid0 = crate::db::encode_fts_rowid(file_id, 0);
        // Delete old FTS entry for line 0
        tx.execute(
            "INSERT INTO lines_fts(lines_fts, rowid, content) VALUES('delete', ?1, ?2)",
            params![rowid0, rename.old_path],
        )?;
        // Insert new FTS entry for line 0
        tx.execute(
            "INSERT INTO lines_fts(rowid, content) VALUES(?1, ?2)",
            params![rowid0, rename.new_path],
        )?;

    }
    tx.commit()?;
    Ok(())
}

// ── Scan timestamp ────────────────────────────────────────────────────────────

pub fn update_last_scan(conn: &Connection, timestamp: i64) -> Result<()> {
    conn.execute(
        "INSERT INTO meta (key, value) VALUES ('last_scan', ?1)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        params![timestamp.to_string()],
    )?;
    Ok(())
}

pub fn get_last_scan(conn: &Connection) -> Result<Option<i64>> {
    let result = conn.query_row(
        "SELECT value FROM meta WHERE key = 'last_scan'",
        [],
        |row| row.get::<_, String>(0),
    );
    match result {
        Ok(s) => Ok(s.parse().ok()),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(e) => Err(e.into()),
    }
}

// ── File lines ────────────────────────────────────────────────────────────────

/// Resolve the file_id for a path. Returns None if the path is not in the files table.
fn resolve_file_id(conn: &Connection, path: &str) -> rusqlite::Result<Option<i64>> {
    conn.query_row(
        "SELECT id FROM files WHERE path = ?1",
        params![path],
        |row| row.get(0),
    )
    .optional()
}

/// Returns every indexed line for a file, ordered by line number, plus a flag
/// indicating whether content is pending archive processing.
///
/// `content_unavailable` is `true` when the file has a file_hash but the
/// content store does not yet contain that blob — i.e. phase 1 is done but
/// the archive worker has not yet run.
///
/// `path` may be a composite path ("archive.zip::member.txt") or a plain path.
pub fn get_file_lines(
    conn: &Connection,
    content_store: &dyn ContentStore,
    path: &str,
) -> Result<(Vec<ContextLine>, bool)> {
    let Some(file_id) = resolve_file_id(conn, path)? else {
        return Ok((vec![], false));
    };

    let file_info: Option<(Option<i64>, Option<String>)> = conn.query_row(
        "SELECT line_count, file_hash FROM files WHERE id = ?1",
        params![file_id],
        |r| Ok((r.get(0)?, r.get(1)?)),
    ).optional()?;
    let (line_count, file_hash) = file_info.unwrap_or((None, None));
    let line_count = line_count.unwrap_or(0);

    let content_unavailable = content_unavailable(conn, content_store, file_id, &file_hash);

    let lines: Vec<ContextLine> = (0..line_count)
        .filter_map(|ln| {
            let content = read_chunk_for_file(conn, content_store, file_id, ln)?;
            Some(ContextLine { line_number: ln as usize, content })
        })
        .collect();

    Ok((lines, content_unavailable))
}

/// Paged variant of `get_file_lines`.
///
/// Always returns lines 0 (path) and 1 (metadata).  Content rows
/// (line_number >= LINE_CONTENT_START = 2) are returned starting from `offset`
/// (0-based index into the ordered set of content lines), limited to `limit`
/// rows when provided.
///
/// Returns `(combined_lines, total_content_count, content_unavailable)` where
/// `combined_lines` contains metadata lines followed by the content page.
/// `total_content_count` is the true total regardless of `offset`/`limit`.
pub fn get_file_lines_paged(
    conn: &Connection,
    content_store: &dyn ContentStore,
    path: &str,
    offset: usize,
    limit: Option<usize>,
) -> Result<(Vec<ContextLine>, usize, bool)> {
    let Some(file_id) = resolve_file_id(conn, path)? else {
        return Ok((vec![], 0, false));
    };

    let file_info: Option<(Option<i64>, Option<String>)> = conn.query_row(
        "SELECT line_count, file_hash FROM files WHERE id = ?1",
        params![file_id],
        |r| Ok((r.get(0)?, r.get(1)?)),
    ).optional()?;
    let (line_count, file_hash) = file_info.unwrap_or((None, None));
    let line_count = line_count.unwrap_or(0) as usize;
    let total_count = line_count.saturating_sub(LINE_CONTENT_START);

    let content_unavail = content_unavailable(conn, content_store, file_id, &file_hash);

    // Lines 0 (path) and 1 (metadata) always included.
    let mut lines: Vec<ContextLine> = Vec::new();
    for meta_ln in 0..LINE_CONTENT_START {
        if let Some(content) = read_chunk_for_file(conn, content_store, file_id, meta_ln as i64) {
            lines.push(ContextLine { line_number: meta_ln, content });
        }
    }

    // Content lines (line_number >= LINE_CONTENT_START):
    // - When limit is None: return all content lines (backward-compat).
    // - When limit is Some: paginate with offset.
    let (content_start, content_end) = match limit {
        Some(lim) => {
            let start = LINE_CONTENT_START + offset;
            let end = (start + lim).min(line_count);
            (start, end)
        }
        None => (LINE_CONTENT_START, line_count),
    };

    for ln in content_start..content_end {
        if let Some(content) = read_chunk_for_file(conn, content_store, file_id, ln as i64) {
            lines.push(ContextLine { line_number: ln, content });
        }
    }

    Ok((lines, total_count, content_unavail))
}

// ── Context ───────────────────────────────────────────────────────────────────

pub fn get_context(
    conn: &Connection,
    content_store: &dyn ContentStore,
    file_path: &str,
    center: usize,
    window: usize,
) -> Result<Vec<ContextLine>> {
    let kind = get_file_kind(conn, file_path)?;

    match kind {
        FileKind::Image | FileKind::Audio => get_metadata_context(conn, content_store, file_path),
        _ => get_line_context(conn, content_store, file_path, center, window),
    }
}

fn get_file_kind(conn: &Connection, file_path: &str) -> Result<FileKind> {
    conn.query_row(
        "SELECT kind FROM files WHERE path = ?1 LIMIT 1",
        params![file_path],
        |row| row.get::<_, String>(0),
    )
    .map(|s| FileKind::from(s.as_str()))
    .map_err(Into::into)
}

fn get_metadata_context(
    conn: &Connection,
    content_store: &dyn ContentStore,
    file_path: &str,
) -> Result<Vec<ContextLine>> {
    let Some(file_id) = resolve_file_id(conn, file_path)? else {
        return Ok(vec![]);
    };
    let content = read_chunk_for_file(conn, content_store, file_id, 0).unwrap_or_default();
    Ok(vec![ContextLine { line_number: 0, content }])
}

fn get_line_context(
    conn: &Connection,
    content_store: &dyn ContentStore,
    file_path: &str,
    center: usize,
    window: usize,
) -> Result<Vec<ContextLine>> {
    let Some(file_id) = resolve_file_id(conn, file_path)? else {
        return Ok(vec![]);
    };

    let lo = center.saturating_sub(window);
    let hi = center + window;

    let lines: Vec<ContextLine> = (lo..=hi)
        .filter_map(|ln| {
            let content = read_chunk_for_file(conn, content_store, file_id, ln as i64)?;
            Some(ContextLine { line_number: ln, content })
        })
        .collect();

    Ok(lines)
}

/// Returns `true` when the file has a `file_hash` but the content store does
/// not yet contain that blob — i.e. phase 1 is done but the archive worker
/// has not yet run.
fn content_unavailable(
    _conn: &Connection,
    content_store: &dyn ContentStore,
    _file_id: i64,
    file_hash: &Option<String>,
) -> bool {
    let Some(ref hash) = file_hash else { return false; };
    let key = ContentKey::new(hash.as_str());
    !content_store.contains(&key).unwrap_or(false)
}

// ── Unit tests ─────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use find_common::api::PathRename;

    /// Create an in-memory database with the full schema and scalar functions.
    fn test_conn() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("PRAGMA foreign_keys = ON;").unwrap();
        conn.execute_batch(include_str!("../schema_v4.sql")).unwrap();
        register_scalar_functions(&conn).unwrap();
        conn
    }

    /// Insert a plain-text file with FTS entries.
    /// `lines[0]` is the path (line_number = 0); subsequent entries are content lines.
    /// Returns the `file_id`.
    fn insert_file(conn: &Connection, path: &str, mtime: i64, lines: &[&str]) -> i64 {
        conn.execute(
            "INSERT INTO files (path, mtime, kind, line_count) VALUES (?1, ?2, 'text', ?3)",
            params![path, mtime, lines.len() as i64],
        ).unwrap();
        let file_id = conn.last_insert_rowid();

        for (i, &line) in lines.iter().enumerate() {
            let rowid = crate::db::encode_fts_rowid(file_id, i as i64);
            conn.execute(
                "INSERT INTO lines_fts(rowid, content) VALUES (?1, ?2)",
                params![rowid, line],
            ).unwrap();
        }

        file_id
    }

    /// Count FTS matches that still have a live `files` row (i.e. not orphaned by deletion).
    /// Wraps the query in FTS5 phrase quotes.
    fn fts_live_count(conn: &Connection, query: &str) -> usize {
        let phrase = format!("\"{}\"", query);
        conn.query_row(
            &format!("SELECT COUNT(*) FROM lines_fts lf
             JOIN files f ON f.id = (lf.rowid / {MAX_LINES_PER_FILE})
             WHERE lines_fts MATCH ?1"),
            params![phrase],
            |r| r.get::<_, i64>(0),
        ).unwrap_or(0) as usize
    }

    fn file_exists(conn: &Connection, path: &str) -> bool {
        conn.query_row(
            "SELECT COUNT(*) FROM files WHERE path = ?1",
            params![path],
            |r| r.get::<_, i64>(0),
        ).unwrap_or(0) > 0
    }

    // ── delete_files_phase1 ────────────────────────────────────────────────────

    #[test]
    fn test_delete_basic() {
        let conn = test_conn();
        let _fid = insert_file(&conn, "docs/readme.txt", 1000, &["docs/readme.txt", "hello world"]);
        assert!(file_exists(&conn, "docs/readme.txt"));

        delete_files_phase1(&conn, &["docs/readme.txt".to_string()]).unwrap();

        assert!(!file_exists(&conn, "docs/readme.txt"));
    }

    #[test]
    fn test_delete_noop_missing() {
        let conn = test_conn();
        // Should not error when path doesn't exist.
        delete_files_phase1(&conn, &["nonexistent.txt".to_string()]).unwrap();
    }

    #[test]
    fn test_delete_removes_archive_members() {
        let conn = test_conn();
        // Insert outer archive and two inner members.
        insert_file(&conn, "archive.zip", 1000, &["archive.zip"]);
        insert_file(&conn, "archive.zip::a.txt", 1000, &["archive.zip::a.txt", "content a"]);
        insert_file(&conn, "archive.zip::b.txt", 1000, &["archive.zip::b.txt", "content b"]);

        delete_files_phase1(&conn, &["archive.zip".to_string()]).unwrap();

        assert!(!file_exists(&conn, "archive.zip"));
        assert!(!file_exists(&conn, "archive.zip::a.txt"));
        assert!(!file_exists(&conn, "archive.zip::b.txt"));
    }

    // ── FTS round-trip ─────────────────────────────────────────────────────────

    #[test]
    fn test_fts_insert_then_find() {
        let conn = test_conn();
        insert_file(&conn, "src/main.rs", 1000, &["src/main.rs", "fn main() {}", "let x = 420;"]);
        assert!(fts_live_count(&conn, "main") > 0);
        // trigram requires >= 3 chars; "420" is 3 chars and indexable
        assert!(fts_live_count(&conn, "420") > 0);
    }

    #[test]
    fn test_fts_delete_orphans_entries() {
        let conn = test_conn();
        insert_file(&conn, "src/lib.rs", 1000, &["src/lib.rs", "unique_token_xyz"]);
        assert_eq!(fts_live_count(&conn, "unique_token_xyz"), 1);

        delete_files_phase1(&conn, &["src/lib.rs".to_string()]).unwrap();

        // Lines are CASCADE deleted; FTS rowids are orphaned but JOIN returns nothing.
        assert_eq!(fts_live_count(&conn, "unique_token_xyz"), 0);
    }

    // ── rename_files ───────────────────────────────────────────────────────────

    #[test]
    fn test_rename_updates_path() {
        let conn = test_conn();
        insert_file(&conn, "old/path.txt", 1000, &["old/path.txt", "content"]);

        rename_files(&conn, &[PathRename {
            old_path: "old/path.txt".to_string(),
            new_path: "new/path.txt".to_string(),
        }]).unwrap();

        assert!(!file_exists(&conn, "old/path.txt"));
        assert!(file_exists(&conn, "new/path.txt"));
    }

    #[test]
    fn test_rename_updates_archive_members() {
        let conn = test_conn();
        insert_file(&conn, "data.zip", 1000, &["data.zip"]);
        insert_file(&conn, "data.zip::member.txt", 1000, &["data.zip::member.txt", "content"]);

        rename_files(&conn, &[PathRename {
            old_path: "data.zip".to_string(),
            new_path: "renamed.zip".to_string(),
        }]).unwrap();

        assert!(file_exists(&conn, "renamed.zip"));
        assert!(file_exists(&conn, "renamed.zip::member.txt"));
        assert!(!file_exists(&conn, "data.zip::member.txt"));
    }

    #[test]
    fn test_rename_updates_fts_line0() {
        let conn = test_conn();
        insert_file(&conn, "before.txt", 1000, &["before.txt", "hello"]);

        // before.txt should be found by FTS (via files JOIN)
        assert!(fts_live_count(&conn, "before.txt") > 0);

        rename_files(&conn, &[PathRename {
            old_path: "before.txt".to_string(),
            new_path: "after.txt".to_string(),
        }]).unwrap();

        // after rename, the FTS entry for line 0 is updated
        assert!(fts_live_count(&conn, "after.txt") > 0);
    }

    #[test]
    fn test_rename_skip_existing_target() {
        let conn = test_conn();
        insert_file(&conn, "a.txt", 1000, &["a.txt", "content a"]);
        insert_file(&conn, "b.txt", 2000, &["b.txt", "content b"]);

        // Rename a.txt → b.txt should be skipped (b.txt already exists).
        rename_files(&conn, &[PathRename {
            old_path: "a.txt".to_string(),
            new_path: "b.txt".to_string(),
        }]).unwrap();

        // Both files should still exist unchanged.
        assert!(file_exists(&conn, "a.txt"));
        assert!(file_exists(&conn, "b.txt"));
    }

    // ── list_files ─────────────────────────────────────────────────────────────

    #[test]
    fn test_list_files_returns_indexed_at() {
        let conn = test_conn();
        insert_file(&conn, "file.txt", 1000, &["file.txt"]);
        // Set indexed_at on the file.
        conn.execute(
            "UPDATE files SET indexed_at = 9999 WHERE path = 'file.txt'",
            [],
        ).unwrap();

        let records = list_files(&conn).unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].path, "file.txt");
        assert_eq!(records[0].indexed_at, Some(9999));
    }

    #[test]
    fn test_list_files_indexed_at_null() {
        let conn = test_conn();
        insert_file(&conn, "unindexed.txt", 1000, &["unindexed.txt"]);

        let records = list_files(&conn).unwrap();
        assert_eq!(records[0].indexed_at, None);
    }

    // ── log_activity / recent_activity ────────────────────────────────────────

    #[test]
    fn test_activity_log_round_trip() {
        let conn = test_conn();

        log_activity(
            &conn, 1000,
            &["new_file.txt".to_string()],
            &[],
            &[],
            &[],
            100,
        ).unwrap();

        let rows = recent_activity(&conn, 10).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].0, "added");
        assert_eq!(rows[0].1, "new_file.txt");
        assert_eq!(rows[0].2, None);
        assert_eq!(rows[0].3, 1000);
    }

    #[test]
    fn test_activity_log_all_actions() {
        let conn = test_conn();

        log_activity(
            &conn, 500,
            &["a.txt".to_string()],
            &["b.txt".to_string()],
            &["c.txt".to_string()],
            &[("d.txt".to_string(), "e.txt".to_string())],
            100,
        ).unwrap();

        let rows = recent_activity(&conn, 10).unwrap();
        assert_eq!(rows.len(), 4);
        let actions: Vec<&str> = rows.iter().map(|r| r.0.as_str()).collect();
        assert!(actions.contains(&"added"));
        assert!(actions.contains(&"modified"));
        assert!(actions.contains(&"deleted"));
        assert!(actions.contains(&"renamed"));

        let rename = rows.iter().find(|r| r.0 == "renamed").unwrap();
        assert_eq!(rename.1, "d.txt");
        assert_eq!(rename.2.as_deref(), Some("e.txt"));
    }

    #[test]
    fn test_activity_log_skips_composite_paths() {
        let conn = test_conn();

        log_activity(
            &conn, 1000,
            &["archive.zip::member.txt".to_string(), "normal.txt".to_string()],
            &[],
            &[],
            &[],
            100,
        ).unwrap();

        let rows = recent_activity(&conn, 10).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].1, "normal.txt");
    }

    #[test]
    fn test_activity_log_prunes_to_max() {
        let conn = test_conn();

        for i in 0..10i64 {
            log_activity(
                &conn, i,
                &[format!("file{i}.txt")],
                &[], &[], &[],
                5, // keep only 5
            ).unwrap();
        }

        let rows = recent_activity(&conn, 100).unwrap();
        assert_eq!(rows.len(), 5);
    }

    // ── update_last_scan / get_last_scan ──────────────────────────────────────

    #[test]
    fn test_last_scan_round_trip() {
        let conn = test_conn();

        assert_eq!(get_last_scan(&conn).unwrap(), None);

        update_last_scan(&conn, 42000).unwrap();
        assert_eq!(get_last_scan(&conn).unwrap(), Some(42000));

        update_last_scan(&conn, 99000).unwrap();
        assert_eq!(get_last_scan(&conn).unwrap(), Some(99000));
    }
}
