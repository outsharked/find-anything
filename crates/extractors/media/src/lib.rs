use std::fs::File;
use std::io::{BufReader, Read, Seek, SeekFrom};
use std::path::Path;

use find_extract_types::IndexLine;
use find_extract_types::ExtractorConfig;
use id3::TagLike;

/// Extract metadata from media files (images, audio, video).
///
/// Supports:
/// - Images: EXIF metadata (JPEG, TIFF, HEIC, RAW formats)
/// - Audio: ID3/Vorbis/M4A tags (MP3, FLAC, M4A, AAC)
/// - Video: Format, resolution, duration (MP4, MKV, WebM, etc.)
///
/// # Arguments
/// * `path` - Path to the media file
/// * `_max_size_kb` - Maximum file size in KB (currently unused)
///
/// # Returns
/// Vector of IndexLine objects with metadata at line_number=0
pub fn extract(path: &Path, _cfg: &ExtractorConfig) -> anyhow::Result<Vec<IndexLine>> {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();

    // Dispatch to appropriate extractor based on extension
    if is_image_ext(&ext) {
        extract_image(path)
    } else if is_audio_ext(&ext) {
        extract_audio(path)
    } else if is_video_ext(&ext) {
        extract_video(path)
    } else {
        Ok(vec![])
    }
}

/// Extract metadata from media bytes.
///
/// Writes bytes to a temp file with the correct extension and delegates to `extract`.
/// Used by `find-extract-dispatch` for archive members.
pub fn extract_from_bytes(bytes: &[u8], entry_name: &str, cfg: &ExtractorConfig) -> anyhow::Result<Vec<IndexLine>> {
    use std::io::Write;
    let ext = Path::new(entry_name)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("");
    let mut tmp = tempfile::Builder::new()
        .suffix(&format!(".{}", ext))
        .tempfile()?;
    tmp.write_all(bytes)?;
    tmp.flush()?;
    extract(tmp.path(), cfg)
}

/// Check if a file is a media file based on extension.
pub fn accepts(path: &Path) -> bool {
    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
        let ext = ext.to_lowercase();
        is_image_ext(&ext) || is_audio_ext(&ext) || is_video_ext(&ext)
    } else {
        false
    }
}

// ============================================================================
// IMAGE EXTRACTION
// ============================================================================

fn extract_image(path: &Path) -> anyhow::Result<Vec<IndexLine>> {
    let file = File::open(path)?;
    let mut bufreader = BufReader::new(file);

    let lines = match exif::Reader::new().read_from_container(&mut bufreader) {
        Ok(exif) => {
            let mut lines = Vec::new();
            for field in exif.fields() {
                let tag = field.tag.to_string();
                let value = field.display_value().to_string();
                if !value.is_empty() && !value.starts_with("[") {
                    lines.push(IndexLine {
                        archive_path: None,
                        line_number: 0,
                        content: format!("[EXIF:{}] {}", tag, value),
                    });
                }
            }
            lines
        }
        Err(_) => vec![],
    };

    if !lines.is_empty() {
        return Ok(lines);
    }

    // Fallback: read native image header for basic dimensions/color info.
    if let Some(basic) = extract_image_basic(path) {
        return Ok(basic);
    }

    Ok(vec![IndexLine {
        archive_path: None,
        line_number: 0,
        content: "[IMAGE] no metadata available".to_string(),
    }])
}

