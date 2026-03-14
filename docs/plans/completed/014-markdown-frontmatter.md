# Plan 014: Markdown Frontmatter Extraction

## Overview

Enhance the existing text extractor to parse and index YAML/TOML/JSON frontmatter from Markdown files. Frontmatter is metadata at the beginning of Markdown files, commonly used in static site generators (Jekyll, Hugo), documentation tools, and note-taking apps.

**Parent Plan:** [013-additional-content-types.md](013-additional-content-types.md)

## Example

Input file (`notes/meeting.md`):
```markdown
---
title: Team Meeting Notes
author: John Doe
tags: [planning, roadmap, q1-2024]
date: 2024-01-15
priority: high
---

# Meeting Notes

Discussion about Q1 roadmap...
```

Indexed as:
```
line_number=0: notes/meeting.md
line_number=0: [FRONTMATTER:title] Team Meeting Notes
line_number=0: [FRONTMATTER:author] John Doe
line_number=0: [FRONTMATTER:tags] planning, roadmap, q1-2024
line_number=0: [FRONTMATTER:date] 2024-01-15
line_number=0: [FRONTMATTER:priority] high
line_number=1: # Meeting Notes
line_number=2: (blank)
line_number=3: Discussion about Q1 roadmap...
```

## Design Decisions

### 1. Frontmatter Format Support

**Supported:**
- YAML (most common): `---\n...\n---`
- TOML: `+++\n...\n+++`
- JSON: `{\n...\n}` (if at file start)

**Detection:**
- File must start with frontmatter delimiter (after optional BOM/whitespace)
- Must have closing delimiter
- Must be valid YAML/TOML/JSON

### 2. Indexing Strategy

**Metadata as line_number=0:**
- All frontmatter fields indexed at `line_number=0`
- Prefix with `[FRONTMATTER:key]` to distinguish from path
- Values converted to strings, arrays joined with ", "

**Content indexing:**
- Content after frontmatter indexed normally (line-by-line)
- Line numbers adjusted to account for frontmatter removal
- Or: Keep original line numbers and just skip frontmatter lines?

**Decision:** Keep original line numbers for content. This means:
- If frontmatter is lines 1-5, content starts at line 6
- When displaying context, we show correct line numbers from original file
- No adjustment needed, simpler implementation

### 3. Library Choice

**Use `gray_matter` crate:**
- Supports YAML, TOML, JSON
- Simple API: `Matter::new().parse(content)`
- Returns `(data, content)` where data is parsed frontmatter
- Well-maintained, MIT license

**Alternative:** Manual parsing with `serde_yaml`
- More control but more code
- Would need to handle delimiter detection ourselves

**Decision:** Use `gray_matter` for simplicity and robustness.

### 4. File Type Detection

**Only apply to:**
- Files with `.md` or `.markdown` extension
- Already detected as `kind="text"` by existing logic

**Approach:**
- In `extract_lines()` function, check extension
- If `.md`, attempt frontmatter parsing
- If frontmatter found, extract it; otherwise proceed normally
- This keeps changes localized to text extraction

## Implementation

### Files to Modify

1. **`crates/common/Cargo.toml`**
   - Add `gray_matter = "0.2"` dependency

2. **`crates/common/src/extract/text.rs`**
   - Add frontmatter extraction logic
   - Keep existing line extraction for content

3. **`crates/common/src/extract/mod.rs`** (if needed)
   - Export any new helper functions

### Implementation Steps

#### Step 1: Add dependency
```toml
[dependencies]
gray_matter = "0.2"
```

#### Step 2: Enhance `extract_lines()`

Current signature:
```rust
pub fn extract_lines(path: &Path, content: &[u8]) -> io::Result<Vec<IndexLine>>
```

Add frontmatter parsing:
```rust
use gray_matter::Matter;
use std::path::Path;

pub fn extract_lines(path: &Path, content: &[u8]) -> io::Result<Vec<IndexLine>> {
    let mut lines = Vec::new();

    // Always index the filename
    lines.push(IndexLine::new(0, path.display().to_string()));

    // Check if this is a Markdown file
    let is_markdown = path.extension()
        .and_then(|e| e.to_str())
        .map(|e| e.eq_ignore_ascii_case("md") || e.eq_ignore_ascii_case("markdown"))
        .unwrap_or(false);

    // Convert content to string
    let text = String::from_utf8_lossy(content);

    // If Markdown, try to extract frontmatter
    let content_to_index = if is_markdown {
        if let Some(frontmatter_lines) = extract_frontmatter(&text) {
            // Add frontmatter fields as line_number=0
            lines.extend(frontmatter_lines);
        }
        // Return content after frontmatter for normal indexing
        text.as_ref()
    } else {
        text.as_ref()
    };

    // Index content lines normally
    for (idx, line) in content_to_index.lines().enumerate() {
        let trimmed = line.trim();
        if !trimmed.is_empty() {
            lines.push(IndexLine::new((idx + 1) as u32, trimmed.to_string()));
        }
    }

    Ok(lines)
}

fn extract_frontmatter(text: &str) -> Option<Vec<IndexLine>> {
    let matter = Matter::new();

    // Parse frontmatter
    let parsed = matter.parse(text);

    // If no frontmatter found, return None
    if parsed.data.is_none() {
        return None;
    }

    let mut lines = Vec::new();

    // Convert frontmatter to IndexLines
    if let Some(data) = parsed.data {
        for (key, value) in data.as_object()? {
            let value_str = match value {
                serde_json::Value::String(s) => s.clone(),
                serde_json::Value::Array(arr) => {
                    // Join array values with ", "
                    arr.iter()
                        .filter_map(|v| v.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                }
                serde_json::Value::Number(n) => n.to_string(),
                serde_json::Value::Bool(b) => b.to_string(),
                serde_json::Value::Null => continue, // Skip null values
                serde_json::Value::Object(_) => {
                    // Nested objects: serialize as JSON string
                    serde_json::to_string(value).ok()?
                }
            };

            let line = format!("[FRONTMATTER:{}] {}", key, value_str);
            lines.push(IndexLine::new(0, line));
        }
    }

    Some(lines)
}
```

