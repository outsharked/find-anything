# 004 - ZIP Content Storage with Async Processing

## Overview

Replace SQLite-based content storage with ZIP archives to improve scalability and decouple indexing from HTTP request handling. Implement async processing using a filesystem mailbox pattern.

## Problem Statement

**Current architecture issues:**
1. **Poor scalability** - All content stored in SQLite bloats database size
2. **Blocking requests** - Index operations block HTTP responses for seconds
3. **Memory inefficiency** - Large scans hold HTTP connections open
4. **Mass reorganization** - Moving files doubles storage (old + new paths)

**Example:**
- Index 100GB of text → SQLite database becomes 100GB+
- Client sends 10,000 files → HTTP request blocks for 30+ seconds
- User renames project folder → all files deleted and re-added → 200GB storage

## Design Decisions

### 1. ZIP Archive Content Storage

Store all content in fixed-size ZIP archives, SQLite contains only index.

**Architecture:**
```
SQLite (index only):
- files table (path, mtime, size, kind)
- lines table (file_id, line_number, chunk_archive, chunk_offset)
- lines_fts (trigram index with content='')

ZIP archives (content only):
- content_00001.zip (10MB target size)
- content_00002.zip
- content_00003.zip
- ...
```

**Content chunking:**
- Split file content into 1KB chunks
- Each chunk stored as separate ZIP entry
- Naming: `{file_path}.chunk{N}.txt`
- Example: `/home/user/code/main.rs.chunk0.txt`

**Why ZIP format:**
- ✅ Fast random access via central directory (O(1) entry lookup)
- ✅ Good compression (DEFLATE, 3-5x for text)
- ✅ Standard format (inspect with `unzip`, `7z`)
- ✅ Well-supported Rust library (`zip` crate)
- ❌ tar.gz/bz2 require sequential decompression (O(archive_size))

### 2. Async Mailbox Architecture

Decouple HTTP request handling from index processing.

**Flow:**
```
Client → POST /api/v1/index → Write to inbox → 202 Accepted
                                     ↓
                              Worker thread monitors inbox
                                     ↓
                              Process → Update DB/ZIPs → Delete request
```

**Inbox structure:**
```
/var/lib/find-anything/
  ├── sources/
  │   ├── source1.db
  │   ├── source2.db
  │   ├── content_00001.zip
  │   └── content_00002.zip
  └── inbox/
      ├── req_20260212_143022_a3f9c8e1.gz
      ├── req_20260212_143023_b7e2d4f5.gz
      └── failed/
          └── req_20260212_142000_bad123.gz
```

**Why filesystem over in-memory channel:**
- ✅ Crash-resistant (requests persist across restarts)
- ✅ Observable (can see queue: `ls inbox/ | wc -l`)
- ✅ Debuggable (inspect failed requests)
- ✅ Simple (just file I/O)
- ✅ Natural backpressure (disk space limits queue)

### 3. Client Compression

Clients send gzip-compressed JSON to reduce network transfer and enable direct-to-disk writes.

**API change:**
```
POST /api/v1/index
Content-Type: application/json
Content-Encoding: gzip

[gzipped JSON payload]
```

**Benefits:**
- Network efficiency (smaller transfers)
- Server writes compressed bytes directly to inbox
- Worker decompresses only when processing (not on HTTP thread)

### 4. Immediate ZIP Deletion

When files are deleted from index, immediately remove chunks from ZIP archives.

**Why necessary:**
Mass reorganization scenario:
```
Before: /old/path/project/ → 10,000 files → 500MB in ZIPs
After:  /new/path/project/ → 10,000 files
Without deletion: 1000MB (old + new chunks)
With deletion:    500MB (only new chunks)
```

**Implementation:**
ZIP doesn't support in-place deletion, must rewrite:
1. Create temporary ZIP
2. Copy all entries except deleted chunks
3. Atomically replace original ZIP

**Cost:** ~50-100ms per 10MB archive (acceptable for infrequent operation)

### 5. Worker Implementation

Single worker thread using `notify` crate for filesystem watching.

**Responsibilities:**
- Watch inbox directory for new `.gz` files
- Process requests sequentially
- Update database and ZIP archives
- Move failed requests to `inbox/failed/`
- Delete successfully processed requests

