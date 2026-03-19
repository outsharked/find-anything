use std::fs::File;
use std::io::{BufReader, Read, Seek, SeekFrom};
use std::path::Path;

use find_extract_types::{IndexLine, LINE_METADATA};
use find_extract_types::ExtractorConfig;
use tracing::warn;

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
        extract_audio(path, &path.to_string_lossy())
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
    // Pass entry_name (not the temp path) so probe-failure warnings include the
    // original member name rather than an opaque temp-file path.
    if is_audio_ext(ext) {
        return extract_audio(tmp.path(), entry_name);
    }
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

    let parts: Vec<String> = match exif::Reader::new().read_from_container(&mut bufreader) {
        Ok(exif) => exif.fields()
            .filter_map(|field| {
                let tag = field.tag.to_string();
                let value = field.display_value().to_string();
                if !value.is_empty() && !value.starts_with('[') {
                    Some(format!("[EXIF:{}] {}", tag, value))
                } else {
                    None
                }
            })
            .collect(),
        Err(_) => vec![],
    };

    if !parts.is_empty() {
        return Ok(vec![IndexLine {
            archive_path: None,
            line_number: LINE_METADATA,
            content: parts.join(" "),
        }]);
    }

    // Fallback: read native image header for basic dimensions/color info.
    if let Some(parts) = extract_image_basic_parts(path) {
        return Ok(vec![IndexLine {
            archive_path: None,
            line_number: LINE_METADATA,
            content: parts.join(" "),
        }]);
    }

    Ok(vec![IndexLine {
        archive_path: None,
        line_number: LINE_METADATA,
        content: "[IMAGE] no metadata available".to_string(),
    }])
}

fn extract_image_basic_parts(path: &Path) -> Option<Vec<String>> {
    let mut f = File::open(path).ok()?;
    let mut buf = [0u8; 34];
    let n = f.read(&mut buf).ok()?;
    if n < 2 {
        return None;
    }

    // JPEG: FF D8 — scan for SOF marker to get dimensions
    if buf[0] == 0xFF && buf[1] == 0xD8 {
        return extract_jpeg_basic_parts(&mut f);
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
            image_part("dimensions", &format!("{}x{}", width, height)),
            image_part("bit_depth",  &bit_depth.to_string()),
            image_part("color",      color_name),
        ]);
    }

    // GIF: GIF87a or GIF89a
    if (buf.starts_with(b"GIF87a") || buf.starts_with(b"GIF89a")) && n >= 10 {
        let width  = u16::from_le_bytes([buf[6], buf[7]]);
        let height = u16::from_le_bytes([buf[8], buf[9]]);
        return Some(vec![
            image_part("dimensions", &format!("{}x{}", width, height)),
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
                        image_part("dimensions", &format!("{}x{}", width, height)),
                    ]);
                }
            } else if sub_chunk == b"VP8L" && n >= 25 && buf[20] == 0x2F {
                let packed = (buf[21] as u32) | ((buf[22] as u32) << 8) | ((buf[23] as u32) << 16) | ((buf[24] as u32) << 24);
                let width  = (packed & 0x3FFF) + 1;
                let height = ((packed >> 14) & 0x3FFF) + 1;
                return Some(vec![
                    image_part("dimensions", &format!("{}x{}", width, height)),
                ]);
            } else if sub_chunk == b"VP8X" && n >= 30 {
                let width  = (buf[24] as u32) | ((buf[25] as u32) << 8) | ((buf[26] as u32) << 16);
                let height = (buf[27] as u32) | ((buf[28] as u32) << 8) | ((buf[29] as u32) << 16);
                return Some(vec![
                    image_part("dimensions", &format!("{}x{}", width + 1, height + 1)),
                ]);
            }
        }
        return Some(vec![image_part("format", "WebP")]);
    }

    // BMP: BM
    if buf.starts_with(b"BM") && n >= 30 {
        let width     = i32::from_le_bytes([buf[18], buf[19], buf[20], buf[21]]).unsigned_abs();
        let height    = i32::from_le_bytes([buf[22], buf[23], buf[24], buf[25]]).unsigned_abs();
        let bit_count = u16::from_le_bytes([buf[28], buf[29]]);
        return Some(vec![
            image_part("dimensions", &format!("{}x{}", width, height)),
            image_part("bit_depth",  &bit_count.to_string()),
        ]);
    }

    None
}

