/// Per-file SQLite processing (phase 1 — no ZIP I/O).
///
/// These functions operate on a single `IndexFile` at a time and are called
/// by `process_request_phase1` in the request-level coordinator.
use anyhow::Result;
use rusqlite::Connection;
use rusqlite::OptionalExtension;

use find_common::api::{FileKind, IndexFile, IndexLine};
use find_common::path::{composite_like_prefix, is_composite};

// ── Public entry points ───────────────────────────────────────────────────────

/// Outcome returned by `process_file_phase1` / `process_file_phase1_fallback`.
/// Used by the caller to decide how to log this file in the activity log.
pub(super) enum Phase1Outcome {
    /// Path was not previously in the `files` table — this is the first index.
    New,
    /// Path already existed in the `files` table — this is a re-index.
    Modified { old_size: i64, old_kind: FileKind },
    /// Stale-mtime guard: incoming mtime < stored mtime; nothing was written.
    Skipped,
}

/// Write a single file's metadata and content lines to SQLite.
/// Thin wrapper that calls `process_file_phase1_fallback` with `skip_inner_delete = false`.
pub(super) fn process_file_phase1(
    conn: &mut Connection,
    file: &IndexFile,
    inline_threshold_bytes: u64,
) -> Result<Phase1Outcome> {
    process_file_phase1_fallback(conn, file, false, inline_threshold_bytes)
}

