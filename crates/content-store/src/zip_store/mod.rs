pub mod archive;
pub mod chunk;
pub mod shared;
pub(super) mod db;

use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};
use zip::ZipArchive;

use crate::key::ContentKey;
use crate::store::{CompactResult, ContentStore};

use archive::{ArchiveManager, ChunkRef};
use chunk::chunk_blob;
use shared::SharedArchiveState;

/// Content-addressable ZIP-based blob store.
///
/// - Owns `data_dir/content.db` — all chunk metadata.
/// - Owns `data_dir/sources/content/` ZIP archives (shared directory with
///   the old archive path; new blobs use key-prefix naming so names never
///   collide with old block_id-based names once SCHEMA_VERSION bumps).
///
/// # Thread safety
///
/// Writes use two locks:
/// - `writer` — a `Mutex<ArchiveManager>` that is held only during ZIP I/O.
///   Keeping it alive across `put` calls lets successive writes pack into the
///   same archive rather than each allocating a new one.
/// - `db_conn` — a `Mutex<Connection>` held only during the SQLite commit.
///
/// The two locks are never held simultaneously, so there is no deadlock risk.
/// Reads open a fresh read-only connection per call (WAL mode).
pub struct ZipContentStore {
    data_dir: PathBuf,
    /// Persistent writer: retains the "current archive" across `put` calls so
    /// small blobs pack together instead of each getting their own ZIP.
    writer: Mutex<ArchiveManager>,
    /// Exclusive write connection to content.db.
    db_conn: Mutex<rusqlite::Connection>,
    /// Shared archive atomics and per-archive rewrite locks.
    shared: Arc<SharedArchiveState>,
}

impl ZipContentStore {
    /// Open (or create) the content store for `data_dir`.
    pub fn open(data_dir: &Path) -> Result<Self> {
        let conn = db::open_write(data_dir).context("opening content.db")?;
        let shared = SharedArchiveState::new(data_dir.to_path_buf())
            .context("initialising archive state")?;
        let writer = ArchiveManager::new(Arc::clone(&shared));
        Ok(Self {
            data_dir: data_dir.to_path_buf(),
            writer: Mutex::new(writer),
            db_conn: Mutex::new(conn),
            shared,
        })
    }
}

impl ContentStore for ZipContentStore {
    fn put(&self, key: &ContentKey, blob: &str) -> Result<bool> {
        // Fast-path: already stored.
        if self.contains(key)? {
            return Ok(false);
        }

        let key_str = key.as_str();
        let key_prefix: String = key_str.chars().take(16).collect();

        // Chunk the blob.
        let blob_chunks = chunk_blob(blob);
        if blob_chunks.is_empty() {
            // Nothing to store (empty file).
            let conn = self.db_conn.lock().map_err(|_| anyhow::anyhow!("db lock poisoned"))?;
            let tx = conn.unchecked_transaction()?;
            db::insert_blob(&tx, key_str)?;
            tx.commit()?;
            return Ok(true);
        }

        // Write chunks to ZIP archives (outside DB lock).
        // Use the persistent writer so successive puts pack into the same archive.
        let mut chunk_refs: Vec<(ChunkRef, usize, usize)> = Vec::new(); // (ref, start_pos, end_pos)
        {
            let mut mgr = self.writer.lock().map_err(|_| anyhow::anyhow!("writer lock poisoned"))?;
            for blob_chunk in &blob_chunks {
                let chunk_name = format!("{}.{}", key_prefix, blob_chunk.chunk_num);
                let cref = mgr.append_raw(&chunk_name, blob_chunk.content.as_bytes())?;
                chunk_refs.push((cref, blob_chunk.start_pos, blob_chunk.end_pos));
            }
        } // writer lock released before acquiring db_conn

        // Write metadata to content.db inside the lock.
        let conn = self.db_conn.lock().map_err(|_| anyhow::anyhow!("db lock poisoned"))?;
        let tx = conn.unchecked_transaction()?;

        // Race-guard: re-check inside the transaction.
        if db::blob_exists(&conn, key_str)? {
            tx.commit()?; // Nothing to do; orphaned ZIP chunks cleaned at compact.
            return Ok(false);
        }

        db::insert_blob(&tx, key_str)?;

        for (cref, start_pos, end_pos) in &chunk_refs {
            let archive_id = db::upsert_archive(&tx, &cref.archive_name)?;
            let chunk_num = cref
                .chunk_name
                .rsplit('.')
                .next()
                .and_then(|s| s.parse::<usize>().ok())
                .unwrap_or(0);
            db::insert_chunk(&tx, key_str, chunk_num, archive_id, *start_pos, *end_pos)?;
        }

        tx.commit()?;
        Ok(true)
    }

