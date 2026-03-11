use anyhow::{Context, Result};
use std::collections::{HashMap, HashSet};
use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use zip::{CompressionMethod, ZipArchive, ZipWriter};
use zip::write::{SimpleFileOptions, FullFileOptions};

const TARGET_ARCHIVE_SIZE: usize = 10 * 1024 * 1024; // 10MB
const CHUNK_SIZE: usize = 1024; // 1KB chunks

/// State shared across all `ArchiveManager` instances (i.e. all workers).
///
/// - `next_archive_num` is an atomic counter; each worker atomically claims a
///   unique number and owns that archive exclusively for appending. No locks
///   are needed on the append path.
/// - `rewrite_locks` is a per-archive mutex registry used only during rewrite
///   operations (chunk removal for re-indexing / deletion). Two workers
///   rewriting the same old sealed archive acquire its lock and serialise.
pub struct SharedArchiveState {
    data_dir: PathBuf,
    /// Next archive number to allocate. Monotonically increasing; each
    /// `fetch_add` gives the caller exclusive ownership of that archive.
    next_archive_num: AtomicU32,
    /// Running count of archive ZIP files on disk.
    total_archives: AtomicU64,
    /// Running sum of archive ZIP on-disk sizes (compressed bytes).
    archive_size_bytes: AtomicU64,
    /// Per-archive rewrite lock, keyed by absolute archive path.
    rewrite_locks: Mutex<HashMap<PathBuf, Arc<Mutex<()>>>>,
    /// Per-source write serialisation lock.  Both the indexing thread and the
    /// archive thread acquire this before any SQLite write to the source DB.
    /// Released before (and re-acquired after) ZIP I/O so the two threads
    /// cannot block each other on disk work.
    source_locks: Mutex<HashMap<String, Arc<Mutex<()>>>>,
}

impl SharedArchiveState {
    /// Initialise shared state for `data_dir`, scanning the existing content
    /// directory to seed the counter above the highest existing archive number
    /// and to populate the running archive count and size totals.
    pub fn new(data_dir: PathBuf) -> Result<Arc<Self>> {
        let (max_num, total_archives, archive_size_bytes) = Self::scan_archives(&data_dir);
        Ok(Arc::new(Self {
            data_dir,
            next_archive_num: AtomicU32::new(max_num.saturating_add(1)),
            total_archives: AtomicU64::new(total_archives),
            archive_size_bytes: AtomicU64::new(archive_size_bytes),
            rewrite_locks: Mutex::new(HashMap::new()),
            source_locks: Mutex::new(HashMap::new()),
        }))
    }

    /// Running count of archive ZIP files (updated incrementally).
    pub fn total_archives(&self) -> u64 {
        self.total_archives.load(Ordering::Relaxed)
    }

    /// Running sum of archive ZIP on-disk sizes in bytes (updated incrementally).
    pub fn archive_size_bytes(&self) -> u64 {
        self.archive_size_bytes.load(Ordering::Relaxed)
    }

    /// Scan the content directory tree; return (max_archive_num, count, total_bytes).
    fn scan_archives(data_dir: &Path) -> (u32, u64, u64) {
        let content_dir = data_dir.join("sources").join("content");
        let mut max_num = 0u32;
        let mut count = 0u64;
        let mut size_bytes = 0u64;
        if let Ok(entries) = std::fs::read_dir(&content_dir) {
            for entry in entries.flatten() {
                if entry.path().is_dir() {
                    if let Ok(subdir) = std::fs::read_dir(entry.path()) {
                        for file_entry in subdir.flatten() {
                            if let Some(name) = file_entry.file_name().to_str() {
                                if let Some(num) = parse_archive_number(name) {
                                    max_num = max_num.max(num as u32);
                                    count += 1;
                                    size_bytes += file_entry.metadata().map(|m| m.len()).unwrap_or(0);
                                }
                            }
                        }
                    }
                }
            }
        }
        (max_num, count, size_bytes)
    }

    /// Atomically claim the next archive number. The caller has exclusive
    /// write ownership of the archive at that number.
    pub fn allocate_archive_num(&self) -> u32 {
        self.next_archive_num.fetch_add(1, Ordering::Relaxed)
    }

    /// Return (or lazily create) the per-archive rewrite lock for `path`.
    pub fn rewrite_lock_for(&self, path: &Path) -> Arc<Mutex<()>> {
        let mut locks = self.rewrite_locks.lock().unwrap();
        locks.entry(path.to_path_buf())
            .or_insert_with(|| Arc::new(Mutex::new(())))
            .clone()
    }

