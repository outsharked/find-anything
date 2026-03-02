use anyhow::{Context, Result};
use flate2::read::GzDecoder;
use rusqlite::{Connection, OptionalExtension};
use std::collections::HashMap;
use std::ffi::OsStr;
use std::io::Read;
use std::path::{Path, PathBuf};

use find_common::api::{BulkRequest, IndexFile, IndexLine, IndexingFailure, WorkerStatus};

use crate::archive::{self, ArchiveManager, ChunkRef};
use crate::db;

const POLL_INTERVAL: std::time::Duration = std::time::Duration::from_secs(1);

/// Log a warning if `start` is older than `threshold_secs`.
fn warn_slow(start: std::time::Instant, threshold_secs: u64, step: &str, context: &str) {
    let elapsed = start.elapsed();
    if elapsed.as_secs() >= threshold_secs {
        tracing::warn!(
            elapsed_secs = elapsed.as_secs(),
            step,
            context,
            "Slow step: {step} took {:.1}s for {context}",
            elapsed.as_secs_f64(),
        );
    }
}

type StatusHandle = std::sync::Arc<std::sync::Mutex<WorkerStatus>>;

/// Start the inbox worker that processes index requests asynchronously.
/// Polls the inbox directory every second for new `.gz` files.
pub async fn start_inbox_worker(data_dir: PathBuf, status: StatusHandle) -> Result<()> {
    let inbox_dir = data_dir.join("inbox");
    tokio::fs::create_dir_all(&inbox_dir).await?;

    let failed_dir = inbox_dir.join("failed");
    tokio::fs::create_dir_all(&failed_dir).await?;

    tracing::info!("Starting inbox worker, monitoring: {}", inbox_dir.display());

    let mut interval = tokio::time::interval(POLL_INTERVAL);
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    loop {
        interval.tick().await;

        let mut entries = match tokio::fs::read_dir(&inbox_dir).await {
            Ok(e) => e,
            Err(e) => {
                tracing::error!("Failed to read inbox dir: {e}");
                continue;
            }
        };

        while let Ok(Some(entry)) = entries.next_entry().await {
            let path = entry.path();
            if path.extension() == Some(OsStr::new("gz")) {
                process_request_async(&data_dir, &path, &failed_dir, status.clone()).await;
            }
        }
    }
}

async fn process_request_async(
    data_dir: &Path,
    request_path: &Path,
    failed_dir: &Path,
    status: StatusHandle,
) {
    let status_reset = status.clone(); // held outside the closure to ensure Idle on any exit path

    let result = tokio::task::spawn_blocking({
        let data_dir = data_dir.to_path_buf();
        let request_path = request_path.to_path_buf();
        move || process_request(&data_dir, &request_path, &status)
    })
    .await;

    // Ensure idle is set even if process_request errored or panicked.
    if let Ok(mut guard) = status_reset.lock() {
        *guard = WorkerStatus::Idle;
    }

    match result {
        Ok(Ok(())) => {
            // Success - delete request file
            if let Err(e) = tokio::fs::remove_file(&request_path).await {
                tracing::error!("Failed to delete processed request {}: {}", request_path.display(), e);
            } else {
                tracing::info!("Successfully processed: {}", request_path.display());
            }
        }
        Ok(Err(e)) => {
            // Processing error - move to failed
            handle_failure(request_path, failed_dir, e).await;
        }
        Err(e) => {
            // Task panicked or was cancelled
            handle_failure(
                request_path,
                failed_dir,
                anyhow::anyhow!("Task error: {}", e),
            )
            .await;
        }
    }
}