**Concurrency model:**
- Single worker initially (simpler, no DB contention)
- Future: one worker per source (parallel across sources)

## Implementation Plan

### Phase 1: Database Schema Changes

**Add chunk reference columns to `lines` table:**
```sql
ALTER TABLE lines ADD COLUMN chunk_archive TEXT;
ALTER TABLE lines ADD COLUMN chunk_offset INTEGER;
```

**Update FTS5 to not store content:**
```sql
DROP TABLE lines_fts;

CREATE VIRTUAL TABLE lines_fts USING fts5(
    content,
    content='',              -- Don't store content
    tokenize='trigram'
);
```

**Add archive metadata tracking:**
```sql
CREATE TABLE IF NOT EXISTS archives (
    id INTEGER PRIMARY KEY,
    archive_name TEXT UNIQUE,  -- e.g., "content_00001.zip"
    size_bytes INTEGER,
    chunk_count INTEGER,
    created_at INTEGER
);
```

### Phase 2: ZIP Archive Management

**Create `crates/server/src/archive.rs`:**
```rust
pub struct ArchiveManager {
    data_dir: PathBuf,
    current_archive: Option<String>,
    current_size: usize,
}

impl ArchiveManager {
    /// Append chunks to archives, creating new ones as needed
    pub fn append_chunks(&mut self, chunks: Vec<Chunk>) -> Result<Vec<ChunkRef>>;

    /// Remove chunks from archives, rewriting affected ZIPs
    pub fn remove_chunks(&mut self, refs: Vec<ChunkRef>) -> Result<()>;

    /// Read chunk content from archive
    pub fn read_chunk(&self, chunk_ref: &ChunkRef) -> Result<String>;

    /// Get or create current archive for appending
    fn current_archive_path(&mut self) -> Result<PathBuf>;
}

pub struct Chunk {
    pub file_path: String,
    pub chunk_number: usize,
    pub content: String,
}

pub struct ChunkRef {
    pub archive_name: String,
    pub entry_name: String,
    pub chunk_number: usize,
}
```

**Append chunks:**
```rust
fn append_chunks(&mut self, chunks: Vec<Chunk>) -> Result<Vec<ChunkRef>> {
    let mut refs = Vec::new();

    for chunk in chunks {
        let archive_path = self.current_archive_path()?;
        let entry_name = format!("{}.chunk{}.txt", chunk.file_path, chunk.chunk_number);

        // Append to ZIP
        let file = OpenOptions::new()
            .write(true)
            .create(true)
            .open(&archive_path)?;
        let mut zip = ZipWriter::new_append(file)?;

        zip.start_file(&entry_name, FileOptions::default())?;
        zip.write_all(chunk.content.as_bytes())?;
        zip.finish()?;

        self.current_size += chunk.content.len();

        // Create reference
        refs.push(ChunkRef {
            archive_name: archive_path.file_name().unwrap().to_string(),
            entry_name,
            chunk_number: chunk.chunk_number,
        });

        // Check if we need a new archive
        if self.current_size > 10 * 1024 * 1024 {  // 10MB
            self.current_archive = None;
            self.current_size = 0;
        }
    }

    Ok(refs)
}
```

**Remove chunks:**
```rust
fn remove_chunks(&mut self, refs: Vec<ChunkRef>) -> Result<()> {
    // Group by archive
    let mut by_archive: HashMap<String, HashSet<String>> = HashMap::new();
    for chunk_ref in refs {
        by_archive.entry(chunk_ref.archive_name)
            .or_default()
            .insert(chunk_ref.entry_name);
    }

    // Rewrite each affected archive
    for (archive_name, entries_to_remove) in by_archive {
        let archive_path = self.data_dir.join("sources").join(&archive_name);
        rewrite_archive(&archive_path, &entries_to_remove)?;
    }

    Ok(())
}

fn rewrite_archive(path: &Path, entries_to_remove: &HashSet<String>) -> Result<()> {
    let temp_path = path.with_extension("zip.tmp");

    // Read existing archive
    let file = File::open(path)?;
    let mut old_zip = ZipArchive::new(file)?;

    // Create new archive
    let temp_file = File::create(&temp_path)?;
    let mut new_zip = ZipWriter::new(temp_file);

    // Copy entries except removed ones
    for i in 0..old_zip.len() {
        let mut entry = old_zip.by_index(i)?;
        let name = entry.name().to_string();

        if !entries_to_remove.contains(&name) {
            let options = FileOptions::default()
                .compression_method(entry.compression());
            new_zip.start_file(&name, options)?;
            std::io::copy(&mut entry, &mut new_zip)?;
        }
    }

    new_zip.finish()?;
    drop(old_zip);

    // Atomic replace
    std::fs::rename(&temp_path, path)?;

    Ok(())
}
```

