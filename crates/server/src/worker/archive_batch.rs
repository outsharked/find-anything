/// Archive phase (phase 2) of the inbox worker.
///
/// Reads `.gz` files from `inbox/to-archive/`, parses them, appends content
/// chunks to ZIP archives, and inserts `content_chunks` rows in SQLite.
/// Separated from the phase-1 indexing loop so neither phase blocks the other.
use std::collections::HashMap;
use std::ffi::OsStr;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result};
use flate2::read::GzDecoder;
use rusqlite::OptionalExtension;

use find_common::api::{BulkRequest, IndexFile};

use crate::archive::{self, ArchiveManager, ChunkRef, SharedArchiveState};
use crate::db;
use super::WorkerConfig;

macro_rules! timed {
    ($tag:expr, $label:expr, $body:expr) => {{
        tracing::debug!("{} → {}", $tag, $label);
        let __t = std::time::Instant::now();
        let __r = $body;
        tracing::debug!("{} ← {} ({:.1}ms)", $tag, $label, __t.elapsed().as_secs_f64() * 1000.0);
        __r
    }};
}

// ── Public entry points ───────────────────────────────────────────────────────

/// Scan `to_archive_dir` for `.gz` files, process up to `cfg.archive_batch_size`
/// of them through the archive phase, and return the number processed.
pub(super) fn run_archive_batch(
    data_dir: &Path,
    to_archive_dir: &Path,
    cfg: WorkerConfig,
    shared_archive_state: &Arc<SharedArchiveState>,
) -> Result<usize> {
    // Scan to-archive/ for .gz files sorted by mtime.
    let mut gz_files: Vec<(std::time::SystemTime, PathBuf)> = Vec::new();
    for entry in std::fs::read_dir(to_archive_dir)?.flatten() {
        let path = entry.path();
        if path.extension() == Some(OsStr::new("gz")) {
            let mtime = entry.metadata()
                .ok()
                .and_then(|m| m.modified().ok())
                .unwrap_or(std::time::UNIX_EPOCH);
            gz_files.push((mtime, path));
        }
    }
    gz_files.sort_unstable_by_key(|(mtime, _)| *mtime);

    if gz_files.is_empty() {
        return Ok(0);
    }

    let batch: Vec<PathBuf> = gz_files.into_iter()
        .take(cfg.archive_batch_size)
        .map(|(_, p)| p)
        .collect();
    let processed = batch.len();

    process_archive_batch(data_dir, &batch, shared_archive_state)?;

    Ok(processed)
}

// ── Internal ──────────────────────────────────────────────────────────────────

