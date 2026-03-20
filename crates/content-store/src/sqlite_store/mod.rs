mod db;

use std::collections::HashSet;
use std::io::{Read as _, Write as _};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use anyhow::{Context, Result};
use flate2::Compression;
use flate2::read::GzDecoder;
use flate2::write::GzEncoder;

use crate::key::ContentKey;
use crate::store::{CompactResult, ContentStore};

// ── Read connection pool ──────────────────────────────────────────────────────

/// How many idle connections to retain between calls.
const MAX_IDLE_READ_CONNS: usize = 16;

/// Default hard cap on total open read connections (idle + in-use).
pub const DEFAULT_MAX_READ_CONNECTIONS: u32 = 100;

struct PoolState {
    idle: Vec<rusqlite::Connection>,
    /// Total open connections: idle + currently borrowed.
    open_count: usize,
}

/// Elastic pool of read-only SQLite connections with a hard cap.
///
/// - If an idle connection is available it is returned immediately.
/// - If the pool is empty but below `max_connections`, a new read-only
///   connection is opened on the spot.
/// - If `max_connections` is reached, the caller blocks until one is returned.
/// - On release, connections are kept idle up to `MAX_IDLE_READ_CONNS`; extras
///   are closed (decrementing `open_count` and waking any blocked callers).
struct ReadPool {
    state: Mutex<PoolState>,
    available: std::sync::Condvar,
    data_dir: PathBuf,
    max_connections: usize,
}

impl ReadPool {
    fn new(data_dir: PathBuf, max_connections: usize) -> Self {
        Self {
            state: Mutex::new(PoolState { idle: Vec::new(), open_count: 0 }),
            available: std::sync::Condvar::new(),
            data_dir,
            max_connections,
        }
    }

    fn acquire(&self) -> Result<PooledConn<'_>> {
        let conn = {
            let mut state = self.state.lock().unwrap();
            loop {
                // 1. Reuse an idle connection if available.
                if let Some(c) = state.idle.pop() {
                    break c;
                }
                // 2. Open a new connection if below the cap.
                if state.open_count < self.max_connections {
                    state.open_count += 1;
                    let conn = db::open_read_only(&self.data_dir)
                        .context("opening read connection")?;
                    break conn;
                }
                // 3. At the cap — wait for a connection to be returned.
                state = self.available.wait(state).unwrap();
            }
        };
        Ok(PooledConn { conn: Some(conn), pool: self })
    }

    fn release(&self, conn: rusqlite::Connection) {
        let mut state = self.state.lock().unwrap();
        if state.idle.len() < MAX_IDLE_READ_CONNS {
            state.idle.push(conn);
            // Wake a waiter: an idle connection is now available.
            self.available.notify_one();
        } else {
            // Drop the connection; decrement open_count so a waiter can open a new one.
            drop(conn);
            state.open_count -= 1;
            self.available.notify_one();
        }
    }
}

/// RAII guard: returns the connection to the pool on drop.
struct PooledConn<'a> {
    conn: Option<rusqlite::Connection>,
    pool: &'a ReadPool,
}

impl std::ops::Deref for PooledConn<'_> {
    type Target = rusqlite::Connection;
    fn deref(&self) -> &rusqlite::Connection {
        self.conn.as_ref().unwrap()
    }
}

impl Drop for PooledConn<'_> {
    fn drop(&mut self) {
        if let Some(conn) = self.conn.take() {
            self.pool.release(conn);
        }
    }
}

// ── Store ─────────────────────────────────────────────────────────────────────

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
/// - **Writes** (`put`, `delete`, `compact`) use a single `Mutex<Connection>`
///   (SQLite allows only one writer at a time).
/// - **Reads** (`get_lines`, `contains`) use an elastic pool of read-only
///   connections (WAL mode allows unlimited concurrent readers).  Up to
///   `MAX_IDLE_READ_CONNS` connections are kept open between calls; if all
///   are in use a new connection is opened rather than blocking.
pub struct SqliteContentStore {
    data_dir: PathBuf,
    write_conn: Mutex<rusqlite::Connection>,
    read_pool: ReadPool,
    /// Target chunk size in bytes.  Configurable per instance to allow
    /// side-by-side benchmarking of 1 KB / 4 KB / 12 KB configurations.
    chunk_size: usize,
    /// Whether to gzip-compress chunk data before storing.
    compress: bool,
}