### Phase 3: Async Inbox Processing

**Update `crates/server/src/routes.rs`:**
```rust
async fn index_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<StatusCode> {
    // Check Content-Encoding
    let is_compressed = headers
        .get(header::CONTENT_ENCODING)
        .and_then(|v| v.to_str().ok())
        .map(|v| v == "gzip")
        .unwrap_or(false);

    // Generate request ID
    let request_id = format!(
        "req_{}_{}",
        chrono::Utc::now().format("%Y%m%d_%H%M%S"),
        uuid::Uuid::new_v4().simple()
    );

    // Write to inbox
    let inbox_path = state.data_dir
        .join("inbox")
        .join(format!("{}.gz", request_id));

    if is_compressed {
        // Already compressed, write directly
        tokio::fs::write(&inbox_path, body).await?;
    } else {
        // Compress before writing
        use flate2::write::GzEncoder;
        let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
        encoder.write_all(&body)?;
        let compressed = encoder.finish()?;
        tokio::fs::write(&inbox_path, compressed).await?;
    }

    Ok(StatusCode::ACCEPTED)  // 202 Accepted
}
```

**Create `crates/server/src/worker.rs`:**
```rust
use notify::{Watcher, RecursiveMode, Event};
use tokio::sync::mpsc;
use flate2::read::GzDecoder;

pub async fn start_inbox_worker(data_dir: PathBuf) -> Result<()> {
    let inbox_dir = data_dir.join("inbox");
    tokio::fs::create_dir_all(&inbox_dir).await?;

    let failed_dir = inbox_dir.join("failed");
    tokio::fs::create_dir_all(&failed_dir).await?;

    // Watch for new files
    let (tx, mut rx) = mpsc::channel(100);
    let mut watcher = notify::recommended_watcher(move |res: Result<Event, _>| {
        if let Ok(event) = res {
            if event.kind.is_create() {
                for path in event.paths {
                    let _ = tx.blocking_send(path);
                }
            }
        }
    })?;

    watcher.watch(&inbox_dir, RecursiveMode::NonRecursive)?;

    // Process existing files on startup
    let mut entries = tokio::fs::read_dir(&inbox_dir).await?;
    while let Some(entry) = entries.next_entry().await? {
        let path = entry.path();
        if path.extension() == Some(OsStr::new("gz")) {
            process_request(&data_dir, &path).await?;
        }
    }

    // Process new files as they arrive
    while let Some(path) = rx.recv().await {
        if path.extension() == Some(OsStr::new("gz")) {
            if let Err(e) = process_request(&data_dir, &path).await {
                handle_failure(&path, &failed_dir, e).await?;
            } else {
                tokio::fs::remove_file(&path).await?;
            }
        }
    }

    Ok(())
}

async fn process_request(data_dir: &Path, request_path: &Path) -> Result<()> {
    // Decompress request
    let compressed = tokio::fs::read(request_path).await?;
    let mut decoder = GzDecoder::new(&compressed[..]);
    let mut json = String::new();
    decoder.read_to_string(&mut json)?;

    let request: IndexRequest = serde_json::from_str(&json)?;

    // Open source database
    let db_path = data_dir.join("sources").join(format!("{}.db", request.source));
    let conn = db::open(&db_path)?;

    // Initialize archive manager
    let mut archive_mgr = ArchiveManager::new(data_dir.to_path_buf());

    // Chunk files and append to archives
    let mut all_chunks = Vec::new();
    for file in &request.files {
        let chunks = chunk_file(file)?;
        all_chunks.extend(chunks);
    }

    let chunk_refs = archive_mgr.append_chunks(all_chunks)?;

    // Update database with chunk references
    db::upsert_files_with_chunks(&conn, &request.files, &chunk_refs)?;

    // Update FTS5 index
    rebuild_fts5(&conn)?;

    // Update metadata
    db::update_last_scan(&conn, chrono::Utc::now().timestamp())?;
    if let Some(base_url) = &request.base_url {
        db::update_base_url(&conn, Some(base_url))?;
    }

    Ok(())
}

async fn handle_failure(path: &Path, failed_dir: &Path, error: anyhow::Error) -> Result<()> {
    tracing::error!("Failed to process {}: {}", path.display(), error);

    let failed_path = failed_dir.join(path.file_name().unwrap());
    tokio::fs::rename(path, failed_path).await?;

    Ok(())
}

fn chunk_file(file: &IndexFile) -> Result<Vec<Chunk>> {
    let mut chunks = Vec::new();
    let mut current_chunk = String::new();
    let mut chunk_number = 0;

    for line in &file.lines {
        let line_text = format!("{}\n", line.content);

        if current_chunk.len() + line_text.len() > 1024 {
            // Flush current chunk
            if !current_chunk.is_empty() {
                chunks.push(Chunk {
                    file_path: file.path.clone(),
                    chunk_number,
                    content: current_chunk.clone(),
                });
                chunk_number += 1;
                current_chunk.clear();
            }
        }

        current_chunk.push_str(&line_text);
    }

    // Flush final chunk
    if !current_chunk.is_empty() {
        chunks.push(Chunk {
            file_path: file.path.clone(),
            chunk_number,
            content: current_chunk,
        });
    }

    Ok(chunks)
}

fn rebuild_fts5(conn: &Connection) -> Result<()> {
    // With content='', we need to manually populate FTS5
    // This is done during upsert_files_with_chunks
    Ok(())
}
```

