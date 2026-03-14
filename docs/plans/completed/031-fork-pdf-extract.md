# Fork pdf-extract and Fix name_to_unicode Panics

## Overview

`pdf-extract` panics when processing PDFs with ZapfDingbats fonts because glyph names
like `a71` are not in the Adobe Glyph List used by `glyphnames::name_to_unicode()`. The
upstream has 30+ open panic issues and an unmerged error-handling PR (open since 2024).
No forks have traction or fix this specific bug.

Our code already wraps extraction in `catch_unwind`, so these panics are caught — but
forking gives us clean, non-panicking behavior and positions us to fix future issues.

In a second pass, ~35 additional bad-input panic sites were hardened across all major
subsystems (font encoding, ToUnicode CMap, Type0/CID fonts, colorspace, content stream
operators). The philosophy throughout: **log a warning and return a safe default so
extraction of remaining content can continue**.

## Design Decisions

- **Fork rather than patch upstream** — upstream is effectively unmaintained for this
  class of bug; a fork gives us full control and a stable target for future fixes.
- **`unwrap_or(0)` for unknown glyph names** — returning 0 (null character) is a safe
  no-op: it maps to no visible text, which is correct for a font whose glyphs we can't
  resolve. Panicking on unknown names is wrong for a parsing library.
- **Try both glyph tables in the with-encoding branch** — the with-encoding path only
  tried `glyphnames`, missing ZapfDingbats names. The no-encoding branch already handled
  this correctly; the fix makes the two branches consistent.
- **Replace `dlog!` with `log::debug!`** — the old macro permanently silenced all debug
  output. Using the `log` façade lets consumers opt into debug output via their logger
  while remaining silent by default. The `log` crate was already in `Cargo.toml`.
- **Warn + continue, not error + abort** — for every bad-input panic, the fix logs a
  `warn!` and returns a safe default (empty string, 0.0, `DeviceRGB`, Identity-H, etc.)
  so extraction of other pages/fonts can continue. An entire document shouldn't be lost
  because one font has a malformed colorspace.
- **Colorspace rewritten to return `Option`** — `make_colorspace` was refactored into
  a thin wrapper + `try_make_colorspace` that returns `Option<ColorSpace>`. All 14 panic
  sites inside the colorspace parser were replaced with `?` operators. Falls back to
  `DeviceRGB` on failure since colorspace never affects text content.
- **Type0 (CID) font falls back to Identity-H encoding** — when the Encoding entry is
  missing, unparseable, or uses an unknown CMap name, Identity-H is used as a fallback.
  This treats every two bytes as a direct CID, which is the most common CID encoding and
  often produces usable output for common-case fonts.
- **Highly-corrupted scenarios left as panics** — missing `Root`/`Pages` tree (lines
  92–116), broken object reference chains (`maybe_deref` line 176), and internal type
  framework errors (lines 203–234) are left as-is. These indicate a document so
  malformed that nothing is salvageable; `catch_unwind` in find-anything handles them.

## Implementation

### 1. Fork and clone

```sh
gh repo fork jrmuizel/pdf-extract --clone --fork-name pdf-extract
# creates jamietre/pdf-extract on GitHub
```

### 2. Initial panic fixes in `src/lib.rs` (ZapfDingbats)

Four `unwrap()` call sites converted to non-panicking alternatives:

| Location | Fix |
|----------|-----|
| `encoding_to_unicode_table` (MAC_ROMAN / MAC_EXPERT / WIN_ANSI) | `unwrap()` → `unwrap_or(0)` |
| TrueType WIN_ANSI encoding table | `unwrap()` → `unwrap_or(0)` |
| **CRITICAL** — core font with-encoding loop (ZapfDingbats) | Try `glyphnames` then `zapfglyphnames`; skip if neither matches |
| Core font no-encoding loop | `unwrap()` → `unwrap_or(0)` |

Line 589 (`zapfdigbats_names_to_unicode(...).unwrap_or_else(|| panic!(...))`) is
intentional defensive code and was left alone.

### 3. Comprehensive panic hardening (second pass)

~35 additional bad-input panic sites replaced with warn + safe default:

**UTF-16 decode (`pdf_to_utf8`, `to_utf8`):** switched to `decode_without_bom_handling`
(lossy), replacing invalid sequences with U+FFFD instead of panicking.

**`get_name_string`:** returns empty `String` instead of panicking on missing key or
non-Name value.

**`encoding_to_unicode_table`:** unknown encoding names fall back to `PDFDocEncoding`
with a warning.

**Type1/CFF font parsing:**
- `type1_encoding_parser` failure → warn + skip encoding table
- `cff_parser::Table::parse` failure → warn + skip unicode map
- `cff_parser::string_by_id` → `?` in `filter_map` to skip bad SIDs
- CFF unicode: `char::from_u32` instead of `String::from_utf16` to skip surrogates

**Differences array (`PdfSimpleFont` + `PdfType3Font`):** unexpected object types →
warn + skip entry; FontAwesome occupied-entry case → warn instead of panic; UTF-16
conversions replaced with `char::from_u32`.

**Encoding type mismatch:** unknown encoding object type → warn + proceed without
encoding table (both `PdfSimpleFont` and `PdfType3Font`).

**`decode_char` fallback:** missing encoding when unicode map lookup fails → return
`String::new()` instead of panicking.

**Type3 width:** missing glyph width → return `0.0` with warning.

**ToUnicode CMap (`get_unicode_map`):**
- CMap parse failure → warn + return `None`
- `from_utf16` failure per entry → warn + skip entry
- Unknown `ToUnicode` object type → warn + ignore
- Unknown name variant → warn + ignore
- `dlog!` stream logging → `from_utf8_lossy`

**`PdfCIDFont::new` (Type0 fonts):**
- Missing/malformed `DescendantFonts` → warn + empty dict
- Unknown CMap name → warn + Identity-H fallback
- CMap stream parse failure → warn + Identity-H fallback
- Missing `Encoding` → warn + Identity-H fallback
- Unexpected `Encoding` type → warn + Identity-H fallback
- `FontDescriptor` assert removed (was unused `_f`)
- `W` array widths: bounds-checked; non-numeric entries skipped with warning
- `dlog!` stream logging → `from_utf8_lossy`

**`make_colorspace`:** refactored into `make_colorspace` + `try_make_colorspace`
(returns `Option`). All 14 panic/unwrap/expect sites replaced with `?` or explicit
`None` returns; falls back to `DeviceRGB`.

**Content stream processing:**
- `Content::decode` failure → warn + `return Ok(())` (skip page)
- `CS`/`cs` operator non-Name operand → warn + skip
- `Tj` non-String operand → warn + skip
- `Tf`/`gs`/`Do` non-Name operand → warn + `continue`

**`show_text`:** no font selected → warn + `return Ok(())` instead of unwrap.

**`apply_state` (SMask/ExtGState):** unexpected SMask name/type → warn + ignore;
unexpected `Type` value → warn instead of assert.

### 3. `dlog!` macro update

```rust
// Before (permanently silent):
macro_rules! dlog {
    ($($e:expr),*) => { {$(let _ = $e;)*} }
}
// After (routes through log façade):
macro_rules! dlog {
    ($($t:tt)*) => { log::debug!($($t)*) }
}
```

Also fixed a latent format-string bug at line 539 exposed by the macro change:
`dlog!("name: {}", name)` where `name` is `Result<_, _>` — changed `{}` to `{:?}`.

### 4. Update `Cargo.toml`

In `crates/extractors/pdf/Cargo.toml`:

```toml
# Before:
pdf-extract = "0.7"

# After:
pdf-extract = { git = "https://github.com/jamietre/pdf-extract" }
```

## Files Changed

- `jamietre/pdf-extract` (separate repo) — `src/lib.rs` fixes
- `crates/extractors/pdf/Cargo.toml` — points to forked git dependency

## Testing

```sh
cargo run -p find-extract-pdf -- /tmp/i941_a.pdf
```

Produces JSON output (no panic, no noisy log lines) for a ZapfDingbats PDF that
previously triggered the panic.

## Breaking Changes

None. The fork is a drop-in replacement; the public API is unchanged.
