/// Per-file SQLite processing (phase 1 — no ZIP I/O).
///
/// These functions operate on a single `IndexFile` at a time and are called
/// by `process_request_phase1` in the request-level coordinator.
use anyhow::Result;
use rusqlite::Connection;
use rusqlite::OptionalExtension;

use find_common::api::{IndexFile, IndexLine};
use find_common::path::{composite_like_prefix, is_composite};

// ── Public entry points ───────────────────────────────────────────────────────

/// Write a single file's metadata and content lines to SQLite.
/// Thin wrapper that calls `process_file_phase1_fallback` with `skip_inner_delete = false`.
pub(super) fn process_file_phase1(
    conn: &mut Connection,
    file: &IndexFile,
    inline_threshold_bytes: u64,
) -> Result<()> {
    process_file_phase1_fallback(conn, file, false, inline_threshold_bytes)
}

/// Like `process_file_phase1` but optionally skips the deletion of inner
/// archive members (used when indexing a fallback/stub for a failed outer archive).
pub(super) fn process_file_phase1_fallback(
    conn: &mut Connection,
    file: &IndexFile,
    skip_inner_delete: bool,
    inline_threshold_bytes: u64,
) -> Result<()> {
    let now_secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;

    // If re-indexing an outer archive, delete stale inner members first (SQL only).
    if !skip_inner_delete && is_outer_archive(&file.path, &file.kind) && file.mtime == 0 {
        let like_pat = composite_like_prefix(&file.path);
        // Collect chunk refs for inner members and queue them for removal.
        let file_ids: Vec<i64> = {
            let mut stmt = conn.prepare("SELECT id FROM files WHERE path LIKE ?1")?;
            let ids = stmt.query_map(rusqlite::params![like_pat], |row| row.get(0))?
                .collect::<rusqlite::Result<_>>()?;
            ids
        };
        let tx = conn.unchecked_transaction()?;
        for fid in file_ids {
            tx.execute(
                "INSERT INTO pending_chunk_removes (archive_name, chunk_name)
                 SELECT DISTINCT chunk_archive, chunk_name
                 FROM lines
                 WHERE file_id = ?1 AND chunk_archive IS NOT NULL",
                rusqlite::params![fid],
            )?;
        }
        tx.execute("DELETE FROM files WHERE path LIKE ?1", rusqlite::params![like_pat])?;
        tx.commit()?;
    }

    // Dedup check: if another canonical with the same content hash exists,
    // register this file as an alias and skip chunk/lines/FTS writes.
    if let Some(hash) = &file.content_hash {
        let canonical_id: Option<i64> = conn.query_row(
            "SELECT id FROM files
             WHERE content_hash = ?1
               AND canonical_file_id IS NULL
               AND path != ?2
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
                    file.path, file.mtime, file.size, file.kind,
                    now_secs,
                    file.extract_ms.map(|ms| ms as i64),
                    hash,
                    canonical_id,
                ],
            )?;
            return Ok(());
        }
    }

    // Stale-mtime guard: skip if the stored mtime is already newer.
    let stored_mtime: Option<i64> = conn.query_row(
        "SELECT mtime FROM files WHERE path = ?1",
        rusqlite::params![file.path],
        |row| row.get(0),
    ).optional()?;
    if let Some(stored) = stored_mtime {
        if file.mtime > 0 && file.mtime < stored {
            tracing::debug!(
                "skipping stale upsert for {} (incoming mtime={} < stored={})",
                file.path, file.mtime, stored
            );
            return Ok(());
        }
    }

    // Collect old chunk refs for this file and queue them for removal.
    let existing_id: Option<i64> = conn.query_row(
        "SELECT id FROM files WHERE path = ?1",
        rusqlite::params![file.path],
        |row| row.get(0),
    ).optional()?;

    // Decide: inline vs deferred archive storage.
    let total_content_bytes: usize = file.lines.iter().map(|l| l.content.len()).sum();
    let use_inline = inline_threshold_bytes > 0
        && total_content_bytes as u64 <= inline_threshold_bytes;

    // Single transaction for the whole file.
    let t_fts = std::time::Instant::now();
    let tx = conn.transaction()?;

    // Queue old chunk refs for removal (before we overwrite the file record).
    if let Some(fid) = existing_id {
        tx.execute(
            "INSERT INTO pending_chunk_removes (archive_name, chunk_name)
             SELECT DISTINCT chunk_archive, chunk_name
             FROM lines
             WHERE file_id = ?1 AND chunk_archive IS NOT NULL",
            rusqlite::params![fid],
        )?;
    }

    // Upsert the file record and get the stable file_id.
    let file_id: i64 = tx.query_row(
        "INSERT INTO files (path, mtime, size, kind, indexed_at, extract_ms, content_hash, canonical_file_id)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, NULL)
         ON CONFLICT(path) DO UPDATE SET
           mtime             = excluded.mtime,
           size              = excluded.size,
           kind              = excluded.kind,
           indexed_at        = excluded.indexed_at,
           extract_ms        = excluded.extract_ms,
           content_hash      = excluded.content_hash,
           canonical_file_id = NULL
         RETURNING id",
        rusqlite::params![
            file.path, file.mtime, file.size, file.kind,
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

    Ok(())
}

// ── Helper constructors ───────────────────────────────────────────────────────

/// Returns `true` if `file` is a top-level archive (kind="archive" with no
/// "::" in the path).
pub(crate) fn is_outer_archive(path: &str, kind: &str) -> bool {
    kind == "archive" && !is_composite(path)
}

/// Build a fallback `IndexFile` that records only the file path (line 0).
/// Used when content extraction fails so the file is still findable by name.
pub(super) fn filename_only_file(file: &IndexFile) -> IndexFile {
    IndexFile {
        path: file.path.clone(),
        mtime: file.mtime,
        size: file.size,
        kind: if file.kind == "archive" { "unknown".to_string() } else { file.kind.clone() },
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
        kind: "archive".to_string(),
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