fn image_part(key: &str, value: &str) -> String {
    format!("[IMAGE:{}] {}", key, value)
}

/// Scan a JPEG file for the SOF (Start of Frame) marker and extract dimensions.
/// Reads up to 64 KB, which is more than enough to find any JPEG SOF header.
fn extract_jpeg_basic_parts(f: &mut File) -> Option<Vec<String>> {
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
                    image_part("dimensions", &format!("{}x{}", width, height)),
                    image_part("bit_depth",  &precision.to_string()),
                    image_part("color",      color),
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

fn extract_audio(path: &Path, label: &str) -> anyhow::Result<Vec<IndexLine>> {
    use symphonia::core::codecs::CODEC_TYPE_NULL;
    use symphonia::core::formats::FormatOptions;
    use symphonia::core::io::MediaSourceStream;
    use symphonia::core::meta::MetadataOptions;
    use symphonia::core::probe::Hint;

    let file = match File::open(path) {
        Ok(f) => f,
        Err(_) => return Ok(vec![]),
    };
    let mss = MediaSourceStream::new(Box::new(file), Default::default());

    let mut hint = Hint::new();
    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
        hint.with_extension(ext);
    }

    let probed = match symphonia::default::get_probe()
        .format(&hint, mss, &FormatOptions::default(), &MetadataOptions::default())
    {
        Ok(p) => p,
        Err(e) => {
            warn!("audio probe failed for '{}': {e}", label);
            return Ok(vec![]);
        }
    };

    let mut format = probed.format;
    let mut probed_meta = probed.metadata;
    let mut parts: Vec<String> = Vec::new();

    // ── Tags ──────────────────────────────────────────────────────────────────
    // Pre-container metadata (e.g. ID3v2 prepended to MP3) lives in probed_meta;
    // container-native metadata (Vorbis comments in FLAC/OGG, MP4 atoms) lives
    // in format.metadata(). Check both and merge.
    if let Some(meta) = probed_meta.get() {
        if let Some(rev) = meta.current() {
            collect_audio_tags(rev.tags(), &mut parts);
        }
    }
    {
        let meta = format.metadata();
        if let Some(rev) = meta.current() {
            collect_audio_tags(rev.tags(), &mut parts);
        }
    }

    // ── Technical metadata from the first real audio track ────────────────────
    if let Some(track) = format.tracks().iter().find(|t| t.codec_params.codec != CODEC_TYPE_NULL) {
        let params = &track.codec_params;

        let codec = audio_codec_name(params.codec);
        if !codec.is_empty() {
            parts.push(audio_part("codec", codec));
        }

        if let Some(sr) = params.sample_rate {
            parts.push(audio_part("sample_rate", &format!("{sr} Hz")));
        }

        if let Some(ch) = params.channels {
            let label = match ch.count() {
                1 => "1 (mono)".to_string(),
                2 => "2 (stereo)".to_string(),
                n => n.to_string(),
            };
            parts.push(audio_part("channels", &label));
        }

        if let Some(bps) = params.bits_per_sample {
            parts.push(audio_part("bit_depth", &format!("{bps} bit")));
        }

        if let (Some(n_frames), Some(tb)) = (params.n_frames, params.time_base) {
            let secs = (n_frames * tb.numer as u64) / tb.denom as u64;
            if secs > 0 {
                parts.push(audio_part("duration", &format!("{}:{:02}", secs / 60, secs % 60)));
            }
        }
    }

    if parts.is_empty() {
        return Ok(vec![]);
    }

    Ok(vec![IndexLine {
        archive_path: None,
        line_number: LINE_METADATA,
        content: parts.join(" "),
    }])
}

