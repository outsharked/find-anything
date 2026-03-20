/// Per-file SQLite processing (phase 1 — no ZIP I/O).
///
/// These functions operate on a single `IndexFile` at a time and are called
/// by `process_request_phase1` in the request-level coordinator.
use anyhow::Result;
use rusqlite::Connection;
use rusqlite::OptionalExtension;

use find_common::api::{FileKind, IndexFile, IndexLine, LINE_PATH, LINE_METADATA};
use find_common::path::{composite_like_prefix, is_composite};
use find_content_store::{ContentKey, ContentStore};

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
    content_store: Option<&dyn ContentStore>,
) -> Result<Phase1Outcome> {
    process_file_phase1_fallback(conn, file, false, content_store)
}

/// Like `process_file_phase1` but optionally skips the deletion of inner
/// archive members (used when indexing a fallback/stub for a failed outer archive).
pub(super) fn process_file_phase1_fallback(
    conn: &mut Connection,
    file: &IndexFile,
    skip_inner_delete: bool,
    content_store: Option<&dyn ContentStore>,
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

    // Single query for the existing record: id, mtime, size, kind, file_hash, line_count.
    let existing_record: Option<(i64, i64, i64, String, Option<String>, i64)> = conn.query_row(
        "SELECT id, mtime, COALESCE(size,0), kind, file_hash, COALESCE(line_count,0) FROM files WHERE path = ?1",
        rusqlite::params![file.path],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?, row.get(5)?)),
    ).optional()?;
    let existing_id     = existing_record.as_ref().map(|(id, _, _, _, _, _)| *id);
    let stored_mtime    = existing_record.as_ref().map(|(_, mtime, _, _, _, _)| *mtime);
    let old_file_hash   = existing_record.as_ref().and_then(|(_, _, _, _, h, _)| h.clone());
    let old_line_count  = existing_record.as_ref().map(|(_, _, _, _, _, lc)| *lc).unwrap_or(0);
    let old_size_kind   = existing_record.map(|(_, _, size, kind, _, _)| (size, FileKind::from(kind.as_str())));

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

    // Single transaction for the whole file.
    let t_fts = std::time::Instant::now();
    let tx = conn.transaction()?;

    let line_count = file.lines.len() as i64;

    // Upsert the file record, keeping the same file_id on re-index.
    let file_id: i64 = tx.query_row(
        "INSERT INTO files (path, mtime, size, kind, scanner_version, indexed_at, extract_ms, file_hash, line_count)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
         ON CONFLICT(path) DO UPDATE SET
           mtime             = excluded.mtime,
           size              = excluded.size,
           kind              = excluded.kind,
           scanner_version   = excluded.scanner_version,
           indexed_at        = excluded.indexed_at,
           extract_ms        = excluded.extract_ms,
           file_hash         = excluded.file_hash,
           line_count        = excluded.line_count
         RETURNING id",
        rusqlite::params![
            file.path, file.mtime, file.size, file.kind.to_string(),
            file.scanner_version,
            now_secs,
            file.extract_ms.map(|ms| ms as i64),
            file.file_hash.as_deref(),
            line_count,
        ],
        |row| row.get(0),
    )?;

    // On re-index: remove old FTS entries using the FTS5 'delete' command.
    // contentless FTS5 supports 'delete' as long as we supply the original content —
    // the content store holds old content keyed by file_hash from the previous index.
    // If old content is unavailable (hash missing or not yet archived), we skip
    // cleanup; the stale entries become orphaned but are harmless (search JOIN on
    // file_id still returns correct results once the new entries are inserted, and
    // the old entries for the same rowids are not distinguishable by file).
    if existing_id.is_some() {
        if let (Some(store), Some(hash)) = (content_store, old_file_hash.as_deref()) {
            let key = ContentKey::new(hash);
            if let Ok(Some(old_lines)) = store.get_lines(&key, 0, old_line_count as usize) {
                for (pos, content) in old_lines {
                    // Empty content has no trigrams in the FTS index; issuing
                    // 'delete' with "" corrupts FTS5 state for that rowid.
                    if content.is_empty() {
                        continue;
                    }
                    if (pos as i64) < MAX_LINES_PER_FILE {
                        let old_rowid = encode_fts_rowid(file_id, pos as i64);
                        tx.execute(
                            "INSERT INTO lines_fts(lines_fts, rowid, content) VALUES('delete', ?1, ?2)",
                            rusqlite::params![old_rowid, content],
                        )?;
                    }
                }
            }
        }
    }

    // Insert FTS rows for search availability.
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
            rusqlite::params![rowid, line.content.trim_end()],
        )?;
    }

    // Update duplicate tracking.
    if let Some(hash) = &file.file_hash {
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

/// Insert duplicate tracking entries when 2+ files share a file_hash.
fn upsert_duplicate_tracking(
    tx: &rusqlite::Transaction,
    hash: &str,
    file_id: i64,
) -> Result<()> {
    // Find other files with the same file_hash.
    let other_ids: Vec<i64> = {
        let mut stmt = tx.prepare(
            "SELECT id FROM files WHERE file_hash = ?1 AND id != ?2",
        )?;
        let ids: Vec<i64> = stmt.query_map(rusqlite::params![hash, file_id], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        ids
    };
    if !other_ids.is_empty() {
        // Insert for all existing duplicates and for the current file.
        for other_id in &other_ids {
            tx.execute(
                "INSERT OR IGNORE INTO duplicates(file_hash, file_id) VALUES(?1, ?2)",
                rusqlite::params![hash, other_id],
            )?;
        }
        tx.execute(
            "INSERT OR IGNORE INTO duplicates(file_hash, file_id) VALUES(?1, ?2)",
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
        lines: vec![
            IndexLine {
                archive_path: None,
                line_number: LINE_PATH,
                content: file.path.clone(),
            },
            IndexLine {
                archive_path: None,
                line_number: LINE_METADATA,
                content: String::new(),
            },
        ],
        extract_ms: None,
        file_hash: None,
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
        lines: vec![
            IndexLine {
                archive_path: None,
                line_number: LINE_PATH,
                content: file.path.clone(),
            },
            IndexLine {
                archive_path: None,
                line_number: LINE_METADATA,
                content: String::new(),
            },
        ],
        extract_ms: None,
        file_hash: None,
        scanner_version: file.scanner_version,
        is_new: file.is_new,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use find_common::api::IndexLine;
    use find_content_store::{ContentKey, ContentStore, SqliteContentStore};
    use rusqlite::Connection;
    use std::sync::Arc;

    fn test_conn() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("PRAGMA foreign_keys = ON;").unwrap();
        conn.execute_batch(include_str!("../schema_v4.sql")).unwrap();
        crate::db::register_scalar_functions(&conn).unwrap();
        conn
    }

    fn make_file(path: &str, mtime: i64, content: &str) -> IndexFile {
        use find_common::api::{LINE_PATH, LINE_METADATA, LINE_CONTENT_START};
        IndexFile {
            path: path.to_string(),
            mtime,
            size: Some(content.len() as i64),
            kind: FileKind::Text,
            scanner_version: 1,
            lines: vec![
                IndexLine { archive_path: None, line_number: LINE_PATH, content: path.to_string() },
                IndexLine { archive_path: None, line_number: LINE_METADATA, content: String::new() },
                IndexLine { archive_path: None, line_number: LINE_CONTENT_START, content: content.to_string() },
            ],
            extract_ms: None,
            file_hash: None,
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
        let outcome = process_file_phase1(&mut conn, &file, None).unwrap();
        assert!(matches!(outcome, Phase1Outcome::New));
        assert_eq!(stored_mtime(&conn, "docs/readme.txt"), Some(1000));
        // 3 FTS entries (line 0 path + line 1 metadata + line 2 content)
        assert_eq!(fts_row_count(&conn), 3);
    }

    #[test]
    fn re_index_newer_mtime_returns_modified() {
        let mut conn = test_conn();
        process_file_phase1(&mut conn, &make_file("readme.txt", 1000, "v1"), None).unwrap();
        let outcome = process_file_phase1(&mut conn, &make_file("readme.txt", 2000, "v2"), None).unwrap();
        assert!(matches!(outcome, Phase1Outcome::Modified { .. }));
        assert_eq!(stored_mtime(&conn, "readme.txt"), Some(2000));
    }

    #[test]
    fn stale_mtime_is_skipped() {
        let mut conn = test_conn();
        process_file_phase1(&mut conn, &make_file("readme.txt", 2000, "current"), None).unwrap();
        let outcome = process_file_phase1(&mut conn, &make_file("readme.txt", 1000, "stale"), None).unwrap();
        assert!(matches!(outcome, Phase1Outcome::Skipped));
        assert_eq!(stored_mtime(&conn, "readme.txt"), Some(2000));
    }

    #[test]
    fn file_hash_registers_duplicate_pair() {
        let mut conn = test_conn();

        let mut file_a = make_file("original.txt", 1000, "shared content");
        file_a.file_hash = Some("abc123".to_string());
        let outcome_a = process_file_phase1(&mut conn, &file_a, None).unwrap();
        assert!(matches!(outcome_a, Phase1Outcome::New));

        // Only one file → no duplicates yet.
        let dup_count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM duplicates WHERE file_hash = 'abc123'",
            [],
            |r| r.get(0),
        ).unwrap();
        assert_eq!(dup_count, 0, "single file should not be in duplicates");

        let mut file_b = make_file("duplicate.txt", 1100, "shared content");
        file_b.file_hash = Some("abc123".to_string());
        let outcome_b = process_file_phase1(&mut conn, &file_b, None).unwrap();
        assert!(matches!(outcome_b, Phase1Outcome::New));

        // Both files should now be in duplicates.
        let dup_count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM duplicates WHERE file_hash = 'abc123'",
            [],
            |r| r.get(0),
        ).unwrap();
        assert_eq!(dup_count, 2, "both files should be in duplicates table");
    }

    #[test]
    fn no_duplicate_entry_for_unique_hash() {
        let mut conn = test_conn();
        let mut file = make_file("unique.txt", 1000, "unique content");
        file.file_hash = Some("unique_hash".to_string());
        process_file_phase1(&mut conn, &file, None).unwrap();

        let dup_count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM duplicates",
            [],
            |r| r.get(0),
        ).unwrap();
        assert_eq!(dup_count, 0, "unique file should not be in duplicates");
    }

    #[test]
    fn storage_stores_file_hash_in_files() {
        let mut conn = test_conn();
        let mut file = make_file("doc.txt", 1000, "content");
        file.file_hash = Some("myhash".to_string());
        process_file_phase1(&mut conn, &file, None).unwrap();

        let stored_hash: Option<String> = conn.query_row(
            "SELECT file_hash FROM files WHERE path = 'doc.txt'",
            [],
            |r| r.get(0),
        ).ok();
        assert_eq!(stored_hash.as_deref(), Some("myhash"));
    }

    /// Open an in-tempdir content store and return it alongside a function
    /// that puts a blob (lines joined with '\n') under a given hash.
    fn open_store() -> (tempfile::TempDir, Arc<dyn ContentStore>) {
        let tmp = tempfile::tempdir().unwrap();
        let store: Arc<dyn ContentStore> =
            Arc::new(SqliteContentStore::open(tmp.path(), None, None, None).unwrap());
        (tmp, store)
    }

    /// Verify that the FTS5 index contains exactly `expected` rows matching `term`.
    fn fts_match_count(conn: &Connection, term: &str) -> i64 {
        conn.query_row(
            "SELECT COUNT(*) FROM lines_fts WHERE lines_fts MATCH ?1",
            rusqlite::params![term],
            |r| r.get(0),
        ).unwrap_or(0)
    }

    /// When a file is re-indexed and the content store holds the old content:
    /// - all old FTS rows (including empty-content ones) are deleted
    /// - new FTS rows for the updated, normalized content are inserted
    /// - the total FTS row count returns to the original number (no orphans)
    /// - old terms are no longer findable; new terms are
    #[test]
    fn re_index_cleans_fts_using_content_store() {
        use find_common::api::{LINE_PATH, LINE_METADATA, LINE_CONTENT_START};

        let (_tmp, store) = open_store();
        let mut conn = test_conn();

        // ── First index ──────────────────────────────────────────────────────
        // Build v1 file: 3 lines (path, metadata="", content).
        // The hash is arbitrary but must match what we seed into the content store.
        let old_hash = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        let mut file_v1 = make_file("notes.txt", 1000, "version one unique phrase");
        file_v1.file_hash = Some(old_hash.to_string());

        // Index v1 without the content store (first index, no old entries to clean).
        process_file_phase1(&mut conn, &file_v1, None).unwrap();
        assert_eq!(fts_row_count(&conn), 3, "3 FTS rows after first index");

        // Seed the content store with the SAME content that was inserted into FTS5.
        // The content stored in the archive is the normalized lines joined with '\n'.
        // Lines: [LINE_PATH="notes.txt", LINE_METADATA="", LINE_CONTENT_START="version one unique phrase"]
        let old_blob = format!("{}\n\n{}", "notes.txt", "version one unique phrase");
        store.put(&ContentKey::new(old_hash), &old_blob).unwrap();

        // Confirm v1 content is searchable.
        assert!(fts_match_count(&conn, "uni") > 0, "trigram 'uni' from 'unique' should match");

        // ── Re-index with v2 ─────────────────────────────────────────────────
        let new_hash = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
        let mut file_v2 = make_file("notes.txt", 2000, "version two distinct phrase");
        file_v2.file_hash = Some(new_hash.to_string());

        process_file_phase1(&mut conn, &file_v2, Some(store.as_ref())).unwrap();

        // FTS row count must be back to 3 — no orphans accumulated.
        assert_eq!(fts_row_count(&conn), 3, "FTS row count must stay at 3 after re-index");

        // Old content must be gone: "unique" (7 chars, trigrams guaranteed ≥ 3 chars)
        // only appeared in v1 and must no longer match anything.
        assert_eq!(fts_match_count(&conn, "unique"), 0, "old term 'unique' must be removed from FTS");

        // New content must be findable: "distinct" only appears in v2.
        assert!(fts_match_count(&conn, "distinct") > 0, "new term 'distinct' must be in FTS");
    }
}