Wait, I need to check the actual structure of `gray_matter` - let me revise this based on the actual API.

Actually, looking at gray_matter crate more carefully:
- It returns `ParsedEntity` with `data` (frontmatter) and `content` (body)
- Data is a `serde_yaml::Value` or similar

Let me refine the approach:

```rust
fn extract_frontmatter(text: &str) -> Option<(Vec<IndexLine>, String)> {
    use gray_matter::{Matter, engine::YAML};

    let matter = Matter::<YAML>::new();
    let parsed = matter.parse(text);

    // Check if frontmatter exists
    if parsed.data.is_none() {
        return None;
    }

    let mut lines = Vec::new();

    // Extract frontmatter as IndexLines
    if let Some(data) = parsed.data {
        fn value_to_string(value: &serde_yaml::Value) -> Option<String> {
            match value {
                serde_yaml::Value::String(s) => Some(s.clone()),
                serde_yaml::Value::Number(n) => Some(n.to_string()),
                serde_yaml::Value::Bool(b) => Some(b.to_string()),
                serde_yaml::Value::Sequence(arr) => {
                    let items: Vec<String> = arr.iter()
                        .filter_map(value_to_string)
                        .collect();
                    Some(items.join(", "))
                }
                serde_yaml::Value::Null => None,
                serde_yaml::Value::Mapping(_) => {
                    // Serialize nested objects as YAML
                    serde_yaml::to_string(value).ok()
                }
                serde_yaml::Value::Tagged(tagged) => {
                    value_to_string(&tagged.value)
                }
            }
        }

        if let serde_yaml::Value::Mapping(map) = data {
            for (key, value) in map {
                if let Some(key_str) = key.as_str() {
                    if let Some(value_str) = value_to_string(&value) {
                        let line = format!("[FRONTMATTER:{}] {}", key_str, value_str);
                        lines.push(IndexLine::new(0, line));
                    }
                }
            }
        }
    }

    // Return frontmatter lines and remaining content
    Some((lines, parsed.content))
}
```

### Testing Strategy

**Unit Tests:**
1. Parse YAML frontmatter correctly
2. Parse TOML frontmatter (if supported)
3. Handle missing frontmatter (no delimiter)
4. Handle invalid frontmatter (invalid YAML)
5. Handle empty frontmatter
6. Handle various value types (string, number, array, object)

**Integration Tests:**
1. Create test `.md` file with frontmatter
2. Scan and index it
3. Query for frontmatter fields
4. Verify content is also indexed

**Test Files:**
Create in `crates/common/src/extract/test_files/`:
- `frontmatter-yaml.md` - YAML frontmatter
- `frontmatter-toml.md` - TOML frontmatter (if supported)
- `no-frontmatter.md` - Regular Markdown
- `invalid-frontmatter.md` - Malformed frontmatter
- `nested-frontmatter.md` - Nested objects/arrays

## Breaking Changes

None. This is a pure addition:
- Existing Markdown files will get enhanced indexing
- All other files unaffected
- No API changes
- No schema changes

Re-indexing existing Markdown files will pick up frontmatter automatically.

## Migration

No migration needed. Users can:
1. Upgrade find-anything
2. Re-scan Markdown directories to pick up frontmatter

Or just wait for next scan cycle (if using continuous scanning in future).

## User-Facing Changes

**Search improvements:**
- Can search for `frontmatter:tags planning` to find by tag
- Can search for `frontmatter:author "John Doe"`
- Can search for `frontmatter:date 2024`

**UI considerations:**
- Frontmatter appears in search results at line 0
- Might want to display it differently in UI (future enhancement)
- For now, shows as regular lines with `[FRONTMATTER:key]` prefix

## Future Enhancements

1. **UI for frontmatter:** Display frontmatter as structured metadata
2. **Frontmatter filtering:** Dedicated filters in UI (by tag, by author, etc.)
3. **Date parsing:** Parse dates and enable date range queries
4. **Support more formats:** JSON frontmatter, other delimiters

## Success Criteria

- [ ] Dependency added to Cargo.toml
- [ ] Frontmatter extraction implemented
- [ ] Unit tests pass (5+ test cases)
- [ ] Integration test with sample .md file
- [ ] Can search frontmatter fields
- [ ] Content after frontmatter still indexed correctly
- [ ] No regressions on non-Markdown files
- [ ] Documentation updated (CHANGELOG, README if needed)

## Estimated Scope

**Code changes:**
- ~100 lines in `text.rs` (frontmatter extraction)
- ~50 lines in tests
- 1 line in `Cargo.toml`

**Time:** ~2-3 hours including testing

**Complexity:** Low (extends existing extractor)