impl SqliteContentStore {
    /// Open (or create) the SQLite content store at `data_dir/blobs.db`.
    ///
    /// `chunk_size_kb` controls how large each chunk can grow before a new
    /// one is started.  Defaults to 1 KB (matching `ZipContentStore`) if
    /// `None` is passed.
    pub fn open(
        data_dir: &Path,
        chunk_size_kb: Option<u32>,
        max_read_connections: Option<u32>,
        compress: Option<bool>,
    ) -> Result<Self> {
        let write_conn = db::open_write(data_dir).context("opening blobs.db")?;
        let max_conns = max_read_connections.unwrap_or(DEFAULT_MAX_READ_CONNECTIONS) as usize;
        Ok(Self {
            data_dir: data_dir.to_path_buf(),
            write_conn: Mutex::new(write_conn),
            read_pool: ReadPool::new(data_dir.to_path_buf(), max_conns),
            chunk_size: chunk_size_kb.unwrap_or(1) as usize * 1024,
            compress: compress.unwrap_or(false),
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

    // Use `lines()` so an empty blob produces zero iterations and no trailing
    // phantom line.  Lines are stored joined by '\n' with no trailing newline,
    // so `split('\n')` in `get_lines` reconstructs them exactly.
    for (pos, line) in blob.lines().enumerate() {
        // Adding a separator '\n' before every non-first line.
        let add_size = if current.is_empty() { line.len() } else { 1 + line.len() };

        if current.len() + add_size > chunk_size && !current.is_empty() {
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
        if !current.is_empty() {
            current.push('\n');
        }
        current.push_str(line);
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

// ── Compression helpers ───────────────────────────────────────────────────────

const GZIP_MAGIC: [u8; 2] = [0x1f, 0x8b];

fn gzip_compress(data: &str) -> Result<Vec<u8>> {
    let mut enc = GzEncoder::new(Vec::new(), Compression::fast());
    enc.write_all(data.as_bytes())?;
    Ok(enc.finish()?)
}

/// Decompress bytes if they look like gzip; otherwise interpret as UTF-8.
fn decode_chunk(bytes: &[u8]) -> Result<String> {
    if bytes.starts_with(&GZIP_MAGIC) {
        let mut out = String::new();
        GzDecoder::new(bytes).read_to_string(&mut out)?;
        Ok(out)
    } else {
        Ok(std::str::from_utf8(bytes)?.to_owned())
    }
}

// ── ContentStore impl ─────────────────────────────────────────────────────────

impl ContentStore for SqliteContentStore {
    fn put(&self, key: &ContentKey, blob: &str) -> Result<bool> {
        let key_str = key.as_str();
        let conn = self.write_conn.lock().map_err(|_| anyhow::anyhow!("write lock poisoned"))?;

        if db::blob_exists(&conn, key_str)? {
            return Ok(false);
        }

        let chunks = chunk_blob(blob, self.chunk_size);
        let tx = conn.unchecked_transaction()?;

        if chunks.is_empty() {
            db::insert_chunk(&tx, key_str, 0, 0, 0, b"")?;
        } else {
            for chunk in &chunks {
                let bytes: Vec<u8> = if self.compress {
                    gzip_compress(&chunk.data)?
                } else {
                    chunk.data.as_bytes().to_vec()
                };
                db::insert_chunk(&tx, key_str, chunk.chunk_num, chunk.start_line, chunk.end_line, &bytes)?;
            }
        }

        tx.commit()?;
        Ok(true)
    }

    fn delete(&self, key: &ContentKey) -> Result<()> {
        let conn = self.write_conn.lock().map_err(|_| anyhow::anyhow!("write lock poisoned"))?;
        db::delete_blob(&conn, key.as_str())
    }

    fn get_lines(&self, key: &ContentKey, lo: usize, hi: usize) -> Result<Option<Vec<(usize, String)>>> {
        let conn = self.read_pool.acquire()?;

        if !db::blob_exists(&conn, key.as_str())? {
            return Ok(None);
        }

        let rows = db::query_chunks_for_range(&conn, key.as_str(), lo, hi)?;
        let mut result: Vec<(usize, String)> = Vec::new();

        for row in rows {
            let base = row.start_line as usize;
            let text = decode_chunk(&row.data)?;
            if text.is_empty() {
                continue; // sentinel row for empty blobs
            }
            for (offset, line) in text.lines().enumerate() {
                let pos = base + offset;
                if pos >= lo && pos <= hi {
                    result.push((pos, line.to_owned()));
                }
            }
        }

        Ok(Some(result))
    }

    fn contains(&self, key: &ContentKey) -> Result<bool> {
        let conn = self.read_pool.acquire()?;
        db::blob_exists(&conn, key.as_str())
    }

    fn compact(&self, live_keys: &HashSet<ContentKey>, dry_run: bool) -> Result<CompactResult> {
        let conn = self.write_conn.lock().map_err(|_| anyhow::anyhow!("write lock poisoned"))?;
        let live: Vec<&str> = live_keys.iter().map(|k| k.as_str()).collect();

        // Count orphaned rows, distinct keys, and bytes — used by both paths.
        let (orphaned_rows, orphaned_keys, orphaned_bytes) = db::orphaned_stats(&conn, &live)?;

        if dry_run {
            return Ok(CompactResult {
                units_scanned: 1,
                units_rewritten: 0,
                units_deleted: orphaned_keys,
                chunks_removed: orphaned_rows,
                bytes_freed: orphaned_bytes,
            });
        }

        let deleted_rows = db::delete_orphan_blobs(&conn, &live)?;

        // VACUUM reclaims freed pages on disk. Run in a separate statement batch
        // so it executes outside of any implicit transaction.
        if deleted_rows > 0 {
            conn.execute_batch("VACUUM")?;
        }
        conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE)")?;

        Ok(CompactResult {
            units_scanned: 1,
            units_rewritten: 0,
            units_deleted: orphaned_keys,
            chunks_removed: deleted_rows,
            // Report the logical data bytes removed rather than the physical file
            // size delta, which is unreliable for small datasets (SQLite page
            // granularity means a tiny deletion may not shrink the file at all).
            bytes_freed: orphaned_bytes,
        })
    }

    fn storage_stats(&self) -> Option<(u64, u64)> {
        // 1 unit = the single blobs.db file.
        let bytes = db::db_size_bytes(&self.data_dir);
        Some((1, bytes))
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
        let store = SqliteContentStore::open(dir.path(), Some(0), None, None).unwrap();
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
