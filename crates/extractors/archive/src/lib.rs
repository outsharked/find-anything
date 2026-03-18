use std::fs::File;
use std::io::{Cursor, Read};
use std::path::Path;

use anyhow::{Context, Result};
use bzip2::read::BzDecoder;
use flate2::read::GzDecoder;
use globset::GlobSet;
use tracing::warn;
use xz2::read::XzDecoder;

use find_extract_types::IndexLine;
use find_extract_types::{build_globset, ExtractorConfig};

/// One batch of lines for a single archive member, with its content hash.
#[derive(Default, serde::Serialize, serde::Deserialize)]
pub struct MemberBatch {
    pub lines: Vec<IndexLine>,
    /// blake3 hex hash of the member's raw bytes (decompressed from the archive).
    /// None for filename-only entries (too large, nested archives, or single-compressed).
    pub content_hash: Option<String>,
    /// Set when content extraction was skipped or failed.
    /// The caller (scan.rs) records this as an IndexingFailure for the member's path,
    /// so the reason is surfaced to users in the file viewer and errors panel.
    ///
    /// When `lines` is empty, the failure applies to the outer archive itself
    /// (e.g. a 7z solid block summary) rather than to a specific member.
    pub skip_reason: Option<String>,
    /// Unix timestamp (seconds) of this member's internal archive timestamp, if available.
    /// None means the caller should fall back to the outer archive's filesystem mtime.
    pub mtime: Option<i64>,
}

// Internal callback alias for brevity.
type CB<'a> = &'a mut dyn FnMut(MemberBatch);

/// Returns true if any path component starts with `.` (and is not `.` or `..`).
/// Used to skip hidden members (e.g. `.terraform/`, `.git/`) when
/// `cfg.include_hidden` is false.
fn has_hidden_component(name: &str) -> bool {
    name.split('/').any(|c| c.starts_with('.') && c.len() > 1 && c != "..")
}

use find_extract_types::mem::available_bytes as available_memory_bytes;

/// Extract content from archive files (ZIP, TAR, TGZ, TBZ2, TXZ, GZ, BZ2, XZ, 7Z).
///
/// Calls `callback` once per top-level archive member with that member's lines
/// (including recursively extracted nested-archive content).  This keeps memory
/// usage proportional to one member at a time rather than the whole archive.
///
/// Use `extract` if you need a `Vec<IndexLine>` instead of a callback.
pub fn extract_streaming<F>(path: &Path, cfg: &ExtractorConfig, callback: &mut F) -> Result<()>
where
    F: FnMut(MemberBatch),
{
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
    let kind = detect_kind_from_name(name).context("not a recognized archive")?;
    dispatch_streaming(path, &kind, cfg, callback)
}

/// Extract content from archive files, collecting all lines into a `Vec`.
///
/// For large archives prefer `extract_streaming` to avoid accumulating all
/// member lines in memory simultaneously.
pub fn extract(path: &Path, cfg: &ExtractorConfig) -> Result<Vec<IndexLine>> {
    let mut lines = Vec::new();
    extract_streaming(path, cfg, &mut |batch| lines.extend(batch.lines))?;
    Ok(lines)
}

/// Check if a file is an archive based on extension.
pub fn accepts(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(is_archive_ext)
        .unwrap_or(false)
}

pub fn is_archive_ext(ext: &str) -> bool {
    matches!(
        ext.to_lowercase().as_str(),
        "zip" | "tar" | "gz" | "bz2" | "xz" | "tgz" | "tbz2" | "txz" | "7z"
    )
}

// ============================================================================
// ARCHIVE KIND DETECTION
// ============================================================================

#[derive(Debug, Clone, PartialEq)]
enum ArchiveKind {
    Zip,
    TarGz,
    TarBz2,
    TarXz,
    Tar,
    Gz,       // single-file gzip (e.g. foo.log.gz)
    Bz2,      // single-file bzip2
    Xz,       // single-file xz
    SevenZip,
}

fn detect_kind_from_name(name: &str) -> Option<ArchiveKind> {
    let n = name.to_lowercase();
    // Compound extensions must be checked before simple ones
    if n.ends_with(".tar.gz") || n.ends_with(".tgz")   { return Some(ArchiveKind::TarGz);   }
    if n.ends_with(".tar.bz2") || n.ends_with(".tbz2") { return Some(ArchiveKind::TarBz2);  }
    if n.ends_with(".tar.xz") || n.ends_with(".txz")   { return Some(ArchiveKind::TarXz);   }
    if n.ends_with(".tar")                              { return Some(ArchiveKind::Tar);     }
    if n.ends_with(".zip")                              { return Some(ArchiveKind::Zip);     }
    if n.ends_with(".gz")                               { return Some(ArchiveKind::Gz);      }
    if n.ends_with(".bz2")                              { return Some(ArchiveKind::Bz2);     }
    if n.ends_with(".xz")                               { return Some(ArchiveKind::Xz);      }
    if n.ends_with(".7z")                               { return Some(ArchiveKind::SevenZip);}
    None
}