    fn delete(&self, key: &ContentKey) -> Result<()> {
        let conn = self.db_conn.lock().map_err(|_| anyhow::anyhow!("db lock poisoned"))?;
        let tx = conn.unchecked_transaction()?;
        db::delete_blob(&tx, key.as_str())?;
        tx.commit()?;
        // Orphaned ZIP chunks are cleaned up at the next compaction pass.
        Ok(())
    }

    fn get_lines(
        &self,
        key: &ContentKey,
        lo: usize,
        hi: usize,
    ) -> Result<Option<Vec<(usize, String)>>> {
        let read_conn = db::open_read(&self.data_dir)?;

        if !db::blob_exists(&read_conn, key.as_str())? {
            return Ok(None);
        }

        let chunks = db::query_chunks_for_range(&read_conn, key.as_str(), lo, hi)?;
        if chunks.is_empty() {
            return Ok(Some(vec![]));
        }

        let key_prefix: String = key.as_str().chars().take(16).collect();

        // Use a per-call read-only ArchiveManager (no write coordination needed).
        let mgr = ArchiveManager::new_for_reading(self.data_dir.clone());
        let mut result = Vec::new();

        for meta in chunks {
            let chunk_member = format!("{}.{}", key_prefix, meta.chunk_num);
            let cref = ChunkRef {
                archive_name: meta.archive_name,
                chunk_name: chunk_member,
            };
            let content = mgr.read_chunk(&cref)?;

            // Each chunk stores lines as "{content}\n" repeated.
            // Position `start_pos + i` corresponds to the i-th "\n"-delimited segment.
            for (i, line) in content.split('\n').enumerate() {
                if line.is_empty() {
                    continue; // trailing newline artefact
                }
                let pos = meta.start_pos as usize + i;
                if pos >= lo && pos <= hi {
                    result.push((pos, line.to_string()));
                }
            }
        }

        Ok(Some(result))
    }

    fn contains(&self, key: &ContentKey) -> Result<bool> {
        let read_conn = db::open_read(&self.data_dir)?;
        db::blob_exists(&read_conn, key.as_str())
    }