/// Like `process_file_phase1` but optionally skips the deletion of inner
/// archive members (used when indexing a fallback/stub for a failed outer archive).
pub(super) fn process_file_phase1_fallback(
    conn: &mut Connection,
    file: &IndexFile,
    skip_inner_delete: bool,
    inline_threshold_bytes: u64,
) -> Result<Phase1Outcome> {
    let now_secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;

    // If re-indexing an outer archive, delete stale inner members first (SQL only).
    // Orphaned chunks left in ZIPs are reclaimed by the periodic compaction pass.
    if !skip_inner_delete && is_outer_archive(&file.path, &file.kind) && file.mtime == 0 {
        let like_pat = composite_like_prefix(&file.path);
        let tx = conn.unchecked_transaction()?;
        tx.execute("DELETE FROM files WHERE path LIKE ?1", rusqlite::params![like_pat])?;
        tx.commit()?;
    }

    // Single query for the existing record id, stored mtime, size, and kind.
    // Used for: dedup outcome, stale-mtime guard, chunk-removal queue,
    // Phase1Outcome (New vs Modified), and incremental stats delta.
    let existing_record: Option<(i64, i64, i64, String)> = conn.query_row(
        "SELECT id, mtime, COALESCE(size,0), kind FROM files WHERE path = ?1",
        rusqlite::params![file.path],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
    ).optional()?;
    let existing_id    = existing_record.as_ref().map(|(id, _, _, _)| *id);
    let stored_mtime   = existing_record.as_ref().map(|(_, mtime, _, _)| *mtime);
    let old_size_kind  = existing_record.map(|(_, _, size, kind)| (size, FileKind::from(kind.as_str())));

    // Dedup check: if another canonical with the same content hash exists,
    // register this file as an alias and skip chunk/lines/FTS writes.
    if let Some(hash) = &file.content_hash {
        let canonical_id: Option<i64> = conn.query_row(
            "SELECT id FROM files
             WHERE content_hash = ?1
               AND canonical_file_id IS NULL
               AND path != ?2
               AND EXISTS (SELECT 1 FROM lines WHERE file_id = id LIMIT 1)
             LIMIT 1",
            rusqlite::params![hash, file.path],
            |row| row.get(0),
        ).optional()?;

        if let Some(canonical_id) = canonical_id {
            conn.execute(
                "INSERT INTO files (path, mtime, size, kind, indexed_at, extract_ms, content_hash, canonical_file_id)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
                 ON CONFLICT(path) DO UPDATE SET
                   mtime            = excluded.mtime,
                   size             = excluded.size,
                   kind             = excluded.kind,
                   extract_ms       = excluded.extract_ms,
                   content_hash     = excluded.content_hash,
                   canonical_file_id = excluded.canonical_file_id",
                rusqlite::params![
                    file.path, file.mtime, file.size, file.kind.to_string(),
                    now_secs,
                    file.extract_ms.map(|ms| ms as i64),
                    hash,
                    canonical_id,
                ],
            )?;
            if existing_id.is_none() {
                return Ok(Phase1Outcome::New);
            } else {
                let (old_size, old_kind) = old_size_kind.unwrap_or((0, FileKind::Unknown));
                return Ok(Phase1Outcome::Modified { old_size, old_kind });
            }
        }
    }

    // Stale-mtime guard: skip if the stored mtime is already newer.
    if let Some(stored) = stored_mtime {
        if file.mtime > 0 && file.mtime < stored {
            tracing::debug!(
                "skipping stale upsert for {} (incoming mtime={} < stored={})",
                file.path, file.mtime, stored
            );
            return Ok(Phase1Outcome::Skipped);
        }
    }

    // Decide: inline vs deferred archive storage.
    let total_content_bytes: usize = file.lines.iter().map(|l| l.content.len()).sum();
    let use_inline = inline_threshold_bytes > 0
        && total_content_bytes as u64 <= inline_threshold_bytes;

    // Single transaction for the whole file.
    let t_fts = std::time::Instant::now();
    let tx = conn.transaction()?;

    // Upsert the file record and get the stable file_id.
    let file_id: i64 = tx.query_row(
        "INSERT INTO files (path, mtime, size, kind, scanner_version, indexed_at, extract_ms, content_hash, canonical_file_id)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, NULL)
         ON CONFLICT(path) DO UPDATE SET
           mtime             = excluded.mtime,
           size              = excluded.size,
           kind              = excluded.kind,
           scanner_version   = excluded.scanner_version,
           indexed_at        = excluded.indexed_at,
           extract_ms        = excluded.extract_ms,
           content_hash      = excluded.content_hash,
           canonical_file_id = NULL
         RETURNING id",
        rusqlite::params![
            file.path, file.mtime, file.size, file.kind.to_string(),
            file.scanner_version,
            now_secs,
            file.extract_ms.map(|ms| ms as i64),
            file.content_hash.as_deref(),
        ],
        |row| row.get(0),
    )?;

    // Delete existing lines and inline content.
    tx.execute("DELETE FROM lines WHERE file_id = ?1", rusqlite::params![file_id])?;

    if use_inline {
        // Store content inline in file_content table.
        let mut sorted_lines = file.lines.iter().collect::<Vec<_>>();
        sorted_lines.sort_by_key(|l| l.line_number);
        let inline_text: String = sorted_lines.iter()
            .map(|l| l.content.as_str())
            .collect::<Vec<_>>()
            .join("\n");

        tx.execute(
            "INSERT INTO file_content (file_id, content) VALUES (?1, ?2)
             ON CONFLICT(file_id) DO UPDATE SET content = excluded.content",
            rusqlite::params![file_id, inline_text],
        )?;

        // Insert line rows with NULL chunk refs; offset = position in inline_text lines.
        for (offset, line) in sorted_lines.iter().enumerate() {
            let line_id = tx.query_row(
                "INSERT INTO lines (file_id, line_number, chunk_archive, chunk_name, line_offset_in_chunk)
                 VALUES (?1, ?2, NULL, NULL, ?3)
                 RETURNING id",
                rusqlite::params![
                    file_id,
                    line.line_number as i64,
                    offset as i64,
                ],
                |row| row.get::<_, i64>(0),
            )?;
            tx.execute(
                "INSERT INTO lines_fts(rowid, content) VALUES (?1, ?2)",
                rusqlite::params![line_id, line.content],
            )?;
        }
    } else {
        // Remove old inline content if this file was previously stored inline.
        tx.execute(
            "DELETE FROM file_content WHERE file_id = ?1",
            rusqlite::params![file_id],
        )?;

        // Deferred archive: insert lines with NULL chunk refs (archive thread will fill them in).
        // FTS is indexed immediately for search availability.
        let mut sorted_lines = file.lines.iter().collect::<Vec<_>>();
        sorted_lines.sort_by_key(|l| l.line_number);
        for line in &sorted_lines {
            let line_id = tx.query_row(
                "INSERT INTO lines (file_id, line_number, chunk_archive, chunk_name, line_offset_in_chunk)
                 VALUES (?1, ?2, NULL, NULL, 0)
                 RETURNING id",
                rusqlite::params![
                    file_id,
                    line.line_number as i64,
                ],
                |row| row.get::<_, i64>(0),
            )?;
            tx.execute(
                "INSERT INTO lines_fts(rowid, content) VALUES (?1, ?2)",
                rusqlite::params![line_id, line.content],
            )?;
        }
    }

    tx.commit()?;
    super::warn_slow(t_fts, 10, "fts_insert_phase1", &file.path);

    if existing_id.is_none() {
        Ok(Phase1Outcome::New)
    } else {
        let (old_size, old_kind) = old_size_kind.unwrap_or((0, FileKind::Unknown));
        Ok(Phase1Outcome::Modified { old_size, old_kind })
    }
}