    /// Return (or lazily create) the per-source write serialisation lock.
    ///
    /// Both the indexing thread and the archive thread must hold this mutex
    /// for the duration of any SQLite write transaction to the named source DB.
    /// The lock must be released before ZIP I/O so neither thread blocks the
    /// other on disk work.
    pub fn source_lock(&self, source: &str) -> Arc<Mutex<()>> {
        let mut locks = self.source_locks.lock().unwrap();
        locks.entry(source.to_string())
            .or_insert_with(|| Arc::new(Mutex::new(())))
            .clone()
    }

    /// Compute the on-disk path for a given archive number.
    ///
    /// Archives are organised in thousands-based subfolders:
    /// - `content/0000/` → `content_00000.zip` … `content_00999.zip`
    /// - `content/0001/` → `content_01000.zip` … `content_01999.zip`
    pub fn archive_path_for_number(&self, archive_num: u32) -> PathBuf {
        let n = archive_num as usize;
        let filename = format!("content_{n:05}.zip");
        let subfolder = format!("{:04}", n / 1000);
        self.data_dir
            .join("sources")
            .join("content")
            .join(subfolder)
            .join(filename)
    }

    fn sources_dir(&self) -> PathBuf {
        self.data_dir.join("sources")
    }
}

/// Per-worker archive manager.
///
/// Each worker has its own `ArchiveManager` that holds an exclusively-owned
/// in-progress archive number. Multiple workers never write to the same archive
/// on the append path. Rewrite operations (chunk removal) serialise via the
/// per-archive lock in `SharedArchiveState`.
pub struct ArchiveManager {
    state: Arc<SharedArchiveState>,
    /// Archive number this worker is currently appending to. `None` until the
    /// first chunk is written, or after a rotation.
    current_archive_num: Option<u32>,
}

/// A chunk of file content to be stored
#[derive(Debug, Clone)]
pub struct Chunk {
    pub file_id: i64,
    pub file_path: String,  // kept for ZIP entry comment
    pub chunk_number: usize,
    pub content: String,
}

/// Reference to a chunk stored in an archive
#[derive(Debug, Clone)]
pub struct ChunkRef {
    pub archive_name: String,
    pub chunk_name: String,
}

impl ArchiveManager {
    pub fn new(state: Arc<SharedArchiveState>) -> Self {
        Self { state, current_archive_num: None }
    }

    /// Create an `ArchiveManager` for **read-only** use (e.g. route handlers
    /// that read chunk content). The shared state is not coordinated with any
    /// worker pool — no appending or rewriting should be done through this
    /// instance.
    pub fn new_for_reading(data_dir: PathBuf) -> Self {
        // Counters start at 0; irrelevant since this instance never writes.
        let state = Arc::new(SharedArchiveState {
            data_dir,
            next_archive_num: AtomicU32::new(0),
            total_archives: AtomicU64::new(0),
            archive_size_bytes: AtomicU64::new(0),
            rewrite_locks: Mutex::new(HashMap::new()),
            source_locks: Mutex::new(HashMap::new()),
        });
        Self { state, current_archive_num: None }
    }

    /// Append chunks to archives, creating new ones as needed.
    pub fn append_chunks(&mut self, chunks: Vec<Chunk>) -> Result<Vec<ChunkRef>> {
        let mut refs = Vec::new();

        for chunk in chunks {
            let archive_path = self.current_archive_path()?;
            let chunk_name = format!("{}.{}", chunk.file_id, chunk.chunk_number);

            self.append_to_zip_with_comment(&archive_path, &chunk_name, chunk.content.as_bytes(), &chunk.file_path)?;

            refs.push(ChunkRef {
                archive_name: archive_path
                    .file_name()
                    .unwrap()
                    .to_string_lossy()
                    .to_string(),
                chunk_name,
            });

            // Rotate when the on-disk size reaches the target.
            let on_disk = std::fs::metadata(&archive_path)
                .map(|m| m.len() as usize)
                .unwrap_or(0);
            if on_disk >= TARGET_ARCHIVE_SIZE {
                self.current_archive_num = None;
            }
        }

        Ok(refs)
    }

