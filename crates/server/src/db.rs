#![allow(dead_code)] // some helpers reserved for future endpoints

use std::collections::HashMap;
use std::path::Path;

use anyhow::{Context, Result};
use rusqlite::{Connection, OptionalExtension, params};

use find_common::api::{ContextLine, DirEntry, FileRecord, IndexFile, IndexingError, IndexingFailure, KindStats, ScanHistoryPoint};

use crate::archive::{ArchiveManager, ChunkRef};

// ── Schema ────────────────────────────────────────────────────────────────────

/// The current schema version. Stored in SQLite's built-in `user_version` pragma.
/// Increment this whenever the schema changes incompatibly.
pub const SCHEMA_VERSION: i64 = 6;

pub fn open(db_path: &Path) -> Result<Connection> {
    let conn = Connection::open(db_path)
        .with_context(|| format!("opening {}", db_path.display()))?;
    check_schema_version(&conn, db_path)?;
    conn.execute_batch(include_str!("schema_v2.sql"))
        .context("initialising schema")?;
    migrate_v3(&conn).context("v3 migration")?;
    migrate_v4(&conn).context("v4 migration")?;
    migrate_v5(&conn).context("v5 migration")?;
    migrate_v6(&conn).context("v6 migration")?;
    Ok(conn)
}

/// Check that the database schema version is compatible before touching any tables.
/// Fails with a clear message if the DB is from an incompatible version.
fn check_schema_version(conn: &Connection, db_path: &Path) -> Result<()> {
    let version: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;

    // Current version: fine.
    if version == SCHEMA_VERSION {
        return Ok(());
    }

    // v3, v4, and v5 are migratable forward via the migration chain.
    if version == 3 || version == 4 || version == 5 {
        return Ok(());
    }

    // Known incompatible future version or very old version.
    if version != 0 {
        anyhow::bail!(
            "database schema is v{version} but this server requires v{SCHEMA_VERSION}. \
             Delete {} and re-run `find-scan --full` to rebuild.",
            db_path.display()
        );
    }

    // version == 0: either a brand-new empty DB (fine) or a pre-versioned old DB (not fine).
    // Distinguish by checking whether `lines` exists without the `chunk_archive` column.
    let lines_exists: bool = conn
        .query_row(
            "SELECT count(*) FROM sqlite_master WHERE type='table' AND name='lines'",
            [],
            |r| r.get::<_, i64>(0),
        )
        .unwrap_or(0)
        > 0;

    if lines_exists && conn.prepare("SELECT chunk_archive FROM lines LIMIT 0").is_err() {
        anyhow::bail!(
            "database has an incompatible schema (predates chunk-based storage). \
             Delete {} and re-run `find-scan --full` to rebuild.",
            db_path.display()
        );
    }

    Ok(())
}

fn migrate_v3(conn: &Connection) -> Result<()> {
    let version: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    if version >= 3 {
        return Ok(());
    }
    conn.execute_batch(
        "ALTER TABLE files ADD COLUMN indexed_at INTEGER;
         ALTER TABLE files ADD COLUMN extract_ms INTEGER;
         CREATE TABLE IF NOT EXISTS scan_history (
             id          INTEGER PRIMARY KEY AUTOINCREMENT,
             scanned_at  INTEGER NOT NULL,
             total_files INTEGER NOT NULL,
             total_size  INTEGER NOT NULL,
             by_kind     TEXT    NOT NULL
         );
         PRAGMA user_version = 3;",
    )?;
    Ok(())
}

fn migrate_v4(conn: &Connection) -> Result<()> {
    let version: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    if version >= 4 {
        return Ok(());
    }
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS indexing_errors (
             id         INTEGER PRIMARY KEY AUTOINCREMENT,
             path       TEXT    NOT NULL UNIQUE,
             error      TEXT    NOT NULL,
             first_seen INTEGER NOT NULL,
             last_seen  INTEGER NOT NULL,
             count      INTEGER NOT NULL DEFAULT 1
         );
         PRAGMA user_version = 4;",
    )?;
    Ok(())
}