fn collect_audio_tags(tags: &[symphonia::core::meta::Tag], parts: &mut Vec<String>) {
    use symphonia::core::meta::{StandardTagKey, Value};
    for tag in tags {
        let key = if let Some(std_key) = tag.std_key {
            match std_key {
                StandardTagKey::TrackTitle  => "title",
                StandardTagKey::Artist      => "artist",
                StandardTagKey::AlbumArtist => "album_artist",
                StandardTagKey::Album       => "album",
                StandardTagKey::Date        => "year",
                StandardTagKey::Genre       => "genre",
                StandardTagKey::Comment     => "comment",
                StandardTagKey::Composer    => "composer",
                StandardTagKey::TrackNumber => "track",
                StandardTagKey::DiscNumber  => "disc",
                _ => continue,
            }
        } else {
            continue;
        };
        let value = match &tag.value {
            Value::String(s)      => s.trim().to_string(),
            Value::UnsignedInt(n) => n.to_string(),
            Value::SignedInt(n)   => n.to_string(),
            Value::Float(f)       => format!("{f}"),
            Value::Boolean(b)     => b.to_string(),
            _                     => continue, // skip binary (album art) and flags
        };
        if !value.is_empty() {
            parts.push(tag_part(key, &value));
        }
    }
}

fn audio_codec_name(codec: symphonia::core::codecs::CodecType) -> &'static str {
    use symphonia::core::codecs::*;
    match codec {
        CODEC_TYPE_MP3       => "MP3",
        CODEC_TYPE_FLAC      => "FLAC",
        CODEC_TYPE_VORBIS    => "Vorbis",
        CODEC_TYPE_AAC       => "AAC",
        CODEC_TYPE_ALAC      => "ALAC",
        CODEC_TYPE_PCM_S8    => "PCM",
        CODEC_TYPE_PCM_U8    => "PCM",
        CODEC_TYPE_PCM_S16LE => "PCM",
        CODEC_TYPE_PCM_S16BE => "PCM",
        CODEC_TYPE_PCM_S24LE => "PCM",
        CODEC_TYPE_PCM_S24BE => "PCM",
        CODEC_TYPE_PCM_S32LE => "PCM",
        CODEC_TYPE_PCM_S32BE => "PCM",
        CODEC_TYPE_PCM_F32LE => "PCM",
        CODEC_TYPE_PCM_F32BE => "PCM",
        CODEC_TYPE_PCM_F64LE => "PCM",
        CODEC_TYPE_PCM_F64BE => "PCM",
        _                    => "",
    }
}

fn tag_part(key: &str, value: &str) -> String {
    format!("[TAG:{}] {}", key, value)
}

fn audio_part(key: &str, value: &str) -> String {
    format!("[AUDIO:{}] {}", key, value)
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
        Err(_) => return Ok(vec![make_meta_line(ext)]),
    };

    if !ms.has_track() {
        return Ok(vec![make_meta_line(ext)]);
    }

    let info: Option<TrackInfo> = MEDIA_PARSER.with(|p| {
        p.borrow_mut().parse(ms).ok()
    });

    let Some(info) = info else {
        return Ok(vec![make_meta_line(ext)]);
    };

    let mut parts = vec![video_part("format", ext)];

    if let (Some(w), Some(h)) = (
        info.get(TrackInfoTag::ImageWidth).and_then(|v| v.as_u32()),
        info.get(TrackInfoTag::ImageHeight).and_then(|v| v.as_u32()),
    ) {
        parts.push(video_part("resolution", &format!("{}x{}", w, h)));
    }

    if let Some(ms) = info.get(TrackInfoTag::DurationMs).and_then(|v| v.as_u64()) {
        let total_secs = ms / 1000;
        let mins = total_secs / 60;
        let secs = total_secs % 60;
        parts.push(video_part("duration", &format!("{}:{:02}", mins, secs)));
    }

    Ok(vec![IndexLine {
        archive_path: None,
        line_number: LINE_METADATA,
        content: parts.join(" "),
    }])
}

fn video_part(key: &str, value: &str) -> String {
    format!("[VIDEO:{}] {}", key, value)
}

