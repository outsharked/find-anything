# 003 - Smart Context Retrieval

## Overview

Implement context-aware retrieval that adapts to different file types. The current line-number-based windowing doesn't work well for metadata (images/audio) or PDFs where semantic units differ from lines.

## Problem Statement

Current `get_context()` uses line number windows (`BETWEEN lo AND hi`) which:
- ❌ Returns incomplete context for metadata files (all at line_number = 0)
- ❌ May span pages in PDFs (not semantically meaningful)
- ❌ Doesn't respect natural boundaries (paragraphs, metadata groups)

## Design Decisions

### Strategy: File-Kind Aware Context

Different file types need different context strategies:

| File Kind | Context Strategy | Rationale |
|-----------|------------------|-----------|
| **text** | Line window (current) | Lines are semantic units |
| **archive** | Line window (current) | Archive text entries use lines |
| **image** | All metadata | All EXIF tags are one logical unit |
| **audio** | All metadata | All tags are one logical unit |
| **pdf** | Text extract with char limit | Paragraphs are semantic units, not lines |

### PDF Context: Character-Based Extraction

For PDFs, instead of fixed line counts, use **character limits** to extract natural text boundaries:

**Parameters:**
- `window_chars` - characters to include before/after match (default: 500)
- Algorithm:
  1. Find the matched line
  2. Collect lines before match until reaching `window_chars` characters
  3. Collect lines after match until reaching `window_chars` characters
  4. Return the combined text extract

**Benefits:**
- Respects paragraph boundaries
- Consistent context size regardless of line length
- Better reading experience (shows complete thoughts)

**Example:**
```
Match: "API authentication" on line 42

Traditional (window=5):
  Line 37: ...
  Line 38: ...
  Line 39: ...
  Line 40: ...
  Line 41: ...
  Line 42: The API authentication system uses JWT tokens.  ← MATCH
  Line 43: ...
  Line 44: ...
  Line 45: ...
  Line 46: ...
  Line 47: ...

Character-based (500 chars before/after):
  ...existing session. The system provides role-based access control
  for all endpoints. Each user is assigned a role upon registration.

  The API authentication system uses JWT tokens.  ← MATCH

  Tokens expire after 24 hours and must be refreshed. The refresh
  endpoint is /api/v1/auth/refresh and requires a valid...
```

### Metadata Context: All Tags

For images and audio, return **all metadata** regardless of which tag matched:

**Rationale:**
- Metadata is small (typically <50 tags)
- All tags provide valuable context (camera settings, GPS, dates)
- Users want to see full metadata when examining a result

**Example:**
```
Search: "Canon"
Match: [EXIF:Make] Canon

Context returned (all EXIF tags):
  [EXIF:Make] Canon
  [EXIF:Model] EOS R5
  [EXIF:DateTimeOriginal] 2024:01:15 14:30:22
  [EXIF:GPSLatitude] 37.774929
  [EXIF:GPSLongitude] -122.419416
  [EXIF:ISO] 800
  [EXIF:FNumber] f/2.8
  [EXIF:ExposureTime] 1/250
  [EXIF:ImageDescription] Golden Gate Bridge at sunset
```

## Implementation

### Phase 1: Refactor get_context

Split `get_context()` into file-kind-specific functions:

```rust
pub fn get_context(
    conn: &Connection,
    file_path: &str,
    archive_path: Option<&str>,
    center: usize,
    window: usize,
) -> Result<Vec<ContextLine>> {
    // Determine file kind
    let kind = get_file_kind(conn, file_path)?;

    match kind.as_str() {
        "image" | "audio" => get_metadata_context(conn, file_path),
        "pdf" => get_pdf_context(conn, file_path, archive_path, center, window),
        _ => get_line_context(conn, file_path, archive_path, center, window),
    }
}
```

### Phase 2: Implement get_metadata_context

Return all lines with `line_number = 0`:

```rust
fn get_metadata_context(
    conn: &Connection,
    file_path: &str,
) -> Result<Vec<ContextLine>> {
    let mut stmt = conn.prepare(
        "SELECT l.line_number, l.content
         FROM lines l
         JOIN files f ON f.id = l.file_id
         WHERE f.path = ?1
           AND l.line_number = 0
         ORDER BY l.content",
    )?;

    stmt.query_map([file_path], |row| {
        Ok(ContextLine {
            line_number: row.get::<_, i64>(0)? as usize,
            content: row.get(1)?,
        })
    })?
    .collect()
}
```

