# Plan: Additional Content Type Support - Research

## Context

Currently, find-anything supports these content types:
- **Text files** - Line-by-line extraction
- **PDF** - Text extraction via pdf-extract
- **Archives** - ZIP, TAR, 7Z with nested member extraction
- **Images** - EXIF metadata via kamadak-exif
- **Audio** - ID3/Vorbis/M4A tags via id3, metaflac, mp4ameta
- **Video** - Format/resolution/duration via audio-video-metadata

The ROADMAP identifies several additional content types to explore:
- Office documents (DOCX, XLSX, PPTX)
- Markdown frontmatter extraction
- Email indexing (mbox, EML)

This plan researches each category, evaluates Rust libraries, and recommends implementation priorities.

---

## Research Phase: Content Type Categories

### 1. Office Documents (Microsoft Office, OpenDocument)

**File Types:**
- **Word**: .docx, .doc (legacy), .odt
- **Excel**: .xlsx, .xls (legacy), .ods
- **PowerPoint**: .pptx, .ppt (legacy), .odp

**Use Cases:**
- Index document text for searchability
- Extract metadata (author, title, creation date)
- Potentially extract embedded images/objects

**Rust Libraries to Investigate:**

**DOCX (Office Open XML):**
- `docx-rs` - Pure Rust DOCX reader/writer
  - Status: Actively maintained
  - Features: Read/write DOCX, extract text and structure
  - License: MIT
  - Note: OOXML format is ZIP-based, could potentially use zip crate directly

- `docx` crate - Simpler reader
  - Status: Less maintained
  - Features: Basic text extraction

**XLSX (Excel):**
- `calamine` - Pure Rust Excel/ODS reader
  - Status: Well-maintained, popular
  - Features: Read XLS, XLSX, XLSB, ODS
  - Extract cell values, formulas, formatting
  - License: MIT
  - Performance: Designed for large files

- `umya-spreadsheet` - Excel reader/writer
  - Status: Active
  - Features: Read/write XLSX, formatting support
  - More complex API

**PPTX (PowerPoint):**
- `pptx-rs` - Limited library
  - Status: Less mature
  - Features: Basic PPTX reading
  - Note: PowerPoint extraction less common, lower priority

**OpenDocument Format (ODF):**
- `odf` crate - ODF reader
  - Status: Minimal
  - Could parse as XML (ODF is XML-based)

**Legacy Formats (.doc, .xls, .ppt):**
- No pure Rust parsers found
- Would require external tools (antiword, xls2csv)
- **Recommendation**: Skip legacy formats initially

**Recommended Approach:**
1. Start with **DOCX** via `docx-rs` (most common modern format)
2. Add **XLSX** via `calamine` (widely used for data)
3. **PPTX** - Lower priority (less text content typically)
4. **ODF** - Lower priority (less common than OOXML)

---

### 2. Markdown Frontmatter

**File Types:**
- `.md`, `.markdown`
- YAML/TOML/JSON frontmatter at file start

**Example:**
```markdown
---
title: My Document
author: John Doe
tags: [rust, indexing]
date: 2024-01-01
---

# Content starts here
```

**Use Cases:**
- Index metadata separately from content
- Enable filtering by tags, author, date
- Useful for documentation sites, blogs, notes

**Rust Libraries:**
- `gray_matter` - Frontmatter parser
  - Supports YAML, TOML, JSON
  - Actively maintained
  - License: MIT

- `markdown` crate - Full Markdown parser
  - Includes frontmatter support
  - Heavier than needed if only extracting frontmatter

- Manual parsing: Simple regex/split approach
  - Look for `---` delimiters
  - Parse YAML with `serde_yaml`

**Recommended Approach:**
1. Use `gray_matter` for robust frontmatter extraction
2. Index frontmatter as `[FRONTMATTER:key] value` (line_number=0)
3. Index content normally (line-by-line)
4. Already detected as "text", just need to enhance extraction

---

### 3. Email Formats

