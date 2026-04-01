/// Archive phase (phase 2) of the inbox worker.
///
/// Reads `.gz` files from `inbox/to-archive/`, parses them, and stores file
/// content in the `ContentStore`.  No SQLite writes are made to source DBs
/// here — all metadata is owned by the content store's internal database.
///
/// # Design
///
/// For each gz file:
/// 1. Parse the `BulkRequest`.
/// 2. For each `IndexFile`, check whether the `content_hash` in the gz matches
///    what the source DB currently records.  If it doesn't match (stale gz),
///    skip the file — a newer gz will archive the correct content.
/// 3. Call `content_store.put_overwrite(key, blob)` to store (or refresh) the blob.
/// 4. Delete the gz file.
use std::ffi::OsStr;
use std::io::BufReader;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result};
use rusqlite::OptionalExtension;

use find_content_store::{ContentKey, ContentStore};

use crate::db;
use super::WorkerConfig;

// ── Public entry point ────────────────────────────────────────────────────────

/// Scan `to_archive_dir` for `.gz` files, process up to `cfg.archive_batch_size`
/// of them through the archive phase, and return the number processed.
pub(super) fn run_archive_batch(
    data_dir: &Path,
    to_archive_dir: &Path,
    cfg: WorkerConfig,
    content_store: &Arc<dyn ContentStore>,
) -> Result<usize> {
    let mut gz_files: Vec<(std::time::SystemTime, PathBuf)> = Vec::new();
    for entry in std::fs::read_dir(to_archive_dir)?.flatten() {
        let path = entry.path();
        if path.extension() == Some(OsStr::new("gz")) {
            let mtime = entry
                .metadata()
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

    let batch: Vec<PathBuf> = gz_files
        .into_iter()
        .take(cfg.archive_batch_size)
        .map(|(_, p)| p)
        .collect();
    let n_processed = batch.len();

    for gz_path in &batch {
        if let Err(e) = archive_gz(data_dir, gz_path, content_store) {
            tracing::error!(
                "Archive batch: failed to process {}: {e:#}",
                gz_path.display()
            );
            // Leave the file in to-archive/ for the next batch tick.
            continue;
        }
        if let Err(e) = std::fs::remove_file(gz_path) {
            tracing::error!(
                "Archive batch: failed to delete {}: {e}",
                gz_path.display()
            );
        }
    }

    Ok(n_processed)
}

// ── Internal ──────────────────────────────────────────────────────────────────

/// Process one gz file: for each file whose content_hash matches the DB, store
/// (or overwrite) the blob in the content store.
fn archive_gz(
    data_dir: &Path,
    gz_path: &Path,
    content_store: &Arc<dyn ContentStore>,
) -> Result<()> {
    let request = parse_gz_request(gz_path)?;
    let source = request.source;
    let tag = format!("[archive:{source}]");

    let db_path = data_dir.join("sources").join(format!("{source}.db"));
    if !db_path.exists() {
        // Source was deleted since this gz was queued — nothing to do.
        return Ok(());
    }
    let conn = db::open(&db_path)
        .with_context(|| format!("opening DB for source {source}"))?;

    let mut stored = 0usize;
    let mut skipped = 0usize;

    for file in request.files {
        let Some(file_content_hash) = &file.file_hash else {
            continue;
        };

        // Read the current file_hash from the DB for this path.
        let db_hash: Option<String> = conn
            .query_row(
                "SELECT file_hash FROM files WHERE path = ?1",
                rusqlite::params![file.path],
                |r| r.get(0),
            )
            .optional()
            .unwrap_or(None)
            .flatten();

        let Some(db_hash) = db_hash else {
            continue;
        };

        // Skip stale content: if this gz carries an older version of the file,
        // the newer gz will archive the correct content when it is processed.
        if file_content_hash != &db_hash {
            tracing::debug!(
                "{tag} skipping {} (stale: gz={}, DB={})",
                file.path,
                file_content_hash,
                db_hash
            );
            skipped += 1;
            continue;
        }

        let key = ContentKey::new(db_hash.as_str());

        // Build blob: sort lines by line_number, join with '\n'.
        let mut sorted_lines = file.lines.clone();
        sorted_lines.sort_by_key(|l| l.line_number);
        if sorted_lines.is_empty() {
            continue;
        }
        let blob: String = sorted_lines
            .into_iter()
            .map(|l| l.content.trim_end().to_string())
            .collect::<Vec<_>>()
            .join("\n");

        // Always overwrite: extraction output may differ from what was previously
        // stored (e.g. SCANNER_VERSION bump adds new metadata tags), even if the
        // raw file bytes — and therefore file_hash — are unchanged.
        match content_store.put_overwrite(&key, &blob) {
            Ok(_) => stored += 1,
            Err(e) => tracing::error!("{tag} failed to store content for {}: {e:#}", file.path),
        }
    }

    tracing::info!(
        "{tag} archived {stored} files ({skipped} stale/skipped)"
    );
    Ok(())
}

pub(super) fn parse_gz_request(gz_path: &Path) -> Result<find_common::api::BulkRequest> {
    let file = std::fs::File::open(gz_path)
        .with_context(|| format!("opening {}", gz_path.display()))?;
    let decoder = flate2::read::GzDecoder::new(BufReader::new(file));
    serde_json::from_reader(decoder).context("parsing bulk request JSON")
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::path::Path;
    use std::sync::Arc;

    use find_common::api::{BulkRequest, FileKind, IndexFile, IndexLine};
    use find_common::config::NormalizationSettings;
    use find_content_store::{ContentKey, ContentStore, SqliteContentStore};

    use crate::db::encode_fts_rowid;

    fn setup_data_dir(_data_dir: &Path) {
        // No extra setup needed for SqliteContentStore.
    }

    fn open_content_store(data_dir: &Path) -> Arc<dyn ContentStore> {
        Arc::new(SqliteContentStore::open(data_dir, None, None, None).unwrap())
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
                file_hash: Some("testhash".to_string()),
                is_new: true,
                force: false,
            }],
            delete_paths: vec![],
            rename_paths: vec![],
            scan_timestamp: None,
            indexing_failures: vec![],
        }
    }

    /// Seed the source DB with a file that has `content_hash = 'testhash'`.
    fn seed_db(data_dir: &Path, source: &str, path: &str) -> (rusqlite::Connection, i64) {
        let db_path = data_dir.join("sources").join(format!("{source}.db"));
        std::fs::create_dir_all(db_path.parent().unwrap()).unwrap();
        let conn = crate::db::open(&db_path).unwrap();

        conn.execute(
            "INSERT INTO files (path, mtime, size, kind, indexed_at, extract_ms, file_hash, line_count)
             VALUES (?1, 1000, 100, 'text', 0, NULL, 'testhash', 2)",
            rusqlite::params![path],
        )
        .unwrap();
        let file_id = conn.last_insert_rowid();

        conn.execute(
            "INSERT INTO lines_fts(rowid, content) VALUES (?1, ?2)",
            rusqlite::params![encode_fts_rowid(file_id, 0), path],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO lines_fts(rowid, content) VALUES (?1, ?2)",
            rusqlite::params![encode_fts_rowid(file_id, 1), "hello world"],
        )
        .unwrap();

        (conn, file_id)
    }

    #[test]
    fn content_stored_and_gz_deleted() {
        let data_tmp = tempfile::tempdir().unwrap();
        let to_archive_tmp = tempfile::tempdir().unwrap();
        let data_dir = data_tmp.path();
        let to_archive_dir = to_archive_tmp.path();
        setup_data_dir(data_dir);

        seed_db(data_dir, "test_source", "docs/readme.txt");
        write_bulk_gz(
            &to_archive_dir.join("batch_001.gz"),
            &make_bulk_request("test_source", "docs/readme.txt", "hello world"),
        );

        let cs = open_content_store(data_dir);
        let processed = run_archive_batch(data_dir, to_archive_dir, make_worker_config(), &cs).unwrap();
        assert_eq!(processed, 1);

        let gz_count = std::fs::read_dir(to_archive_dir)
            .unwrap()
            .flatten()
            .filter(|e| e.path().extension().map_or(false, |x| x == "gz"))
            .count();
        assert_eq!(gz_count, 0, "gz should be removed after processing");

        let key = ContentKey::new("testhash");
        assert!(cs.contains(&key).unwrap(), "content should be in store");
    }

    #[test]
    fn gz_deleted_when_no_source_db() {
        let data_tmp = tempfile::tempdir().unwrap();
        let to_archive_tmp = tempfile::tempdir().unwrap();
        let data_dir = data_tmp.path();
        let to_archive_dir = to_archive_tmp.path();
        setup_data_dir(data_dir);

        write_bulk_gz(
            &to_archive_dir.join("ghost_001.gz"),
            &make_bulk_request("ghost_source", "nonexistent.txt", "x"),
        );

        let cs = open_content_store(data_dir);
        let processed =
            run_archive_batch(data_dir, to_archive_dir, make_worker_config(), &cs).unwrap();
        assert_eq!(processed, 1);

        let gz_count = std::fs::read_dir(to_archive_dir)
            .unwrap()
            .flatten()
            .filter(|e| e.path().extension().map_or(false, |x| x == "gz"))
            .count();
        assert_eq!(gz_count, 0, "gz should be removed even when source has no DB");
    }

    #[test]
    fn already_stored_content_is_skipped() {
        let data_tmp = tempfile::tempdir().unwrap();
        let to_archive_tmp = tempfile::tempdir().unwrap();
        let data_dir = data_tmp.path();
        let to_archive_dir = to_archive_tmp.path();
        setup_data_dir(data_dir);

        seed_db(data_dir, "test_source", "docs/readme.txt");
        let cs = open_content_store(data_dir);

        write_bulk_gz(
            &to_archive_dir.join("first_001.gz"),
            &make_bulk_request("test_source", "docs/readme.txt", "hello world"),
        );
        run_archive_batch(data_dir, to_archive_dir, make_worker_config(), &cs).unwrap();

        // Pre-store: content already there, put should be a no-op.
        write_bulk_gz(
            &to_archive_dir.join("second_001.gz"),
            &make_bulk_request("test_source", "docs/readme.txt", "hello world"),
        );
        run_archive_batch(data_dir, to_archive_dir, make_worker_config(), &cs).unwrap();

        // Content store still intact.
        let key = ContentKey::new("testhash");
        assert!(cs.contains(&key).unwrap());
    }

    #[test]
    fn stale_content_hash_is_skipped() {
        let data_tmp = tempfile::tempdir().unwrap();
        let to_archive_tmp = tempfile::tempdir().unwrap();
        let data_dir = data_tmp.path();
        let to_archive_dir = to_archive_tmp.path();
        setup_data_dir(data_dir);

        // DB has "newhash" (a newer request was already indexed).
        let db_path = data_dir.join("sources").join("test_source.db");
        std::fs::create_dir_all(db_path.parent().unwrap()).unwrap();
        let conn = crate::db::open(&db_path).unwrap();
        conn.execute(
            "INSERT INTO files (path, mtime, size, kind, indexed_at, extract_ms, file_hash, line_count)
             VALUES ('docs/readme.txt', 2000, 100, 'text', 0, NULL, 'newhash', 1)",
            [],
        )
        .unwrap();

        // gz carries "oldhash" — stale.
        let stale_req = BulkRequest {
            source: "test_source".to_string(),
            files: vec![IndexFile {
                path: "docs/readme.txt".to_string(),
                mtime: 1000,
                size: Some(10),
                kind: FileKind::Text,
                scanner_version: 1,
                lines: vec![IndexLine {
                    archive_path: None,
                    line_number: 1,
                    content: "old content".to_string(),
                }],
                extract_ms: None,
                file_hash: Some("oldhash".to_string()),
                is_new: false,
                force: false,
            }],
            delete_paths: vec![],
            rename_paths: vec![],
            scan_timestamp: None,
            indexing_failures: vec![],
        };
        write_bulk_gz(&to_archive_dir.join("stale_001.gz"), &stale_req);

        let cs = open_content_store(data_dir);
        run_archive_batch(data_dir, to_archive_dir, make_worker_config(), &cs).unwrap();

        // gz deleted.
        let gz_count = std::fs::read_dir(to_archive_dir)
            .unwrap()
            .flatten()
            .filter(|e| e.path().extension().map_or(false, |x| x == "gz"))
            .count();
        assert_eq!(gz_count, 0, "stale gz should be deleted");

        // Stale content was NOT stored.
        let stale_key = ContentKey::new("oldhash");
        assert!(!cs.contains(&stale_key).unwrap());
    }
}