fn process_request(data_dir: &Path, request_path: &Path, status: &StatusHandle) -> Result<()> {
    tracing::info!("Processing request: {}", request_path.display());
    let request_start = std::time::Instant::now();

    // Decompress and parse request
    let compressed = std::fs::read(request_path)?;
    let mut decoder = GzDecoder::new(&compressed[..]);
    let mut json = String::new();
    decoder.read_to_string(&mut json)?;

    let request: BulkRequest = serde_json::from_str(&json)
        .context("parsing bulk request JSON")?;

    // Open source database
    let db_path = data_dir.join("sources").join(format!("{}.db", request.source));
    let mut conn = db::open(&db_path)?;

    // Initialize archive manager
    let mut archive_mgr = ArchiveManager::new(data_dir.to_path_buf());

    // 1. Deletions first (handles renames where path appears in both lists)
    if !request.delete_paths.is_empty() {
        db::delete_files(&conn, &mut archive_mgr, &request.delete_paths)?;
    }

    // 2. Upserts — update status per file so the UI shows what is being indexed.
    //    Errors are recorded per-file and processing continues; a single bad file
    //    must not prevent the rest of the batch from being indexed.
    let mut server_side_failures: Vec<IndexingFailure> = Vec::new();
    let mut successfully_indexed: Vec<String> = Vec::new();
    for file in &request.files {
        if let Ok(mut guard) = status.lock() {
            *guard = WorkerStatus::Processing {
                source: request.source.clone(),
                file: file.path.clone(),
            };
        }
        let file_start = std::time::Instant::now();
        match process_file(&mut conn, &mut archive_mgr, file, false) {
            Ok(()) => {
                successfully_indexed.push(file.path.clone());
            }
            Err(e) => {
                tracing::error!("Failed to index {}: {e:#}", file.path);
                // Insert a fallback record so the file appears in search and tree:
                //   1. appears in search by path name (line_number=0 entry)
                //   2. appears in the directory tree
                //
                // For outer archive files (kind="archive", no "::" in path) we use a
                // stub with mtime=0 so that re-indexing is triggered on the next scan
                // (any real mtime > 0).  We skip the inner-member DELETE in process_file
                // for the stub call to avoid disrupting inner members that may still be
                // alive (if the original failure occurred before that DELETE ran).
                //
                // For all other files we use the real mtime so they are not re-submitted
                // on every scan unless they actually change.
                let (fallback, skip_inner) = if is_outer_archive(&file.path, &file.kind) {
                    (outer_archive_stub(file), true)
                } else {
                    (filename_only_file(file), false)
                };
                if let Err(e2) = process_file(&mut conn, &mut archive_mgr, &fallback, skip_inner) {
                    tracing::error!("Filename-only fallback also failed for {}: {e2:#}", file.path);
                }
                // Do NOT add to successfully_indexed: the error should remain visible
                // in the UI even though the fallback put a filename-only record in the DB.
                server_side_failures.push(IndexingFailure {
                    path: file.path.clone(),
                    error: format!("{e:#}"),
                });
            }
        }
        warn_slow(file_start, 30, "process_file", &file.path);
    }

    // 3. Indexing error tracking
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;

    // Clear errors for successfully (re-)indexed paths.
    db::clear_errors_for_paths(&conn, &successfully_indexed)?;

    // Clear errors for explicitly deleted paths.
    db::clear_errors_for_paths(&conn, &request.delete_paths)?;

    // Store extraction failures reported by the client.
    if !request.indexing_failures.is_empty() {
        db::upsert_indexing_errors(&conn, &request.indexing_failures, now)?;
    }

    // Store server-side indexing failures encountered in this batch.
    if !server_side_failures.is_empty() {
        db::upsert_indexing_errors(&conn, &server_side_failures, now)?;
    }

    warn_slow(request_start, 120, "process_request", &request_path.display().to_string());

    // 4. Metadata
    if let Some(ts) = request.scan_timestamp {
        db::update_last_scan(&conn, ts)?;
        db::append_scan_history(&conn, ts)?;
    }
    if let Some(base_url) = &request.base_url {
        db::update_base_url(&conn, Some(base_url))?;
    }

    Ok(())
}

/// Returns `true` if `file` is a top-level archive (kind="archive" with no
/// "::" in the path).  These files require special handling in the fallback
/// path: see `outer_archive_stub` and the `skip_inner_delete` parameter of
/// `process_file`.
pub(crate) fn is_outer_archive(path: &str, kind: &str) -> bool {
    kind == "archive" && !path.contains("::")
}

