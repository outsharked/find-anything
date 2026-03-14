# Plans 018–020: Format Extractors (HTML, Office, EPUB)

## Overview

Three new extractor crates following the established pattern from plan 015.
Each is a library crate + standalone binary (`find-extract-{name}`) with
`accepts(path) -> bool` and `extract(path, max_size_kb) -> Result<Vec<IndexLine>>`.

The existing dispatch chain in `extract.rs` (priority order: archive → pdf →
media → text) gets three new entries before the text fallback.

Committed separately: one commit per extractor.

---

## Pattern

- `crates/extractors/{name}/src/lib.rs` — `pub fn accepts`, `pub fn extract`
- `crates/extractors/{name}/src/main.rs` — thin CLI: args → extract → JSON stdout
- `crates/extractors/{name}/Cargo.toml` — workspace deps + format-specific crates
- Add to root `Cargo.toml` `[workspace] members`
- Add to `crates/client/Cargo.toml` `[dependencies]`
- Add dispatch block to `crates/client/src/extract.rs`
- Add arms to `detect_kind_from_ext()` in `crates/common/src/api.rs`
- Add arms to `extractor_binary_for()` in `crates/client/src/watch.rs`

**Key invariants:**
- `line_number = 0` → metadata (title, author, etc.)
- `line_number ≥ 1` → content (paragraphs, cells, lines)
- `archive_path = None` for all three (not archive members)
- Errors propagate; caller handles gracefully

---

## Extractor 1: `find-extract-html` (plan 018)

**New files:** `crates/extractors/html/src/lib.rs`, `main.rs`, `Cargo.toml`

**`accepts()`:** `.html`, `.htm`, `.xhtml`

**`extract()`:**
- Parse with `scraper` crate (pure Rust, built on html5ever)
- Metadata lines (`line_number = 0`):
  - `[HTML:title] …` from `<title>`
  - `[HTML:description] …` from `<meta name="description" content="…">`
- Content lines: visible text from `<h1>`–`<h6>`, `<p>`, `<li>`, `<td>`, `<th>`, `<pre>`, `<blockquote>`, `<figcaption>`
- Skip entirely: `<script>`, `<style>`, `<nav>`, `<header>`, `<footer>` (detected by ancestor walk)
- Skip blank/whitespace-only strings after extraction
- `kind`: `"text"` (HTML is text; no new UI kind needed)

**Deps:** `scraper = "0.21"`

---

## Extractor 2: `find-extract-office` (plan 019)

**New files:** `crates/extractors/office/src/lib.rs`, `main.rs`, `Cargo.toml`

**`accepts()`:** `.docx`, `.xlsx`, `.xls`, `.xlsm`, `.pptx`

**`extract()`:** dispatch on extension:

### DOCX
- Open as ZIP; extract `word/document.xml` and `docProps/core.xml`
- Parse with `quick-xml`: collect `<w:t>` element text, separated at `<w:p>` boundaries
- Each non-empty paragraph → one IndexLine (line_number = paragraph index + 1)
- Metadata from `docProps/core.xml`: `dc:title` → `[DOCX:title]`, `dc:creator` → `[DOCX:author]`

### XLSX / XLS / XLSM
- `calamine::open_workbook_auto` (handles all Excel formats)
- For each sheet: `[XLSX:sheet] SheetName` as a metadata line
- Rows → tab-joined non-empty cells as IndexLines; skip empty rows

### PPTX
- Open as ZIP; find `ppt/slides/slide*.xml` files (sorted numerically)
- Parse with `quick-xml`: collect `<a:t>` text nodes grouped by `<a:p>` paragraph
- Each slide → metadata line `[PPTX:slide] N`, then content lines per paragraph

**`kind`:** `"document"` (new value; added to `detect_kind_from_ext` alongside `epub`)

**Deps:** `zip = "2"`, `quick-xml = "0.37"`, `calamine = "0.26"`

---

## Extractor 3: `find-extract-epub` (plan 020)

**New files:** `crates/extractors/epub/src/lib.rs`, `main.rs`, `Cargo.toml`

**`accepts()`:** `.epub`

**`extract()`:**
1. Open as ZIP
2. Read `META-INF/container.xml` → find `rootfile full-path` (OPF location)
3. Parse OPF:
   - Metadata → `[EPUB:title]`, `[EPUB:creator]`, `[EPUB:publisher]`, `[EPUB:language]` (line_number=0)
   - Spine → ordered `idref` list → map through manifest to get XHTML paths
4. For each spine XHTML file:
   - Walk XML with `quick-xml` text-node walk
   - Accumulate text until block-level closing tags (`</p>`, `</h1>`, `</li>`, etc.)
   - Skip `<script>`, `<style>`, `<head>` containers via depth counter

**`kind`:** `"document"`

**Deps:** `zip = "2"`, `quick-xml = "0.37"`

---

## Dispatch Order After Changes

```
archive → pdf → media → html → office → epub → text
```

HTML, office, and epub must come before text because the text extractor's
`accepts()` would match them (via extension list or content sniffing).

---

## Files Changed

| File | Change |
|------|--------|
| `crates/extractors/html/` | New crate |
| `crates/extractors/office/` | New crate |
| `crates/extractors/epub/` | New crate |
| `Cargo.toml` (root) | Added 3 workspace members |
| `crates/client/Cargo.toml` | Added 3 extractor deps |
| `crates/client/src/extract.rs` | Added 3 dispatch blocks (before text fallback) |
| `crates/common/src/api.rs` | Added `"document"` kind for office/epub extensions |
| `crates/client/src/watch.rs` | Added arms in `extractor_binary_for` |
| `README.md` | Updated binaries table and supported file types table |

## Testing

Each extractor has unit tests verifiable with:

```bash
cargo test -p find-extract-html
cargo test -p find-extract-office
cargo test -p find-extract-epub
cargo build --workspace
```
