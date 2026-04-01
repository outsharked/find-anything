use std::fs::File;
use std::io::{Cursor, Read};
use std::path::Path;

use anyhow::{Context, Result};
use bzip2::read::BzDecoder;
use flate2::read::GzDecoder;
use globset::GlobSet;
use tracing::warn;
use xz2::read::XzDecoder;

use find_extract_types::{IndexLine, build_globset, ExternalDispatchMode, ExternalMemberDispatch, ExtractorConfig};

mod iwork;
pub use iwork::is_iwork_ext;

/// One batch of lines for a single archive member, with its content hash.
#[derive(Default, serde::Serialize, serde::Deserialize)]
pub struct MemberBatch {
    pub lines: Vec<IndexLine>,
    /// blake3 hex hash of the member's raw bytes (decompressed from the archive).
    /// None for filename-only entries (too large, nested archives, or single-compressed).
    pub file_hash: Option<String>,
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
    /// Uncompressed byte size of this archive member, as declared in the archive header.
    /// None for entries where the size is not available (e.g. corrupt/streaming entries).
    #[serde(default)]
    pub size: Option<u64>,
    /// If set, the member's raw bytes were written to this temp file path instead of being
    /// extracted inline.  The caller (scan.rs) is responsible for uploading the file to the
    /// server and deleting it afterwards.  Only set for extensions listed in
    /// `ExtractorConfig::server_only_exts`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delegate_temp_path: Option<String>,
    /// Lines to be attached to the outer archive file's own content entry rather than to
    /// this specific member.  Used by iWork extraction to store IWA text content alongside
    /// the outer .pages/.numbers/.key file without requiring a separate extractor binary.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub outer_lines: Vec<IndexLine>,
}