    /// Remove chunks from archives by rewriting affected ZIPs.
    ///
    /// Acquires the per-archive rewrite lock from `SharedArchiveState` before
    /// each rewrite so that two workers rewriting the same archive serialise.
    /// Remove chunks from their archives and return the total compressed bytes freed.
    pub fn remove_chunks(&self, refs: Vec<ChunkRef>) -> Result<u64> {
        // Group by archive
        let mut by_archive: HashMap<String, HashSet<String>> = HashMap::new();
        for chunk_ref in refs {
            by_archive
                .entry(chunk_ref.archive_name)
                .or_default()
                .insert(chunk_ref.chunk_name);
        }

        let mut bytes_freed: u64 = 0;

        for (archive_name, chunks_to_remove) in by_archive {
            let archive_path = if let Some(num) = parse_archive_number(&archive_name) {
                self.state.archive_path_for_number(num as u32)
            } else {
                self.state.sources_dir().join(&archive_name)
            };

            if archive_path.exists() {
                let valid = File::open(&archive_path)
                    .ok()
                    .and_then(|f| ZipArchive::new(f).ok())
                    .is_some();
                if valid {
                    // Serialise concurrent rewrites to the same archive.
                    let lock = self.state.rewrite_lock_for(&archive_path);
                    let _guard = lock.lock().unwrap();
                    bytes_freed += self.rewrite_archive(&archive_path, &chunks_to_remove)?;
                } else {
                    tracing::warn!(
                        "skipping corrupt archive during chunk removal: {}",
                        archive_path.display()
                    );
                }
            }
        }

        Ok(bytes_freed)
    }

    /// Read chunk content from archive
    pub fn read_chunk(&self, chunk_ref: &ChunkRef) -> Result<String> {
        let archive_path = if let Some(num) = parse_archive_number(&chunk_ref.archive_name) {
            self.state.archive_path_for_number(num as u32)
        } else {
            self.state.sources_dir().join(&chunk_ref.archive_name)
        };

        let file = File::open(&archive_path)
            .with_context(|| format!("opening archive {}", archive_path.display()))?;

        let mut zip = ZipArchive::new(file)?;

        let mut entry = zip
            .by_name(&chunk_ref.chunk_name)
            .with_context(|| format!("finding chunk {} in archive", chunk_ref.chunk_name))?;

        let mut content = String::new();
        entry.read_to_string(&mut content)?;

        Ok(content)
    }

    /// Return the path of this worker's current in-progress archive, allocating
    /// a new one (with exclusive ownership) if needed.
    fn current_archive_path(&mut self) -> Result<PathBuf> {
        if let Some(num) = self.current_archive_num {
            let path = self.state.archive_path_for_number(num);
            let on_disk = std::fs::metadata(&path).map(|m| m.len() as usize).unwrap_or(0);
            if on_disk < TARGET_ARCHIVE_SIZE {
                return Ok(path);
            }
            // Full; fall through to allocate the next one.
            self.current_archive_num = None;
        }

        // Claim an exclusive archive number and create the empty ZIP.
        let new_num = self.state.allocate_archive_num();
        let new_path = self.state.archive_path_for_number(new_num);

        if let Some(parent) = new_path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let file = File::create(&new_path)?;
        ZipWriter::new(file).finish()?;

        self.state.total_archives.fetch_add(1, Ordering::Relaxed);
        let initial_size = std::fs::metadata(&new_path).map(|m| m.len()).unwrap_or(0);
        self.state.archive_size_bytes.fetch_add(initial_size, Ordering::Relaxed);

        self.current_archive_num = Some(new_num);
        Ok(new_path)
    }

    /// Append a single entry to a ZIP file with an optional per-entry comment.
    ///
    /// If `entry_name` already exists (left over from a previous partial run),
    /// the existing entry is removed first so the write is idempotent.
    fn append_to_zip_with_comment(&self, archive_path: &Path, entry_name: &str, content: &[u8], comment: &str) -> Result<()> {
        {
            let file = File::open(archive_path)?;
            let zip = ZipArchive::new(file)?;
            if zip.index_for_name(entry_name).is_some() {
                let to_remove: HashSet<String> = std::iter::once(entry_name.to_string()).collect();
                self.rewrite_archive(archive_path, &to_remove)?;
                tracing::warn!("removed stale chunk {entry_name} before re-appending");
            }
        }

        // Measure size after any stale-chunk removal above, before the new write.
        let size_before = std::fs::metadata(archive_path).map(|m| m.len()).unwrap_or(0);

        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .open(archive_path)?;

        let mut zip = ZipWriter::new_append(file)?;

        let options: FullFileOptions<'_> = SimpleFileOptions::default()
            .compression_method(CompressionMethod::Deflated)
            .compression_level(Some(6))
            .into_full_options()
            .with_file_comment(comment);

        zip.start_file(entry_name, options)?;
        zip.write_all(content)?;
        zip.finish()?;

        let size_after = std::fs::metadata(archive_path).map(|m| m.len()).unwrap_or(0);
        if size_after > size_before {
            self.state.archive_size_bytes.fetch_add(size_after - size_before, Ordering::Relaxed);
        }

        Ok(())
    }