    fn compact(
        &self,
        live_keys: &HashSet<ContentKey>,
        dry_run: bool,
    ) -> Result<CompactResult> {
        let live_key_strs: Vec<&str> = live_keys.iter().map(|k| k.as_str()).collect();

        // ── 1. Identify orphaned chunk names per archive ──────────────────────
        let read_conn = db::open_read(&self.data_dir)?;
        let orphan_refs = db::collect_orphan_chunks(&read_conn, &live_key_strs)?;

        // Group orphaned chunk member names by archive.
        let mut orphans_by_archive: HashMap<String, HashSet<String>> = HashMap::new();
        for oref in orphan_refs {
            orphans_by_archive
                .entry(oref.archive_name)
                .or_default()
                .insert(oref.chunk_member);
        }
        drop(read_conn);

        // ── 2. Scan all archives and compact ─────────────────────────────────
        let content_dir = self.data_dir.join("sources").join("content");

        let mut archives_scanned   = 0usize;
        let mut archives_rewritten = 0usize;
        let mut archives_deleted   = 0usize;
        let mut chunks_removed     = 0usize;
        let mut bytes_freed        = 0u64;

        for subdir_entry in std::fs::read_dir(&content_dir).into_iter().flatten().flatten() {
            let subdir = subdir_entry.path();
            if !subdir.is_dir() {
                continue;
            }
            for file_entry in std::fs::read_dir(&subdir).into_iter().flatten().flatten() {
                let path = file_entry.path();
                if path.extension().and_then(|e| e.to_str()) != Some("zip") {
                    continue;
                }
                let archive_name = match path.file_name().and_then(|n| n.to_str()) {
                    Some(n) => n.to_string(),
                    None => continue,
                };
                archives_scanned += 1;

                // Determine orphaned entries within this archive.
                let orphaned = orphans_by_archive
                    .get(&archive_name)
                    .cloned()
                    .unwrap_or_default();

                // Read archive entry count and orphaned sizes.
                let (total_entries, orphaned_size) = {
                    let file = match File::open(&path) {
                        Ok(f) => f,
                        Err(_) => continue,
                    };
                    let mut zip = match ZipArchive::new(file) {
                        Ok(z) => z,
                        Err(_) => continue,
                    };
                    let total = zip.len();
                    let mut orph_size = 0u64;
                    for i in 0..zip.len() {
                        if let Ok(entry) = zip.by_index_raw(i) {
                            if orphaned.contains(entry.name()) {
                                orph_size += entry.compressed_size();
                            }
                        }
                    }
                    (total, orph_size)
                };

                if orphaned.is_empty() && total_entries > 0 {
                    continue; // Nothing to do.
                }

                if total_entries == 0 {
                    // Pre-existing empty archive — delete it.
                    archives_deleted += 1;
                    if !dry_run {
                        let lock = self.shared.rewrite_lock_for(&path);
                        let _guard = lock.lock().unwrap();
                        if std::fs::remove_file(&path).is_ok() {
                            tracing::info!("compaction: deleted empty archive {}", archive_name);
                        } else {
                            archives_deleted -= 1;
                        }
                    }
                    continue;
                }

                chunks_removed += orphaned.len();
                bytes_freed    += orphaned_size;

                if orphaned.len() == total_entries {
                    // All entries orphaned — delete the whole file.
                    archives_deleted += 1;
                    if !dry_run {
                        let lock = self.shared.rewrite_lock_for(&path);
                        let _guard = lock.lock().unwrap();
                        if std::fs::remove_file(&path).is_ok() {
                            tracing::info!(
                                "compaction: deleted {} — all {} chunks orphaned",
                                archive_name,
                                orphaned.len()
                            );
                        } else {
                            archives_deleted -= 1;
                            chunks_removed   -= orphaned.len();
                            bytes_freed      -= orphaned_size;
                        }
                    }
                } else {
                    // Partial — rewrite keeping only referenced entries.
                    archives_rewritten += 1;
                    if !dry_run {
                        let lock = self.shared.rewrite_lock_for(&path);
                        let _guard = lock.lock().unwrap();
                        let tmp = path.with_extension("zip.tmp");
                        if let Err(e) = rewrite_without(&path, &orphaned, &tmp) {
                            tracing::error!(
                                "compaction: failed to rewrite {}: {e:#}",
                                archive_name
                            );
                            archives_rewritten -= 1;
                            chunks_removed     -= orphaned.len();
                            bytes_freed        -= orphaned_size;
                        } else {
                            tracing::info!(
                                "compaction: rewrote {} — removed {} orphaned chunks",
                                archive_name,
                                orphaned.len()
                            );
                        }
                    }
                }
            }
        }

        // ── 3. Delete orphaned blob rows from content.db ──────────────────────
        if !dry_run {
            let write_conn = self
                .db_conn
                .lock()
                .map_err(|_| anyhow::anyhow!("db lock poisoned"))?;
            let tx = write_conn.unchecked_transaction()?;
            db::delete_orphan_blobs(&tx, &live_key_strs)?;
            tx.commit()?;
        }

        Ok(CompactResult {
            archives_scanned,
            archives_rewritten,
            archives_deleted,
            chunks_removed,
            bytes_freed,
        })
    }