// Internal callback alias for brevity.
pub(crate) type CB<'a> = &'a mut dyn FnMut(MemberBatch);

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
    let ext = Path::new(name).extension().and_then(|e| e.to_str()).unwrap_or("");
    if is_iwork_ext(ext) {
        return iwork::iwork_streaming(path, cfg, callback);
    }
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
        | "pages" | "numbers" | "key"
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

        // Uncompressed size from the central directory; available before reading.
        let member_size = Some(entry.size());

        // Multi-file nested archive: recurse without writing to disk where possible.
        if let Some(kind) = detect_kind_from_name(&name) {
            if is_multifile_archive(&kind) {
                handle_nested_archive(&mut entry as &mut dyn Read, &name, &kind, member_size, cfg, callback);
                continue;
            }
        }

        // server_only delegation: read full bytes and forward to scan.rs for upload.
        let ext_lc = Path::new(&name).extension().and_then(|e| e.to_str()).unwrap_or("").to_lowercase();
        if cfg.server_only_exts.iter().any(|s| s == &ext_lc) {
            let delegation_limit = (cfg.max_temp_file_mb * 1024 * 1024) as u64;
            let mut full_bytes = Vec::new();
            let _ = (&mut entry as &mut dyn Read).take(delegation_limit).read_to_end(&mut full_bytes);
            let file_hash = find_extract_types::content_hash(&full_bytes);
            let mut lines = make_filename_line(&name);
            if is_iwork_ext(&ext_lc) {
                iwork::iwork_extract_preview_into_lines(&full_bytes, &name, &mut lines);
            }
            let delegate_temp_path = write_delegate_temp_file(&full_bytes, &name)
                .map_err(|e| warn!("server_only: temp write failed for {name} in {display_prefix}: {e:#}"))
                .ok()
                .map(|p| p.to_string_lossy().into_owned());
            callback(MemberBatch { lines, file_hash, skip_reason: None, mtime, size: member_size, delegate_temp_path, outer_lines: vec![] });
            continue;
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
        let file_hash = find_extract_types::content_hash(&bytes);
        callback(MemberBatch { lines: extract_member_bytes(bytes, &name, display_prefix, cfg), file_hash, skip_reason, mtime, size: member_size, delegate_temp_path: None, outer_lines: vec![] });
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
        let member_size = entry.header().size().ok();

        // Multi-file nested archive: recurse without writing to disk where possible.
        if let Some(kind) = detect_kind_from_name(&name) {
            if is_multifile_archive(&kind) {
                handle_nested_archive(&mut entry as &mut dyn Read, &name, &kind, member_size, cfg, callback);
                continue;
            }
        }

        // server_only delegation: read full bytes and forward to scan.rs for upload.
        // The tar crate drains remaining entry bytes on Entry::drop() so no explicit
        // drain is needed after a partial read.
        let ext_lc = Path::new(&name).extension().and_then(|e| e.to_str()).unwrap_or("").to_lowercase();
        if cfg.server_only_exts.iter().any(|s| s == &ext_lc) {
            let delegation_limit = (cfg.max_temp_file_mb * 1024 * 1024) as u64;
            let mut full_bytes = Vec::new();
            let _ = (&mut entry as &mut dyn Read).take(delegation_limit).read_to_end(&mut full_bytes);
            let file_hash = find_extract_types::content_hash(&full_bytes);
            let mut lines = make_filename_line(&name);
            if is_iwork_ext(&ext_lc) {
                iwork::iwork_extract_preview_into_lines(&full_bytes, &name, &mut lines);
            }
            let delegate_temp_path = write_delegate_temp_file(&full_bytes, &name)
                .map_err(|e| warn!("server_only: temp write failed for {name} in {display_prefix}: {e:#}"))
                .ok()
                .map(|p| p.to_string_lossy().into_owned());
            callback(MemberBatch { lines, file_hash, skip_reason: None, mtime, size: member_size, delegate_temp_path, outer_lines: vec![] });
            continue;
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
        let file_hash = find_extract_types::content_hash(&bytes);
        callback(MemberBatch { lines: extract_member_bytes(bytes, &name, display_prefix, cfg), file_hash, skip_reason, mtime, size: member_size, delegate_temp_path: None, outer_lines: vec![] });
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
            handle_nested_archive(reader, &name, &kind, Some(entry.size()), cfg, callback);
            return Ok(true);
        }
    }

    // Compute mtime before reading (uses entry metadata, not stream data).
    let mtime = if entry.has_last_modified_date {
        std::time::SystemTime::from(entry.last_modified_date)
            .duration_since(std::time::UNIX_EPOCH)
            .ok()
            .map(|d| d.as_secs() as i64)
            .and_then(sanitize_archive_mtime)
    } else {
        None
    };

    // server_only delegation: read full bytes (up to max_temp_file_mb), drain the
    // rest to keep the solid-block stream in sync, write to temp file for upload.
    let ext_lc = Path::new(&name).extension().and_then(|e| e.to_str()).unwrap_or("").to_lowercase();
    if cfg.server_only_exts.iter().any(|s| s == &ext_lc) {
        let delegation_limit = (cfg.max_temp_file_mb * 1024 * 1024) as u64;
        let mut full_bytes = Vec::new();
        let _ = (reader as &mut dyn Read).take(delegation_limit).read_to_end(&mut full_bytes);
        let _ = std::io::copy(reader, &mut std::io::sink());
        let file_hash = find_extract_types::content_hash(&full_bytes);
        let lines = make_filename_line(&name);
        let delegate_temp_path = write_delegate_temp_file(&full_bytes, &name)
            .map_err(|e| warn!("server_only: temp write failed for {name} in {display_prefix}: {e:#}"))
            .ok()
            .map(|p| p.to_string_lossy().into_owned());
        callback(MemberBatch { lines, file_hash, skip_reason: None, mtime, size: Some(entry.size()), delegate_temp_path, outer_lines: vec![] });
        return Ok(true);
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

    let file_hash = find_extract_types::content_hash(&bytes);
    callback(MemberBatch { lines: extract_member_bytes(bytes, &name, display_prefix, cfg), file_hash, skip_reason, mtime, size: Some(entry.size()), delegate_temp_path: None, outer_lines: vec![] });
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
            file_hash: None,
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
                            file_hash: None,
                            size: Some(entry.size()),
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
        // Helper: collect non-directory (name, size) pairs in this block.
        let block_files = || -> Vec<(&str, u64)> {
            archive
                .stream_map
                .file_block_index
                .iter()
                .enumerate()
                .filter(|(_, b)| b.is_some_and(|bi| bi == block_index))
                .filter_map(|(fi, _)| {
                    let e = &archive.files[fi];
                    if e.is_directory() { None } else { Some((e.name(), e.size())) }
                })
                .collect()
        };

        if let Some(avail) = available_memory_bytes() {
            let budget = avail * 3 / 4;
            if unpack_size > budget {
                let file_infos = block_files();
                warn!(
                    "7z: '{}': block {} needs ~{} MB but only ~{} MB available \
                     (75% budget ~{} MB); {} file(s) indexed by filename only",
                    path.display(), block_index,
                    unpack_size / (1024 * 1024),
                    avail / (1024 * 1024),
                    budget / (1024 * 1024),
                    file_infos.len(),
                );
                let skip_reason = Some(format!(
                    "insufficient memory to extract \
                     (~{} MB needed, ~{} MB available)",
                    unpack_size / (1024 * 1024),
                    avail / (1024 * 1024),
                ));
                for (name, entry_size) in file_infos {
                    callback(MemberBatch {
                        lines: make_filename_line(name),
                        file_hash: None,
                        skip_reason: skip_reason.clone(),
                        size: Some(entry_size),
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

    let decompressed_size = bytes.len() as u64;
    let file_hash = find_extract_types::content_hash(&bytes);
    Ok(MemberBatch {
        lines: extract_member_bytes(bytes, &inner_name, path.to_str().unwrap_or(""), cfg),
        file_hash,
        skip_reason: None,
        mtime: None, // single-file wrapper: caller uses outer archive's filesystem mtime
        size: Some(decompressed_size),
        delegate_temp_path: None,
        outer_lines: vec![],
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
    outer_size: Option<u64>,
    cfg: &ExtractorConfig,
    callback: CB<'_>,
) {
    // Always emit the filename of the nested archive itself, with its size.
    callback(MemberBatch { lines: make_filename_line(outer_name), file_hash: None, skip_reason: None, mtime: None, size: outer_size, delegate_temp_path: None, outer_lines: vec![] });

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
        callback(MemberBatch { lines: p, file_hash: inner_batch.file_hash, skip_reason: inner_batch.skip_reason, mtime: inner_batch.mtime, size: inner_batch.size, delegate_temp_path: inner_batch.delegate_temp_path, outer_lines: vec![] });
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
    let span = tracing::debug_span!("member", path = name);
    let _guard = span.enter();
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
/// Run an external extractor on `bytes`, returning the extracted lines.
///
/// Used by `extract_member_bytes` when a member's extension matches an entry
/// in `cfg.external_dispatch` — ensuring that the same extractor is used
/// whether a file is found at the top level or nested inside an archive.
fn run_external_member_dispatch(
    bytes: &[u8],
    entry_name: &str,
    cfg: &ExtractorConfig,
    spec: &ExternalMemberDispatch,
) -> Vec<IndexLine> {
    use std::io::Write as _;

    // Write bytes to a temp file with the member's original extension.
    let ext = Path::new(entry_name)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("bin");
    let mut tmp = match tempfile::Builder::new().suffix(&format!(".{ext}")).tempfile() {
        Ok(f) => f,
        Err(e) => {
            warn!("external dispatch: failed to create temp file for '{}': {e}", entry_name);
            return vec![];
        }
    };
    if let Err(e) = tmp.write_all(bytes) {
        warn!("external dispatch: failed to write temp file for '{}': {e}", entry_name);
        return vec![];
    }

    // Substitute {file} and {dir} placeholders.
    let substitute = |args: &[String], file: &Path, dir: Option<&Path>| -> Vec<std::ffi::OsString> {
        args.iter().map(|a| {
            let s = a
                .replace("{file}", &file.to_string_lossy())
                .replace("{dir}", &dir.map(|d| d.to_string_lossy().into_owned()).unwrap_or_default());
            s.into()
        }).collect()
    };

    match spec.mode {
        ExternalDispatchMode::TempDir => {
            let out_dir = match tempfile::TempDir::new() {
                Ok(d) => d,
                Err(e) => {
                    warn!("external dispatch: failed to create output dir for '{}': {e}", entry_name);
                    return vec![];
                }
            };
            let args = substitute(&spec.args, tmp.path(), Some(out_dir.path()));
            let status = std::process::Command::new(&spec.bin).args(&args).status();
            match status {
                Ok(s) if !s.success() => {
                    warn!("external dispatch: '{}' exited {:?} for '{}'", spec.bin, s.code(), entry_name);
                    return vec![];
                }
                Err(e) => {
                    warn!("external dispatch: failed to run '{}' for '{}': {e}", spec.bin, entry_name);
                    return vec![];
                }
                Ok(_) => {}
            }
            // Walk output dir: dispatch each extracted file, prefixing archive_path with entry_name.
            let mut lines = make_filename_line(entry_name);
            for file_entry in walkdir::WalkDir::new(out_dir.path())
                .into_iter()
                .filter_map(|e| e.ok())
                .filter(|e| e.file_type().is_file())
            {
                let member_full = file_entry.path();
                let member_rel = match member_full.strip_prefix(out_dir.path()) {
                    Ok(r) => r.to_string_lossy().replace('\\', "/"),
                    Err(_) => continue,
                };
                let member_bytes = match std::fs::read(member_full) {
                    Ok(b) => b,
                    Err(e) => {
                        warn!("external dispatch: failed to read '{}': {e}", member_full.display());
                        continue;
                    }
                };
                let member_name = member_full
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .into_owned();
                let mut content = dispatch_catching_panics(&member_bytes, &member_name, cfg);
                for l in &mut content {
                    // Members of the externally-extracted file get a composite archive_path.
                    let inner = l.archive_path.as_deref().unwrap_or("");
                    l.archive_path = Some(if inner.is_empty() {
                        format!("{}::{}", entry_name, member_rel)
                    } else {
                        format!("{}::{}::{}", entry_name, member_rel, inner)
                    });
                }
                // Add a filename marker for the extracted member.
                lines.push(IndexLine {
                    archive_path: Some(format!("{}::{}", entry_name, member_rel)),
                    line_number: 0,
                    content: format!("[PATH] {}::{}", entry_name, member_rel),
                });
                lines.extend(content);
            }
            lines
        }
        ExternalDispatchMode::Stdout => {
            let args = substitute(&spec.args, tmp.path(), None);
            let out = match std::process::Command::new(&spec.bin).args(&args).output() {
                Ok(o) => o,
                Err(e) => {
                    warn!("external dispatch: failed to run '{}' for '{}': {e}", spec.bin, entry_name);
                    return make_filename_line(entry_name);
                }
            };
            if !out.status.success() {
                warn!("external dispatch: '{}' exited {:?} for '{}'", spec.bin, out.status.code(), entry_name);
                return make_filename_line(entry_name);
            }
            match serde_json::from_slice::<Vec<IndexLine>>(&out.stdout) {
                Ok(mut parsed) => {
                    for l in &mut parsed {
                        if l.archive_path.is_none() {
                            l.archive_path = Some(entry_name.to_string());
                        }
                    }
                    let mut lines = make_filename_line(entry_name);
                    lines.extend(parsed);
                    lines
                }
                Err(e) => {
                    warn!("external dispatch: failed to parse output from '{}' for '{}': {e}", spec.bin, entry_name);
                    make_filename_line(entry_name)
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write as _;
    use tempfile::NamedTempFile;

    // ── accepts / is_archive_ext ─────────────────────────────────────────────

    #[test]
    fn accepts_known_extensions() {
        for ext in &["zip", "tar", "gz", "bz2", "xz", "tgz", "tbz2", "txz", "7z"] {
            let name = format!("archive.{ext}");
            let p = std::path::Path::new(&name);
            assert!(accepts(p), "expected accepts() for .{ext}");
        }
    }

    #[test]
    fn accepts_rejects_non_archive() {
        for ext in &["txt", "pdf", "rs", "docx", "mp3", "exe"] {
            let name = format!("file.{ext}");
            let p = std::path::Path::new(&name);
            assert!(!accepts(p), "expected !accepts() for .{ext}");
        }
    }

    #[test]
    fn accepts_no_extension() {
        assert!(!accepts(std::path::Path::new("noextfile")));
    }

    #[test]
    fn is_archive_ext_case_insensitive() {
        assert!(is_archive_ext("ZIP"));
        assert!(is_archive_ext("Zip"));
        assert!(is_archive_ext("TAR"));
        assert!(is_archive_ext("GZ"));
        assert!(!is_archive_ext("TXT"));
    }

    // ── detect_kind_from_name ───────────────────────────────────────────────

    #[test]
    fn detect_kind_compound_extensions() {
        assert_eq!(detect_kind_from_name("foo.tar.gz"),  Some(ArchiveKind::TarGz));
        assert_eq!(detect_kind_from_name("foo.tgz"),     Some(ArchiveKind::TarGz));
        assert_eq!(detect_kind_from_name("foo.tar.bz2"), Some(ArchiveKind::TarBz2));
        assert_eq!(detect_kind_from_name("foo.tbz2"),    Some(ArchiveKind::TarBz2));
        assert_eq!(detect_kind_from_name("foo.tar.xz"),  Some(ArchiveKind::TarXz));
        assert_eq!(detect_kind_from_name("foo.txz"),     Some(ArchiveKind::TarXz));
        assert_eq!(detect_kind_from_name("foo.tar"),     Some(ArchiveKind::Tar));
        assert_eq!(detect_kind_from_name("foo.zip"),     Some(ArchiveKind::Zip));
        assert_eq!(detect_kind_from_name("foo.gz"),      Some(ArchiveKind::Gz));
        assert_eq!(detect_kind_from_name("foo.bz2"),     Some(ArchiveKind::Bz2));
        assert_eq!(detect_kind_from_name("foo.xz"),      Some(ArchiveKind::Xz));
        assert_eq!(detect_kind_from_name("foo.7z"),      Some(ArchiveKind::SevenZip));
        assert_eq!(detect_kind_from_name("foo.txt"),     None);
    }

    #[test]
    fn detect_kind_case_insensitive() {
        assert_eq!(detect_kind_from_name("FOO.ZIP"), Some(ArchiveKind::Zip));
        assert_eq!(detect_kind_from_name("FOO.TAR.GZ"), Some(ArchiveKind::TarGz));
    }

    // ── has_hidden_component ────────────────────────────────────────────────

    #[test]
    fn hidden_component_detects_dot_prefix() {
        assert!(has_hidden_component(".hidden/file.txt"));
        assert!(has_hidden_component("dir/.git/config"));
        assert!(has_hidden_component(".terraform/lock.hcl"));
    }

    #[test]
    fn hidden_component_allows_visible_paths() {
        assert!(!has_hidden_component("src/main.rs"));
        assert!(!has_hidden_component("docs/README.md"));
        assert!(!has_hidden_component("a/b/c.txt"));
    }

    #[test]
    fn hidden_component_single_dot_and_double_dot_are_not_hidden() {
        assert!(!has_hidden_component("./file.txt"));
        assert!(!has_hidden_component("../sibling/file.txt"));
    }

    // ── sanitize_archive_mtime ──────────────────────────────────────────────

    #[test]
    fn sanitize_past_timestamp_accepted_as_is() {
        // 2020-01-01 UTC is clearly in the past.
        let ts: i64 = 1_577_836_800;
        assert_eq!(sanitize_archive_mtime(ts), Some(ts));
    }

    #[test]
    fn sanitize_future_within_2099_subtracts_100_years() {
        // 2077-01-01 UTC — plausible Y2K artifact (meant 1977).
        let ts: i64 = 3_376_684_800;
        let result = sanitize_archive_mtime(ts).expect("should return Some");
        assert!(result < ts, "100-year subtraction expected");
        // Should land around 1977 (roughly ts - 3_155_760_000).
        assert!(result > 0, "result must be positive");
    }

    #[test]
    fn sanitize_after_2099_returns_none() {
        // 2150-01-01 UTC is after UNIX_END_OF_2099.
        let ts: i64 = 5_680_281_600;
        assert_eq!(sanitize_archive_mtime(ts), None);
    }

    // ── zip_unix_mtime ──────────────────────────────────────────────────────

    fn make_unix_extra(mtime: i32) -> Vec<u8> {
        // tag=0x5455, size=5, flags=0x01 (mtime present), mtime as LE i32
        let mut v = vec![0x55, 0x54, 0x05, 0x00, 0x01];
        v.extend_from_slice(&mtime.to_le_bytes());
        v
    }

    #[test]
    fn zip_unix_mtime_parses_valid_block() {
        let extra = make_unix_extra(1_700_000_000_i32);
        assert_eq!(zip_unix_mtime(&extra), Some(1_700_000_000));
    }

    #[test]
    fn zip_unix_mtime_returns_none_for_empty_extra() {
        assert_eq!(zip_unix_mtime(&[]), None);
    }

    #[test]
    fn zip_unix_mtime_returns_none_wrong_tag() {
        // tag=0x000A (NTFS), size=5, flags=0x01
        let extra = vec![0x0A, 0x00, 0x05, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00];
        assert_eq!(zip_unix_mtime(&extra), None);
    }

    #[test]
    fn zip_unix_mtime_returns_none_when_mtime_flag_not_set() {
        // tag=0x5455, size=5, flags=0x00 (no mtime bit)
        let extra = vec![0x55, 0x54, 0x05, 0x00, 0x00, 0x01, 0x02, 0x03, 0x04];
        assert_eq!(zip_unix_mtime(&extra), None);
    }

    #[test]
    fn zip_unix_mtime_returns_none_for_truncated_block() {
        // Only the tag + partial size — not enough data
        let extra = vec![0x55, 0x54, 0x05];
        assert_eq!(zip_unix_mtime(&extra), None);
    }

    // ── zip_dos_to_unix ─────────────────────────────────────────────────────

    #[test]
    fn zip_dos_to_unix_returns_none_for_epoch_default() {
        // year <= 1980 is the DOS epoch default → None
        let dt = zip::DateTime::from_date_and_time(1980, 1, 1, 0, 0, 0).unwrap();
        assert_eq!(zip_dos_to_unix(dt), None);
    }

    #[test]
    fn zip_dos_to_unix_converts_known_date() {
        // 2023-01-15 12:30:00 UTC — round-trip via known seconds calculation.
        let dt = zip::DateTime::from_date_and_time(2023, 1, 15, 12, 30, 0).unwrap();
        let result = zip_dos_to_unix(dt).expect("should convert");
        // 2023-01-15 12:30:00 UTC = 1673785800
        assert_eq!(result, 1_673_785_800);
    }

    // ── single_compressed (bare .gz / .bz2 / .xz via extract_streaming) ────

    fn make_gz_file(content: &[u8]) -> NamedTempFile {
        let mut tmp = NamedTempFile::with_suffix(".gz").unwrap();
        let mut enc = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        enc.write_all(content).unwrap();
        let gz = enc.finish().unwrap();
        tmp.write_all(&gz).unwrap();
        tmp
    }

    fn default_cfg() -> ExtractorConfig {
        ExtractorConfig {
            max_content_kb: 1024,
            max_depth: 10,
            max_line_length: 512,
            ..Default::default()
        }
    }

    #[test]
    fn single_gz_extracts_text_content() {
        let content = b"hello from gz\nsecond line\n";
        let tmp = make_gz_file(content);
        let cfg = default_cfg();
        let mut batches = vec![];
        extract_streaming(tmp.path(), &cfg, &mut |b| batches.push(b)).unwrap();
        assert_eq!(batches.len(), 1);
        let batch = &batches[0];
        assert!(batch.size.unwrap() > 0);
        // Should have a filename line (line_number=0) plus content lines.
        assert!(batch.lines.iter().any(|l| l.line_number == 0));
        assert!(batch.lines.iter().any(|l| l.content.contains("hello from gz")));
    }

    #[test]
    fn single_gz_empty_content() {
        let tmp = make_gz_file(b"");
        let cfg = default_cfg();
        let mut batches = vec![];
        extract_streaming(tmp.path(), &cfg, &mut |b| batches.push(b)).unwrap();
        // One batch with size = 0; no content lines beyond the filename.
        assert_eq!(batches.len(), 1);
        assert_eq!(batches[0].size, Some(0));
    }

    // ── empty ZIP ───────────────────────────────────────────────────────────

    #[test]
    fn empty_zip_produces_no_member_batches() {
        use std::io::Cursor;
        let mut buf = Vec::new();
        {
            let writer = zip::ZipWriter::new(Cursor::new(&mut buf));
            writer.finish().unwrap();
        }
        let mut tmp = NamedTempFile::with_suffix(".zip").unwrap();
        tmp.write_all(&buf).unwrap();

        let cfg = default_cfg();
        let mut batches = vec![];
        extract_streaming(tmp.path(), &cfg, &mut |b| batches.push(b)).unwrap();
        assert!(batches.is_empty(), "empty ZIP should yield no batches");
    }

    // ── hidden member filtering ─────────────────────────────────────────────

    #[test]
    fn zip_hidden_members_excluded_by_default() {
        use std::io::Cursor;
        let mut buf = Vec::new();
        {
            let mut zip = zip::ZipWriter::new(Cursor::new(&mut buf));
            let opts = zip::write::SimpleFileOptions::default();
            zip.start_file(".hidden/secret.txt", opts).unwrap();
            zip.write_all(b"should not appear\n").unwrap();
            zip.start_file("visible.txt", opts).unwrap();
            zip.write_all(b"should appear\n").unwrap();
            zip.finish().unwrap();
        }
        let mut tmp = NamedTempFile::with_suffix(".zip").unwrap();
        tmp.write_all(&buf).unwrap();

        let cfg = ExtractorConfig {
            include_hidden: false,
            ..default_cfg()
        };
        let mut batches = vec![];
        extract_streaming(tmp.path(), &cfg, &mut |b| batches.push(b)).unwrap();

        let all_names: Vec<_> = batches.iter()
            .flat_map(|b| &b.lines)
            .filter_map(|l| l.archive_path.as_deref())
            .collect();
        assert!(!all_names.iter().any(|n| n.contains(".hidden")), "hidden member leaked: {all_names:?}");
        assert!(all_names.iter().any(|n| n.contains("visible.txt")), "visible member missing: {all_names:?}");
    }

    #[test]
    fn zip_hidden_members_included_when_flag_set() {
        use std::io::Cursor;
        let mut buf = Vec::new();
        {
            let mut zip = zip::ZipWriter::new(Cursor::new(&mut buf));
            let opts = zip::write::SimpleFileOptions::default();
            zip.start_file(".hidden/secret.txt", opts).unwrap();
            zip.write_all(b"present\n").unwrap();
            zip.finish().unwrap();
        }
        let mut tmp = NamedTempFile::with_suffix(".zip").unwrap();
        tmp.write_all(&buf).unwrap();

        let cfg = ExtractorConfig {
            include_hidden: true,
            ..default_cfg()
        };
        let mut batches = vec![];
        extract_streaming(tmp.path(), &cfg, &mut |b| batches.push(b)).unwrap();

        let all_names: Vec<_> = batches.iter()
            .flat_map(|b| &b.lines)
            .filter_map(|l| l.archive_path.as_deref())
            .collect();
        assert!(all_names.iter().any(|n| n.contains(".hidden")), "hidden member should be included: {all_names:?}");
    }

    // ── corrupt ZIP → graceful error ────────────────────────────────────────

    #[test]
    fn corrupt_zip_returns_error() {
        let mut tmp = NamedTempFile::with_suffix(".zip").unwrap();
        tmp.write_all(b"this is not a zip file at all").unwrap();
        let cfg = default_cfg();
        let result = extract(tmp.path(), &cfg);
        assert!(result.is_err(), "corrupt zip should return Err");
    }

    // ── ZIP text member content extraction ──────────────────────────────────

    #[test]
    fn zip_text_member_content_indexed() {
        use std::io::Cursor;
        let mut buf = Vec::new();
        {
            let mut zip = zip::ZipWriter::new(Cursor::new(&mut buf));
            let opts = zip::write::SimpleFileOptions::default();
            zip.start_file("readme.txt", opts).unwrap();
            zip.write_all(b"hello_unique_word_xyz\nline two here\n").unwrap();
            zip.finish().unwrap();
        }
        let mut tmp = NamedTempFile::with_suffix(".zip").unwrap();
        tmp.write_all(&buf).unwrap();

        let lines = extract(tmp.path(), &default_cfg()).unwrap();
        assert!(
            lines.iter().any(|l| l.content.contains("hello_unique_word_xyz")),
            "text content not indexed: {:?}", lines.iter().map(|l| &l.content).collect::<Vec<_>>()
        );
    }
}

/// Write `bytes` to a uniquely-named temp file for server-side delegation.
///
/// Creates `<tempdir>/fa-member-XXXXXXXX/<leaf_filename>` and returns the path.
/// The directory is NOT auto-deleted on drop — the caller (scan.rs) is responsible
/// for removing it after uploading.
fn write_delegate_temp_file(bytes: &[u8], entry_name: &str) -> Result<std::path::PathBuf> {
    let leaf = Path::new(entry_name)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("member");
    let dir = tempfile::Builder::new()
        .prefix("fa-member-")
        .tempdir()
        .context("creating delegate temp dir")?;
    let path = dir.path().join(leaf);
    std::fs::write(&path, bytes).context("writing delegate temp file")?;
    // Prevent auto-deletion: scan.rs cleans up after upload.
    let _ = dir.keep();
    Ok(path)
}

pub(crate) fn extract_member_bytes(mut bytes: Vec<u8>, entry_name: &str, display_prefix: &str, cfg: &ExtractorConfig) -> Vec<IndexLine> {
    // Check external_dispatch first: if this extension has a registered external
    // extractor, delegate to it and return its output.  This ensures consistent
    // behaviour regardless of whether the file is found at top level or nested
    // inside an archive.
    let member_ext = Path::new(entry_name)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();
    if let Some(spec) = cfg.external_dispatch.get(&member_ext) {
        return run_external_member_dispatch(&bytes, entry_name, cfg, spec);
    }

    // Apple iWork members nested inside another archive: extract only preview.jpg.
    // Move bytes into a Cursor since we return early regardless of success.
    if is_iwork_ext(&member_ext) {
        let mut lines = make_filename_line(entry_name);
        iwork::iwork_extract_preview_into_lines(&bytes, entry_name, &mut lines);
        return lines;
    }

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