    /// Rewrite an archive omitting the named chunks; returns compressed bytes freed.
    fn rewrite_archive(&self, archive_path: &Path, chunks_to_remove: &HashSet<String>) -> Result<u64> {
        let size_before = std::fs::metadata(archive_path).map(|m| m.len()).unwrap_or(0);
        let temp_path = archive_path.with_extension("zip.tmp");

        let result = self.rewrite_archive_inner(archive_path, chunks_to_remove, &temp_path);
        if result.is_err() {
            // Clean up the partial temp file so we don't leave corrupt `.zip.tmp` files on disk.
            let _ = std::fs::remove_file(&temp_path);
        }
        let bytes_freed = result?;

        // Update running size total.
        let size_after = std::fs::metadata(archive_path).map(|m| m.len()).unwrap_or(0);
        match size_after.cmp(&size_before) {
            std::cmp::Ordering::Greater => {
                self.state.archive_size_bytes.fetch_add(size_after - size_before, Ordering::Relaxed);
            }
            std::cmp::Ordering::Less => {
                let _ = self.state.archive_size_bytes.fetch_update(
                    Ordering::Relaxed, Ordering::Relaxed,
                    |v| Some(v.saturating_sub(size_before - size_after)),
                );
            }
            std::cmp::Ordering::Equal => {}
        }

        Ok(bytes_freed)
    }

    fn rewrite_archive_inner(
        &self,
        archive_path: &Path,
        chunks_to_remove: &HashSet<String>,
        temp_path: &Path,
    ) -> Result<u64> {
        let file = File::open(archive_path)?;
        let mut old_zip = ZipArchive::new(file)?;

        let temp_file = File::create(temp_path)?;
        let mut new_zip = ZipWriter::new(temp_file);

        let base_options = SimpleFileOptions::default()
            .compression_method(CompressionMethod::Deflated)
            .compression_level(Some(6))
            .into_full_options();

        let mut bytes_freed: u64 = 0;
        for i in 0..old_zip.len() {
            let mut entry = old_zip.by_index(i)?;
            let name = entry.name().to_string();
            let comment = entry.comment().to_string();

            if chunks_to_remove.contains(&name) {
                bytes_freed += entry.compressed_size();
            } else {
                let entry_options = base_options.clone().with_file_comment(comment.as_str());
                new_zip.start_file(&name, entry_options)?;
                std::io::copy(&mut entry, &mut new_zip)?;
            }
        }

        new_zip.finish()?;
        drop(old_zip);

        std::fs::rename(temp_path, archive_path)?;
        Ok(bytes_freed)
    }
}

/// Extract archive number from filename (e.g., "content_00123.zip" → 123)
fn parse_archive_number(filename: &str) -> Option<usize> {
    filename
        .strip_prefix("content_")
        .and_then(|s| s.strip_suffix(".zip"))
        .and_then(|s| s.parse::<usize>().ok())
}

/// Information about where a line ended up after chunking
#[derive(Debug, Clone)]
pub struct LineMapping {
    pub line_number: usize,
    pub chunk_number: usize,
    pub offset_in_chunk: usize,
}

/// Result of chunking: chunks + mapping of line numbers to their locations
pub struct ChunkResult {
    pub chunks: Vec<Chunk>,
    pub line_mappings: Vec<LineMapping>,
}