fn filename_only_file(file: &IndexFile) -> IndexFile {
    IndexFile {
        path: file.path.clone(),
        mtime: file.mtime,
        size: file.size,
        // Force a non-archive kind so process_file never triggers the
        // is_outer_archive path (which deletes inner members outside the transaction).
        kind: if file.kind == "archive" { "unknown".to_string() } else { file.kind.clone() },
        lines: vec![IndexLine {
            archive_path: None,
            line_number: 0,
            content: file.path.clone(),
        }],
        extract_ms: None,
        content_hash: None,
    }
}

/// Build a minimal stub for an outer archive that failed extraction.
///
/// Unlike `filename_only_file`, this preserves `kind="archive"` so the file
/// appears as an expandable entry in the directory tree.  It uses `mtime=0`
/// so the scan client always sees a mtime mismatch and re-submits the file on
/// the next scan, giving extraction another chance to succeed.
///
/// The caller must pass `skip_inner_delete=true` to `process_file` when using
/// this stub so that any inner members still alive in the DB are not deleted.
fn outer_archive_stub(file: &IndexFile) -> IndexFile {
    IndexFile {
        path: file.path.clone(),
        mtime: 0, // force re-indexing on every subsequent scan
        size: file.size,
        kind: "archive".to_string(),
        lines: vec![IndexLine {
            archive_path: None,
            line_number: 0,
            content: file.path.clone(),
        }],
        extract_ms: None,
        content_hash: None,
    }
}

