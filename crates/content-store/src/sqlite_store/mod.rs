mod db;

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use anyhow::{Context, Result};

use crate::key::ContentKey;
use crate::store::{CompactResult, ContentStore};

/// Content-addressable SQLite blob store.
///
/// Stores all chunks in a single `blobs.db` SQLite database under `data_dir`.
/// Each chunk is a plain-text slice of the original file content stored
/// directly as a TEXT column — no ZIP archives, no separate metadata DB.
///
/// The key read advantage over `ZipContentStore`: `get_lines` resolves to a
/// PK-indexed range query returning only the 1–2 rows needed, rather than
/// loading an entire 10 MB ZIP archive.
///
/// # Thread safety
///
/// A single `Mutex<Connection>` serialises all reads and writes.  For
/// read-heavy workloads a pool of read-only connections can be added later
/// without changing the `ContentStore` interface.
pub struct SqliteContentStore {
    data_dir: PathBuf,
    conn: Mutex<rusqlite::Connection>,
    /// Target chunk size in bytes.  Configurable per instance to allow
    /// side-by-side benchmarking of 1 KB / 4 KB / 10 KB configurations.
    chunk_size: usize,
}

impl SqliteContentStore {
    /// Open (or create) the SQLite content store at `data_dir/blobs.db`.
    ///
    /// `chunk_size_kb` controls how large each chunk can grow before a new
    /// one is started.  Defaults to 1 KB (matching `ZipContentStore`) if
    /// `None` is passed.
    pub fn open(data_dir: &Path, chunk_size_kb: Option<u32>) -> Result<Self> {
        let conn = db::open_write(data_dir).context("opening blobs.db")?;
        Ok(Self {
            data_dir: data_dir.to_path_buf(),
            conn: Mutex::new(conn),
            chunk_size: chunk_size_kb.unwrap_or(1) as usize * 1024,
        })
    }
}

// ── Chunking ─────────────────────────────────────────────────────────────────

struct Chunk {
    chunk_num: usize,
    start_line: usize,
    end_line: usize,
    data: String,
}

/// Split a blob (lines joined by `'\n'`) into chunks no larger than
/// `chunk_size` bytes.  Each chunk records the 0-based line range it covers.
fn chunk_blob(blob: &str, chunk_size: usize) -> Vec<Chunk> {
    let mut chunks: Vec<Chunk> = Vec::new();
    let mut current = String::new();
    let mut chunk_num = 0usize;
    let mut chunk_start: Option<usize> = None;
    let mut chunk_last: usize = 0;

    for (pos, line) in blob.split('\n').enumerate() {
        let line_text = format!("{line}\n");

        if current.len() + line_text.len() > chunk_size && !current.is_empty() {
            chunks.push(Chunk {
                chunk_num,
                start_line: chunk_start.unwrap_or(0),
                end_line: chunk_last,
                data: std::mem::take(&mut current),
            });
            chunk_num += 1;
            chunk_start = None;
        }

        if chunk_start.is_none() {
            chunk_start = Some(pos);
        }
        chunk_last = pos;
        current.push_str(&line_text);
    }

    if !current.is_empty() {
        chunks.push(Chunk {
            chunk_num,
            start_line: chunk_start.unwrap_or(0),
            end_line: chunk_last,
            data: current,
        });
    }

    chunks
}

// ── ContentStore impl ─────────────────────────────────────────────────────────

impl ContentStore for SqliteContentStore {
    fn put(&self, key: &ContentKey, blob: &str) -> Result<bool> {
        let key_str = key.as_str();
        let conn = self.conn.lock().map_err(|_| anyhow::anyhow!("db lock poisoned"))?;

        // Fast-path: already stored.
        if db::blob_exists(&conn, key_str)? {
            return Ok(false);
        }

        let chunks = chunk_blob(blob, self.chunk_size);
        let tx = conn.unchecked_transaction()?;

        if chunks.is_empty() {
            // Empty file: insert a sentinel so `contains()` returns true.
            db::insert_chunk(&tx, key_str, 0, 0, 0, "")?;
        } else {
            for chunk in &chunks {
                db::insert_chunk(
                    &tx,
                    key_str,
                    chunk.chunk_num,
                    chunk.start_line,
                    chunk.end_line,
                    &chunk.data,
                )?;
            }
        }

        tx.commit()?;
        Ok(true)
    }