### Phase 4: Update Client for Compression

**Update `crates/client/src/scan.rs`:**
```rust
use flate2::write::GzEncoder;
use flate2::Compression;

async fn send_batch(
    client: &ApiClient,
    source: &str,
    base_url: Option<&str>,
    batch: Vec<IndexFile>,
) -> Result<()> {
    let request = IndexRequest {
        source: source.to_string(),
        base_url: base_url.map(|s| s.to_string()),
        files: batch,
    };

    // Serialize to JSON
    let json = serde_json::to_vec(&request)?;

    // Compress
    let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
    encoder.write_all(&json)?;
    let compressed = encoder.finish()?;

    // Send with Content-Encoding header
    let response = client
        .post("/api/v1/index")
        .header("Content-Type", "application/json")
        .header("Content-Encoding", "gzip")
        .body(compressed)
        .send()
        .await?;

    if response.status() != 202 {
        anyhow::bail!("Index request failed: {}", response.status());
    }

    Ok(())
}
```

### Phase 5: Update Context Retrieval

**Update `crates/server/src/db.rs`:**
```rust
pub fn get_context(
    conn: &Connection,
    archive_mgr: &ArchiveManager,
    file_path: &str,
    archive_path: Option<&str>,
    center: usize,
    window: usize,
) -> Result<Vec<ContextLine>> {
    let kind = get_file_kind(conn, file_path)?;

    match kind.as_str() {
        "image" | "audio" => get_metadata_context(conn, archive_mgr, file_path),
        "pdf" => get_pdf_context(conn, archive_mgr, file_path, archive_path, center, window),
        _ => get_line_context(conn, archive_mgr, file_path, archive_path, center, window),
    }
}

fn get_line_context(
    conn: &Connection,
    archive_mgr: &ArchiveManager,
    file_path: &str,
    archive_path: Option<&str>,
    center: usize,
    window: usize,
) -> Result<Vec<ContextLine>> {
    let lo = center.saturating_sub(window) as i64;
    let hi = (center + window) as i64;

    let mut stmt = conn.prepare(
        "SELECT l.line_number, l.chunk_archive, l.chunk_offset
         FROM lines l
         JOIN files f ON f.id = l.file_id
         WHERE f.path = ?1
           AND ((?2 IS NULL AND l.archive_path IS NULL)
                OR l.archive_path = ?2)
           AND l.line_number BETWEEN ?3 AND ?4
         ORDER BY l.line_number",
    )?;

    let rows = stmt.query_map(
        params![file_path, archive_path, lo, hi],
        |row| {
            Ok((
                row.get::<_, i64>(0)? as usize,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)? as usize,
            ))
        },
    )?;

    let mut lines = Vec::new();
    for row in rows {
        let (line_number, chunk_archive, chunk_offset) = row?;

        // Read chunk from archive
        let chunk_ref = ChunkRef {
            archive_name: chunk_archive,
            entry_name: format!("{}.chunk{}.txt", file_path, chunk_offset),
            chunk_number: chunk_offset,
        };

        let content = archive_mgr.read_chunk(&chunk_ref)?;

        // Extract the specific line from chunk
        let line_content = content.lines().nth(line_number - chunk_offset).unwrap_or("");

        lines.push(ContextLine {
            line_number,
            content: line_content.to_string(),
        });
    }

    Ok(lines)
}
```

