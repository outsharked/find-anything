#![allow(dead_code)] // some helpers reserved for future endpoints

use std::collections::HashMap;
use std::path::Path;

use anyhow::{Context, Result};
use rusqlite::{Connection, OptionalExtension, functions::FunctionFlags, params};

use find_common::api::{ContextLine, FileRecord, IndexFile};

use crate::archive::{ArchiveManager, ChunkRef};

pub mod search;
pub mod stats;
pub mod tree;

pub use search::{
    document_candidates, fetch_aliases_for_canonical_ids, fts_candidates, fts_count, DateFilter,
};
pub use stats::{
    append_scan_history, clear_errors_for_paths, get_fts_row_count, get_indexing_error,
    get_indexing_error_count, get_indexing_errors, get_scan_history, get_stats, get_stats_by_ext,
    upsert_indexing_errors,
};
pub use tree::{list_dir, split_composite_path};

// ── Schema ────────────────────────────────────────────────────────────────────

/// The current schema version. Stored in SQLite's built-in `user_version` pragma.
/// Increment this whenever the schema changes incompatibly.
pub const SCHEMA_VERSION: i64 = 8;

pub fn open(db_path: &Path) -> Result<Connection> {
    let conn = Connection::open(db_path)
        .with_context(|| format!("opening {}", db_path.display()))?;
    register_scalar_functions(&conn)?;

    let version: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    if version == 0 {
        // Brand-new database — initialise the full current schema and stamp the version.
        conn.execute_batch(include_str!("../schema_v2.sql"))
            .context("initialising schema")?;
        conn.execute_batch(&format!("PRAGMA user_version = {SCHEMA_VERSION};"))
            .context("stamping schema version")?;
    } else if version == 6 {
        // v6 → v7: make files.size nullable (archive members store NULL instead of 0
        // when individual member sizes are not available).
        conn.execute_batch(
            "PRAGMA foreign_keys=OFF;
             CREATE TABLE files_v7 (
                 id                INTEGER PRIMARY KEY AUTOINCREMENT,
                 path              TEXT    NOT NULL UNIQUE,
                 mtime             INTEGER NOT NULL,
                 size              INTEGER,
                 kind              TEXT    NOT NULL DEFAULT 'text',
                 indexed_at        INTEGER,
                 extract_ms        INTEGER,
                 content_hash      TEXT,
                 canonical_file_id INTEGER REFERENCES files_v7(id) ON DELETE SET NULL
             );
             INSERT INTO files_v7 SELECT * FROM files;
             DROP TABLE files;
             ALTER TABLE files_v7 RENAME TO files;
             CREATE INDEX IF NOT EXISTS files_content_hash ON files(content_hash)
                 WHERE content_hash IS NOT NULL;
             CREATE INDEX IF NOT EXISTS files_canonical ON files(canonical_file_id)
                 WHERE canonical_file_id IS NOT NULL;
             CREATE INDEX IF NOT EXISTS idx_files_mtime ON files(mtime);
             PRAGMA foreign_keys=ON;",
        ).context("migrating schema v6 → v7")?;
        // fall through to v7 → v8
        conn.execute_batch(
            "ALTER TABLE files ADD COLUMN scanner_version INTEGER NOT NULL DEFAULT 0;",
        ).context("migrating schema v7 → v8")?;
        conn.execute_batch(&format!("PRAGMA user_version = {SCHEMA_VERSION};"))
            .context("stamping schema version")?;
    } else if version == 7 {
        // v7 → v8: add scanner_version column so --upgrade can selectively
        // re-index files extracted by an older version of the client.
        conn.execute_batch(
            "ALTER TABLE files ADD COLUMN scanner_version INTEGER NOT NULL DEFAULT 0;",
        ).context("migrating schema v7 → v8")?;
        conn.execute_batch(&format!("PRAGMA user_version = {SCHEMA_VERSION};"))
            .context("stamping schema version")?;
    } else if version != SCHEMA_VERSION {
        anyhow::bail!(
            "database schema is v{version} but this server requires v{SCHEMA_VERSION}. \
             Delete {} and re-run find-scan to rebuild.",
            db_path.display()
        );
    }

    // Idempotent index additions — safe to run on existing databases at any version.
    conn.execute_batch(
        "CREATE INDEX IF NOT EXISTS idx_files_mtime ON files(mtime);"
    ).context("creating mtime index")?;

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
        open(&path).with_context(|| format!("migrating {}", path.display()))?;
    }
    Ok(())
}