fn make_meta_line(ext: &str) -> IndexLine {
    IndexLine {
        archive_path: None,
        line_number: LINE_METADATA,
        content: video_part("format", ext),
    }
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
        return Ok(vec![make_meta_line("avi")]);
    }
    // ASF / WMV / WMA: ASF Header Object GUID
    if n >= 16 && buf == [0x30, 0x26, 0xB2, 0x75, 0x8E, 0x66, 0xCF, 0x11,
                          0xA6, 0xD9, 0x00, 0xAA, 0x00, 0x62, 0xCE, 0x6C] {
        return Ok(vec![make_meta_line("wmv")]);
    }
    // FLV
    if n >= 3 && &buf[..3] == b"FLV" {
        return Ok(vec![make_meta_line("flv")]);
    }
    // MPEG-PS pack header or video sequence header
    if &buf[..4] == b"\x00\x00\x01\xBA" || &buf[..4] == b"\x00\x00\x01\xB3" {
        return Ok(vec![make_meta_line("mpeg")]);
    }
    // OGG (covers OGV)
    if &buf[..4] == b"OggS" {
        return Ok(vec![make_meta_line("ogv")]);
    }

    Ok(vec![])
}

pub fn is_video_ext(ext: &str) -> bool {
    matches!(
        ext,
        "mp4" | "m4v" | "mkv" | "webm" | "ogv" | "ogg" | "avi" | "mov" | "wmv" | "flv" | "mpg" | "mpeg" | "3gp"
    )
}