// ── Helper constructors ───────────────────────────────────────────────────────

/// Returns `true` if `file` is a top-level archive (kind=Archive with no
/// "::" in the path).
pub(crate) fn is_outer_archive(path: &str, kind: &FileKind) -> bool {
    *kind == FileKind::Archive && !is_composite(path)
}

/// Build a fallback `IndexFile` that records only the file path (line 0).
/// Used when content extraction fails so the file is still findable by name.
pub(super) fn filename_only_file(file: &IndexFile) -> IndexFile {
    IndexFile {
        path: file.path.clone(),
        mtime: file.mtime,
        size: file.size,
        kind: if file.kind == FileKind::Archive { FileKind::Unknown } else { file.kind.clone() },
        lines: vec![IndexLine {
            archive_path: None,
            line_number: 0,
            content: file.path.clone(),
        }],
        extract_ms: None,
        content_hash: None,
        scanner_version: file.scanner_version,
        is_new: file.is_new,
    }
}

/// Build an outer-archive sentinel stub (mtime=0) so that the archive's members
/// can be re-indexed on the next scan even if extraction itself failed.
pub(super) fn outer_archive_stub(file: &IndexFile) -> IndexFile {
    IndexFile {
        path: file.path.clone(),
        mtime: 0,
        size: file.size,
        kind: FileKind::Archive,
        lines: vec![IndexLine {
            archive_path: None,
            line_number: 0,
            content: file.path.clone(),
        }],
        extract_ms: None,
        content_hash: None,
        scanner_version: file.scanner_version,
        is_new: file.is_new,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use find_common::api::IndexLine;
    use rusqlite::Connection;

    fn test_conn() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("PRAGMA foreign_keys = ON;").unwrap();
        conn.execute_batch(include_str!("../schema_v2.sql")).unwrap();
        conn.execute_batch(
            "DROP TABLE IF EXISTS pending_chunk_removes;
             CREATE TABLE IF NOT EXISTS activity_log (
                 id          INTEGER PRIMARY KEY AUTOINCREMENT,
                 occurred_at INTEGER NOT NULL,
                 action      TEXT    NOT NULL,
                 path        TEXT    NOT NULL,
                 new_path    TEXT
             );",
        ).unwrap();
        crate::db::register_scalar_functions(&conn).unwrap();
        conn
    }

    fn make_file(path: &str, mtime: i64, content: &str) -> IndexFile {
        IndexFile {
            path: path.to_string(),
            mtime,
            size: Some(content.len() as i64),
            kind: FileKind::Text,
            scanner_version: 1,
            lines: vec![
                IndexLine { archive_path: None, line_number: 0, content: path.to_string() },
                IndexLine { archive_path: None, line_number: 1, content: content.to_string() },
            ],
            extract_ms: None,
            content_hash: None,
            is_new: true,
        }
    }

    fn stored_mtime(conn: &Connection, path: &str) -> Option<i64> {
        conn.query_row(
            "SELECT mtime FROM files WHERE path = ?1",
            rusqlite::params![path],
            |r| r.get(0),
        ).ok()
    }

    fn line_count(conn: &Connection, path: &str) -> usize {
        let file_id: i64 = conn.query_row(
            "SELECT id FROM files WHERE path = ?1",
            rusqlite::params![path],
            |r| r.get(0),
        ).unwrap();
        conn.query_row(
            "SELECT COUNT(*) FROM lines WHERE file_id = ?1",
            rusqlite::params![file_id],
            |r| r.get::<_, i64>(0),
        ).unwrap() as usize
    }

    #[test]
    fn new_file_returns_new_outcome() {
        let mut conn = test_conn();
        let file = make_file("docs/readme.txt", 1000, "hello world");
        let outcome = process_file_phase1(&mut conn, &file, 0).unwrap();
        assert!(matches!(outcome, Phase1Outcome::New));
        assert_eq!(stored_mtime(&conn, "docs/readme.txt"), Some(1000));
        assert_eq!(line_count(&conn, "docs/readme.txt"), 2); // line 0 + line 1
    }

    #[test]
    fn re_index_newer_mtime_returns_modified() {
        let mut conn = test_conn();
        process_file_phase1(&mut conn, &make_file("readme.txt", 1000, "v1"), 0).unwrap();
        let outcome = process_file_phase1(&mut conn, &make_file("readme.txt", 2000, "v2"), 0).unwrap();
        assert!(matches!(outcome, Phase1Outcome::Modified { .. }));
        assert_eq!(stored_mtime(&conn, "readme.txt"), Some(2000));
    }

    #[test]
    fn stale_mtime_is_skipped() {
        let mut conn = test_conn();
        process_file_phase1(&mut conn, &make_file("readme.txt", 2000, "current"), 0).unwrap();
        let outcome = process_file_phase1(&mut conn, &make_file("readme.txt", 1000, "stale"), 0).unwrap();
        assert!(matches!(outcome, Phase1Outcome::Skipped));
        assert_eq!(stored_mtime(&conn, "readme.txt"), Some(2000));
    }

    #[test]
    fn content_hash_dedup_registers_alias() {
        let mut conn = test_conn();

        let mut file_a = make_file("original.txt", 1000, "shared content");
        file_a.content_hash = Some("abc123".to_string());
        let outcome_a = process_file_phase1(&mut conn, &file_a, 0).unwrap();
        assert!(matches!(outcome_a, Phase1Outcome::New));

        let canonical_id: i64 = conn.query_row(
            "SELECT id FROM files WHERE path = 'original.txt'",
            [],
            |r| r.get(0),
        ).unwrap();

        let mut file_b = make_file("duplicate.txt", 1100, "shared content");
        file_b.content_hash = Some("abc123".to_string());
        let outcome_b = process_file_phase1(&mut conn, &file_b, 0).unwrap();
        assert!(matches!(outcome_b, Phase1Outcome::New));

        let alias_canonical: Option<i64> = conn.query_row(
            "SELECT canonical_file_id FROM files WHERE path = 'duplicate.txt'",
            [],
            |r| r.get(0),
        ).unwrap();
        assert_eq!(alias_canonical, Some(canonical_id));
    }

    /// Regression: phantom canonical created by ON DELETE SET NULL must not be used for dedup.
    ///
    /// Production scenario that triggered this bug:
    ///   1. `archive.7z::tsconfig.json` is indexed → becomes the canonical for hash H.
    ///   2. `archive.v2.7z::tsconfig.json` (same content) is indexed → becomes an alias
    ///      pointing at the canonical.
    ///   3. `find-scan --force archive.7z` sends a mtime=0 sentinel → server runs
    ///      `DELETE FROM files WHERE path LIKE 'archive.7z::%'`, deleting the canonical.
    ///   4. SQLite fires ON DELETE SET NULL on the alias row: its `canonical_file_id`
    ///      becomes NULL.  The alias was inserted without content lines (aliases don't store
    ///      their own lines), so it is now a "phantom canonical" — `canonical_file_id IS NULL`
    ///      but zero lines.
    ///   5. The worker re-inserts `archive.7z::tsconfig.json` from the fresh extraction.
    ///      The dedup query finds the phantom canonical (it has `canonical_file_id IS NULL`
    ///      and the same content hash) and registers the new file as an alias pointing at it.
    ///   6. Both entries now have zero content lines; the file is un-searchable.
    ///
    /// The fix: require `EXISTS (SELECT 1 FROM lines …)` in the dedup query so that a
    /// contentless phantom is never selected as a canonical target.
    ///
    /// Note: the phantom canonical state (step 4) is still reachable in production — it is
    /// a DB-level side effect of ON DELETE SET NULL, not prevented by this fix.  The fix
    /// is purely in the dedup query, which is why this test remains valid.
    #[test]
    fn dedup_ignores_phantom_canonical_with_no_lines() {
        let mut conn = test_conn();

        // Step 1: insert a canonical (file_a) with real content.
        let mut file_a = make_file("archive.7z::tsconfig.json", 1000, "{}");
        file_a.content_hash = Some("hash_abc".to_string());
        process_file_phase1(&mut conn, &file_a, 0).unwrap();

        let canonical_id: i64 = conn.query_row(
            "SELECT id FROM files WHERE path = 'archive.7z::tsconfig.json'",
            [],
            |r| r.get(0),
        ).unwrap();

        // Step 2: file_b (from another archive) deduplicates against file_a.
        let mut file_b = make_file("archive.v2.7z::tsconfig.json", 1100, "{}");
        file_b.content_hash = Some("hash_abc".to_string());
        process_file_phase1(&mut conn, &file_b, 0).unwrap();

        // Confirm file_b is an alias.
        let alias_canonical: Option<i64> = conn.query_row(
            "SELECT canonical_file_id FROM files WHERE path = 'archive.v2.7z::tsconfig.json'",
            [],
            |r| r.get(0),
        ).unwrap();
        assert_eq!(alias_canonical, Some(canonical_id));

        // Step 3: simulate --force re-index of archive.7z by deleting file_a.
        // ON DELETE SET NULL fires: file_b now has canonical_file_id=NULL (phantom canonical).
        conn.execute("DELETE FROM files WHERE path = 'archive.7z::tsconfig.json'", []).unwrap();

        // Confirm file_b is now a phantom canonical (canonical_file_id IS NULL, no lines).
        let phantom_canonical_id: i64 = conn.query_row(
            "SELECT id FROM files WHERE path = 'archive.v2.7z::tsconfig.json' AND canonical_file_id IS NULL",
            [],
            |r| r.get(0),
        ).unwrap();
        let phantom_line_count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM lines WHERE file_id = ?1",
            rusqlite::params![phantom_canonical_id],
            |r| r.get(0),
        ).unwrap();
        assert_eq!(phantom_line_count, 0, "phantom canonical should have no lines");

        // Step 4: re-index archive.7z::tsconfig.json.
        // WITHOUT the fix, this would find the phantom canonical and register as alias → no content.
        // WITH the fix, the phantom canonical is ignored → new canonical inserted with content.
        let mut file_a_reindexed = make_file("archive.7z::tsconfig.json", 1200, "{}");
        file_a_reindexed.content_hash = Some("hash_abc".to_string());
        process_file_phase1(&mut conn, &file_a_reindexed, 0).unwrap();

        // file_a_reindexed must NOT be an alias — it must be the new canonical with content.
        let new_canonical: Option<i64> = conn.query_row(
            "SELECT canonical_file_id FROM files WHERE path = 'archive.7z::tsconfig.json'",
            [],
            |r| r.get(0),
        ).unwrap();
        assert!(new_canonical.is_none(), "re-indexed file must be a canonical, not an alias pointing to a phantom");

        let new_line_count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM lines l
             JOIN files f ON l.file_id = f.id
             WHERE f.path = 'archive.7z::tsconfig.json'",
            [],
            |r| r.get(0),
        ).unwrap();
        assert!(new_line_count > 0, "re-indexed file must have content lines, got {new_line_count}");
    }

    #[test]
    fn inline_storage_used_when_below_threshold() {
        let mut conn = test_conn();
        process_file_phase1(&mut conn, &make_file("small.txt", 1000, "tiny"), 10_000).unwrap();

        let file_id: i64 = conn.query_row(
            "SELECT id FROM files WHERE path = 'small.txt'", [], |r| r.get(0),
        ).unwrap();
        let inline: Option<String> = conn.query_row(
            "SELECT content FROM file_content WHERE file_id = ?1",
            rusqlite::params![file_id],
            |r| r.get(0),
        ).ok();
        assert!(inline.is_some(), "small file should be stored inline");
    }

    #[test]
    fn deferred_storage_when_threshold_zero() {
        let mut conn = test_conn();
        process_file_phase1(&mut conn, &make_file("big.txt", 1000, "some content"), 0).unwrap();

        let file_id: i64 = conn.query_row(
            "SELECT id FROM files WHERE path = 'big.txt'", [], |r| r.get(0),
        ).unwrap();
        let inline: Option<String> = conn.query_row(
            "SELECT content FROM file_content WHERE file_id = ?1",
            rusqlite::params![file_id],
            |r| r.get(0),
        ).ok();
        assert!(inline.is_none(), "deferred file should not be stored inline");

        let null_chunk_count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM lines WHERE file_id = ?1 AND chunk_archive IS NULL",
            rusqlite::params![file_id],
            |r| r.get(0),
        ).unwrap();
        assert_eq!(null_chunk_count, 2);
    }
}