### Phase 3: Implement get_pdf_context

Character-based extraction with configurable limit:

```rust
fn get_pdf_context(
    conn: &Connection,
    file_path: &str,
    archive_path: Option<&str>,
    center: usize,
    window: usize,
) -> Result<Vec<ContextLine>> {
    // Convert line window to character budget
    // Assume average line is ~80 chars
    let window_chars = window * 80;

    // Get lines before the match
    let before = get_lines_before_with_limit(
        conn, file_path, archive_path, center, window_chars
    )?;

    // Get the matched line
    let matched = get_line_exact(conn, file_path, archive_path, center)?;

    // Get lines after the match
    let after = get_lines_after_with_limit(
        conn, file_path, archive_path, center, window_chars
    )?;

    // Combine: before + matched + after
    let mut result = Vec::new();
    result.extend(before);
    result.extend(matched);
    result.extend(after);

    Ok(result)
}

fn get_lines_before_with_limit(
    conn: &Connection,
    file_path: &str,
    archive_path: Option<&str>,
    center: usize,
    max_chars: usize,
) -> Result<Vec<ContextLine>> {
    let mut stmt = conn.prepare(
        "SELECT l.line_number, l.content
         FROM lines l
         JOIN files f ON f.id = l.file_id
         WHERE f.path = ?1
           AND ((?2 IS NULL AND l.archive_path IS NULL)
                OR l.archive_path = ?2)
           AND l.line_number < ?3
         ORDER BY l.line_number DESC",  -- Reverse order to collect upward
    )?;

    let mut lines = Vec::new();
    let mut char_count = 0;

    let rows = stmt.query_map(
        params![file_path, archive_path, center as i64],
        |row| {
            Ok(ContextLine {
                line_number: row.get::<_, i64>(0)? as usize,
                content: row.get(1)?,
            })
        },
    )?;

    for row in rows {
        let line = row?;
        char_count += line.content.len();

        if char_count > max_chars && !lines.is_empty() {
            break;
        }

        lines.push(line);
    }

    // Reverse back to natural order
    lines.reverse();
    Ok(lines)
}

// Similar implementation for get_lines_after_with_limit
```

### Phase 4: Rename get_context to get_line_context

The current implementation becomes the fallback for text/archive files:

```rust
fn get_line_context(
    conn: &Connection,
    file_path: &str,
    archive_path: Option<&str>,
    center: usize,
    window: usize,
) -> Result<Vec<ContextLine>> {
    // Existing implementation unchanged
    let lo = center.saturating_sub(window) as i64;
    let hi = (center + window) as i64;
    // ... rest of current get_context code
}
```

## Files Changed

- `crates/server/src/db.rs` - add file-kind-aware context functions
- (Optional) `crates/common/src/config.rs` - add `pdf_context_chars` setting

## Testing Strategy

### Text Files
1. Verify existing behavior unchanged
2. Window=5 still returns 11 lines (5 before, match, 5 after)

### PDF Files
1. Create test PDF with known content
2. Search for middle-line match
3. Verify context includes ~500 chars before/after
4. Verify doesn't include excessive text
5. Test page boundaries (if using archive_path)

### Image Files
1. Search for EXIF tag (e.g., "Canon")
2. Verify ALL EXIF tags returned in context
3. Verify no duplicate metadata

### Audio Files
1. Search for audio tag (e.g., "Queen")
2. Verify ALL tags returned
3. Test MP3, FLAC, M4A

## Configuration

Add optional PDF-specific setting:

```toml
[search]
pdf_context_chars = 500  # characters to show before/after PDF matches
```

## Breaking Changes

None. Context API remains unchanged:
- Same endpoint: `GET /api/v1/context`
- Same parameters: `source, path, line, window`
- Enhanced behavior based on file type

## Performance Considerations

### Metadata Context
- ✅ Fast - typically <50 lines per file
- ✅ Single query

### PDF Context
- ⚠️ Slightly slower - two queries (before + after)
- ✅ Mitigated by character limit (bounded iteration)
- ✅ Still O(1) relative to file size

## Future Enhancements

1. **Smart paragraph detection** - break at sentence/paragraph boundaries
2. **Syntax-aware context** - for code files, include full function/class
3. **Configurable strategies** - let users choose context style per source
4. **Preview truncation** - add "..." to indicate truncated context
