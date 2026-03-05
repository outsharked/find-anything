use anyhow::{Context, Result};
use std::collections::{HashMap, HashSet};
use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use zip::{CompressionMethod, ZipArchive, ZipWriter};
use zip::write::SimpleFileOptions;

const TARGET_ARCHIVE_SIZE: usize = 10 * 1024 * 1024; // 10MB
const CHUNK_SIZE: usize = 1024; // 1KB chunks

/// Manages ZIP archive storage for file content
pub struct ArchiveManager {
    data_dir: PathBuf,
    current_archive: Option<String>,
}

/// A chunk of file content to be stored
#[derive(Debug, Clone)]
pub struct Chunk {
    pub file_path: String,
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
    pub fn new(data_dir: PathBuf) -> Self {
        Self { data_dir, current_archive: None }
    }

    /// Calculate full path for an archive number using subfolder structure.
    /// Archives are organized in thousands-based subfolders:
    /// - content/0000/ → content_00000.zip to content_00999.zip
    /// - content/0001/ → content_01000.zip to content_01999.zip
    /// - content/0010/ → content_10000.zip to content_10999.zip
    fn archive_path_for_number(&self, archive_num: usize) -> PathBuf {
        let filename = format!("content_{:05}.zip", archive_num);
        let subfolder = archive_num / 1000;
        let subfolder_name = format!("{:04}", subfolder);

        let subfolder_path = self.sources_dir()
            .join("content")
            .join(subfolder_name);

        subfolder_path.join(filename)
    }

    /// Append chunks to archives, creating new ones as needed
    pub fn append_chunks(&mut self, chunks: Vec<Chunk>) -> Result<Vec<ChunkRef>> {
        let mut refs = Vec::new();

        for chunk in chunks {
            let archive_path = self.current_archive_path()?;
            let chunk_name = format!("{}.chunk{}.txt", chunk.file_path, chunk.chunk_number);

            self.append_to_zip(&archive_path, &chunk_name, chunk.content.as_bytes())?;

            refs.push(ChunkRef {
                archive_name: archive_path
                    .file_name()
                    .unwrap()
                    .to_string_lossy()
                    .to_string(),
                chunk_name,
            });

            // Rotate based on actual on-disk size (more accurate than tracking
            // uncompressed bytes, which ignores deflate compression ratio).
            let on_disk = std::fs::metadata(&archive_path)
                .map(|m| m.len() as usize)
                .unwrap_or(0);
            if on_disk >= TARGET_ARCHIVE_SIZE {
                self.current_archive = None;
            }
        }

        Ok(refs)
    }

    /// Remove chunks from archives by rewriting affected ZIPs.
    pub fn remove_chunks(&mut self, refs: Vec<ChunkRef>) -> Result<()> {
        // Group by archive
        let mut by_archive: HashMap<String, HashSet<String>> = HashMap::new();
        for chunk_ref in refs {
            by_archive
                .entry(chunk_ref.archive_name)
                .or_default()
                .insert(chunk_ref.chunk_name);
        }

        // Rewrite each affected archive
        for (archive_name, chunks_to_remove) in by_archive {
            let archive_path = if let Some(num) = parse_archive_number(&archive_name) {
                self.archive_path_for_number(num)
            } else {
                self.sources_dir().join(&archive_name)
            };

            if archive_path.exists() {
                // Guard against corrupt archives (e.g. truncated from a previous crash).
                // A corrupt archive contains no valid chunks, so there is nothing to remove.
                let valid = File::open(&archive_path)
                    .ok()
                    .and_then(|f| ZipArchive::new(f).ok())
                    .is_some();
                if valid {
                    self.rewrite_archive(&archive_path, &chunks_to_remove)?;
                } else {
                    tracing::warn!(
                        "skipping corrupt archive during chunk removal: {}",
                        archive_path.display()
                    );
                }
            }
        }

        Ok(())
    }