/// Chunk file content into fixed-size pieces, tracking where each line ends up
pub fn chunk_lines(file_id: i64, file_path: &str, lines: &[(usize, String)]) -> ChunkResult {
    let mut chunks = Vec::new();
    let mut line_mappings = Vec::new();
    let mut current_chunk = String::new();
    let mut chunk_number = 0;
    let mut offset_in_current_chunk = 0;

    for (line_num, content) in lines {
        let line_text = format!("{}\n", content);

        if current_chunk.len() + line_text.len() > CHUNK_SIZE && !current_chunk.is_empty() {
            chunks.push(Chunk {
                file_id,
                file_path: file_path.to_string(),
                chunk_number,
                content: current_chunk.clone(),
            });
            chunk_number += 1;
            current_chunk.clear();
            offset_in_current_chunk = 0;
        }

        current_chunk.push_str(&line_text);

        line_mappings.push(LineMapping {
            line_number: *line_num,
            chunk_number,
            offset_in_chunk: offset_in_current_chunk,
        });

        offset_in_current_chunk += 1;
    }

    if !current_chunk.is_empty() {
        chunks.push(Chunk {
            file_id,
            file_path: file_path.to_string(),
            chunk_number,
            content: current_chunk,
        });
    }

    ChunkResult {
        chunks,
        line_mappings,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_subfolder_calculation() {
        assert_eq!(0 / 1000, 0);
        assert_eq!(999 / 1000, 0);
        assert_eq!(1000 / 1000, 1);
        assert_eq!(12345 / 1000, 12);
    }

    #[test]
    fn test_parse_archive_number() {
        assert_eq!(parse_archive_number("content_00001.zip"), Some(1));
        assert_eq!(parse_archive_number("content_00999.zip"), Some(999));
        assert_eq!(parse_archive_number("content_12345.zip"), Some(12345));
        assert_eq!(parse_archive_number("invalid.zip"), None);
        assert_eq!(parse_archive_number("content_00001.tar"), None);
        assert_eq!(parse_archive_number("other_00001.zip"), None);
    }

    #[test]
    fn test_chunk_lines() {
        let lines = vec![
            (1, "a".repeat(500)),
            (2, "b".repeat(500)),
            (3, "c".repeat(500)),
        ];

        let result = chunk_lines(0, "/test/file.txt", &lines);

        assert_eq!(result.chunks.len(), 2);
        assert_eq!(result.chunks[0].chunk_number, 0);
        assert_eq!(result.chunks[1].chunk_number, 1);

        assert_eq!(result.line_mappings.len(), 3);
        assert_eq!(result.line_mappings[0].line_number, 1);
        assert_eq!(result.line_mappings[0].chunk_number, 0);
        assert_eq!(result.line_mappings[0].offset_in_chunk, 0);

        assert_eq!(result.line_mappings[1].line_number, 2);
        assert_eq!(result.line_mappings[1].chunk_number, 0);
        assert_eq!(result.line_mappings[1].offset_in_chunk, 1);

        assert_eq!(result.line_mappings[2].line_number, 3);
        assert_eq!(result.line_mappings[2].chunk_number, 1);
        assert_eq!(result.line_mappings[2].offset_in_chunk, 0);
    }

    #[test]
    fn test_chunk_single_large_line() {
        let lines = vec![(1, "x".repeat(2000))];

        let result = chunk_lines(0, "/test/file.txt", &lines);

        assert_eq!(result.chunks.len(), 1);
        assert_eq!(result.line_mappings.len(), 1);
        assert_eq!(result.line_mappings[0].offset_in_chunk, 0);
    }

    /// Two `ArchiveManager` instances sharing one `SharedArchiveState` must
    /// claim different archive numbers.
    #[test]
    fn shared_state_allocates_unique_archive_numbers() {
        let dir = tempfile::tempdir().unwrap();
        let state = SharedArchiveState::new(dir.path().to_path_buf()).unwrap();
        let n1 = state.allocate_archive_num();
        let n2 = state.allocate_archive_num();
        let n3 = state.allocate_archive_num();
        assert_ne!(n1, n2);
        assert_ne!(n2, n3);
        assert_ne!(n1, n3);
    }

    /// The counter is seeded from the max existing archive on disk.
    #[test]
    fn shared_state_seeds_from_existing_archives() {
        let dir = tempfile::tempdir().unwrap();
        let content_dir = dir.path().join("sources").join("content").join("0000");
        std::fs::create_dir_all(&content_dir).unwrap();
        // Simulate existing archives 1 and 5.
        File::create(content_dir.join("content_00001.zip")).unwrap();
        File::create(content_dir.join("content_00005.zip")).unwrap();

        let state = SharedArchiveState::new(dir.path().to_path_buf()).unwrap();
        // Next number should be 6 (max=5, next=6).
        let n = state.allocate_archive_num();
        assert_eq!(n, 6);
    }
}