    fn delete(&self, key: &ContentKey) -> Result<()> {
        let conn = self.conn.lock().map_err(|_| anyhow::anyhow!("db lock poisoned"))?;
        db::delete_blob(&conn, key.as_str())
    }

    fn get_lines(
        &self,
        key: &ContentKey,
        lo: usize,
        hi: usize,
    ) -> Result<Option<Vec<(usize, String)>>> {
        let conn = self.conn.lock().map_err(|_| anyhow::anyhow!("db lock poisoned"))?;

        if !db::blob_exists(&conn, key.as_str())? {
            return Ok(None);
        }

        let rows = db::query_chunks_for_range(&conn, key.as_str(), lo, hi)?;
        let mut result: Vec<(usize, String)> = Vec::new();

        for row in rows {
            let base = row.start_line as usize;
            for (offset, line) in row.data.split('\n').enumerate() {
                let pos = base + offset;
                if pos >= lo && pos <= hi && !line.is_empty() {
                    result.push((pos, line.to_owned()));
                }
            }
        }

        Ok(Some(result))
    }

    fn contains(&self, key: &ContentKey) -> Result<bool> {
        let conn = self.conn.lock().map_err(|_| anyhow::anyhow!("db lock poisoned"))?;
        db::blob_exists(&conn, key.as_str())
    }

    fn compact(&self, live_keys: &HashSet<ContentKey>, dry_run: bool) -> Result<CompactResult> {
        let conn = self.conn.lock().map_err(|_| anyhow::anyhow!("db lock poisoned"))?;

        let before_rows = db::row_count(&conn)?;
        let before_bytes = db::db_size_bytes(&self.data_dir);

        let deleted_rows = if dry_run {
            0
        } else {
            let live: Vec<&str> = live_keys.iter().map(|k| k.as_str()).collect();
            db::delete_orphan_blobs(&conn, &live)?
        };

        if !dry_run {
            conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE)")?;
        }

        let after_rows = db::row_count(&conn)?;
        let after_bytes = db::db_size_bytes(&self.data_dir);

        Ok(CompactResult {
            archives_scanned: 1,
            archives_rewritten: if deleted_rows > 0 { 1 } else { 0 },
            archives_deleted: 0,
            chunks_removed: (before_rows.saturating_sub(after_rows)) as usize,
            bytes_freed: before_bytes.saturating_sub(after_bytes),
        })
    }

    fn archive_stats(&self) -> Option<(u64, u64)> {
        let conn = self.conn.lock().ok()?;
        let rows = db::row_count(&conn).ok()?;
        let bytes = db::db_size_bytes(&self.data_dir);
        // Report 1 "archive" (the single blobs.db) with its size.
        Some((rows, bytes))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    /// Verify that chunk_size_kb=0 forces every line into its own chunk,
    /// and that get_lines still reconstructs a sub-range correctly.
    /// This tests SQLite-specific configuration (chunk size parameter).
    #[test]
    fn tiny_chunk_size_sub_range() {
        let dir = TempDir::new().unwrap();
        let store = SqliteContentStore::open(dir.path(), Some(0)).unwrap();
        let k = ContentKey::new("eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee");
        let lines: Vec<String> = (0..20).map(|i| format!("line {i:03}")).collect();
        store.put(&k, &lines.join("\n")).unwrap();

        let result = store.get_lines(&k, 5, 10).unwrap().unwrap();
        let positions: Vec<usize> = result.iter().map(|(p, _)| *p).collect();
        for p in 5..=10 {
            assert!(positions.contains(&p), "missing position {p}");
        }
    }
}