/// Process a batch of .gz files through the archive phase.
///
/// Write serialisation: the archive thread holds the per-source
/// `source_lock` only during SQLite write transactions.
/// The lock is explicitly released before ZIP I/O so the indexing thread is not
/// blocked during expensive disk work.
fn process_archive_batch(
    data_dir: &Path,
    gz_paths: &[PathBuf],
    shared_archive_state: &Arc<SharedArchiveState>,
) -> Result<()> {
    struct ParsedRequest {
        gz_path: PathBuf,
        request: BulkRequest,
    }

    let tag = "[archive-batch]";
    let mut parsed_requests: Vec<ParsedRequest> = Vec::new();
    timed!(tag, format!("parse {} gz files", gz_paths.len()), {
        for gz_path in gz_paths {
            match parse_gz_request(gz_path) {
                Ok(request) => parsed_requests.push(ParsedRequest { gz_path: gz_path.clone(), request }),
                Err(e) => {
                    tracing::error!("Archive batch: failed to parse {}: {e:#}", gz_path.display());
                    // Skip this file but don't delete it — leave for manual inspection.
                }
            }
        }
    });

    if parsed_requests.is_empty() {
        // Delete any files that failed to parse (to avoid stuck queue).
        for gz_path in gz_paths {
            let _ = std::fs::remove_file(gz_path);
        }
        return Ok(());
    }

    // Group by source.
    let mut by_source: HashMap<String, Vec<&ParsedRequest>> = HashMap::new();
    for pr in &parsed_requests {
        by_source.entry(pr.request.source.clone()).or_default().push(pr);
    }

    for (source, requests) in &by_source {
        let db_path = data_dir.join("sources").join(format!("{source}.db"));
        let conn = match db::open(&db_path) {
            Ok(c) => c,
            Err(e) => {
                tracing::error!("Archive batch: failed to open DB for source {source}: {e:#}");
                continue;
            }
        };

        let src_tag = format!("[archive:{source}]");
        let source_lock = shared_archive_state.source_lock(source);

        // Coalesce upserts: last-writer-wins by submission order (mtime sort).
        let mut upsert_map: HashMap<String, IndexFile> = HashMap::new();
        let mut delete_set: std::collections::HashSet<String> = std::collections::HashSet::new();

        for pr in requests.iter() {
            for path in &pr.request.delete_paths {
                delete_set.insert(path.clone());
                upsert_map.remove(path);
            }
            for file in &pr.request.files {
                if !delete_set.contains(&file.path) {
                    upsert_map.insert(file.path.clone(), file.clone());
                }
            }
        }

        // Create ArchiveManager for appending new chunks.
        let mut archive_mgr = ArchiveManager::new(Arc::clone(shared_archive_state));

        // Collect work items for files that need archiving.
        struct ArchiveWork {
            #[allow(dead_code)]
            file_id: i64,
            block_id: i64,
            #[allow(dead_code)]
            content_hash: String,
            path: String,
            line_data: Vec<(usize, String)>,
        }
        let mut archive_works: Vec<ArchiveWork> = Vec::new();
        // Track block_ids added in this batch to avoid duplicate writes for files
        // that share the same content hash (content-addressable dedup).
        let mut seen_block_ids: std::collections::HashSet<i64> = std::collections::HashSet::new();

        for (path, file) in &upsert_map {
            // Look up file_id and content_hash.
            let file_row: Option<(i64, Option<String>)> = conn.query_row(
                "SELECT id, content_hash FROM files WHERE path = ?1",
                rusqlite::params![path],
                |row| Ok((row.get(0)?, row.get(1)?)),
            ).optional().unwrap_or(None);

            let Some((file_id, Some(content_hash))) = file_row else {
                continue; // file deleted or no content hash
            };

            // Look up block_id from content_blocks.
            let block_id: Option<i64> = conn.query_row(
                "SELECT id FROM content_blocks WHERE content_hash = ?1",
                rusqlite::params![&content_hash],
                |row| row.get(0),
            ).optional().unwrap_or(None);

            let Some(block_id) = block_id else {
                continue; // no content block registered
            };

            // Check if already archived (content_chunks has rows for this block_id).
            let already_archived: i64 = conn.query_row(
                "SELECT COUNT(*) FROM content_chunks WHERE block_id = ?1",
                rusqlite::params![block_id],
                |row| row.get(0),
            ).unwrap_or(0);
            if already_archived > 0 {
                continue; // already archived (possibly by another file with same hash)
            }

            // Check if inline (permanently stored in file_content).
            let is_inline: i64 = conn.query_row(
                "SELECT COUNT(*) FROM file_content WHERE file_id = ?1",
                rusqlite::params![file_id],
                |row| row.get(0),
            ).unwrap_or(0);
            if is_inline > 0 {
                continue; // inline storage, no ZIP needed
            }

            let line_data: Vec<(usize, String)> = file.lines.iter()
                .map(|l| (l.line_number, l.content.clone()))
                .collect();

            // Skip if no lines to archive.
            if line_data.is_empty() {
                continue;
            }

            // Skip if another file in this batch already queued this block_id.
            if !seen_block_ids.insert(block_id) {
                continue;
            }

            archive_works.push(ArchiveWork { file_id, block_id, content_hash, path: path.clone(), line_data });
        }

        // Append chunks and collect mappings.
        struct ArchivedFile {
            block_id: i64,
            chunk_ranges: Vec<archive::ChunkRange>,
            chunk_refs: Vec<ChunkRef>,
        }
        let mut archived_files: Vec<ArchivedFile> = Vec::new();

        let n_archive_works = archive_works.len();
        timed!(src_tag, format!("append chunks for {} files", n_archive_works), {
            for work in archive_works {
                let chunk_result = archive::chunk_lines(work.block_id, &work.line_data);
                match archive_mgr.append_chunks(chunk_result.chunks) {
                    Ok(chunk_refs) => {
                        archived_files.push(ArchivedFile {
                            block_id: work.block_id,
                            chunk_ranges: chunk_result.ranges,
                            chunk_refs,
                        });
                    }
                    Err(e) => {
                        tracing::error!("Archive batch: failed to append chunks for {}: {e:#}", work.path);
                    }
                }
            }
        });

        // --- SQLite write segment 2: insert content_chunks rows ---
        if !archived_files.is_empty() {
            let _guard = timed!(src_tag, "acquire source lock for chunk insert", {
                source_lock.lock()
                    .map_err(|_| anyhow::anyhow!("source lock poisoned for {source}"))?
            });
            timed!(src_tag, format!("insert content_chunks for {} files", archived_files.len()), {
            let tx = conn.unchecked_transaction()?;
            for af in &archived_files {
                // Re-check inside the transaction: another batch may have committed
                // content_chunks for this block_id between our pre-scan check and now.
                // This makes the check-then-insert atomic under the source lock.
                let already_committed: i64 = tx.query_row(
                    "SELECT COUNT(*) FROM content_chunks WHERE block_id = ?1",
                    rusqlite::params![af.block_id],
                    |r| r.get(0),
                )?;
                if already_committed > 0 {
                    tracing::debug!(
                        "block_id={} already committed by concurrent batch — skipping",
                        af.block_id
                    );
                    continue;
                }

                // Build map from chunk_number → ChunkRef.
                let mut chunk_ref_by_number: HashMap<usize, &ChunkRef> = HashMap::new();
                for (i, cr) in af.chunk_refs.iter().enumerate() {
                    chunk_ref_by_number.insert(i, cr);
                }

                for range in &af.chunk_ranges {
                    let Some(chunk_ref) = chunk_ref_by_number.get(&range.chunk_number) else {
                        continue;
                    };

                    // Upsert archive name → id.
                    tx.execute(
                        "INSERT OR IGNORE INTO content_archives(name) VALUES(?1)",
                        rusqlite::params![chunk_ref.archive_name],
                    )?;
                    let archive_id: i64 = tx.query_row(
                        "SELECT id FROM content_archives WHERE name = ?1",
                        rusqlite::params![chunk_ref.archive_name],
                        |r| r.get(0),
                    )?;

                    if let Err(e) = tx.execute(
                        "INSERT INTO content_chunks(block_id, chunk_number, archive_id, start_line, end_line)
                         VALUES(?1, ?2, ?3, ?4, ?5)",
                        rusqlite::params![
                            af.block_id,
                            range.chunk_number as i64,
                            archive_id,
                            range.start_line as i64,
                            range.end_line as i64,
                        ],
                    ) {
                        tracing::error!(
                            "Archive batch: failed to insert content_chunk for block_id={}: {e}",
                            af.block_id
                        );
                    }
                }
            }
            tx.commit()?;
            }); // timed! insert content_chunks
        }

        if !archived_files.is_empty() {
            tracing::info!(
                "[archive:{source}] {} requests: archived {} files",
                requests.len(),
                archived_files.len(),
            );
        }
    }

    // Delete all processed .gz files from to-archive/.
    for pr in &parsed_requests {
        if let Err(e) = std::fs::remove_file(&pr.gz_path) {
            tracing::error!("Archive batch: failed to delete {}: {e}", pr.gz_path.display());
        }
    }

    Ok(())
}