// ============================================================================
// TESTS
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    // ── Embedded fixtures ─────────────────────────────────────────────────────

    /// Small MP3 with full ID3v2 tags (title, artist, album, year, genre,
    /// comment, composer, album_artist, track number).
    static MP3_ID3V2: &[u8] = include_bytes!("../testdata/id3v2.mp3");

    /// Small FLAC (100 samples of silence) with Vorbis comment tags:
    /// title, artist, album, year.  Generated with `flac` 1.4.3.
    static FLAC_TAGGED: &[u8] = include_bytes!("../testdata/tagged.flac");

    // ── Test helpers ──────────────────────────────────────────────────────────

    fn write_fixture(bytes: &[u8], suffix: &str) -> tempfile::NamedTempFile {
        let mut f = tempfile::Builder::new().suffix(suffix).tempfile().unwrap();
        f.write_all(bytes).unwrap();
        f.flush().unwrap();
        f
    }

    fn has_containing(lines: &[IndexLine], s: &str) -> bool {
        lines.iter().any(|l| l.content.contains(s))
    }

    /// Build a minimal PCM WAV in memory (100 samples of silence).
    fn minimal_wav(sample_rate: u32, channels: u16, bps: u16) -> Vec<u8> {
        let n_samples: u32 = 100;
        let data_size  = n_samples * (bps as u32 / 8) * channels as u32;
        let block_align = channels * (bps / 8);
        let byte_rate   = sample_rate * block_align as u32;
        let chunk_size  = 36 + data_size;

        let mut v = Vec::new();
        v.extend_from_slice(b"RIFF");
        v.extend_from_slice(&chunk_size.to_le_bytes());
        v.extend_from_slice(b"WAVE");
        v.extend_from_slice(b"fmt ");
        v.extend_from_slice(&16u32.to_le_bytes());
        v.extend_from_slice(&1u16.to_le_bytes());           // PCM format
        v.extend_from_slice(&channels.to_le_bytes());
        v.extend_from_slice(&sample_rate.to_le_bytes());
        v.extend_from_slice(&byte_rate.to_le_bytes());
        v.extend_from_slice(&block_align.to_le_bytes());
        v.extend_from_slice(&bps.to_le_bytes());
        v.extend_from_slice(b"data");
        v.extend_from_slice(&data_size.to_le_bytes());
        v.extend(std::iter::repeat(0u8).take(data_size as usize));
        v
    }

    // ── Audio extraction tests ────────────────────────────────────────────────

    #[test]
    fn wav_mono_16bit_44khz() {
        let f = write_fixture(&minimal_wav(44100, 1, 16), ".wav");
        let lines = extract_audio(f.path()).unwrap();
        assert_eq!(lines.len(), 1, "audio produces one metadata line");
        assert!(has_containing(&lines, "[AUDIO:codec] PCM"),        "lines: {lines:?}");
        assert!(has_containing(&lines, "[AUDIO:sample_rate] 44100 Hz"));
        assert!(has_containing(&lines, "[AUDIO:channels] 1 (mono)"));
        assert!(has_containing(&lines, "[AUDIO:bit_depth] 16 bit"));
    }

    #[test]
    fn wav_stereo_24bit_48khz() {
        let f = write_fixture(&minimal_wav(48000, 2, 24), ".wav");
        let lines = extract_audio(f.path()).unwrap();
        assert!(has_containing(&lines, "[AUDIO:sample_rate] 48000 Hz"), "lines: {lines:?}");
        assert!(has_containing(&lines, "[AUDIO:channels] 2 (stereo)"));
        assert!(has_containing(&lines, "[AUDIO:bit_depth] 24 bit"));
    }

    #[test]
    fn mp3_extracts_id3v2_tags_and_stream_info() {
        let f = write_fixture(MP3_ID3V2, ".mp3");
        let lines = extract_audio(f.path()).unwrap();
        assert_eq!(lines.len(), 1, "audio produces one metadata line");
        let content = &lines[0].content;
        // Tags
        assert!(content.contains("[TAG:title]"),       "content: {content}");
        assert!(content.contains("[TAG:artist]"));
        assert!(content.contains("[TAG:album]"));
        assert!(content.contains("[TAG:year] 2015"));
        assert!(content.contains("[TAG:track]"));
        assert!(content.contains("[TAG:comment]"));
        assert!(content.contains("[TAG:composer]"));
        assert!(content.contains("[TAG:album_artist]"));
        // Stream info
        assert!(content.contains("[AUDIO:codec] MP3"));
        assert!(content.contains("[AUDIO:sample_rate] 44100 Hz"));
        assert!(content.contains("[AUDIO:channels] 1 (mono)"));
    }

    #[test]
    fn flac_extracts_vorbis_comment_tags_and_stream_info() {
        let f = write_fixture(FLAC_TAGGED, ".flac");
        let lines = extract_audio(f.path()).unwrap();
        assert_eq!(lines.len(), 1, "audio produces one metadata line");
        let content = &lines[0].content;
        // Vorbis comment tags
        assert!(content.contains("[TAG:title] Test FLAC"),    "content: {content}");
        assert!(content.contains("[TAG:artist] FLAC Artist"));
        assert!(content.contains("[TAG:album] Test Album"));
        assert!(content.contains("[TAG:year] 2024"));
        // Stream info
        assert!(content.contains("[AUDIO:codec] FLAC"));
        assert!(content.contains("[AUDIO:sample_rate] 44100 Hz"));
        assert!(content.contains("[AUDIO:channels] 1 (mono)"));
        assert!(content.contains("[AUDIO:bit_depth] 16 bit"));
    }

    #[test]
    fn corrupt_audio_returns_empty_gracefully() {
        let f = write_fixture(b"this is not valid audio data at all", ".mp3");
        let lines = extract_audio(f.path()).unwrap();
        assert!(lines.is_empty(), "corrupt file should yield no lines, got: {lines:?}");
    }

    #[test]
    fn extract_dispatches_wav_by_extension() {
        let cfg = find_extract_types::ExtractorConfig::default();
        let f = write_fixture(&minimal_wav(44100, 1, 16), ".wav");
        let lines = extract(f.path(), &cfg).unwrap();
        assert!(!lines.is_empty(), "extract() should dispatch .wav to audio extractor");
    }

    #[test]
    fn extract_skips_unknown_extension() {
        let cfg = find_extract_types::ExtractorConfig::default();
        let f = write_fixture(b"irrelevant", ".xyz");
        let lines = extract(f.path(), &cfg).unwrap();
        assert!(lines.is_empty());
    }

    // ── Extension detection ───────────────────────────────────────────────────

    #[test]
    fn audio_ext_detection() {
        for ext in ["mp3", "flac", "ogg", "m4a", "aac", "opus", "wav"] {
            assert!(is_audio_ext(ext), "{ext} should be an audio ext");
        }
        assert!(!is_audio_ext("jpg"));
        assert!(!is_audio_ext("txt"));
        assert!(!is_audio_ext("mp4"));
    }
}