fn process_file(
    conn: &mut Connection,
    archive_mgr: &mut ArchiveManager,
    file: &find_common::api::IndexFile,
    skip_inner_delete: bool,
) -> Result<()> {
    let now_secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;

    // ── Pre-transaction reads and ZIP operations ────────────────────────────
    // These must happen outside the transaction: ZIP writes are not part of
    // SQLite, and reads here inform whether ZIP operations are needed at all.

    // If this is an outer archive file being re-indexed, delete stale inner members
    // first. They'll be re-submitted as separate IndexFile entries in the same batch.
    // We detect "outer archive" by kind == "archive" and no "::" in the path.
    // skip_inner_delete is set for the stub fallback path (see process_request error
    // handling) to avoid re-triggering this delete while the original failure is still
    // unresolved and the inner members may still be alive.
    if !skip_inner_delete && is_outer_archive(&file.path, &file.kind) {
        // Collect and remove chunks for all old inner members.
        let like_pat = format!("{}::%", file.path);
        let inner_ids: Vec<i64> = {
            let mut stmt = conn.prepare(
                "SELECT id FROM files WHERE path LIKE ?1",
            )?;
            let ids = stmt.query_map(rusqlite::params![like_pat], |row| row.get(0))?
                .collect::<rusqlite::Result<_>>()?;
            ids
        };
        let mut old_refs: Vec<ChunkRef> = Vec::new();
        for fid in inner_ids {
            let mut stmt = conn.prepare(
                "SELECT DISTINCT chunk_archive, chunk_name FROM lines WHERE file_id = ?1",
            )?;
            for r in stmt.query_map(rusqlite::params![fid], |row| {
                Ok(ChunkRef { archive_name: row.get(0)?, chunk_name: row.get(1)? })
            })? {
                old_refs.push(r?);
            }
        }
        if !old_refs.is_empty() {
            let t = std::time::Instant::now();
            archive_mgr.remove_chunks(old_refs)?;
            warn_slow(t, 10, "remove_chunks(archive_members)", &file.path);
        }
        conn.execute(
            "DELETE FROM files WHERE path LIKE ?1",
            rusqlite::params![like_pat],
        )?;
    }

    // ── Dedup check ────────────────────────────────────────────────────────
    // If the file has a content hash and another canonical with the same hash
    // exists, record this file as an alias and skip chunk/lines/FTS writes.
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
            // Register as alias — no chunks/lines/FTS written.
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
    // ── End dedup check ────────────────────────────────────────────────────

    // Remove old chunks for this specific file before writing new ones.
    let existing_id: Option<i64> = conn.query_row(
        "SELECT id FROM files WHERE path = ?1",
        rusqlite::params![file.path],
        |row| row.get(0),
    ).optional()?;

    if let Some(fid) = existing_id {
        let mut stmt = conn.prepare(
            "SELECT DISTINCT chunk_archive, chunk_name FROM lines WHERE file_id = ?1",
        )?;
        let old_refs: Vec<ChunkRef> = stmt
            .query_map(rusqlite::params![fid], |row| {
                Ok(ChunkRef { archive_name: row.get(0)?, chunk_name: row.get(1)? })
            })?
            .collect::<rusqlite::Result<_>>()?;
        if !old_refs.is_empty() {
            let t = std::time::Instant::now();
            archive_mgr.remove_chunks(old_refs)?;
            warn_slow(t, 10, "remove_chunks(reindex)", &file.path);
        }
    }

    // Prepare lines for chunking (line_number, content)
    let line_data: Vec<(usize, String)> = file.lines.iter()
        .map(|l| (l.line_number, l.content.clone()))
        .collect();

    // Chunk lines into ~1KB pieces
    let chunk_result = archive::chunk_lines(&file.path, &line_data);

    // Append chunks to ZIP archives (must happen before the transaction)
    let t_append = std::time::Instant::now();
    let chunk_refs = archive_mgr.append_chunks(chunk_result.chunks.clone())?;
    warn_slow(t_append, 10, "append_chunks", &file.path);

    // Build mapping: chunk_number → chunk_ref
    let mut chunk_ref_map: HashMap<usize, ChunkRef> = HashMap::new();
    for (chunk, chunk_ref) in chunk_result.chunks.iter().zip(chunk_refs.iter()) {
        chunk_ref_map.insert(chunk.chunk_number, chunk_ref.clone());
    }

    // Build lookup: line_number → original line for FTS5 content
    let mut line_content_map: HashMap<usize, String> = HashMap::new();
    for line in &file.lines {
        line_content_map.insert(line.line_number, line.content.clone());
    }

    // ── Single transaction for all SQLite writes ───────────────────────────
    // Batching every INSERT into one transaction eliminates per-row fsyncs,
    // which dominate wall-clock time for large files (e.g. verbose XML logs).
    // ZIP chunks are already written above; a rollback here leaves orphaned
    // chunks that are harmlessly overwritten on the next re-index.
    let t_fts = std::time::Instant::now();
    let tx = conn.transaction()?;

    // Upsert file record as canonical (canonical_file_id = NULL).
    // indexed_at is set on first insert and not overwritten on conflict.
    tx.execute(
        "INSERT INTO files (path, mtime, size, kind, indexed_at, extract_ms, content_hash, canonical_file_id)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, NULL)
         ON CONFLICT(path) DO UPDATE SET
           mtime             = excluded.mtime,
           size              = excluded.size,
           kind              = excluded.kind,
           extract_ms        = excluded.extract_ms,
           content_hash      = excluded.content_hash,
           canonical_file_id = NULL",
        rusqlite::params![
            file.path, file.mtime, file.size, file.kind,
            now_secs,
            file.extract_ms.map(|ms| ms as i64),
            file.content_hash.as_deref(),
        ],
    )?;

    let file_id: i64 = tx.query_row(
        "SELECT id FROM files WHERE path = ?1",
        rusqlite::params![file.path],
        |row| row.get(0),
    )?;

    // Delete old lines for this file.
    // TODO(fts5-stale): When lines are deleted here (and via CASCADE from `DELETE FROM files`),
    // their corresponding `lines_fts` entries are NOT cleaned up because `lines_fts` is a
    // contentless FTS5 table (content='') with no triggers. Stale FTS5 rowids accumulate
    // over time, causing `fts_count` (and thus the `total` field in search responses) to be
    // inflated. Actual search results are correct because the JOIN with `lines` filters them
    // out, but pagination may misbehave if the client uses `total` to decide whether to
    // fetch more pages.
    tx.execute("DELETE FROM lines WHERE file_id = ?1", rusqlite::params![file_id])?;

    // Insert lines with chunk references and populate FTS5
    for mapping in &chunk_result.line_mappings {
        let chunk_ref = chunk_ref_map.get(&mapping.chunk_number)
            .context("chunk ref not found")?;

        let line_content = line_content_map.get(&mapping.line_number)
            .context("line content not found")?;

        let line_id = tx.query_row(
            "INSERT INTO lines (file_id, line_number, chunk_archive, chunk_name, line_offset_in_chunk)
             VALUES (?1, ?2, ?3, ?4, ?5)
             RETURNING id",
            rusqlite::params![
                file_id,
                mapping.line_number as i64,
                chunk_ref.archive_name,
                chunk_ref.chunk_name,
                mapping.offset_in_chunk as i64,
            ],
            |row| row.get::<_, i64>(0),
        )?;

        tx.execute(
            "INSERT INTO lines_fts(rowid, content) VALUES (?1, ?2)",
            rusqlite::params![line_id, line_content],
        )?;
    }

    tx.commit()?;
    warn_slow(t_fts, 10, "fts_insert", &file.path);

    Ok(())
}