**File Types:**
- `.eml` - RFC 822 email messages (plain text format)
- `.mbox` - Mailbox format (concatenated messages)
- `.msg` - Outlook message format (binary)
- `.pst` - Outlook Personal Storage (binary, proprietary)

**Use Cases:**
- Search email archives
- Index headers (from, to, subject, date)
- Index body content

**Rust Libraries:**

**EML/RFC 822:**
- `mail-parser` - Modern email parser
  - Parses MIME, headers, attachments
  - Actively maintained
  - License: Apache 2.0/MIT

- `mailparse` - Older but stable
  - MIME message parsing
  - Headers, body, attachments

**MBOX:**
- `mbox` crate - MBOX reader
  - Iterate through messages in MBOX file
  - Combine with mail parser for content

**MSG (Outlook):**
- `msg-parser` - MSG file parser
  - Status: Experimental
  - Limited adoption

**PST (Outlook):**
- No pure Rust libraries found
- Complex proprietary format
  - Would require external tools (readpst)
  - **Recommendation**: Out of scope initially

**Recommended Approach:**
1. Start with **EML** via `mail-parser`
2. Add **MBOX** support (iterate + parse each message)
3. Index as:
   - Headers: `[EMAIL:from]`, `[EMAIL:to]`, `[EMAIL:subject]`, `[EMAIL:date]` (line_number=0)
   - Body: Line-by-line content (line_number=1+)
4. **MSG/PST** - Skip initially (complex, less common)

---

### 4. Other Valuable Content Types

**RTF (Rich Text Format):**
- `.rtf` files
- `rtf-grimoire` crate available
- Use case: Legacy documents
- **Priority**: Low

**Epub (E-books):**
- `.epub` - ZIP archive with HTML/XML
- Could extract with zip + HTML parser
- `epub` crate available
- **Priority**: Medium (useful for documentation/books)

**HTML/XML:**
- Already extractable as text
- Could enhance with structure extraction
  - `<title>`, `<meta>` tags
  - `scraper` crate for HTML parsing
- **Priority**: Medium (HTML is common)

**Jupyter Notebooks (.ipynb):**
- JSON format with code cells
- Extract markdown cells and code cells separately
- `serde_json` already in dependencies
- **Priority**: Medium (developer tool)

**SVG (as text):**
- Already handled as XML/text
- Could extract `<text>` elements specifically
- **Priority**: Low

---

## Recommended Implementation Priority

Based on:
- **Commonality**: How often users encounter these files
- **Value**: How much searchability improves
- **Complexity**: Implementation difficulty
- **Dependencies**: Library maturity and size

### Tier 1: High Value, Low Complexity
1. **Markdown Frontmatter** - Extend existing text extractor, adds metadata search
2. **EML Email** - Common format, clean library support
3. **DOCX** - Ubiquitous document format

### Tier 2: Medium Value, Medium Complexity
4. **XLSX** - Data-heavy, less text but valuable
5. **HTML Enhancement** - Extract title/meta tags
6. **Jupyter Notebooks** - Developer-focused, JSON parsing

### Tier 3: Lower Priority
7. **MBOX** - Extension of EML support
8. **EPUB** - E-books, niche but valuable for some users
9. **PPTX** - Less text content typically

### Out of Scope (Initially)
- **PST/MSG** - Proprietary, complex, external tools needed
- **Legacy Office (.doc, .xls)** - No Rust support, use external converters
- **Tree-sitter** - Heavy, wait for validation

---

## Next Steps

1. **User Input**: Confirm priority order and which types to implement first
2. **Proof of Concept**: Test libraries with sample files
3. **Plan Individual Extractors**: Detailed implementation plan per type
4. **Incremental Rollout**: One extractor at a time, versioned releases

---

## Implementation Strategy

Based on user feedback:
- **Implement one at a time** - Incremental approach, one extractor per release
- **Skip code symbols** - Not needed at this time
- **Start with Tier 1** - Focus on high-value, low-complexity extractors first

**Recommended order:**
1. Markdown frontmatter (easiest, extends existing text extractor)
2. EML email (clean library, common format)
3. DOCX documents (most requested office format)