## Migration Strategy

### For Existing Deployments

**Option 1: Fresh index (recommended for initial release)**
1. Backup existing database
2. Update server binary
3. Delete old database
4. Rescan from scratch

**Option 2: Migrate in place (for production later)**
1. Create migration script
2. Read content from existing `lines` table
3. Chunk and write to ZIP archives
4. Update `lines` table with chunk references
5. Drop content column
6. Rebuild FTS5 with `content=''`

## Performance Expectations

### Storage Efficiency
- **Before:** 100GB text → ~100GB SQLite database
- **After:** 100GB text → ~30GB ZIP archives + ~5GB SQLite index

### Query Performance
- FTS5 search: No change (trigram index same size)
- Context retrieval: +5-10ms (ZIP decompression overhead)
- Overall: Negligible difference for end users

### Indexing Performance
- HTTP response time: <10ms (just write to inbox)
- Actual processing: Async in background
- No more blocking client connections

## Error Handling

### Transient Errors
- File lock contention → retry with backoff
- Network timeout → retry

### Permanent Errors
- Invalid JSON → move to failed/
- Unknown source → move to failed/
- Corrupt ZIP → log error, continue

### Monitoring
Add metrics endpoint:
```
GET /api/v1/metrics
{
  "inbox_queue_depth": 3,
  "failed_requests": 1,
  "total_archives": 42,
  "total_chunks": 123456
}
```

## Files Changed

**New files:**
- `crates/server/src/archive.rs` - ZIP archive management
- `crates/server/src/worker.rs` - Async inbox processing
- `crates/server/src/schema_v2.sql` - Updated schema with chunk references

**Modified files:**
- `crates/server/src/db.rs` - Update context retrieval to read from ZIPs
- `crates/server/src/routes.rs` - Change index endpoint to write to inbox
- `crates/server/src/main.rs` - Start worker thread
- `crates/client/src/scan.rs` - Add gzip compression
- `crates/common/Cargo.toml` - Add dependencies: `zip`, `notify`, `flate2`

## Testing Strategy

### Unit Tests
1. Archive manager: append, remove, read chunks
2. Chunking logic: various file sizes, edge cases
3. ZIP rewriting: deletion, empty archives

### Integration Tests
1. End-to-end: client sends → inbox → worker → query results
2. Mass deletion: verify storage doesn't double
3. Crash recovery: verify inbox persists across restarts

### Performance Tests
1. Index 10GB dataset, measure time and storage
2. Query with context retrieval, measure latency
3. Delete 50% of files, measure rewrite time

## Future Enhancements

### Compaction (Long-term)
- Rewrite sparse archives to reclaim space
- Run offline during maintenance
- Trigger if >50% chunks deleted

### Multi-worker (Medium-term)
- One worker per source
- Parallel processing across sources
- Requires connection pooling

### Compression Tuning (Optional)
- Configurable compression level
- Trade-off: speed vs size
- Default: level 6 (balanced)
