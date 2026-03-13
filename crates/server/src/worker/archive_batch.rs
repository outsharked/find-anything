/// Archive phase (phase 2) of the inbox worker.
///
/// Reads `.gz` files from `inbox/to-archive/`, parses them, appends content
/// chunks to ZIP archives, and updates `lines.chunk_archive` / `chunk_name` in
/// SQLite. Separated from the phase-1 indexing loop so neither phase blocks the
/// other.
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
/// `source_lock` only during SQLite write transactions (the UPDATE lines commit).
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
        // Build a map: path → last IndexFile; also track delete_paths.
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

        // Collect (file_id, line_data, path) for files that need archiving.
        struct ArchiveWork {
            file_id: i64,
            path: String,
            line_data: Vec<(usize, String)>,
        }
        let mut archive_works: Vec<ArchiveWork> = Vec::new();

        for (path, file) in &upsert_map {
            // Look up file_id.
            let file_id: Option<i64> = conn.query_row(
                "SELECT id FROM files WHERE path = ?1",
                rusqlite::params![path],
                |row| row.get(0),
            ).optional().unwrap_or(None);

            let Some(file_id) = file_id else {
                continue; // file deleted
            };

            // Check if already archived.
            let already_archived: i64 = conn.query_row(
                "SELECT COUNT(*) FROM lines WHERE file_id = ?1 AND chunk_archive IS NOT NULL",
                rusqlite::params![file_id],
                |row| row.get(0),
            ).unwrap_or(0);
            if already_archived > 0 {
                continue; // processed by a newer batch
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

            // Check if there are any lines at all (might be empty file).
            let line_count: i64 = conn.query_row(
                "SELECT COUNT(*) FROM lines WHERE file_id = ?1",
                rusqlite::params![file_id],
                |row| row.get(0),
            ).unwrap_or(0);
            if line_count == 0 {
                continue;
            }

            let line_data: Vec<(usize, String)> = file.lines.iter()
                .map(|l| (l.line_number, l.content.clone()))
                .collect();

            archive_works.push(ArchiveWork { file_id, path: path.clone(), line_data });
        }

        // Append chunks and collect mappings.
        struct ArchivedFile {
            file_id: i64,
            line_mappings: Vec<archive::LineMapping>,
            chunk_refs: Vec<ChunkRef>,
        }
        let mut archived_files: Vec<ArchivedFile> = Vec::new();

        let n_archive_works = archive_works.len();
        timed!(src_tag, format!("append chunks for {} files", n_archive_works), {
            for work in archive_works {
                let chunk_result = archive::chunk_lines(work.file_id, &work.path, &work.line_data);
                match archive_mgr.append_chunks(chunk_result.chunks) {
                    Ok(chunk_refs) => {
                        archived_files.push(ArchivedFile {
                            file_id: work.file_id,
                            line_mappings: chunk_result.line_mappings,
                            chunk_refs,
                        });
                    }
                    Err(e) => {
                        tracing::error!("Archive batch: failed to append chunks for {}: {e:#}", work.path);
                    }
                }
            }
        });

        // --- SQLite write segment 2: update line refs ---
        // Re-acquire source_lock for this write transaction.
        if !archived_files.is_empty() {
            let _guard = timed!(src_tag, "acquire source lock for line-ref update", {
                source_lock.lock()
                    .map_err(|_| anyhow::anyhow!("source lock poisoned for {source}"))?
            });
            timed!(src_tag, format!("update line refs for {} files", archived_files.len()), {
            let tx = conn.unchecked_transaction()?;
            for af in &archived_files {
                // chunk_refs[i] corresponds to chunk i (by chunk_number order after sort).
                let mut chunk_ref_by_number: HashMap<usize, &ChunkRef> = HashMap::new();
                for (i, cr) in af.chunk_refs.iter().enumerate() {
                    chunk_ref_by_number.insert(i, cr);
                }

                for mapping in &af.line_mappings {
                    if let Some(chunk_ref) = chunk_ref_by_number.get(&mapping.chunk_number) {
                        if let Err(e) = tx.execute(
                            "UPDATE lines SET chunk_archive = ?1, chunk_name = ?2, line_offset_in_chunk = ?3
                             WHERE file_id = ?4 AND line_number = ?5",
                            rusqlite::params![
                                chunk_ref.archive_name,
                                chunk_ref.chunk_name,
                                mapping.offset_in_chunk as i64,
                                af.file_id,
                                mapping.line_number as i64,
                            ],
                        ) {
                            tracing::error!(
                                "Archive batch: failed to update line ref for file_id={}: {e}",
                                af.file_id
                            );
                        }
                    }
                }
            }
            tx.commit()?;
            }); // timed! update line refs
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