pub(super) fn parse_gz_request(gz_path: &Path) -> Result<BulkRequest> {
    let compressed = std::fs::read(gz_path)?;
    let mut decoder = GzDecoder::new(&compressed[..]);
    let mut json = String::new();
    decoder.read_to_string(&mut json)?;
    let request: BulkRequest = serde_json::from_str(&json)
        .context("parsing bulk request JSON")?;
    Ok(request)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    use find_common::api::{BulkRequest, FileKind, IndexFile, IndexLine};
    use find_common::config::NormalizationSettings;

    use crate::db::encode_fts_rowid;

    fn setup_data_dir(data_dir: &Path) {
        std::fs::create_dir_all(data_dir.join("sources").join("content")).unwrap();
    }

    fn read_chunk_ranges(conn: &rusqlite::Connection, block_id: i64) -> Vec<(i64, i64)> {
        let mut stmt = conn.prepare(
            "SELECT start_line, end_line FROM content_chunks WHERE block_id = ?1 ORDER BY chunk_number"
        ).unwrap();
        stmt.query_map(rusqlite::params![block_id], |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?))
        }).unwrap().map(|r| r.unwrap()).collect()
    }

    fn write_bulk_gz(path: &Path, req: &BulkRequest) {
        let json = serde_json::to_vec(req).unwrap();
        let file = std::fs::File::create(path).unwrap();
        let mut enc = flate2::write::GzEncoder::new(file, flate2::Compression::default());
        enc.write_all(&json).unwrap();
        enc.finish().unwrap();
    }

    fn make_worker_config() -> WorkerConfig {
        WorkerConfig {
            request_timeout: std::time::Duration::from_secs(30),
            inline_threshold_bytes: 0,
            archive_batch_size: 10,
            activity_log_max_entries: 100,
            normalization: NormalizationSettings::default(),
        }
    }

    fn make_bulk_request(source: &str, path: &str, content: &str) -> BulkRequest {
        BulkRequest {
            source: source.to_string(),
            files: vec![IndexFile {
                path: path.to_string(),
                mtime: 1000,
                size: Some(content.len() as i64),
                kind: FileKind::Text,
                scanner_version: 1,
                lines: vec![
                    IndexLine {
                        archive_path: None,
                        line_number: 0,
                        content: path.to_string(),
                    },
                    IndexLine {
                        archive_path: None,
                        line_number: 1,
                        content: content.to_string(),
                    },
                ],
                extract_ms: None,
                content_hash: Some("testhash".to_string()),
                is_new: true,
            }],
            delete_paths: vec![],
            rename_paths: vec![],
            scan_timestamp: None,
            indexing_failures: vec![],
        }
    }

    /// Seed the DB with a file + content_block + FTS entries (no content_chunks yet).
    fn seed_db(data_dir: &Path, source: &str, path: &str) -> (rusqlite::Connection, i64, i64) {
        let db_path = data_dir.join("sources").join(format!("{source}.db"));
        std::fs::create_dir_all(db_path.parent().unwrap()).unwrap();
        let conn = crate::db::open(&db_path).unwrap();

        conn.execute(
            "INSERT INTO files (path, mtime, size, kind, indexed_at, extract_ms, content_hash, line_count)
             VALUES (?1, 1000, 100, 'text', 0, NULL, 'testhash', 2)",
            rusqlite::params![path],
        ).unwrap();
        let file_id: i64 = conn.last_insert_rowid();

        conn.execute(
            "INSERT OR IGNORE INTO content_blocks(content_hash) VALUES('testhash')",
            [],
        ).unwrap();
        let block_id: i64 = conn.query_row(
            "SELECT id FROM content_blocks WHERE content_hash = 'testhash'",
            [],
            |r| r.get(0),
        ).unwrap();

        // FTS rows using encode_fts_rowid.
        conn.execute(
            "INSERT INTO lines_fts(rowid, content) VALUES (?1, ?2)",
            rusqlite::params![encode_fts_rowid(file_id, 0), path],
        ).unwrap();
        conn.execute(
            "INSERT INTO lines_fts(rowid, content) VALUES (?1, ?2)",
            rusqlite::params![encode_fts_rowid(file_id, 1), "hello world"],
        ).unwrap();

        (conn, file_id, block_id)
    }

    #[test]
    fn chunks_written_and_content_chunks_inserted() {
        let data_tmp = tempfile::tempdir().unwrap();
        let to_archive_tmp = tempfile::tempdir().unwrap();
        let data_dir = data_tmp.path();
        let to_archive_dir = to_archive_tmp.path();

        setup_data_dir(data_dir);

        let source = "test_source";
        let path = "docs/readme.txt";

        let (conn, _file_id, block_id) = seed_db(data_dir, source, path);

        let req = make_bulk_request(source, path, "hello world");
        write_bulk_gz(&to_archive_dir.join("batch_001.gz"), &req);

        let cfg = make_worker_config();
        let shared = crate::archive::SharedArchiveState::new(data_dir.to_path_buf()).unwrap();

        let processed = run_archive_batch(data_dir, to_archive_dir, cfg, &shared).unwrap();
        assert_eq!(processed, 1, "should have processed 1 gz file");

        // Assert: .gz was removed.
        let gz_count = std::fs::read_dir(to_archive_dir)
            .unwrap()
            .flatten()
            .filter(|e| e.path().extension().map_or(false, |ext| ext == "gz"))
            .count();
        assert_eq!(gz_count, 0, "gz file should be removed after processing");

        // Assert: content_chunks rows exist.
        let ranges = read_chunk_ranges(&conn, block_id);
        assert!(!ranges.is_empty(), "content_chunks should have entries after archiving");

        // Assert: at least one ZIP archive created.
        let content_dir = data_dir.join("sources").join("content");
        let zip_count = std::fs::read_dir(&content_dir)
            .unwrap()
            .flatten()
            .filter(|e| e.path().is_dir())
            .flat_map(|subdir| std::fs::read_dir(subdir.path()).unwrap().flatten())
            .filter(|e| e.path().extension().map_or(false, |ext| ext == "zip"))
            .count();
        assert!(zip_count > 0, "at least one ZIP archive should have been created");
    }

    #[test]
    fn gz_file_removed_after_processing() {
        let data_tmp = tempfile::tempdir().unwrap();
        let to_archive_tmp = tempfile::tempdir().unwrap();
        let data_dir = data_tmp.path();
        let to_archive_dir = to_archive_tmp.path();

        setup_data_dir(data_dir);
        // No DB — file lookup will find nothing.

        let source = "ghost_source";
        let path = "nonexistent/file.txt";
        let req = make_bulk_request(source, path, "some content");
        write_bulk_gz(&to_archive_dir.join("ghost_001.gz"), &req);

        let cfg = make_worker_config();
        let shared = crate::archive::SharedArchiveState::new(data_dir.to_path_buf()).unwrap();

        let processed = run_archive_batch(data_dir, to_archive_dir, cfg, &shared).unwrap();
        assert_eq!(processed, 1, "should count 1 processed gz");

        let gz_count = std::fs::read_dir(to_archive_dir)
            .unwrap()
            .flatten()
            .filter(|e| e.path().extension().map_or(false, |ext| ext == "gz"))
            .count();
        assert_eq!(gz_count, 0, "gz file should be removed even when file has no DB entry");
    }

    #[test]
    fn already_archived_file_is_skipped() {
        let data_tmp = tempfile::tempdir().unwrap();
        let to_archive_tmp = tempfile::tempdir().unwrap();
        let data_dir = data_tmp.path();
        let to_archive_dir = to_archive_tmp.path();

        setup_data_dir(data_dir);

        let source = "test_source";
        let path = "docs/readme.txt";

        let (conn, _file_id, block_id) = seed_db(data_dir, source, path);

        let cfg = make_worker_config();
        let shared = crate::archive::SharedArchiveState::new(data_dir.to_path_buf()).unwrap();

        // First run — writes chunks and inserts content_chunks.
        let req = make_bulk_request(source, path, "hello world");
        write_bulk_gz(&to_archive_dir.join("first_001.gz"), &req);
        run_archive_batch(data_dir, to_archive_dir, cfg.clone(), &shared).unwrap();

        // Capture ranges after first run.
        let ranges_after_first = read_chunk_ranges(&conn, block_id);
        assert!(!ranges_after_first.is_empty(), "content_chunks should be set after first run");

        // Second run — same content, should skip (already archived).
        let req2 = make_bulk_request(source, path, "hello world");
        write_bulk_gz(&to_archive_dir.join("second_001.gz"), &req2);
        run_archive_batch(data_dir, to_archive_dir, cfg, &shared).unwrap();

        // Ranges should be identical to after-first-run state.
        let ranges_after_second = read_chunk_ranges(&conn, block_id);
        assert_eq!(
            ranges_after_first, ranges_after_second,
            "content_chunks should not change on second run"
        );
    }
}
