/// Per-file SQLite processing (phase 1 — no ZIP I/O).
///
/// These functions operate on a single `IndexFile` at a time and are called
/// by `process_request_phase1` in the request-level coordinator.
use anyhow::Result;
use rusqlite::Connection;
use rusqlite::OptionalExtension;

use find_common::api::{FileKind, IndexFile, IndexLine};
use find_common::path::{composite_like_prefix, is_composite};

use crate::db::{encode_fts_rowid, MAX_LINES_PER_FILE};

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
    let existing_record: Option<(i64, i64, i64, String)> = conn.query_row(
        "SELECT id, mtime, COALESCE(size,0), kind FROM files WHERE path = ?1",
        rusqlite::params![file.path],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
    ).optional()?;
    let existing_id    = existing_record.as_ref().map(|(id, _, _, _)| *id);
    let stored_mtime   = existing_record.as_ref().map(|(_, mtime, _, _)| *mtime);
    let old_size_kind  = existing_record.map(|(_, _, size, kind)| (size, FileKind::from(kind.as_str())));

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

    let line_count = file.lines.len() as i64;

    // Upsert the file record and get the stable file_id.
    let file_id: i64 = tx.query_row(
        "INSERT INTO files (path, mtime, size, kind, scanner_version, indexed_at, extract_ms, content_hash, line_count)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
         ON CONFLICT(path) DO UPDATE SET
           mtime             = excluded.mtime,
           size              = excluded.size,
           kind              = excluded.kind,
           scanner_version   = excluded.scanner_version,
           indexed_at        = excluded.indexed_at,
           extract_ms        = excluded.extract_ms,
           content_hash      = excluded.content_hash,
           line_count        = excluded.line_count
         RETURNING id",
        rusqlite::params![
            file.path, file.mtime, file.size, file.kind.to_string(),
            file.scanner_version,
            now_secs,
            file.extract_ms.map(|ms| ms as i64),
            file.content_hash.as_deref(),
            line_count,
        ],
        |row| row.get(0),
    )?;

    // If content_hash is set, ensure content_blocks row exists.
    if let Some(hash) = &file.content_hash {
        tx.execute(
            "INSERT OR IGNORE INTO content_blocks(content_hash) VALUES(?1)",
            rusqlite::params![hash],
        )?;
    }

    // On re-index: delete old inline FTS entries before inserting new ones.
    // This keeps the contentless FTS5 index clean for inline files, where old
    // content is readily available in file_content before it gets overwritten.
    // For deferred files, old FTS entries become stale and are filtered at read
    // time by read_chunk_for_file returning None for uncovered line numbers.
    if existing_id.is_some() && use_inline {
        let old_content: Option<String> = tx.query_row(
            "SELECT content FROM file_content WHERE file_id = ?1",
            rusqlite::params![file_id],
            |r| r.get(0),
        ).optional()?;
        if let Some(old) = old_content {
            // Position i in the '\n'-split corresponds to line_number i for
            // dense (0-based sequential) inline files.
            for (i, old_line) in old.split('\n').enumerate() {
                let old_rowid = encode_fts_rowid(file_id, i as i64);
                tx.execute(
                    "INSERT INTO lines_fts(lines_fts, rowid, content) VALUES('delete', ?1, ?2)",
                    rusqlite::params![old_rowid, old_line],
                )?;
            }
        }
    }

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

        // Insert FTS rows using encode_fts_rowid.
        for line in &sorted_lines {
            let line_number = line.line_number as i64;
            if line_number >= MAX_LINES_PER_FILE {
                tracing::warn!(
                    "file {} line {} exceeds MAX_LINES_PER_FILE — skipping FTS",
                    file.path, line_number
                );
                continue;
            }
            let rowid = encode_fts_rowid(file_id, line_number);
            tx.execute(
                "INSERT INTO lines_fts(rowid, content) VALUES (?1, ?2)",
                rusqlite::params![rowid, line.content],
            )?;
        }
    } else {
        // Remove old inline content if this file was previously stored inline.
        tx.execute(
            "DELETE FROM file_content WHERE file_id = ?1",
            rusqlite::params![file_id],
        )?;

        // Deferred archive: insert FTS rows immediately for search availability.
        let mut sorted_lines = file.lines.iter().collect::<Vec<_>>();
        sorted_lines.sort_by_key(|l| l.line_number);
        for line in &sorted_lines {
            let line_number = line.line_number as i64;
            if line_number >= MAX_LINES_PER_FILE {
                tracing::warn!(
                    "file {} line {} exceeds MAX_LINES_PER_FILE — skipping FTS",
                    file.path, line_number
                );
                continue;
            }
            let rowid = encode_fts_rowid(file_id, line_number);
            tx.execute(
                "INSERT INTO lines_fts(rowid, content) VALUES (?1, ?2)",
                rusqlite::params![rowid, line.content],
            )?;
        }
    }

    // Update duplicate tracking.
    if let Some(hash) = &file.content_hash {
        upsert_duplicate_tracking(&tx, hash, file_id)?;
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

/// Insert duplicate tracking entries when 2+ files share a content_hash.
fn upsert_duplicate_tracking(
    tx: &rusqlite::Transaction,
    hash: &str,
    file_id: i64,
) -> Result<()> {
    // Find other files with the same content_hash.
    let other_ids: Vec<i64> = {
        let mut stmt = tx.prepare(
            "SELECT id FROM files WHERE content_hash = ?1 AND id != ?2",
        )?;
        let ids: Vec<i64> = stmt.query_map(rusqlite::params![hash, file_id], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        ids
    };
    if !other_ids.is_empty() {
        // Insert for all existing duplicates and for the current file.
        for other_id in &other_ids {
            tx.execute(
                "INSERT OR IGNORE INTO duplicates(content_hash, file_id) VALUES(?1, ?2)",
                rusqlite::params![hash, other_id],
            )?;
        }
        tx.execute(
            "INSERT OR IGNORE INTO duplicates(content_hash, file_id) VALUES(?1, ?2)",
            rusqlite::params![hash, file_id],
        )?;
    }
    Ok(())
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
        conn.execute_batch(include_str!("../schema_v3.sql")).unwrap();
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

    fn fts_row_count(conn: &Connection) -> i64 {
        conn.query_row(
            "SELECT COUNT(*) FROM lines_fts",
            [],
            |r| r.get(0),
        ).unwrap_or(0)
    }

    #[test]
    fn new_file_returns_new_outcome() {
        let mut conn = test_conn();
        let file = make_file("docs/readme.txt", 1000, "hello world");
        let outcome = process_file_phase1(&mut conn, &file, 0).unwrap();
        assert!(matches!(outcome, Phase1Outcome::New));
        assert_eq!(stored_mtime(&conn, "docs/readme.txt"), Some(1000));
        // 2 FTS entries (line 0 + line 1)
        assert_eq!(fts_row_count(&conn), 2);
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
    fn content_hash_registers_duplicate_pair() {
        let mut conn = test_conn();

        let mut file_a = make_file("original.txt", 1000, "shared content");
        file_a.content_hash = Some("abc123".to_string());
        let outcome_a = process_file_phase1(&mut conn, &file_a, 0).unwrap();
        assert!(matches!(outcome_a, Phase1Outcome::New));

        // Only one file → no duplicates yet.
        let dup_count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM duplicates WHERE content_hash = 'abc123'",
            [],
            |r| r.get(0),
        ).unwrap();
        assert_eq!(dup_count, 0, "single file should not be in duplicates");

        let mut file_b = make_file("duplicate.txt", 1100, "shared content");
        file_b.content_hash = Some("abc123".to_string());
        let outcome_b = process_file_phase1(&mut conn, &file_b, 0).unwrap();
        assert!(matches!(outcome_b, Phase1Outcome::New));

        // Both files should now be in duplicates.
        let dup_count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM duplicates WHERE content_hash = 'abc123'",
            [],
            |r| r.get(0),
        ).unwrap();
        assert_eq!(dup_count, 2, "both files should be in duplicates table");
    }

    #[test]
    fn no_duplicate_entry_for_unique_hash() {
        let mut conn = test_conn();
        let mut file = make_file("unique.txt", 1000, "unique content");
        file.content_hash = Some("unique_hash".to_string());
        process_file_phase1(&mut conn, &file, 0).unwrap();

        let dup_count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM duplicates",
            [],
            |r| r.get(0),
        ).unwrap();
        assert_eq!(dup_count, 0, "unique file should not be in duplicates");
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
        // FTS entries should still be there (2 lines)
        assert_eq!(fts_row_count(&conn), 2);
    }

    #[test]
    fn deferred_storage_inserts_content_block() {
        let mut conn = test_conn();
        let mut file = make_file("doc.txt", 1000, "content");
        file.content_hash = Some("myhash".to_string());
        process_file_phase1(&mut conn, &file, 0).unwrap();

        let block_count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM content_blocks WHERE content_hash = 'myhash'",
            [],
            |r| r.get(0),
        ).unwrap();
        assert_eq!(block_count, 1, "content_blocks row should exist for the hash");
    }
}