// ── Chunk-read helper ─────────────────────────────────────────────────────────

/// Look up `(chunk_archive, chunk_name)` in `cache`; on miss, read the chunk
/// from `archive_mgr`, split into lines, and store it.
/// Returns a reference to the cached line vector.
pub(crate) fn read_chunk_lines<'a>(
    cache: &'a mut HashMap<(String, String), Vec<String>>,
    archive_mgr: &ArchiveManager,
    chunk_archive: &str,
    chunk_name: &str,
) -> &'a Vec<String> {
    let key = (chunk_archive.to_owned(), chunk_name.to_owned());
    cache.entry(key).or_insert_with(|| {
        let chunk_ref = ChunkRef {
            archive_name: chunk_archive.to_owned(),
            chunk_name: chunk_name.to_owned(),
        };
        let text = archive_mgr.read_chunk(&chunk_ref).unwrap_or_default();
        text.lines().map(|l| l.to_string()).collect()
    })
}

// ── Source-level helpers ──────────────────────────────────────────────────────

/// Collect all chunk refs from every line in this source database.
/// Used by the source-delete route to clean up ZIP archives.
pub fn collect_all_chunk_refs(conn: &Connection) -> Result<Vec<ChunkRef>> {
    let mut stmt = conn.prepare(
        "SELECT DISTINCT chunk_archive, chunk_name FROM lines",
    )?;
    let refs = stmt
        .query_map([], |row| {
            Ok(ChunkRef { archive_name: row.get(0)?, chunk_name: row.get(1)? })
        })?
        .collect::<rusqlite::Result<_>>()?;
    Ok(refs)
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

// ── File listing (for deletion detection) ────────────────────────────────────

pub fn list_files(conn: &Connection) -> Result<Vec<FileRecord>> {
    let mut stmt = conn.prepare(
        "SELECT path, mtime, kind, scanner_version FROM files ORDER BY path"
    )?;
    let rows = stmt
        .query_map([], |row| {
            Ok(FileRecord {
                path: row.get(0)?,
                mtime: row.get(1)?,
                kind: row.get(2)?,
                scanner_version: row.get::<_, u32>(3).unwrap_or(0),
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
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
            params![file.path, file.mtime, file.size.as_ref().map(|&s| s), file.kind, file.scanner_version],
        )?;

        let file_id: i64 = tx.query_row(
            "SELECT id FROM files WHERE path = ?1",
            params![file.path],
            |row| row.get(0),
        )?;

        tx.execute("DELETE FROM lines WHERE file_id = ?1", params![file_id])?;

        {
            let mut stmt = tx.prepare_cached(
                "INSERT INTO lines (file_id, line_number, content)
                 VALUES (?1, ?2, ?3)",
            )?;
            for line in &file.lines {
                stmt.execute(params![
                    file_id,
                    line.line_number as i64,
                    line.content,
                ])?;
            }
        }
    }

    tx.commit()?;
    Ok(())
}

// ── Delete ────────────────────────────────────────────────────────────────────

pub fn delete_files(
    conn: &Connection,
    archive_mgr: &mut crate::archive::ArchiveManager,
    paths: &[String],
) -> Result<()> {
    let tx = conn.unchecked_transaction()?;

    for path in paths {
        delete_one_path(&tx, archive_mgr, path)?;
    }

    tx.commit()?;
    Ok(())
}

/// Delete one path (outer file + all inner archive members), with canonical promotion.
fn delete_one_path(
    tx: &rusqlite::Transaction,
    archive_mgr: &mut crate::archive::ArchiveManager,
    path: &str,
) -> Result<()> {
    // Look up the outer file's id and canonical_file_id.
    let outer: Option<(i64, Option<i64>)> = tx.query_row(
        "SELECT id, canonical_file_id FROM files WHERE path = ?1",
        params![path],
        |row| Ok((row.get::<_, i64>(0)?, row.get::<_, Option<i64>>(1)?)),
    ).optional()?;

    let Some((outer_id, outer_canonical_id)) = outer else {
        return Ok(()); // nothing to delete
    };

    // Delete all inner archive members first (path LIKE 'x::%').
    // These are bulk-deleted without canonical promotion (they self-heal on next scan).
    let inner_refs = collect_chunk_refs_for_pattern(tx, &format!("{}::%", path))?;
    tx.execute(
        "DELETE FROM files WHERE path LIKE ?1",
        params![format!("{}::%", path)],
    )?;
    if !inner_refs.is_empty() {
        archive_mgr.remove_chunks(inner_refs)?;
    }

    // Now delete the outer file itself.
    if outer_canonical_id.is_some() {
        // Outer file is an alias — cheap deletion, no chunks to remove.
        tx.execute("DELETE FROM files WHERE id = ?1", params![outer_id])?;
    } else {
        // Outer file is canonical — check for aliases that need promotion.
        let aliases: Vec<(i64, String)> = {
            let mut stmt = tx.prepare(
                "SELECT id, path FROM files WHERE canonical_file_id = ?1 ORDER BY id",
            )?;
            let v: Vec<(i64, String)> = stmt.query_map(params![outer_id], |row| Ok((row.get(0)?, row.get(1)?)))?
                .collect::<rusqlite::Result<_>>()?;
            v
        };

        if aliases.is_empty() {
            // No aliases — normal deletion: remove chunks then delete the row.
            let refs = collect_chunk_refs_for_file(tx, outer_id)?;
            delete_fts_for_file(tx, archive_mgr, outer_id)?;
            tx.execute("DELETE FROM files WHERE id = ?1", params![outer_id])?;
            if !refs.is_empty() {
                archive_mgr.remove_chunks(refs)?;
            }
        } else {
            // Canonical promotion: promote the first alias to canonical.
            let (new_canonical_id, _new_canonical_path) = &aliases[0];
            let new_canonical_id = *new_canonical_id;

            // Fetch the canonical's lines before deletion.
            struct LineRow {
                id: i64,
                line_number: i64,
                chunk_archive: String,
                chunk_name: String,
                line_offset: i64,
            }
            let old_lines: Vec<LineRow> = {
                let mut stmt = tx.prepare(
                    "SELECT id, line_number, chunk_archive, chunk_name, line_offset_in_chunk
                     FROM lines WHERE file_id = ?1 ORDER BY line_number",
                )?;
                let v: Vec<LineRow> = stmt.query_map(params![outer_id], |row| Ok(LineRow {
                    id:           row.get(0)?,
                    line_number:  row.get(1)?,
                    chunk_archive: row.get(2)?,
                    chunk_name:   row.get(3)?,
                    line_offset:  row.get(4)?,
                }))?
                .collect::<rusqlite::Result<_>>()?;
                v
            };

            // Read content for each line (needed to re-insert FTS entries).
            let mut chunk_cache: HashMap<(String, String), Vec<String>> = HashMap::new();
            let mut line_contents: Vec<(LineRow, String)> = Vec::new();
            for line_row in old_lines {
                let content = read_chunk_lines(
                    &mut chunk_cache, archive_mgr,
                    &line_row.chunk_archive, &line_row.chunk_name,
                )
                .get(line_row.line_offset as usize)
                .cloned()
                .unwrap_or_default();
                line_contents.push((line_row, content));
            }

            // Delete FTS entries for the old canonical's lines (contentless FTS5 requires manual delete).
            for (line_row, content) in &line_contents {
                tx.execute(
                    "INSERT INTO lines_fts(lines_fts, rowid, content) VALUES('delete', ?1, ?2)",
                    params![line_row.id, content],
                )?;
            }

            // Delete the old canonical's files row (CASCADE removes its lines).
            tx.execute("DELETE FROM files WHERE id = ?1", params![outer_id])?;

            // Promote the first alias to canonical.
            tx.execute(
                "UPDATE files SET canonical_file_id = NULL WHERE id = ?1",
                params![new_canonical_id],
            )?;
            // Re-point remaining aliases to the new canonical.
            for (alias_id, _) in aliases.iter().skip(1) {
                tx.execute(
                    "UPDATE files SET canonical_file_id = ?1 WHERE id = ?2",
                    params![new_canonical_id, alias_id],
                )?;
            }

            // Insert lines for the new canonical (reusing old chunk refs).
            for (line_row, content) in &line_contents {
                let new_line_id: i64 = tx.query_row(
                    "INSERT INTO lines (file_id, line_number, chunk_archive, chunk_name, line_offset_in_chunk)
                     VALUES (?1, ?2, ?3, ?4, ?5)
                     RETURNING id",
                    params![
                        new_canonical_id,
                        line_row.line_number,
                        line_row.chunk_archive,
                        line_row.chunk_name,
                        line_row.line_offset,
                    ],
                    |row| row.get(0),
                )?;
                tx.execute(
                    "INSERT INTO lines_fts(rowid, content) VALUES (?1, ?2)",
                    params![new_line_id, content],
                )?;
            }
            // No ZIP rewrite — old chunk files remain valid under new line references.
        }
    }

    Ok(())
}

/// Collect chunk refs for a file by ID.
fn collect_chunk_refs_for_file(
    tx: &rusqlite::Transaction,
    file_id: i64,
) -> Result<Vec<crate::archive::ChunkRef>> {
    let mut stmt = tx.prepare(
        "SELECT DISTINCT chunk_archive, chunk_name FROM lines WHERE file_id = ?1",
    )?;
    let refs = stmt.query_map(params![file_id], |row| {
        Ok(crate::archive::ChunkRef { archive_name: row.get(0)?, chunk_name: row.get(1)? })
    })?
    .collect::<rusqlite::Result<_>>()?;
    Ok(refs)
}

/// Collect chunk refs for all files matching a LIKE pattern.
fn collect_chunk_refs_for_pattern(
    tx: &rusqlite::Transaction,
    like_pat: &str,
) -> Result<Vec<crate::archive::ChunkRef>> {
    let mut refs = Vec::new();
    let file_ids: Vec<i64> = {
        let mut stmt = tx.prepare("SELECT id FROM files WHERE path LIKE ?1")?;
        let ids: Vec<i64> = stmt.query_map(params![like_pat], |row| row.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        ids
    };
    for fid in file_ids {
        refs.extend(collect_chunk_refs_for_file(tx, fid)?);
    }
    Ok(refs)
}

/// Delete FTS entries for all lines belonging to a file.
fn delete_fts_for_file(
    tx: &rusqlite::Transaction,
    archive_mgr: &crate::archive::ArchiveManager,
    file_id: i64,
) -> Result<()> {
    struct LineRef { id: i64, chunk_archive: String, chunk_name: String, line_offset: i64 }
    let line_refs: Vec<LineRef> = {
        let mut stmt = tx.prepare(
            "SELECT id, chunk_archive, chunk_name, line_offset_in_chunk FROM lines WHERE file_id = ?1",
        )?;
        let refs: Vec<LineRef> = stmt.query_map(params![file_id], |row| Ok(LineRef {
            id: row.get(0)?,
            chunk_archive: row.get(1)?,
            chunk_name: row.get(2)?,
            line_offset: row.get(3)?,
        }))?
        .collect::<rusqlite::Result<_>>()?;
        refs
    };

    let mut chunk_cache: HashMap<(String, String), Vec<String>> = HashMap::new();
    for lr in &line_refs {
        let content = read_chunk_lines(
            &mut chunk_cache, archive_mgr,
            &lr.chunk_archive, &lr.chunk_name,
        )
        .get(lr.line_offset as usize)
        .cloned()
        .unwrap_or_default();
        tx.execute(
            "INSERT INTO lines_fts(lines_fts, rowid, content) VALUES('delete', ?1, ?2)",
            params![lr.id, content],
        )?;
    }
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

/// Resolve the effective file_id for a path, following canonical_file_id if the
/// file is a dedup alias.  Returns None if the path is not in the files table.
fn resolve_file_id(conn: &Connection, path: &str) -> rusqlite::Result<Option<i64>> {
    conn.query_row(
        "SELECT COALESCE(canonical_file_id, id) FROM files WHERE path = ?1",
        params![path],
        |row| row.get(0),
    )
    .optional()
}

/// Returns every indexed line for a file, ordered by line number.
/// `path` may be a composite path ("archive.zip::member.txt") or a plain path.
/// Follows canonical_file_id so dedup aliases show the same content as the canonical.
pub fn get_file_lines(
    conn: &Connection,
    archive_mgr: &ArchiveManager,
    path: &str,
) -> Result<Vec<ContextLine>> {
    let Some(file_id) = resolve_file_id(conn, path)? else {
        return Ok(vec![]);
    };

    let mut stmt = conn.prepare(
        "SELECT l.line_number, l.chunk_archive, l.chunk_name, l.line_offset_in_chunk
         FROM lines l
         WHERE l.file_id = ?1
         ORDER BY l.line_number",
    )?;

    let rows: Vec<(usize, String, String, usize)> = stmt
        .query_map(params![file_id], |row| {
            Ok((
                row.get::<_, i64>(0)? as usize,
                row.get(1)?,
                row.get(2)?,
                row.get::<_, i64>(3)? as usize,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;

    let mut lines = resolve_content(archive_mgr, rows);

    // Inject synthetic line_number=0 entries for all alias paths of this
    // canonical file.  The existing line_number=0 entry (the canonical's own
    // path, stored in the ZIP) is already in `lines`.  Adding the alias paths
    // here means both the canonical view and any alias view show the full set
    // of duplicate paths — the UI filters out whichever one matches the
    // currently-viewed file and labels the rest as "DUPLICATE".
    let mut alias_stmt = conn.prepare(
        "SELECT path FROM files WHERE canonical_file_id = ?1 ORDER BY path",
    )?;
    let alias_paths: Vec<String> = alias_stmt
        .query_map(params![file_id], |row| row.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    for alias_path in alias_paths {
        lines.push(ContextLine { line_number: 0, content: alias_path });
    }

    Ok(lines)
}

// ── Context ───────────────────────────────────────────────────────────────────

pub fn get_context(
    conn: &Connection,
    archive_mgr: &ArchiveManager,
    file_path: &str,
    center: usize,
    window: usize,
) -> Result<Vec<ContextLine>> {
    let kind = get_file_kind(conn, file_path)?;

    match kind.as_str() {
        "image" | "audio" => get_metadata_context(conn, archive_mgr, file_path),
        _ => get_line_context(conn, archive_mgr, file_path, center, window),
    }
}

fn get_file_kind(conn: &Connection, file_path: &str) -> Result<String> {
    conn.query_row(
        "SELECT kind FROM files WHERE path = ?1 LIMIT 1",
        params![file_path],
        |row| row.get(0),
    )
    .map_err(Into::into)
}

fn get_metadata_context(
    conn: &Connection,
    archive_mgr: &ArchiveManager,
    file_path: &str,
) -> Result<Vec<ContextLine>> {
    let Some(file_id) = resolve_file_id(conn, file_path)? else {
        return Ok(vec![]);
    };

    let mut stmt = conn.prepare(
        "SELECT l.line_number, l.chunk_archive, l.chunk_name, l.line_offset_in_chunk
         FROM lines l
         WHERE l.file_id = ?1
           AND l.line_number = 0
         ORDER BY l.id",
    )?;

    let rows: Vec<(usize, String, String, usize)> = stmt
        .query_map(params![file_id], |row| {
            Ok((
                row.get::<_, i64>(0)? as usize,
                row.get(1)?,
                row.get(2)?,
                row.get::<_, i64>(3)? as usize,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;

    Ok(resolve_content(archive_mgr, rows))
}

fn get_line_context(
    conn: &Connection,
    archive_mgr: &ArchiveManager,
    file_path: &str,
    center: usize,
    window: usize,
) -> Result<Vec<ContextLine>> {
    let Some(file_id) = resolve_file_id(conn, file_path)? else {
        return Ok(vec![]);
    };

    let lo = center.saturating_sub(window) as i64;
    let hi = (center + window) as i64;

    let mut stmt = conn.prepare(
        "SELECT l.line_number, l.chunk_archive, l.chunk_name, l.line_offset_in_chunk
         FROM lines l
         WHERE l.file_id = ?1
           AND l.line_number BETWEEN ?2 AND ?3
         ORDER BY l.line_number",
    )?;

    let rows: Vec<(usize, String, String, usize)> = stmt
        .query_map(params![file_id, lo, hi], |row| {
            Ok((
                row.get::<_, i64>(0)? as usize,
                row.get(1)?,
                row.get(2)?,
                row.get::<_, i64>(3)? as usize,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;

    Ok(resolve_content(archive_mgr, rows))
}

/// Read line content from ZIP archives, caching chunks to avoid redundant reads.
fn resolve_content(
    archive_mgr: &ArchiveManager,
    rows: Vec<(usize, String, String, usize)>,
) -> Vec<ContextLine> {
    let mut cache: HashMap<(String, String), Vec<String>> = HashMap::new();
    rows.into_iter()
        .map(|(line_number, chunk_archive, chunk_name, offset)| {
            let content = read_chunk_lines(&mut cache, archive_mgr, &chunk_archive, &chunk_name)
                .get(offset)
                .cloned()
                .unwrap_or_default();
            ContextLine { line_number, content }
        })
        .collect()
}