fn is_multifile_archive(kind: &ArchiveKind) -> bool {
    !matches!(kind, ArchiveKind::Gz | ArchiveKind::Bz2 | ArchiveKind::Xz)
}

// ============================================================================
// DISPATCH
// ============================================================================

/// Internal dispatch: uses `dyn FnMut` to avoid infinite monomorphisation when
/// nested archive extraction recurses back through the streaming functions.
fn dispatch_streaming(path: &Path, kind: &ArchiveKind, cfg: &ExtractorConfig, callback: CB<'_>) -> Result<()> {
    match kind {
        ArchiveKind::Zip      => zip_streaming(path, cfg, callback),
        ArchiveKind::TarGz    => tar_streaming(tar::Archive::new(GzDecoder::new(File::open(path)?)), path.to_str().unwrap_or(""), cfg, callback),
        ArchiveKind::TarBz2   => tar_streaming(tar::Archive::new(BzDecoder::new(File::open(path)?)), path.to_str().unwrap_or(""), cfg, callback),
        ArchiveKind::TarXz    => tar_streaming(tar::Archive::new(XzDecoder::new(File::open(path)?)), path.to_str().unwrap_or(""), cfg, callback),
        ArchiveKind::Tar      => tar_streaming(tar::Archive::new(File::open(path)?), path.to_str().unwrap_or(""), cfg, callback),
        ArchiveKind::Gz       => { callback(single_compressed(GzDecoder::new(File::open(path)?), path, cfg)?); Ok(()) }
        ArchiveKind::Bz2      => { callback(single_compressed(BzDecoder::new(File::open(path)?), path, cfg)?); Ok(()) }
        ArchiveKind::Xz       => { callback(single_compressed(XzDecoder::new(File::open(path)?), path, cfg)?); Ok(()) }
        ArchiveKind::SevenZip => sevenz_streaming(path, path.to_str().unwrap_or(""), cfg, callback),
    }
}

// ============================================================================
// FORMAT-SPECIFIC STREAMING EXTRACTORS
// ============================================================================

/// Unix timestamp of 2099-12-31 23:59:59 UTC.
const UNIX_END_OF_2099: i64 = 4_102_444_799;

/// Sanity-check an archive member's mtime against known Y2K artifacts.
///
/// Old ZIP tools often stored 2-digit years in the DOS datetime field, causing
/// the zip reader to add 1980 and produce years like 2077 or 2097 instead of
/// the intended 1977 or 1997.  Heuristic:
///
/// - Timestamp is in the past or present → accept as-is.
/// - Timestamp is in the future but ≤ 2099-12-31 → subtract 100 years (Y2K).
/// - Timestamp is after 2099 → clearly bogus, return None.
fn sanitize_archive_mtime(ts: i64) -> Option<i64> {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    if ts <= now {
        Some(ts)
    } else if ts <= UNIX_END_OF_2099 {
        // Approximate: 100 Julian years = 100 * 365.25 * 86400
        Some(ts - 3_155_760_000)
    } else {
        None
    }
}


/// Parse the UNIX extended timestamp from a ZIP extra data block (tag 0x5455).
/// The local-file version has up to three timestamps; only mtime (flags bit 0) is used.
fn zip_unix_mtime(extra: &[u8]) -> Option<i64> {
    let mut i = 0;
    while i + 4 <= extra.len() {
        let tag = u16::from_le_bytes([extra[i], extra[i + 1]]);
        let size = u16::from_le_bytes([extra[i + 2], extra[i + 3]]) as usize;
        i += 4;
        if i + size > extra.len() { break; }
        if tag == 0x5455 && size >= 5 {
            let flags = extra[i];
            if flags & 0x01 != 0 {
                let secs = i32::from_le_bytes([extra[i + 1], extra[i + 2], extra[i + 3], extra[i + 4]]);
                return Some(secs as i64);
            }
        }
        i += size;
    }
    None
}