fn extract_image_basic(path: &Path) -> Option<Vec<IndexLine>> {
    let mut f = File::open(path).ok()?;
    let mut buf = [0u8; 34];
    let n = f.read(&mut buf).ok()?;
    if n < 2 {
        return None;
    }

    // JPEG: FF D8 — scan for SOF marker to get dimensions
    if buf[0] == 0xFF && buf[1] == 0xD8 {
        return extract_jpeg_basic(&mut f);
    }

    if n < 8 {
        return None;
    }

    // PNG: \x89PNG\r\n\x1a\n
    if buf.starts_with(b"\x89PNG\r\n\x1a\n") && n >= 26 {
        let width  = u32::from_be_bytes([buf[16], buf[17], buf[18], buf[19]]);
        let height = u32::from_be_bytes([buf[20], buf[21], buf[22], buf[23]]);
        let bit_depth  = buf[24];
        let color_type = buf[25];
        let color_name = match color_type {
            0 => "Grayscale",
            2 => "RGB",
            3 => "Indexed",
            4 => "Grayscale+Alpha",
            6 => "RGBA",
            _ => "Unknown",
        };
        return Some(vec![
            make_image_line("dimensions", &format!("{}x{}", width, height)),
            make_image_line("bit_depth",  &bit_depth.to_string()),
            make_image_line("color",      color_name),
        ]);
    }

    // GIF: GIF87a or GIF89a
    if (buf.starts_with(b"GIF87a") || buf.starts_with(b"GIF89a")) && n >= 10 {
        let width  = u16::from_le_bytes([buf[6], buf[7]]);
        let height = u16::from_le_bytes([buf[8], buf[9]]);
        return Some(vec![
            make_image_line("dimensions", &format!("{}x{}", width, height)),
        ]);
    }

    // WebP: RIFF....WEBP
    if buf.starts_with(b"RIFF") && n >= 12 && &buf[8..12] == b"WEBP" {
        if n >= 16 {
            let sub_chunk = &buf[12..16];
            if sub_chunk == b"VP8 " && n >= 30 {
                let frame_tag = (buf[20] as u32) | ((buf[21] as u32) << 8) | ((buf[22] as u32) << 16);
                if frame_tag & 1 == 0 && buf[23] == 0x9D && buf[24] == 0x01 && buf[25] == 0x2A {
                    let width  = u16::from_le_bytes([buf[26], buf[27]]) & 0x3FFF;
                    let height = u16::from_le_bytes([buf[28], buf[29]]) & 0x3FFF;
                    return Some(vec![
                        make_image_line("dimensions", &format!("{}x{}", width, height)),
                    ]);
                }
            } else if sub_chunk == b"VP8L" && n >= 25 && buf[20] == 0x2F {
                let packed = (buf[21] as u32) | ((buf[22] as u32) << 8) | ((buf[23] as u32) << 16) | ((buf[24] as u32) << 24);
                let width  = (packed & 0x3FFF) + 1;
                let height = ((packed >> 14) & 0x3FFF) + 1;
                return Some(vec![
                    make_image_line("dimensions", &format!("{}x{}", width, height)),
                ]);
            } else if sub_chunk == b"VP8X" && n >= 30 {
                let width  = (buf[24] as u32) | ((buf[25] as u32) << 8) | ((buf[26] as u32) << 16);
                let height = (buf[27] as u32) | ((buf[28] as u32) << 8) | ((buf[29] as u32) << 16);
                return Some(vec![
                    make_image_line("dimensions", &format!("{}x{}", width + 1, height + 1)),
                ]);
            }
        }
        return Some(vec![make_image_line("format", "WebP")]);
    }

    // BMP: BM
    if buf.starts_with(b"BM") && n >= 30 {
        let width     = i32::from_le_bytes([buf[18], buf[19], buf[20], buf[21]]).unsigned_abs();
        let height    = i32::from_le_bytes([buf[22], buf[23], buf[24], buf[25]]).unsigned_abs();
        let bit_count = u16::from_le_bytes([buf[28], buf[29]]);
        return Some(vec![
            make_image_line("dimensions", &format!("{}x{}", width, height)),
            make_image_line("bit_depth",  &bit_count.to_string()),
        ]);
    }

    None
}

/// Scan a JPEG file for the SOF (Start of Frame) marker and extract dimensions.
/// Reads up to 64 KB, which is more than enough to find any JPEG SOF header.
fn extract_jpeg_basic(f: &mut File) -> Option<Vec<IndexLine>> {
    f.seek(SeekFrom::Start(0)).ok()?;
    let mut data = vec![0u8; 65536];
    let n = f.read(&mut data).ok()?;
    data.truncate(n);

    if data.len() < 4 || data[0] != 0xFF || data[1] != 0xD8 {
        return None;
    }

    let mut i = 2;
    while i + 3 < data.len() {
        if data[i] != 0xFF {
            break;
        }
        let marker = data[i + 1];

        // SOF markers: C0–C3, C5–C7, C9–CB, CD–CF (excluding C4=DHT, C8=JPG, CC=DAC)
        if matches!(marker, 0xC0..=0xC3 | 0xC5..=0xC7 | 0xC9..=0xCB | 0xCD..=0xCF) {
            // SOF layout: FF Cx | length(2) | precision(1) | height(2) | width(2) | components(1)
            if i + 9 < data.len() {
                let precision  = data[i + 4];
                let height     = u16::from_be_bytes([data[i + 5], data[i + 6]]);
                let width      = u16::from_be_bytes([data[i + 7], data[i + 8]]);
                let components = data[i + 9];
                let color = match components {
                    1 => "Grayscale",
                    3 => "YCbCr",
                    4 => "CMYK",
                    _ => "Unknown",
                };
                return Some(vec![
                    make_image_line("dimensions", &format!("{}x{}", width, height)),
                    make_image_line("bit_depth",  &precision.to_string()),
                    make_image_line("color",      color),
                ]);
            }
        }

        // Standalone markers have no length field
        if matches!(marker, 0xD8 | 0xD9 | 0x01) || marker < 0x02 {
            i += 2;
        } else {
            if i + 3 >= data.len() { break; }
            let length = u16::from_be_bytes([data[i + 2], data[i + 3]]) as usize;
            if length < 2 { break; }
            i += 2 + length;
        }
    }

    None
}