fn migrate_v5(conn: &Connection) -> Result<()> {
    let version: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    if version >= 5 {
        return Ok(());
    }
    conn.execute_batch(
        "ALTER TABLE files ADD COLUMN content_hash TEXT;
         ALTER TABLE files ADD COLUMN canonical_file_id INTEGER REFERENCES files(id) ON DELETE SET NULL;
         CREATE INDEX IF NOT EXISTS files_content_hash ON files(content_hash)
             WHERE content_hash IS NOT NULL;
         CREATE INDEX IF NOT EXISTS files_canonical ON files(canonical_file_id)
             WHERE canonical_file_id IS NOT NULL;
         PRAGMA user_version = 5;",
    )?;
    Ok(())
}

fn migrate_v6(conn: &Connection) -> Result<()> {
    let version: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    if version >= 6 {
        return Ok(());
    }
    // The lines_fts table may have been created with the default unicode61 tokenizer
    // if the database predates the trigram tokenizer being added to schema_v2.sql.
    // The `CREATE VIRTUAL TABLE IF NOT EXISTS` in the schema silently skips recreation,
    // leaving the old tokenizer in place. Drop and recreate to force trigram.
    conn.execute_batch(
        "DROP TABLE IF EXISTS lines_fts;
         CREATE VIRTUAL TABLE lines_fts USING fts5(
             content,
             content       = '',
             tokenize      = 'trigram'
         );
         PRAGMA user_version = 6;",
    )?;
    Ok(())
}

/// Check all existing source databases in `sources_dir` for schema compatibility.
/// Called at server startup so an incompatible DB causes an immediate fatal error
/// rather than a runtime warning when the first request arrives.
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
        let conn = Connection::open(&path)
            .with_context(|| format!("opening {}", path.display()))?;
        check_schema_version(&conn, &path)?;
    }
    Ok(())
}

// ── File listing (for deletion detection) ────────────────────────────────────