async fn handle_failure(path: &Path, failed_dir: &Path, error: anyhow::Error) {
    tracing::error!("Failed to process {}: {}", path.display(), error);

    let failed_path = failed_dir.join(path.file_name().unwrap());
    if let Err(e) = tokio::fs::rename(path, &failed_path).await {
        tracing::error!(
            "Failed to move {} to failed directory: {}",
            path.display(),
            e
        );
    } else {
        tracing::warn!("Moved failed request to: {}", failed_path.display());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use find_common::api::IndexLine;

    fn make_file(path: &str, kind: &str) -> IndexFile {
        IndexFile {
            path: path.to_string(),
            mtime: 1000,
            size: 100,
            kind: kind.to_string(),
            lines: vec![IndexLine {
                archive_path: None,
                line_number: 0,
                content: path.to_string(),
            }],
            extract_ms: None,
            content_hash: None,
        }
    }

    // ── is_outer_archive ─────────────────────────────────────────────────────

    #[test]
    fn outer_archive_detected() {
        assert!(is_outer_archive("data.zip", "archive"));
    }

    #[test]
    fn archive_member_not_outer() {
        assert!(!is_outer_archive("data.zip::inner.txt", "archive"));
    }

    #[test]
    fn non_archive_kind_not_outer() {
        assert!(!is_outer_archive("data.zip", "text"));
    }

    // ── filename_only_file ───────────────────────────────────────────────────

    #[test]
    fn filename_only_converts_archive_kind_to_unknown() {
        // Must not remain "archive" — would re-trigger the outer-archive DELETE
        // path on next process_file call, permanently losing inner members.
        let f = make_file("data.zip", "archive");
        let fallback = filename_only_file(&f);
        assert_eq!(fallback.kind, "unknown");
    }

    #[test]
    fn filename_only_keeps_non_archive_kind() {
        let f = make_file("notes.md", "text");
        let fallback = filename_only_file(&f);
        assert_eq!(fallback.kind, "text");
    }

    #[test]
    fn filename_only_has_single_path_line() {
        let f = make_file("docs/report.pdf", "pdf");
        let fallback = filename_only_file(&f);
        assert_eq!(fallback.lines.len(), 1);
        assert_eq!(fallback.lines[0].line_number, 0);
        assert_eq!(fallback.lines[0].content, "docs/report.pdf");
    }

    #[test]
    fn filename_only_preserves_mtime_and_size() {
        let f = make_file("file.txt", "text");
        let fallback = filename_only_file(&f);
        assert_eq!(fallback.mtime, f.mtime);
        assert_eq!(fallback.size, f.size);
    }

    // ── outer_archive_stub ────────────────────────────────────────────────────

    #[test]
    fn outer_archive_stub_preserves_archive_kind() {
        let f = make_file("backup.7z", "archive");
        let stub = outer_archive_stub(&f);
        assert_eq!(stub.kind, "archive");
    }

    #[test]
    fn outer_archive_stub_uses_zero_mtime() {
        // mtime=0 ensures the scan client always re-submits the file.
        let f = make_file("backup.7z", "archive");
        let stub = outer_archive_stub(&f);
        assert_eq!(stub.mtime, 0);
    }

    #[test]
    fn outer_archive_stub_has_single_path_line() {
        let f = make_file("backups/big.tar.gz", "archive");
        let stub = outer_archive_stub(&f);
        assert_eq!(stub.lines.len(), 1);
        assert_eq!(stub.lines[0].line_number, 0);
        assert_eq!(stub.lines[0].content, "backups/big.tar.gz");
    }
}