/// Convert a ZIP DOS datetime to a unix timestamp (seconds since 1970-01-01 UTC).
/// Returns None when the year is ≤ 1980 (the DOS epoch default, meaning "not set").
fn zip_dos_to_unix(dt: zip::DateTime) -> Option<i64> {
    if dt.year() <= 1980 { return None; }
    let y = dt.year() as i64;
    let mo = dt.month() as i64;
    let d = dt.day() as i64;
    // Shift Jan/Feb to be months 13/14 of the preceding year so leap-day math works out.
    let (y, m) = if mo <= 2 { (y - 1, mo + 9) } else { (y, mo - 3) };
    let era = (if y >= 0 { y } else { y - 399 }) / 400;
    let yoe = y - era * 400;
    let doy = (153 * m + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146097 + doe - 719468; // days since 1970-01-01
    Some(days * 86400 + dt.hour() as i64 * 3600 + dt.minute() as i64 * 60 + dt.second() as i64)
}

fn zip_streaming(path: &Path, cfg: &ExtractorConfig, callback: CB<'_>) -> Result<()> {
    let file = File::open(path)?;
    let archive = zip::ZipArchive::new(file).context("opening zip")?;
    zip_from_archive(archive, path.to_str().unwrap_or(""), cfg, callback)
}

/// Core ZIP extractor, generic over any `Read + Seek` source.
///
/// Called for top-level ZIPs (via a file path) and for nested ZIPs
/// (via a `Cursor<Vec<u8>>` read from an outer archive member).
fn zip_from_archive<R: Read + std::io::Seek>(
    mut archive: zip::ZipArchive<R>,
    display_prefix: &str,
    cfg: &ExtractorConfig,
    callback: CB<'_>,
) -> Result<()> {
    let size_limit = cfg.max_content_kb * 1024;
    let excludes = build_globset(&cfg.exclude_patterns).unwrap_or_default();

    for i in 0..archive.len() {
        let mut entry = match archive.by_index(i) {
            Ok(e) => e,
            Err(e) => { warn!("zip: skipping entry {i}: {e:#}"); continue; }
        };
        if entry.is_dir() {
            continue;
        }
        let name = entry.name().to_string();

        if !cfg.include_hidden && has_hidden_component(&name) {
            continue;
        }

        if excludes.is_match(&*name) {
            continue;
        }

        // Extract member timestamp: prefer extended timestamp (UTC), fall back to DOS datetime.
        // Sanitize to catch Y2K artifacts (2-digit years misread as 20xx).
        let mtime = entry.extra_data().and_then(zip_unix_mtime)
            .or_else(|| entry.last_modified().and_then(zip_dos_to_unix))
            .and_then(sanitize_archive_mtime);

        // Multi-file nested archive: recurse without writing to disk where possible.
        if let Some(kind) = detect_kind_from_name(&name) {
            if is_multifile_archive(&kind) {
                handle_nested_archive(&mut entry as &mut dyn Read, &name, &kind, cfg, callback);
                continue;
            }
        }

        // Read up to size_limit bytes; truncate naturally via take().
        // Content is truncated at the limit rather than skipped.
        let mut bytes = Vec::new();
        let read_result = (&mut entry as &mut dyn Read).take(size_limit as u64).read_to_end(&mut bytes);
        let skip_reason = if let Err(ref e) = read_result {
            let member_path = std::path::Path::new(&name);
            if find_extract_media::accepts(member_path) {
                tracing::debug!("zip: skipping binary entry '{}': {}", name, e);
                None
            } else {
                warn!("zip: failed to read entry '{}': {}", name, e);
                if bytes.is_empty() { Some(format!("failed to read: {e}")) } else { None }
            }
        } else {
            None
        };
        let content_hash = if bytes.is_empty() { None } else { Some(blake3::hash(&bytes).to_hex().to_string()) };
        callback(MemberBatch { lines: extract_member_bytes(bytes, &name, display_prefix, cfg), content_hash, skip_reason, mtime });
    }
    Ok(())
}

fn tar_streaming<R: Read>(mut archive: tar::Archive<R>, display_prefix: &str, cfg: &ExtractorConfig, callback: CB<'_>) -> Result<()> {
    let size_limit = cfg.max_content_kb * 1024;
    let excludes = build_globset(&cfg.exclude_patterns).unwrap_or_default();

    for entry_result in archive.entries().context("reading tar entries")? {
        let mut entry = match entry_result {
            Ok(e) => e,
            Err(e) => { warn!("tar: skipping entry: {e:#}"); continue; }
        };
        if entry.header().entry_type().is_dir() {
            continue;
        }
        let name = entry.path()
            .ok()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_default();

        if !cfg.include_hidden && has_hidden_component(&name) {
            continue;
        }

        if excludes.is_match(&*name) {
            continue;
        }

        let mtime = entry.header().mtime().ok().map(|t| t as i64).and_then(sanitize_archive_mtime);

        // Multi-file nested archive: recurse without writing to disk where possible.
        if let Some(kind) = detect_kind_from_name(&name) {
            if is_multifile_archive(&kind) {
                handle_nested_archive(&mut entry as &mut dyn Read, &name, &kind, cfg, callback);
                continue;
            }
        }

        // Read up to size_limit bytes; truncate naturally via take().
        // The tar crate drains remaining entry bytes on Entry::drop(), so a
        // partial read here won't desync the stream.
        // Content is truncated at the limit rather than skipped.
        let mut bytes = Vec::new();
        let read_result = (&mut entry as &mut dyn Read).take(size_limit as u64).read_to_end(&mut bytes);
        let skip_reason = if let Err(ref e) = read_result {
            let member_path = std::path::Path::new(&name);
            if find_extract_media::accepts(member_path) {
                tracing::debug!("tar: skipping binary entry '{}': {}", name, e);
                None
            } else {
                warn!("tar: failed to read entry '{}': {}", name, e);
                if bytes.is_empty() { Some(format!("failed to read: {e}")) } else { None }
            }
        } else {
            None
        };
        let content_hash = if bytes.is_empty() { None } else { Some(blake3::hash(&bytes).to_hex().to_string()) };
        callback(MemberBatch { lines: extract_member_bytes(bytes, &name, display_prefix, cfg), content_hash, skip_reason, mtime });
    }
    Ok(())
}

/// Process one 7z entry: check size, read content, emit to callback.
///
/// Shared by the per-block loop and the empty-file fallback path.
/// Always fully drains `reader` to keep solid-block streams in sync.
fn sevenz_process_entry(
    entry: &sevenz_rust2::ArchiveEntry,
    reader: &mut dyn Read,
    display_prefix: &str,
    size_limit: usize,
    cfg: &ExtractorConfig,
    excludes: &GlobSet,
    callback: CB<'_>,
) -> Result<bool, sevenz_rust2::Error> {
    if entry.is_directory() {
        return Ok(true);
    }
    let name = entry.name().to_string();

    if !cfg.include_hidden && has_hidden_component(&name) {
        // Drain so solid-block stream stays in sync.
        let _ = std::io::copy(reader, &mut std::io::sink());
        return Ok(true);
    }

    if excludes.is_match(&*name) {
        // Drain so solid-block stream stays in sync.
        let _ = std::io::copy(reader, &mut std::io::sink());
        return Ok(true);
    }

    // Multi-file nested archive: handle_nested_archive always drains `reader`,
    // maintaining solid-block integrity.
    if let Some(kind) = detect_kind_from_name(&name) {
        if is_multifile_archive(&kind) {
            handle_nested_archive(reader, &name, &kind, cfg, callback);
            return Ok(true);
        }
    }

    // Read up to size_limit bytes; truncate naturally via take().
    // Content is truncated at the limit rather than skipped.
    // After the bounded read, drain any remaining bytes for this entry to keep
    // the solid-block stream in sync for subsequent entries.
    let mut bytes = Vec::new();
    let read_result = {
        let mut limited = (reader as &mut dyn Read).take(size_limit as u64);
        limited.read_to_end(&mut bytes)
    };
    // Drain remaining bytes so the solid-block stream stays in sync.
    let _ = std::io::copy(reader, &mut std::io::sink());
    let skip_reason = if let Err(ref e) = read_result {
        let msg = e.to_string();
        if msg.contains("ChecksumVerificationFailed") {
            warn!("7z: checksum mismatch for '{}': {}", name, e);
            Some(format!("checksum verification failed: {e}"))
        } else {
            let member_path = std::path::Path::new(&name);
            if find_extract_media::accepts(member_path) {
                tracing::debug!("7z: skipping binary entry '{}': {}", name, e);
                None
            } else {
                warn!("7z: failed to read entry '{}': {}", name, e);
                if bytes.is_empty() { Some(format!("failed to read: {e}")) } else { None }
            }
        }
    } else {
        None
    };
    let mtime = if entry.has_last_modified_date {
        std::time::SystemTime::from(entry.last_modified_date)
            .duration_since(std::time::UNIX_EPOCH)
            .ok()
            .map(|d| d.as_secs() as i64)
            .and_then(sanitize_archive_mtime)
    } else {
        None
    };

    let content_hash = if bytes.is_empty() { None } else { Some(blake3::hash(&bytes).to_hex().to_string()) };
    callback(MemberBatch { lines: extract_member_bytes(bytes, &name, display_prefix, cfg), content_hash, skip_reason, mtime });
    Ok(true)
}

fn sevenz_streaming(path: &Path, display_prefix: &str, cfg: &ExtractorConfig, callback: CB<'_>) -> Result<()> {
    use std::collections::HashSet;

    let size_limit = cfg.max_content_kb * 1024;
    let excludes = build_globset(&cfg.exclude_patterns).unwrap_or_default();

    // Parse the archive header to inspect block sizes before any decompression.
    // The LZMA decoder allocates a dictionary buffer proportional to the block's
    // unpack size BEFORE our per-file callback is ever called.  On memory-
    // constrained systems this single allocation can exhaust available memory.
    let archive = {
        let mut f = File::open(path)?;
        sevenz_rust2::Archive::read(&mut f, &sevenz_rust2::Password::empty())
            .context("7z: failed to parse archive header")?
    };

    // Static guard: skip blocks that exceed the configured hard ceiling.
    // This is a coarse backstop — the dynamic memory check below handles
    // blocks that pass this limit but still can't be safely allocated at
    // runtime given current system conditions.
    let max_block_bytes = cfg.max_7z_solid_block_mb * 1024 * 1024;

    let oversized: HashSet<usize> = archive
        .blocks
        .iter()
        .enumerate()
        .filter(|(_, b)| b.get_unpack_size() as usize > max_block_bytes)
        .map(|(i, _)| i)
        .collect();

    if !oversized.is_empty() {
        let skipped: usize = archive
            .stream_map
            .file_block_index
            .iter()
            .filter(|opt| opt.is_some_and(|bi| oversized.contains(&bi)))
            .count();
        warn!(
            "7z: '{}': {} solid block(s) exceed {} MB; {} file(s) will be indexed by filename only",
            path.display(),
            oversized.len(),
            cfg.max_7z_solid_block_mb,
            skipped,
        );
        let largest_block_mb = oversized
            .iter()
            .map(|&bi| archive.blocks[bi].get_unpack_size() / (1024 * 1024))
            .max()
            .unwrap_or(0);
        callback(MemberBatch {
            lines: vec![],
            content_hash: None,
            skip_reason: Some(format!(
                "7z: {} file(s) in {} solid block(s) not extracted \
                 (largest block {} MB exceeds memory limit of {} MB); \
                 filenames indexed only",
                skipped, oversized.len(), largest_block_mb, cfg.max_7z_solid_block_mb,
            )),
            ..Default::default()
        });
        for (file_idx, block_opt) in archive.stream_map.file_block_index.iter().enumerate() {
            if let Some(bi) = *block_opt {
                if oversized.contains(&bi) {
                    let entry = &archive.files[file_idx];
                    if !entry.is_directory() {
                        callback(MemberBatch {
                            lines: make_filename_line(entry.name()),
                            content_hash: None,
                            ..Default::default()
                        });
                    }
                }
            }
        }
    }

    let thread_count = std::thread::available_parallelism()
        .map(|n| n.get() as u32)
        .unwrap_or(1);
    let mut source = File::open(path)?;

    let password = sevenz_rust2::Password::empty();
    for block_index in 0..archive.blocks.len() {
        if oversized.contains(&block_index) {
            continue;
        }

        // Dynamic memory guard: check available system memory right before
        // decoding each block.  get_unpack_size() is a lower-bound estimate
        // of what the LZMA decoder will allocate (the actual dictionary can
        // be larger).  Skip the block if decoding it would consume more than
        // 75% of currently available memory, leaving headroom for the OS and
        // the rest of the scan process.
        //
        // get_unpack_size() == 0 means the block header doesn't record a
        // block-level total (common in solid archives where individual file
        // sizes ARE stored but not summed).  Fall back to summing individual
        // file sizes as a memory estimate so we don't skip extractable blocks.
        let unpack_size = {
            let block_size = archive.blocks[block_index].get_unpack_size();
            if block_size == 0 {
                archive
                    .stream_map
                    .file_block_index
                    .iter()
                    .enumerate()
                    .filter(|(_, b)| b.is_some_and(|bi| bi == block_index))
                    .map(|(fi, _)| archive.files[fi].size())
                    .sum::<u64>()
            } else {
                block_size
            }
        };
        // Helper: collect non-directory file names in this block.
        let block_files = || -> Vec<&str> {
            archive
                .stream_map
                .file_block_index
                .iter()
                .enumerate()
                .filter(|(_, b)| b.is_some_and(|bi| bi == block_index))
                .filter_map(|(fi, _)| {
                    let e = &archive.files[fi];
                    if e.is_directory() { None } else { Some(e.name()) }
                })
                .collect()
        };

        if let Some(avail) = available_memory_bytes() {
            let budget = avail * 3 / 4;
            if unpack_size > budget {
                let names = block_files();
                warn!(
                    "7z: '{}': block {} needs ~{} MB but only ~{} MB available \
                     (75% budget ~{} MB); {} file(s) indexed by filename only",
                    path.display(), block_index,
                    unpack_size / (1024 * 1024),
                    avail / (1024 * 1024),
                    budget / (1024 * 1024),
                    names.len(),
                );
                let skip_reason = Some(format!(
                    "insufficient memory to extract \
                     (~{} MB needed, ~{} MB available)",
                    unpack_size / (1024 * 1024),
                    avail / (1024 * 1024),
                ));
                for name in names {
                    callback(MemberBatch {
                        lines: make_filename_line(name),
                        content_hash: None,
                        skip_reason: skip_reason.clone(),
                        ..Default::default()
                    });
                }
                continue;
            }
        }

        let block_dec = sevenz_rust2::BlockDecoder::new(
            thread_count,
            block_index,
            &archive,
            &password,
            &mut source,
        );
        if let Err(e) = block_dec.for_each_entries(&mut |entry, reader| {
            sevenz_process_entry(entry, reader, display_prefix, size_limit, cfg, &excludes, callback)
        }) {
            warn!("7z: '{}': block {} error: {:#}", path.display(), block_index, e);
        }
    }

    // Emit entries for files that have no associated block (empty files / dirs
    // that appear in the file list but have no data stream in the archive).
    for (file_idx, block_opt) in archive.stream_map.file_block_index.iter().enumerate() {
        if block_opt.is_none() {
            let entry = &archive.files[file_idx];
            if !entry.is_directory() {
                let empty: &mut dyn Read = &mut ([0u8; 0].as_slice());
                sevenz_process_entry(entry, empty, display_prefix, size_limit, cfg, &excludes, callback)
                    .map_err(|e| anyhow::anyhow!("{e}"))?;
            }
        }
    }

    Ok(())
}

/// Extract a single-file compressed archive (bare .gz, .bz2, .xz).
/// Decompresses up to `cfg.max_content_kb` bytes and indexes the inner content.
fn single_compressed<R: Read>(reader: R, path: &Path, cfg: &ExtractorConfig) -> Result<MemberBatch> {
    let inner_name = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("file")
        .to_string();

    let size_limit = cfg.max_content_kb * 1024;
    let mut bytes = Vec::new();
    // Truncate at the limit rather than skipping.
    reader.take(size_limit as u64).read_to_end(&mut bytes)?;

    let content_hash = if bytes.is_empty() { None } else { Some(blake3::hash(&bytes).to_hex().to_string()) };
    Ok(MemberBatch {
        lines: extract_member_bytes(bytes, &inner_name, path.to_str().unwrap_or(""), cfg),
        content_hash,
        skip_reason: None,
        mtime: None, // single-file wrapper: caller uses outer archive's filesystem mtime
    })
}

// ============================================================================
// NESTED ARCHIVE EXTRACTION
// ============================================================================

/// Extract a nested multi-file archive from a stream, recursing into its members.
///
/// - **Tar variants** (Tar, TarGz, TarBz2, TarXz): streamed directly from `reader`
///   — zero extra memory and no disk I/O beyond what the tar crate uses internally.
/// - **Zip**: bytes are read into a `Cursor<Vec<u8>>` for in-memory extraction (no
///   disk I/O); falls back to a temp file on disk if the stream exceeds `max_temp_file_mb`.
/// - **7z**: always written to a temp file on disk (the 7z API requires a seekable
///   path); bounded by `max_temp_file_mb`.
///
/// Dynamic dispatch for both callback (`dyn FnMut`) AND reader (`dyn Read`) is used
/// to prevent infinite monomorphisation when the extraction functions recurse through
/// nested archives.
///
/// **Always fully consumes `reader`**, which is required for 7z solid-block stream
/// integrity even when the depth or size limit is exceeded.
fn handle_nested_archive(
    reader: &mut dyn Read,
    outer_name: &str,
    kind: &ArchiveKind,
    cfg: &ExtractorConfig,
    callback: CB<'_>,
) {
    // Always emit the filename of the nested archive itself.
    callback(MemberBatch { lines: make_filename_line(outer_name), content_hash: None, skip_reason: None, mtime: None });

    if cfg.max_depth == 0 {
        warn!(
            "archive nesting limit exceeded at '{}'; indexing filename only",
            outer_name
        );
        // Drain so 7z solid-block stream stays in sync.
        let _ = std::io::copy(reader, &mut std::io::sink());
        return;
    }

    let inner_cfg = ExtractorConfig {
        max_depth: cfg.max_depth.saturating_sub(1),
        ..cfg.clone()
    };

    // Wrapper callback that prefixes inner archive_paths with `outer_name::`.
    let outer_prefix = outer_name.to_string();
    let mut prefixed = |inner_batch: MemberBatch| {
        let p: Vec<IndexLine> = inner_batch.lines
            .into_iter()
            .map(|mut l| {
                let inner = l.archive_path.as_deref().unwrap_or("");
                l.archive_path = Some(if inner.is_empty() {
                    outer_prefix.clone()
                } else {
                    format!("{}::{}", outer_prefix, inner)
                });
                l
            })
            .collect();
        callback(MemberBatch { lines: p, content_hash: inner_batch.content_hash, skip_reason: inner_batch.skip_reason, mtime: inner_batch.mtime });
    };

    // Use `reader` as `&mut dyn Read` throughout so that tar_streaming<GzDecoder<&mut dyn Read>>
    // is always the same monomorphisation regardless of nesting depth.
    let result: Result<()> = match kind {
        // ── Tar variants: stream directly, zero extra memory ─────────────
        ArchiveKind::TarGz  => tar_streaming(tar::Archive::new(GzDecoder::new(reader)), outer_name, &inner_cfg, &mut prefixed),
        ArchiveKind::TarBz2 => tar_streaming(tar::Archive::new(BzDecoder::new(reader)), outer_name, &inner_cfg, &mut prefixed),
        ArchiveKind::TarXz  => tar_streaming(tar::Archive::new(XzDecoder::new(reader)), outer_name, &inner_cfg, &mut prefixed),
        ArchiveKind::Tar    => tar_streaming(tar::Archive::new(reader), outer_name, &inner_cfg, &mut prefixed),

        // ── Zip: read into memory (Cursor); temp file if too large ────────
        ArchiveKind::Zip    => nested_zip(reader, outer_name, &inner_cfg, &mut prefixed),

        // ── 7z: requires a seekable file path — always use temp file ─────
        ArchiveKind::SevenZip => nested_sevenz(reader, outer_name, &inner_cfg, &mut prefixed),

        // Single-file compressed types are not passed to handle_nested_archive.
        _ => return,
    };

    if let Err(e) = result {
        // Corrupt or truncated nested archives (e.g. "Could not find EOCD") are
        // common in real-world data and unactionable — the filename is already
        // indexed above, so demote to DEBUG to avoid noisy logs.
        tracing::debug!("failed to extract nested archive '{}': {:#}", outer_name, e);
    }
}

/// Extract a nested zip from a reader by buffering bytes into a `Cursor<Vec<u8>>`.
///
/// If the stream exceeds `max_temp_file_mb`, spills to a temp file on disk instead.
fn nested_zip(mut reader: &mut dyn Read, outer_name: &str, cfg: &ExtractorConfig, callback: CB<'_>) -> Result<()> {
    let max_bytes = (cfg.max_temp_file_mb * 1024 * 1024) as u64;

    // Read up to max_bytes+1 to detect whether the archive is over the limit.
    let mut bytes = Vec::new();
    let written = {
        let mut limited = (&mut reader).take(max_bytes + 1);
        std::io::copy(&mut limited, &mut bytes)?
    };

    if written > max_bytes {
        warn!(
            "nested zip '{}' exceeds {} MB; falling back to temp file",
            outer_name, cfg.max_temp_file_mb
        );
        // Spill already-read bytes plus remainder to a temp file, then extract from it.
        let ext = Path::new(outer_name).extension().and_then(|e| e.to_str()).unwrap_or("zip");
        let mut tmp = tempfile::Builder::new()
            .suffix(&format!(".{}", ext))
            .tempfile()?;
        {
            use std::io::Write;
            tmp.write_all(&bytes)?;
        }
        std::io::copy(&mut reader, &mut tmp)?;
        {
            use std::io::{Seek, Write};
            tmp.flush()?;
            tmp.seek(std::io::SeekFrom::Start(0))?;
        }
        let archive = zip::ZipArchive::new(tmp).context("opening oversized nested zip from temp file")?;
        return zip_from_archive(archive, outer_name, cfg, callback);
    }

    let archive = zip::ZipArchive::new(Cursor::new(bytes)).context("opening nested zip")?;
    zip_from_archive(archive, outer_name, cfg, callback)
}

/// Extract a nested 7z archive by streaming it to a temp file on disk.
///
/// 7z extraction requires a seekable file path; there is no in-memory API.
/// The temp file is bounded by `max_temp_file_mb`; archives larger than that are
/// skipped (filename only) and the reader is drained for solid-block integrity.
fn nested_sevenz(mut reader: &mut dyn Read, outer_name: &str, cfg: &ExtractorConfig, callback: CB<'_>) -> Result<()> {
    let max_bytes = (cfg.max_temp_file_mb * 1024 * 1024) as u64;
    let ext = Path::new(outer_name).extension().and_then(|e| e.to_str()).unwrap_or("7z");

    let mut tmp = tempfile::Builder::new()
        .suffix(&format!(".{}", ext))
        .tempfile()?;

    // Write at most max_bytes+1 so we can detect oversized archives.
    let written = {
        let mut limited = (&mut reader).take(max_bytes + 1);
        std::io::copy(&mut limited, &mut tmp)?
    };

    if written > max_bytes {
        warn!(
            "nested 7z '{}' exceeds {} MB; indexing filename only",
            outer_name, cfg.max_temp_file_mb
        );
        // Drain remaining bytes for 7z solid-block stream integrity.
        let _ = std::io::copy(&mut reader, &mut std::io::sink());
        return Ok(());
    }

    {
        use std::io::{Seek, Write};
        tmp.flush()?;
        tmp.seek(std::io::SeekFrom::Start(0))?;
    }
    sevenz_streaming(tmp.path(), outer_name, cfg, callback)
}

// ============================================================================
// MEMBER EXTRACTION (handles bytes from any non-archive format)
// ============================================================================

// ============================================================================
// MEMBER EXTRACTION (handles bytes from any non-archive format)
// ============================================================================

/// Returns a Vec containing a single filename-only IndexLine for `name`.
fn make_filename_line(name: &str) -> Vec<IndexLine> {
    vec![IndexLine {
        archive_path: Some(name.to_string()),
        line_number: 0,
        content: name.to_string(),
    }]
}


/// Wrapper around `dispatch_from_bytes` that catches panics from buggy parsers.
///
/// Some malformed files (e.g. XLS files with a garbage allocation-size field in
/// their OLE header) can cause extractors to panic with "capacity overflow" rather
/// than returning an `Err`.  Since `dispatch_from_bytes` runs in-process inside the
/// archive extractor, an uncaught panic kills the entire subprocess and fails the
/// whole archive.  `catch_unwind` intercepts these panics and logs a warning so the
/// remaining members are still processed.
///
/// Note: OOM aborts (`handle_alloc_error`) are NOT catchable — this only handles
/// regular `panic!()` calls, which includes "capacity overflow" in `raw_vec`.
fn dispatch_catching_panics(bytes: &[u8], name: &str, cfg: &ExtractorConfig) -> Vec<IndexLine> {
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        find_extract_dispatch::dispatch_from_bytes(bytes, name, cfg)
    })) {
        Ok(lines) => lines,
        Err(_) => {
            warn!("extractor panicked for '{}'; skipping content", name);
            vec![]
        }
    }
}