pub fn list_files(conn: &Connection) -> Result<Vec<FileRecord>> {
    let mut stmt = conn.prepare("SELECT path, mtime, kind FROM files ORDER BY path")?;
    let rows = stmt
        .query_map([], |row| {
            Ok(FileRecord {
                path: row.get(0)?,
                mtime: row.get(1)?,
                kind: row.get(2)?,
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
            "INSERT INTO files (path, mtime, size, kind)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(path) DO UPDATE SET
               mtime = excluded.mtime,
               size  = excluded.size,
               kind  = excluded.kind",
            params![file.path, file.mtime, file.size, file.kind],
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
                let key = (line_row.chunk_archive.clone(), line_row.chunk_name.clone());
                if !chunk_cache.contains_key(&key) {
                    let chunk_ref = ChunkRef { archive_name: key.0.clone(), chunk_name: key.1.clone() };
                    let text = archive_mgr.read_chunk(&chunk_ref).unwrap_or_default();
                    chunk_cache.insert(key.clone(), text.lines().map(|l| l.to_string()).collect());
                }
                let content = chunk_cache[&key].get(line_row.line_offset as usize).cloned().unwrap_or_default();
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
        let key = (lr.chunk_archive.clone(), lr.chunk_name.clone());
        if !chunk_cache.contains_key(&key) {
            let chunk_ref = ChunkRef { archive_name: key.0.clone(), chunk_name: key.1.clone() };
            let text = archive_mgr.read_chunk(&chunk_ref).unwrap_or_default();
            chunk_cache.insert(key.clone(), text.lines().map(|l| l.to_string()).collect());
        }
        let content = chunk_cache[&key].get(lr.line_offset as usize).cloned().unwrap_or_default();
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

// ── Base URL ──────────────────────────────────────────────────────────────────

pub fn update_base_url(conn: &Connection, base_url: Option<&str>) -> Result<()> {
    if let Some(url) = base_url {
        conn.execute(
            "INSERT INTO meta (key, value) VALUES ('base_url', ?1)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![url],
        )?;
    } else {
        conn.execute("DELETE FROM meta WHERE key = 'base_url'", [])?;
    }
    Ok(())
}

pub fn get_base_url(conn: &Connection) -> Result<Option<String>> {
    let result = conn.query_row(
        "SELECT value FROM meta WHERE key = 'base_url'",
        [],
        |row| row.get(0),
    );
    match result {
        Ok(s) => Ok(Some(s)),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(e) => Err(e.into()),
    }
}

// ── Search ────────────────────────────────────────────────────────────────────

pub struct CandidateRow {
    /// Full path, potentially composite ("archive.zip::member.txt").
    pub file_path: String,
    pub file_kind: String,
    /// For archive members: the part after the first "::".
    /// For outer files: None.
    pub archive_path: Option<String>,
    pub line_number: usize,
    pub content: String,
    /// The file's row ID in the `files` table (used for alias lookup).
    pub file_id: i64,
}

/// FTS5 trigram pre-filter.  Returns up to `limit` candidate rows.
/// Build an FTS5 match expression from a raw query string.
/// Returns None if the query produces no matchable terms.
fn build_fts_query(query: &str, phrase: bool) -> Option<String> {
    if phrase {
        if query.len() < 3 {
            return None;
        }
        Some(format!("\"{}\"", query.replace('"', "\"\"")))
    } else {
        // Use unquoted terms so FTS5 treats each word as a token query rather
        // than a phrase query.  Quoted phrases require ≥3 trigrams to match
        // (i.e. the term must be ≥5 chars), which breaks short-word searches
        // like "test" (4 chars, 2 trigrams).  Unquoted token queries have no
        // such minimum.  Strip FTS5 syntax characters to avoid query errors.
        let terms: Vec<String> = query
            .split_whitespace()
            .map(|w| w.chars().filter(|c| !matches!(c, '"' | '*' | '(' | ')' | '^')).collect::<String>())
            .filter(|w| w.len() >= 3)
            .collect();
        if terms.is_empty() {
            return None;
        }
        Some(terms.join(" AND "))
    }
}

/// Fast FTS5-only count, capped at `limit`. No ZIP reads, no JOINs.
/// Used to compute the approximate total result count efficiently.
pub fn fts_count(conn: &Connection, query: &str, limit: usize, phrase: bool) -> Result<usize> {
    let Some(fts_query) = build_fts_query(query, phrase) else {
        return Ok(0);
    };
    let count: i64 = conn.query_row(
        "SELECT count(*) FROM (SELECT 1 FROM lines_fts WHERE lines_fts MATCH ?1 LIMIT ?2)",
        params![fts_query, limit as i64],
        |row| row.get(0),
    )?;
    Ok(count as usize)
}

pub fn fts_candidates(
    conn: &Connection,
    archive_mgr: &ArchiveManager,
    query: &str,
    limit: usize,
    phrase: bool,
) -> Result<Vec<CandidateRow>> {
    let Some(fts_query) = build_fts_query(query, phrase) else {
        return Ok(vec![]);
    };

    struct RawRow {
        file_path: String,
        file_kind: String,
        line_number: usize,
        chunk_archive: String,
        chunk_name: String,
        line_offset: usize,
        file_id: i64,
    }

    let mut stmt = conn.prepare(
        "SELECT f.path, f.kind, l.line_number,
                l.chunk_archive, l.chunk_name, l.line_offset_in_chunk, f.id
         FROM lines_fts
         JOIN lines l ON l.id = lines_fts.rowid
         JOIN files f ON f.id = l.file_id
         WHERE lines_fts MATCH ?1
         LIMIT ?2",
    )?;

    let raw: Vec<RawRow> = stmt
        .query_map(params![fts_query, limit as i64], |row| {
            Ok(RawRow {
                file_path:    row.get(0)?,
                file_kind:    row.get(1)?,
                line_number:  row.get::<_, i64>(2)? as usize,
                chunk_archive: row.get(3)?,
                chunk_name:   row.get(4)?,
                line_offset:  row.get::<_, i64>(5)? as usize,
                file_id:      row.get(6)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;

    // Read content from ZIP archives, caching chunks to avoid redundant reads.
    let mut chunk_cache: HashMap<(String, String), Vec<String>> = HashMap::new();
    let mut results = Vec::with_capacity(raw.len());

    for row in raw {
        let key = (row.chunk_archive.clone(), row.chunk_name.clone());
        if !chunk_cache.contains_key(&key) {
            let chunk_ref = ChunkRef { archive_name: key.0.clone(), chunk_name: key.1.clone() };
            let text = archive_mgr.read_chunk(&chunk_ref).unwrap_or_default();
            chunk_cache.insert(key.clone(), text.lines().map(|l| l.to_string()).collect());
        }
        let content = chunk_cache[&key].get(row.line_offset).cloned().unwrap_or_default();

        // Split composite path into outer path + archive_path for search result compat.
        let (file_path, archive_path) = split_composite_path(&row.file_path);

        results.push(CandidateRow {
            file_path,
            file_kind:    row.file_kind,
            archive_path,
            line_number:  row.line_number,
            content,
            file_id:      row.file_id,
        });
    }

    Ok(results)
}

/// Return type for `document_candidates`: total qualifying files + per-file (representative, extras).
pub type DocumentCandidates = (usize, Vec<(CandidateRow, Vec<CandidateRow>)>);

/// Document-level fuzzy candidate search.
///
/// Unlike `fts_candidates` (which requires all query terms on the *same* line),
/// this finds files where each query term appears on *any* line, then surfaces
/// one result per file with extra_matches carrying the best line per remaining token.
///
/// Returns `(total, Vec<(representative, extra_matches)>)`.
/// `total` is the number of qualifying files before the limit is applied.
pub fn document_candidates(
    conn: &Connection,
    archive_mgr: &ArchiveManager,
    query: &str,
    limit: usize,
) -> Result<DocumentCandidates> {
    use std::collections::HashSet;

    let tokens: Vec<String> = query
        .split_whitespace()
        .filter(|w| w.len() >= 3)
        .map(|w| w.to_string())
        .collect();

    if tokens.is_empty() {
        return Ok((0, vec![]));
    }

    // For each token, collect the set of file_ids that have at least one matching line.
    let mut per_token_ids: Vec<HashSet<i64>> = Vec::new();
    for token in &tokens {
        let fts_expr = format!("\"{}\"", token.replace('"', "\"\""));
        let mut stmt = conn.prepare(
            "SELECT DISTINCT l.file_id
             FROM lines_fts
             JOIN lines l ON l.id = lines_fts.rowid
             WHERE lines_fts MATCH ?1
             LIMIT 100000",
        )?;
        let ids: HashSet<i64> = stmt
            .query_map(params![fts_expr], |row| row.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        per_token_ids.push(ids);
    }

    // Intersect: files that have ALL tokens somewhere.
    let qualifying_ids: HashSet<i64> = per_token_ids
        .into_iter()
        .reduce(|a, b| a.intersection(&b).copied().collect())
        .unwrap_or_default();

    let total = qualifying_ids.len();
    if total == 0 {
        return Ok((0, vec![]));
    }

    let or_expr = tokens
        .iter()
        .map(|t| format!("\"{}\"", t.replace('"', "\"\"")))
        .collect::<Vec<_>>()
        .join(" OR ");

    // Fetch up to `tokens.len()` lines per qualifying file so we can pick the best
    // line per token. We need enough rows to fill `limit` files × N tokens.
    let per_file_cap = tokens.len().max(1);
    let fetch_limit = (limit * 20 * per_file_cap).max(10_000) as i64;

    struct RawRow {
        file_path: String,
        file_kind: String,
        line_number: usize,
        chunk_archive: String,
        chunk_name: String,
        line_offset: usize,
        file_id: i64,
    }

    let mut stmt = conn.prepare(
        "SELECT f.path, f.kind, l.line_number,
                l.chunk_archive, l.chunk_name, l.line_offset_in_chunk, f.id
         FROM lines_fts
         JOIN lines l ON l.id = lines_fts.rowid
         JOIN files f ON f.id = l.file_id
         WHERE lines_fts MATCH ?1
         ORDER BY lines_fts.rank
         LIMIT ?2",
    )?;

    // Collect up to `per_file_cap` raw rows per qualifying file.
    let mut file_rows: HashMap<i64, Vec<RawRow>> = HashMap::new();
    let mut file_order: Vec<i64> = Vec::new(); // insertion order for stable output

    let mut rows = stmt.query(params![or_expr, fetch_limit])?;
    while let Some(row) = rows.next()? {
        let file_id: i64 = row.get(6)?;
        if !qualifying_ids.contains(&file_id) {
            continue;
        }
        let entry = file_rows.entry(file_id).or_insert_with(|| {
            file_order.push(file_id);
            Vec::new()
        });
        if entry.len() < per_file_cap {
            entry.push(RawRow {
                file_path:    row.get(0)?,
                file_kind:    row.get(1)?,
                line_number:  row.get::<_, i64>(2)? as usize,
                chunk_archive: row.get(3)?,
                chunk_name:   row.get(4)?,
                line_offset:  row.get::<_, i64>(5)? as usize,
                file_id,
            });
        }
        if file_order.len() >= limit && file_rows.get(&file_order[file_order.len()-1]).map_or(0, |v| v.len()) >= per_file_cap {
            break;
        }
    }

    // Read content from ZIP archives, reusing a chunk cache.
    let mut chunk_cache: HashMap<(String, String), Vec<String>> = HashMap::new();

    let read_content = |row: &RawRow, cache: &mut HashMap<(String, String), Vec<String>>| -> String {
        let key = (row.chunk_archive.clone(), row.chunk_name.clone());
        if !cache.contains_key(&key) {
            let chunk_ref = ChunkRef { archive_name: key.0.clone(), chunk_name: key.1.clone() };
            let text = archive_mgr.read_chunk(&chunk_ref).unwrap_or_default();
            cache.insert(key.clone(), text.lines().map(|l| l.to_string()).collect());
        }
        cache[&key].get(row.line_offset).cloned().unwrap_or_default()
    };

    let tokens_lower: Vec<String> = tokens.iter().map(|t| t.to_lowercase()).collect();

    let mut results = Vec::new();
    for file_id in file_order.into_iter().take(limit) {
        let rows = match file_rows.remove(&file_id) {
            Some(r) => r,
            None => continue,
        };

        // First row is the top FTS-ranked line → the representative.
        let rep_row = &rows[0];
        let rep_content = read_content(rep_row, &mut chunk_cache);
        let rep_content_lower = rep_content.to_lowercase();
        let (file_path, archive_path) = split_composite_path(&rep_row.file_path);

        let representative = CandidateRow {
            file_path: file_path.clone(),
            file_kind: rep_row.file_kind.clone(),
            archive_path: archive_path.clone(),
            line_number: rep_row.line_number,
            content: rep_content,
            file_id,
        };

        // For each token not already covered by the representative, find the first
        // subsequent row that covers it (simple case-insensitive substring check).
        let mut uncovered: Vec<&str> = tokens_lower
            .iter()
            .filter(|t| !rep_content_lower.contains(t.as_str()))
            .map(|t| t.as_str())
            .collect();

        let mut extras: Vec<CandidateRow> = Vec::new();
        for extra_row in &rows[1..] {
            if uncovered.is_empty() {
                break;
            }
            let content = read_content(extra_row, &mut chunk_cache);
            let content_lower = content.to_lowercase();
            // Only include this row if it covers at least one new token.
            let newly_covered: Vec<usize> = uncovered
                .iter()
                .enumerate()
                .filter(|(_, t)| content_lower.contains(*t))
                .map(|(i, _)| i)
                .collect();
            if !newly_covered.is_empty() {
                // Skip line_number=0 (metadata/path lines) — not useful as highlights.
                if extra_row.line_number > 0 {
                    let (ep, ea) = split_composite_path(&extra_row.file_path);
                    extras.push(CandidateRow {
                        file_path: ep,
                        file_kind: extra_row.file_kind.clone(),
                        archive_path: ea,
                        line_number: extra_row.line_number,
                        content,
                        file_id,
                    });
                }
                // Remove newly covered tokens (iterate in reverse to preserve indices).
                for i in newly_covered.into_iter().rev() {
                    uncovered.swap_remove(i);
                }
            }
        }

        results.push((representative, extras));
    }

    Ok((total, results))
}

/// Fetch alias paths grouped by their canonical file ID.
/// Returns a map of canonical_id → list of alias paths.
pub fn fetch_aliases_for_canonical_ids(
    conn: &Connection,
    canonical_ids: &[i64],
) -> Result<HashMap<i64, Vec<String>>> {
    let mut map: HashMap<i64, Vec<String>> = HashMap::new();
    if canonical_ids.is_empty() {
        return Ok(map);
    }
    let mut stmt = conn.prepare(
        "SELECT canonical_file_id, path FROM files
         WHERE canonical_file_id = ?1
         ORDER BY path",
    )?;
    for &cid in canonical_ids {
        let paths: Vec<String> = stmt
            .query_map(params![cid], |row| row.get(1))?
            .collect::<rusqlite::Result<_>>()?;
        if !paths.is_empty() {
            map.insert(cid, paths);
        }
    }
    Ok(map)
}

/// Split a potentially composite path ("zip::member") into (outer_path, archive_path).
/// Returns (path, None) for non-composite paths.
pub fn split_composite_path(path: &str) -> (String, Option<String>) {
    if let Some(pos) = path.find("::") {
        (path[..pos].to_string(), Some(path[pos + 2..].to_string()))
    } else {
        (path.to_string(), None)
    }
}

// ── File lines ────────────────────────────────────────────────────────────────

/// Returns every indexed line for a file, ordered by line number.
/// `path` may be a composite path ("archive.zip::member.txt") or a plain path.
pub fn get_file_lines(
    conn: &Connection,
    archive_mgr: &ArchiveManager,
    path: &str,
) -> Result<Vec<ContextLine>> {
    let mut stmt = conn.prepare(
        "SELECT l.line_number, l.chunk_archive, l.chunk_name, l.line_offset_in_chunk
         FROM lines l
         JOIN files f ON f.id = l.file_id
         WHERE f.path = ?1
         ORDER BY l.line_number",
    )?;

    let rows: Vec<(usize, String, String, usize)> = stmt
        .query_map(params![path], |row| {
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
    let mut stmt = conn.prepare(
        "SELECT l.line_number, l.chunk_archive, l.chunk_name, l.line_offset_in_chunk
         FROM lines l
         JOIN files f ON f.id = l.file_id
         WHERE f.path = ?1
           AND l.line_number = 0
         ORDER BY l.id",
    )?;

    let rows: Vec<(usize, String, String, usize)> = stmt
        .query_map([file_path], |row| {
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
    let lo = center.saturating_sub(window) as i64;
    let hi = (center + window) as i64;

    let mut stmt = conn.prepare(
        "SELECT l.line_number, l.chunk_archive, l.chunk_name, l.line_offset_in_chunk
         FROM lines l
         JOIN files f ON f.id = l.file_id
         WHERE f.path = ?1
           AND l.line_number BETWEEN ?2 AND ?3
         ORDER BY l.line_number",
    )?;

    let rows: Vec<(usize, String, String, usize)> = stmt
        .query_map(params![file_path, lo, hi], |row| {
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
    let mut result = Vec::with_capacity(rows.len());

    for (line_number, chunk_archive, chunk_name, offset) in rows {
        let key = (chunk_archive.clone(), chunk_name.clone());
        if !cache.contains_key(&key) {
            let chunk_ref = ChunkRef { archive_name: key.0.clone(), chunk_name: key.1.clone() };
            let text = archive_mgr.read_chunk(&chunk_ref).unwrap_or_default();
            cache.insert(key.clone(), text.lines().map(|l| l.to_string()).collect());
        }
        let content = cache[&key].get(offset).cloned().unwrap_or_default();
        result.push(ContextLine { line_number, content });
    }

    result
}

// ── Directory listing ─────────────────────────────────────────────────────────

/// List the immediate children (dirs + files) of `prefix` within the source.
///
/// `prefix` should end with `/` for non-root directory queries (e.g. `"src/"`).
/// For archive member listings, `prefix` ends with `"::"` (e.g. `"archive.zip::"`).
/// An empty string means the root of the source.
pub fn list_dir(conn: &Connection, prefix: &str) -> Result<Vec<DirEntry>> {
    let is_archive_listing = prefix.contains("::");

    let (low, high) = if prefix.is_empty() {
        (String::new(), "\u{FFFF}".to_string())
    } else {
        (prefix.to_string(), prefix_bump(prefix))
    };

    let mut stmt = conn.prepare(
        "SELECT path, kind, size, mtime FROM files WHERE path >= ?1 AND path < ?2 ORDER BY path",
    )?;

    let rows: Vec<(String, String, i64, i64)> = stmt
        .query_map(params![low, high], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, i64>(3)?,
            ))
        })?
        .collect::<rusqlite::Result<_>>()?;

    let mut seen_dirs: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut seen_files: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut dirs: Vec<DirEntry> = Vec::new();
    let mut files: Vec<DirEntry> = Vec::new();

    // First pass: collect all actual files to avoid creating duplicate virtual dirs
    if is_archive_listing {
        for (path, _, _, _) in &rows {
            let rest = path.strip_prefix(prefix).unwrap_or(path);
            if !rest.contains("::") && !rest.contains('/') {
                seen_files.insert(rest.to_string());
            }
        }
    }

    // Second pass: build the directory listing
    for (path, kind, size, mtime) in rows {
        let rest = path.strip_prefix(prefix).unwrap_or(&path);

        if is_archive_listing {
            // Inside an archive: split at whichever separator comes first —
            // "/" (subdirectory) or "::" (nested archive). Taking the wrong one
            // first (e.g. "::" in "docs/inner.zip::file.txt") would produce a
            // child name containing a slash, breaking the tree and causing the
            // UI to recurse infinitely.
            let sep_pos = match (rest.find("::"), rest.find('/')) {
                (Some(a), Some(b)) => Some(a.min(b)),
                (a, b) => a.or(b),
            };
            if let Some(pos) = sep_pos {
                let child_name = &rest[..pos];
                // Only create virtual dir if we haven't seen a real file with this path
                if !seen_files.contains(child_name) && seen_dirs.insert(child_name.to_string()) {
                    // Append "/" so the next listDir call gets a properly-terminated prefix.
                    // Without this, strip_prefix() leaves a leading "/" in `rest`, sep_pos
                    // hits position 0, child_name is empty, and the same path is returned
                    // forever → infinite UI recursion.
                    dirs.push(DirEntry {
                        name: child_name.to_string(),
                        path: format!("{}{}/", prefix, child_name),
                        entry_type: "dir".to_string(),
                        kind: None,
                        size: None,
                        mtime: None,
                    });
                }
            } else {
                // Leaf member within the archive.
                files.push(DirEntry {
                    name: rest.to_string(),
                    path,
                    entry_type: "file".to_string(),
                    kind: Some(kind),
                    size: Some(size),
                    mtime: Some(mtime),
                });
            }
        } else {
            // Regular directory listing.
            // Skip inner archive members (composite paths) — they appear only when
            // the user explicitly expands the archive.
            if rest.contains("::") {
                continue;
            }

            if let Some(slash_pos) = rest.find('/') {
                let dir_name = &rest[..slash_pos];
                if seen_dirs.insert(dir_name.to_string()) {
                    dirs.push(DirEntry {
                        name: dir_name.to_string(),
                        path: format!("{}{}/", prefix, dir_name),
                        entry_type: "dir".to_string(),
                        kind: None,
                        size: None,
                        mtime: None,
                    });
                }
            } else {
                files.push(DirEntry {
                    name: rest.to_string(),
                    path,
                    entry_type: "file".to_string(),
                    kind: Some(kind),
                    size: Some(size),
                    mtime: Some(mtime),
                });
            }
        }
    }

    let mut entries = dirs;
    entries.extend(files);
    Ok(entries)
}

// ── Stats ─────────────────────────────────────────────────────────────────────

/// Returns (total_files, total_size, by_kind) aggregated from the files table.
pub fn get_stats(conn: &Connection) -> Result<(usize, i64, HashMap<String, KindStats>)> {
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
    let mut by_kind = HashMap::new();

    for (kind, count, size, avg_ms) in rows {
        total_files += count as usize;
        total_size += size;
        by_kind.insert(kind, KindStats { count: count as usize, size, avg_extract_ms: avg_ms });
    }

    Ok((total_files, total_size, by_kind))
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

/// Produce the upper-bound key for a prefix range scan by incrementing the last byte.
fn prefix_bump(prefix: &str) -> String {
    let mut bytes = prefix.as_bytes().to_vec();
    if let Some(last) = bytes.last_mut() {
        *last += 1;
    }
    String::from_utf8(bytes).unwrap_or_else(|_| "\u{FFFF}".to_string())
}
