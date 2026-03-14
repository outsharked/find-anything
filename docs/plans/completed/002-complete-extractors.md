# 002 - Complete Content Extractors

## Overview

Implement the remaining content extractors that are currently stubs:
1. **PDF text extraction** - Extract text content from PDF files
2. **Image EXIF metadata** - Extract metadata from images (JPEG, PNG, TIFF, etc.)
3. **Audio metadata** - Extract ID3 tags and metadata from audio files (MP3, FLAC, M4A, etc.)

These extractors will index structured metadata as pseudo-lines (line_number = 0) and PDF text as actual lines, making them searchable through the existing FTS5 system.

## Current State

All three extractors exist as stubs that return empty results:
- `crates/common/src/extract/pdf.rs` - returns `Ok(vec![])`
- `crates/common/src/extract/image.rs` - returns `Ok(vec![])`
- `crates/common/src/extract/audio.rs` - returns `Ok(vec![])`

## Design Decisions

### PDF Extraction

**Crate choice:** `pdf-extract` (pure Rust, no external dependencies)
- Pro: No system dependencies required
- Pro: Pure Rust, cross-platform
- Con: May not handle all PDF types (scanned PDFs without text)

**Line storage:**
- Store each line of extracted text with sequential line numbers
- Use `archive_path` field to store page numbers: `"page:1"`, `"page:2"`, etc.
- This allows context retrieval per page

**Example indexed data:**
```
archive_path: "page:1", line_number: 1, content: "Chapter 1: Introduction"
archive_path: "page:1", line_number: 2, content: "This document describes..."
archive_path: "page:2", line_number: 1, content: "Section 2.1"
```

### Image EXIF Metadata

**Crate choice:** `kamadak-exif` (pure Rust, well-maintained)
- Supports JPEG, TIFF, HEIF, PNG (via tEXt chunks)

**Metadata format:**
- Each EXIF tag becomes one pseudo-line
- Format: `[EXIF:TagName] value`
- All metadata uses `line_number = 0` (no meaningful line concept)

**Example indexed data:**
```
line_number: 0, content: "[EXIF:Make] Canon"
line_number: 0, content: "[EXIF:Model] EOS R5"
line_number: 0, content: "[EXIF:DateTimeOriginal] 2024:01:15 14:30:22"
line_number: 0, content: "[EXIF:GPSLatitude] 37.774929"
line_number: 0, content: "[EXIF:GPSLongitude] -122.419416"
line_number: 0, content: "[EXIF:ImageDescription] Sunset over Golden Gate Bridge"
```

**Searchable fields:**
- Camera make/model
- Date/time taken
- GPS coordinates
- Image descriptions, keywords
- Software used
- ISO, aperture, shutter speed, focal length

### Audio Metadata

**Crate choices:**
- `id3` - MP3 ID3v1 and ID3v2 tags
- `metaflac` - FLAC Vorbis comments
- `mp4ameta` - M4A/MP4/AAC metadata

**Metadata format:**
- Each tag becomes one pseudo-line
- Format: `[TAG:name] value`
- All metadata uses `line_number = 0`

**Example indexed data:**
```
line_number: 0, content: "[TAG:title] Bohemian Rhapsody"
line_number: 0, content: "[TAG:artist] Queen"
line_number: 0, content: "[TAG:album] A Night at the Opera"
line_number: 0, content: "[TAG:year] 1975"
line_number: 0, content: "[TAG:genre] Rock"
line_number: 0, content: "[TAG:comment] Remastered 2011"
```

**Supported formats:**
- MP3 (via id3)
- FLAC (via metaflac)
- M4A, MP4, AAC (via mp4ameta)
- OGG, Opus (future: lewton or similar)

## Implementation Plan

### Phase 1: Add Dependencies

Update `crates/common/Cargo.toml`:
```toml
[dependencies]
# Existing dependencies...
pdf-extract = "0.7"
kamadak-exif = "0.5"
id3 = "1"
metaflac = "0.2"
mp4ameta = "0.11"
```

### Phase 2: PDF Extractor

**Files to modify:**
1. `crates/common/src/extract/pdf.rs`
   - Implement extraction using `pdf-extract`
   - Extract text per page
   - Store page number in `archive_path` field
   - Handle extraction errors gracefully

### Phase 3: Image Extractor

**Files to modify:**
2. `crates/common/src/extract/image.rs`
   - Implement EXIF reading using `kamadak-exif`
   - Format tags as `[EXIF:TagName] value`
   - Handle common tags (Make, Model, DateTime, GPS, etc.)
   - Skip binary/unknown tags

### Phase 4: Audio Extractor

**Files to modify:**
3. `crates/common/src/extract/audio.rs`
   - Detect format by extension
   - Use appropriate library (id3/metaflac/mp4ameta)
   - Format tags as `[TAG:name] value`
   - Handle common tags across all formats

## Files Changed

- `crates/common/Cargo.toml` - add dependencies
- `crates/common/src/extract/pdf.rs` - implement PDF extraction
- `crates/common/src/extract/image.rs` - implement EXIF extraction
- `crates/common/src/extract/audio.rs` - implement audio metadata extraction
- `examples/client.toml` - potentially update max_file_size_kb recommendation

## Testing Strategy

### PDF Testing
1. Create test PDF with known text content
2. Extract and verify text is indexed
3. Test multi-page PDFs
4. Test search for PDF content
5. Test context retrieval by page

### Image Testing
1. Test JPEG with EXIF data
2. Test image without EXIF (should not error)
3. Search for camera model
4. Search for GPS coordinates
5. Search for image descriptions

### Audio Testing
1. Test MP3 with ID3v2 tags
2. Test FLAC with Vorbis comments
3. Test M4A with MP4 metadata
4. Search for artist, album, title
5. Verify tags are searchable

### Error Handling
1. Corrupted PDF - should skip gracefully
2. Binary file misidentified as image - should skip
3. Audio file without metadata - should not error
4. Large files beyond max_file_size_kb - already handled

## Breaking Changes

None. This is purely additive functionality:
- Existing file types continue to work
- New file types now get indexed instead of skipped
- No API or config changes required

## Performance Considerations

1. **PDF extraction can be slow** - large PDFs may take seconds
   - Respect max_file_size_kb limit
   - Consider logging slow extractions

2. **EXIF reading is fast** - typically milliseconds per image

3. **Audio metadata is fast** - no need to read full audio data

## Future Enhancements

1. **OCR for scanned PDFs**
   - Detect PDFs with no text
   - Use `tesseract-rs` for OCR
   - Opt-in via config (expensive operation)

2. **Additional image formats**
   - RAW camera formats (CR2, NEF, ARW)
   - HEIF/HEIC support

3. **Additional audio formats**
   - OGG Vorbis
   - Opus
   - WAV (minimal metadata but possible)

4. **Video metadata**
   - MP4, MKV, AVI metadata
   - Using `ffmpeg` or similar

## Example Search Queries

After implementation, users can search for:

**PDFs:**
- `find "chapter introduction"` - finds text in PDFs
- `find "copyright 2024"` - finds copyright notices

**Images:**
- `find "Canon EOS"` - finds images from Canon cameras
- `find "37.7749"` - finds images near coordinates
- `find "vacation photos"` - finds images with that description

**Audio:**
- `find "Bohemian Rhapsody"` - finds songs by title
- `find "Queen"` - finds songs by artist
- `find "1975"` - finds songs from that year