    /// Read chunk content from archive
    pub fn read_chunk(&self, chunk_ref: &ChunkRef) -> Result<String> {
        let archive_path = if let Some(num) = parse_archive_number(&chunk_ref.archive_name) {
            self.archive_path_for_number(num)
        } else {
            self.sources_dir().join(&chunk_ref.archive_name)
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

    /// Get or create the current archive for appending.
    ///
    /// If we already have a current archive that's under the size limit, return
    /// it.  Otherwise, scan the sources directory for the latest archive: if it
    /// is still under the limit we reuse it (avoids creating one archive per
    /// request).  Only creates a new numbered archive when the latest is full.
    fn current_archive_path(&mut self) -> Result<PathBuf> {
        if let Some(name) = &self.current_archive {
            // Parse the archive number to construct the proper subfolder path
            let path = if let Some(num) = parse_archive_number(name) {
                self.archive_path_for_number(num)
            } else {
                self.sources_dir().join(name)
            };
            let on_disk = std::fs::metadata(&path).map(|m| m.len() as usize).unwrap_or(0);
            if on_disk < TARGET_ARCHIVE_SIZE {
                return Ok(path);
            }
            // Current archive is full; fall through to find/create the next one.
            self.current_archive = None;
        }

        let content_dir = self.sources_dir().join("content");
        std::fs::create_dir_all(&content_dir)?;

        // Find the highest-numbered content archive across all subfolders.
        let mut max_num = 0;
        if let Ok(entries) = std::fs::read_dir(&content_dir) {
            for entry in entries.flatten() {
                if entry.path().is_dir() {
                    if let Ok(subdir) = std::fs::read_dir(entry.path()) {
                        for file_entry in subdir.flatten() {
                            if let Some(name) = file_entry.file_name().to_str() {
                                if let Some(num) = parse_archive_number(name) {
                                    max_num = max_num.max(num);
                                }
                            }
                        }
                    }
                }
            }
        }

        // Reuse the latest archive if it still has room and is not corrupt.
        if max_num > 0 {
            let latest_name = format!("content_{:05}.zip", max_num);
            let latest_path = self.archive_path_for_number(max_num);
            let on_disk = std::fs::metadata(&latest_path)
                .map(|m| m.len() as usize)
                .unwrap_or(usize::MAX);
            if on_disk < TARGET_ARCHIVE_SIZE {
                let valid = File::open(&latest_path)
                    .ok()
                    .and_then(|f| ZipArchive::new(f).ok())
                    .is_some();
                if valid {
                    self.current_archive = Some(latest_name);
                    return Ok(latest_path);
                }
                tracing::warn!(
                    "content archive is corrupt (truncated write?), skipping: {}",
                    latest_path.display()
                );
                // Fall through to create the next archive number.
            }
        }

        // All existing archives are full or corrupt (or there are none): create a new one.
        let new_num = max_num + 1;
        let new_name = format!("content_{:05}.zip", new_num);
        let new_path = self.archive_path_for_number(new_num);

        // Ensure the subfolder exists
        if let Some(parent) = new_path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let file = File::create(&new_path)?;
        let zip = ZipWriter::new(file);
        zip.finish()?;

        self.current_archive = Some(new_name);
        Ok(new_path)
    }

    /// Append a single entry to a ZIP file.
    ///
    /// If `entry_name` already exists in the archive (e.g. left over from a
    /// previous partially-failed run), the existing entry is removed first so
    /// that the write is idempotent and never produces a "Duplicate filename"
    /// error.
    fn append_to_zip(&self, archive_path: &Path, entry_name: &str, content: &[u8]) -> Result<()> {
        // Remove a pre-existing entry with the same name before appending.
        {
            let file = File::open(archive_path)?;
            let zip = ZipArchive::new(file)?;
            if zip.index_for_name(entry_name).is_some() {
                let to_remove: HashSet<String> = std::iter::once(entry_name.to_string()).collect();
                self.rewrite_archive(archive_path, &to_remove)?;
                tracing::warn!("removed stale chunk {entry_name} before re-appending");
            }
        }

        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .open(archive_path)?;

        let mut zip = ZipWriter::new_append(file)?;

        let options = SimpleFileOptions::default()
            .compression_method(CompressionMethod::Deflated)
            .compression_level(Some(6));

        zip.start_file(entry_name, options)?;
        zip.write_all(content)?;
        zip.finish()?;

        Ok(())
    }

    fn rewrite_archive(&self, archive_path: &Path, chunks_to_remove: &HashSet<String>) -> Result<()> {
        let temp_path = archive_path.with_extension("zip.tmp");

        // Read existing archive
        let file = File::open(archive_path)?;
        let mut old_zip = ZipArchive::new(file)?;

        // Create new archive
        let temp_file = File::create(&temp_path)?;
        let mut new_zip = ZipWriter::new(temp_file);

        let options = SimpleFileOptions::default()
            .compression_method(CompressionMethod::Deflated)
            .compression_level(Some(6));

        // Copy entries except removed ones
        for i in 0..old_zip.len() {
            let mut entry = old_zip.by_index(i)?;
            let name = entry.name().to_string();

            if !chunks_to_remove.contains(&name) {
                new_zip.start_file(&name, options)?;
                std::io::copy(&mut entry, &mut new_zip)?;
            }
        }

        new_zip.finish()?;
        drop(old_zip);

        // Atomic replace
        std::fs::rename(&temp_path, archive_path)?;

        Ok(())
    }

    fn sources_dir(&self) -> PathBuf {
        self.data_dir.join("sources")
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
pub fn chunk_lines(file_path: &str, lines: &[(usize, String)]) -> ChunkResult {
    let mut chunks = Vec::new();
    let mut line_mappings = Vec::new();
    let mut current_chunk = String::new();
    let mut chunk_number = 0;
    let mut offset_in_current_chunk = 0;

    for (line_num, content) in lines {
        let line_text = format!("{}\n", content);

        // Check if adding this line would exceed chunk size
        if current_chunk.len() + line_text.len() > CHUNK_SIZE && !current_chunk.is_empty() {
            // Flush current chunk
            chunks.push(Chunk {
                file_path: file_path.to_string(),
                chunk_number,
                content: current_chunk.clone(),
            });
            chunk_number += 1;
            current_chunk.clear();
            offset_in_current_chunk = 0;
        }

        // Add line to current chunk
        current_chunk.push_str(&line_text);

        // Record where this line ended up
        line_mappings.push(LineMapping {
            line_number: *line_num,
            chunk_number,
            offset_in_chunk: offset_in_current_chunk,
        });

        offset_in_current_chunk += 1;
    }

    // Flush final chunk
    if !current_chunk.is_empty() {
        chunks.push(Chunk {
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
        assert_eq!(0 / 1000, 0);      // archive 0-999 → folder 0
        assert_eq!(999 / 1000, 0);
        assert_eq!(1000 / 1000, 1);   // archive 1000-1999 → folder 1
        assert_eq!(12345 / 1000, 12); // archive 12345 → folder 12
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

        let result = chunk_lines("/test/file.txt", &lines);

        // First two lines should fit in one chunk (500 + 1 + 500 + 1 = 1002 < 1024)
        // Third line needs its own chunk
        assert_eq!(result.chunks.len(), 2);
        assert_eq!(result.chunks[0].chunk_number, 0);
        assert_eq!(result.chunks[1].chunk_number, 1);

        // Check line mappings
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

        let result = chunk_lines("/test/file.txt", &lines);

        // One large line goes into its own chunk (exceeds 1KB but we don't split lines)
        assert_eq!(result.chunks.len(), 1);
        assert_eq!(result.line_mappings.len(), 1);
        assert_eq!(result.line_mappings[0].offset_in_chunk, 0);
    }
}