    fn archive_stats(&self) -> Option<(u64, u64)> {
        let count = self.shared.total_archives();
        let bytes = self.shared.archive_size_bytes();
        Some((count, bytes))
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────────

fn rewrite_without(
    archive_path: &Path,
    to_remove: &HashSet<String>,
    temp_path: &Path,
) -> Result<()> {
    use zip::{CompressionMethod, ZipWriter};
    use zip::write::SimpleFileOptions;

    let src_file = File::open(archive_path)?;
    let mut old_zip = ZipArchive::new(src_file)?;
    let tmp_file = File::create(temp_path)?;
    let mut new_zip = ZipWriter::new(tmp_file);

    let base_opts = SimpleFileOptions::default()
        .compression_method(CompressionMethod::Deflated)
        .compression_level(Some(6))
        .into_full_options();

    for i in 0..old_zip.len() {
        let mut entry = old_zip.by_index(i)?;
        if !to_remove.contains(entry.name()) {
            let comment = entry.comment().to_string();
            let opts = base_opts.clone().with_file_comment(comment.as_str());
            new_zip.start_file(entry.name(), opts)?;
            std::io::copy(&mut entry, &mut new_zip)?;
        }
    }
    new_zip.finish()?;
    drop(old_zip);
    std::fs::rename(temp_path, archive_path)?;
    Ok(())
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn open_store(dir: &Path) -> ZipContentStore {
        std::fs::create_dir_all(dir.join("sources").join("content")).unwrap();
        ZipContentStore::open(dir).unwrap()
    }

    fn key(s: &str) -> ContentKey {
        ContentKey::new(s)
    }

    #[test]
    fn put_then_contains() {
        let tmp = tempfile::tempdir().unwrap();
        let store = open_store(tmp.path());
        let k = key("aabbccddeeff00112233445566778899");
        assert!(!store.contains(&k).unwrap());
        store.put(&k, "line0\nline1\nline2").unwrap();
        assert!(store.contains(&k).unwrap());
    }

    #[test]
    fn put_idempotent() {
        let tmp = tempfile::tempdir().unwrap();
        let store = open_store(tmp.path());
        let k = key("aabbccddeeff00112233445566778899");
        assert!(store.put(&k, "hello").unwrap());
        assert!(!store.put(&k, "hello").unwrap()); // second put is no-op
    }

    #[test]
    fn get_lines_roundtrip() {
        let tmp = tempfile::tempdir().unwrap();
        let store = open_store(tmp.path());
        let k = key("aabbccddeeff00112233445566778899");
        let blob = "alpha\nbeta\ngamma\ndelta";
        store.put(&k, blob).unwrap();

        let lines = store.get_lines(&k, 0, 3).unwrap().unwrap();
        assert_eq!(lines.len(), 4);
        assert_eq!(lines[0], (0, "alpha".to_string()));
        assert_eq!(lines[1], (1, "beta".to_string()));
        assert_eq!(lines[2], (2, "gamma".to_string()));
        assert_eq!(lines[3], (3, "delta".to_string()));
    }

    #[test]
    fn get_lines_sub_range() {
        let tmp = tempfile::tempdir().unwrap();
        let store = open_store(tmp.path());
        let k = key("aabbccddeeff00112233445566778899");
        store.put(&k, "a\nb\nc\nd\ne").unwrap();

        let lines = store.get_lines(&k, 1, 3).unwrap().unwrap();
        assert_eq!(lines.len(), 3);
        assert_eq!(lines[0], (1, "b".to_string()));
        assert_eq!(lines[2], (3, "d".to_string()));
    }

    #[test]
    fn get_lines_key_not_found() {
        let tmp = tempfile::tempdir().unwrap();
        let store = open_store(tmp.path());
        let k = key("aabbccddeeff00112233445566778899");
        let result = store.get_lines(&k, 0, 5).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn delete_removes_blob() {
        let tmp = tempfile::tempdir().unwrap();
        let store = open_store(tmp.path());
        let k = key("aabbccddeeff00112233445566778899");
        store.put(&k, "some content").unwrap();
        assert!(store.contains(&k).unwrap());
        store.delete(&k).unwrap();
        assert!(!store.contains(&k).unwrap());
        assert!(store.get_lines(&k, 0, 0).unwrap().is_none());
    }

    #[test]
    fn compact_removes_orphaned_blobs() {
        let tmp = tempfile::tempdir().unwrap();
        let store = open_store(tmp.path());

        let k_live   = key("aabbccddeeff00112233445566778899");
        let k_orphan = key("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb");

        store.put(&k_live,   "live content").unwrap();
        store.put(&k_orphan, "orphaned content").unwrap();

        assert!(store.contains(&k_live).unwrap());
        assert!(store.contains(&k_orphan).unwrap());

        let live_keys: HashSet<ContentKey> = std::iter::once(k_live.clone()).collect();
        let result = store.compact(&live_keys, false).unwrap();

        assert!(result.chunks_removed > 0 || result.archives_deleted > 0 || result.archives_rewritten > 0);
        assert!(store.contains(&k_live).unwrap());
        assert!(!store.contains(&k_orphan).unwrap());
    }

    #[test]
    fn compact_dry_run_does_not_remove() {
        let tmp = tempfile::tempdir().unwrap();
        let store = open_store(tmp.path());
        let k = key("aabbccddeeff00112233445566778899");
        store.put(&k, "content").unwrap();

        let live_keys = HashSet::new(); // no live keys → k is orphan
        store.compact(&live_keys, true /* dry_run */).unwrap();

        // Blob should still exist because dry_run=true.
        assert!(store.contains(&k).unwrap());
    }

    #[test]
    fn many_small_blobs_pack_into_one_archive() {
        let tmp = tempfile::tempdir().unwrap();
        let store = open_store(tmp.path());

        // Store 20 distinct small blobs (each well under 1 KB).
        let hashes: Vec<String> = (0..20)
            .map(|i| format!("{:032x}", i))
            .collect();
        for (i, hash) in hashes.iter().enumerate() {
            store.put(&key(hash), &format!("content of file {i}")).unwrap();
        }

        // Count how many ZIP archives were created.
        let content_dir = tmp.path().join("sources").join("content");
        let archive_count: usize = std::fs::read_dir(&content_dir)
            .into_iter()
            .flatten()
            .flatten()
            .filter(|e| e.path().is_dir())
            .flat_map(|d| std::fs::read_dir(d.path()).into_iter().flatten().flatten())
            .filter(|e| e.path().extension().map_or(false, |x| x == "zip"))
            .count();

        // All 20 blobs should have packed into a single archive (they're tiny).
        assert_eq!(archive_count, 1, "20 small blobs should share one archive, got {archive_count}");
    }
}