/// Extract an archive member from raw bytes.
///
/// Single-file compressed formats (.gz/.bz2/.xz) are decompressed inline and
/// dispatched via `find_extract_dispatch`.  All other non-archive formats are
/// dispatched directly.  Multi-file archives are NOT handled here — the
/// caller routes those through `handle_nested_archive` before reaching
/// this function.
fn extract_member_bytes(mut bytes: Vec<u8>, entry_name: &str, display_prefix: &str, cfg: &ExtractorConfig) -> Vec<IndexLine> {
    // Always index the filename so the member is discoverable by name.
    let mut lines = make_filename_line(entry_name);

    // Truncate bytes at the content limit (defensive — callers already use take()).
    let size_limit = cfg.max_content_kb * 1024;
    bytes.truncate(size_limit);

    // ── Single-file compressed (.gz / .bz2 / .xz) ────────────────────────────
    // Multi-file archive kinds (.zip, .tar, etc.) are intercepted by the caller;
    // only single-file compressed formats are handled here.
    if let Some(kind) = detect_kind_from_name(entry_name) {
        match kind {
            ArchiveKind::Gz | ArchiveKind::Bz2 | ArchiveKind::Xz => {
                // Decompress, capping output at size_limit to prevent RAM spikes
                // when a small compressed blob expands to a very large plaintext.
                let decompressed: Option<Vec<u8>> = match kind {
                    ArchiveKind::Gz => {
                        let mut out = Vec::new();
                        let _ = GzDecoder::new(Cursor::new(&bytes))
                            .take(size_limit as u64)
                            .read_to_end(&mut out);
                        if out.is_empty() { None } else { Some(out) }
                    }
                    ArchiveKind::Bz2 => {
                        let mut out = Vec::new();
                        let _ = BzDecoder::new(Cursor::new(&bytes))
                            .take(size_limit as u64)
                            .read_to_end(&mut out);
                        if out.is_empty() { None } else { Some(out) }
                    }
                    ArchiveKind::Xz => {
                        let mut out = Vec::new();
                        let _ = XzDecoder::new(Cursor::new(&bytes))
                            .take(size_limit as u64)
                            .read_to_end(&mut out);
                        if out.is_empty() { None } else { Some(out) }
                    }
                    _ => unreachable!(),
                };

                if let Some(inner_bytes) = decompressed {
                    // Dispatch decompressed bytes; use inner name (strip .gz/.bz2/.xz).
                    let inner_name = Path::new(entry_name)
                        .file_stem()
                        .and_then(|s| s.to_str())
                        .unwrap_or(entry_name);
                    let display_name = format!("{display_prefix}::{inner_name}");
                    let content_lines = dispatch_catching_panics(&inner_bytes, &display_name, cfg);
                    let with_path = content_lines.into_iter().map(|mut l| {
                        l.archive_path = Some(entry_name.to_string());
                        l
                    });
                    lines.extend(with_path);
                }
                return lines;
            }
            // Multi-file archive: caller should have routed this through
            // handle_nested_archive; return filename only as a fallback.
            _ => return lines,
        }
    }

    // ── All other formats: unified dispatch ───────────────────────────────────
    let display_name = format!("{display_prefix}::{entry_name}");
    let content_lines = dispatch_catching_panics(&bytes, &display_name, cfg);
    let with_path = content_lines.into_iter().map(|mut l| {
        l.archive_path = Some(entry_name.to_string());
        l
    });
    lines.extend(with_path);
    lines
}