fn make_image_line(key: &str, value: &str) -> IndexLine {
    IndexLine {
        archive_path: None,
        line_number: 0,
        content: format!("[IMAGE:{}] {}", key, value),
    }
}

pub fn is_image_ext(ext: &str) -> bool {
    matches!(
        ext,
        "jpg" | "jpeg" | "tiff" | "tif" | "heic" | "heif" | "webp"
        | "png" | "gif" | "bmp" | "cr2" | "cr3" | "nef" | "arw" | "orf" | "rw2"
    )
}

// ============================================================================
// AUDIO EXTRACTION
// ============================================================================

fn extract_audio(path: &Path) -> anyhow::Result<Vec<IndexLine>> {
    let ext = path.extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();

    match ext.as_str() {
        "mp3" => extract_mp3_tags(path),
        "flac" => extract_flac_tags(path),
        "m4a" | "aac" => extract_mp4_tags(path),
        _ => Ok(vec![]),  // Unsupported format
    }
}

fn extract_mp3_tags(path: &Path) -> anyhow::Result<Vec<IndexLine>> {
    match id3::Tag::read_from_path(path) {
        Ok(tag) => {
            let mut lines = Vec::new();

            if let Some(title) = tag.title() {
                lines.push(make_tag_line("title", title));
            }
            if let Some(artist) = tag.artist() {
                lines.push(make_tag_line("artist", artist));
            }
            if let Some(album) = tag.album() {
                lines.push(make_tag_line("album", album));
            }
            if let Some(year) = tag.year() {
                lines.push(make_tag_line("year", &year.to_string()));
            }
            if let Some(genre) = tag.genre() {
                lines.push(make_tag_line("genre", genre));
            }
            for comment in tag.comments() {
                let text = &comment.text;
                if !text.is_empty() {
                    lines.push(make_tag_line("comment", text));
                }
            }

            Ok(lines)
        }
        Err(_) => {
            // File may not have ID3 tags or tags may be unreadable
            Ok(vec![])
        }
    }
}

fn extract_flac_tags(path: &Path) -> anyhow::Result<Vec<IndexLine>> {
    match metaflac::Tag::read_from_path(path) {
        Ok(tag) => {
            let mut lines = Vec::new();
            let vorbis = tag.vorbis_comments();

            if let Some(vorbis) = vorbis {
                for (key, values) in vorbis.comments.iter() {
                    for value in values {
                        if !value.is_empty() {
                            lines.push(make_tag_line(key, value));
                        }
                    }
                }
            }

            Ok(lines)
        }
        Err(_) => {
            // File may not have FLAC tags or tags may be unreadable
            Ok(vec![])
        }
    }
}

fn extract_mp4_tags(path: &Path) -> anyhow::Result<Vec<IndexLine>> {
    match mp4ameta::Tag::read_from_path(path) {
        Ok(tag) => {
            let mut lines = Vec::new();

            if let Some(title) = tag.title() {
                lines.push(make_tag_line("title", title));
            }
            if let Some(artist) = tag.artist() {
                lines.push(make_tag_line("artist", artist));
            }
            if let Some(album) = tag.album() {
                lines.push(make_tag_line("album", album));
            }
            if let Some(year) = tag.year() {
                lines.push(make_tag_line("year", year));
            }
            if let Some(genre) = tag.genre() {
                lines.push(make_tag_line("genre", genre));
            }
            if let Some(comment) = tag.comment() {
                if !comment.is_empty() {
                    lines.push(make_tag_line("comment", comment));
                }
            }

            Ok(lines)
        }
        Err(_) => {
            // File may not have MP4 tags or tags may be unreadable
            Ok(vec![])
        }
    }
}

fn make_tag_line(key: &str, value: &str) -> IndexLine {
    IndexLine {
        archive_path: None,
        line_number: 0,
        content: format!("[TAG:{}] {}", key, value),
    }
}

pub fn is_audio_ext(ext: &str) -> bool {
    matches!(
        ext,
        "mp3" | "flac" | "ogg" | "m4a" | "aac" | "opus" | "wav"
    )
}

// ============================================================================
// VIDEO EXTRACTION
// ============================================================================

// ── nom-exif MediaParser (reused across calls via thread-local) ───────────────

thread_local! {
    static MEDIA_PARSER: std::cell::RefCell<nom_exif::MediaParser> =
        std::cell::RefCell::new(nom_exif::MediaParser::new());
}

fn extract_video(path: &Path) -> anyhow::Result<Vec<IndexLine>> {
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("").to_lowercase();

    match ext.as_str() {
        // nom-exif handles ISOBMFF and Matroska natively, with seek-based I/O.
        "mp4" | "m4v" | "mov" | "3gp" | "mkv" | "webm" | "mka" => {
            extract_video_nom_exif(path, &ext)
        }
        // Other formats: detect container from magic bytes, emit format line only.
        _ => extract_video_header_only(path),
    }
}

/// Parse video metadata using nom-exif (seek-based, no full-file read).
fn extract_video_nom_exif(path: &Path, ext: &str) -> anyhow::Result<Vec<IndexLine>> {
    use nom_exif::{MediaSource, TrackInfo, TrackInfoTag};

    let ms = match MediaSource::file_path(path) {
        Ok(ms) => ms,
        Err(_) => return Ok(vec![make_meta_line("format", ext)]),
    };

    if !ms.has_track() {
        return Ok(vec![make_meta_line("format", ext)]);
    }

    let info: Option<TrackInfo> = MEDIA_PARSER.with(|p| {
        p.borrow_mut().parse(ms).ok()
    });

    let Some(info) = info else {
        return Ok(vec![make_meta_line("format", ext)]);
    };

    let mut lines = vec![make_meta_line("format", ext)];

    if let (Some(w), Some(h)) = (
        info.get(TrackInfoTag::ImageWidth).and_then(|v| v.as_u32()),
        info.get(TrackInfoTag::ImageHeight).and_then(|v| v.as_u32()),
    ) {
        lines.push(make_meta_line("resolution", &format!("{}x{}", w, h)));
    }

    if let Some(ms) = info.get(TrackInfoTag::DurationMs).and_then(|v| v.as_u64()) {
        let total_secs = ms / 1000;
        let mins = total_secs / 60;
        let secs = total_secs % 60;
        lines.push(make_meta_line("duration", &format!("{}:{:02}", mins, secs)));
    }

    Ok(lines)
}

/// For formats nom-exif doesn't support (AVI, WMV, FLV, etc.): detect the
/// container from magic bytes and emit a format line so the file is at least
/// findable by container type.
fn extract_video_header_only(path: &Path) -> anyhow::Result<Vec<IndexLine>> {
    let mut f = match File::open(path) {
        Ok(f) => f,
        Err(_) => return Ok(vec![]),
    };
    let mut buf = [0u8; 16];
    let n = f.read(&mut buf).unwrap_or(0);
    if n < 4 {
        return Ok(vec![]);
    }

    // AVI: RIFF....AVI
    if &buf[..4] == b"RIFF" && n >= 12 && &buf[8..12] == b"AVI " {
        return Ok(vec![make_meta_line("format", "avi")]);
    }
    // ASF / WMV / WMA: ASF Header Object GUID
    if n >= 16 && buf == [0x30, 0x26, 0xB2, 0x75, 0x8E, 0x66, 0xCF, 0x11,
                          0xA6, 0xD9, 0x00, 0xAA, 0x00, 0x62, 0xCE, 0x6C] {
        return Ok(vec![make_meta_line("format", "wmv")]);
    }
    // FLV
    if n >= 3 && &buf[..3] == b"FLV" {
        return Ok(vec![make_meta_line("format", "flv")]);
    }
    // MPEG-PS pack header or video sequence header
    if &buf[..4] == b"\x00\x00\x01\xBA" || &buf[..4] == b"\x00\x00\x01\xB3" {
        return Ok(vec![make_meta_line("format", "mpeg")]);
    }
    // OGG (covers OGV)
    if &buf[..4] == b"OggS" {
        return Ok(vec![make_meta_line("format", "ogv")]);
    }

    Ok(vec![])
}

fn make_meta_line(key: &str, value: &str) -> IndexLine {
    IndexLine {
        archive_path: None,
        line_number: 0,
        content: format!("[VIDEO:{}] {}", key, value),
    }
}

pub fn is_video_ext(ext: &str) -> bool {
    matches!(
        ext,
        "mp4" | "m4v" | "mkv" | "webm" | "ogv" | "ogg" | "avi" | "mov" | "wmv" | "flv" | "mpg" | "mpeg" | "3gp"
    )
}
