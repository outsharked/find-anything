# File Type Config (`[scan.file_types]`)

## Overview

The extension → kind mapping (`detect_kind_from_ext`) is currently a hardcoded
`match` in `find-extract-types`.  Users with non-standard extensions
(`.myformat`, `.conf.bak`, `.log.1`) cannot tell the indexer how to treat their
files — they are silently classified as `"unknown"` or `"text"` at the
extractor's discretion.

This plan adds a `[scan.file_types]` config table that is checked *before*
`detect_kind_from_ext`, letting users override or extend the built-in mapping
without changing code.

This is a companion to plan 062 (external extractors) but is independent of it.
A user can override kinds without configuring a custom extractor, and vice versa.

---

## Design Decisions

### Sparse overlay — only specify what you want to change

The user's table is checked first; the built-in `detect_kind_from_ext` remains
the fallback.  An empty `[scan.file_types]` section (or no section at all) is
identical to today's behaviour.

### Keys are lowercase extensions without the leading dot

Consistent with how `detect_kind_from_ext` works internally.

### Valid kind values

The same strings accepted elsewhere in the codebase:
`"text"`, `"archive"`, `"image"`, `"audio"`, `"pdf"`, `"document"`,
`"binary"`, `"unknown"`.

Unknown values are rejected at config-parse time with a clear error message.

### Interaction with plan 062 external extractors

`file_types` controls the `kind` stored in the `files` table for files that
go through built-in extraction.  For files handled by an external extractor
(plan 062), the extractor config's own `kind` field determines the stored kind
instead — `file_types` is not consulted for those files.

This avoids ambiguity: the extractor config is the single source of truth for
externally extracted files.

---

## Config Format

```toml
# ── File type overrides ──────────────────────────────────────────────────────
# Override or extend the built-in extension → kind table.
# Keys are lowercase extensions without the leading dot.
# Valid kinds: "text", "archive", "image", "audio", "pdf",
#              "document", "binary", "unknown"
# Built-in detect_kind_from_ext is the fallback for extensions not listed here.
#
# [scan.file_types]
# log      = "text"    # index .log files as text (they already are, just an example)
# backup   = "text"    # treat .backup files as text
# myformat = "text"    # proprietary format that contains plain text
```

---

## Implementation

### 1. Add `file_types` to `ScanConfig` (`crates/common/src/config.rs`)

```rust
/// Extension → kind overrides.
/// Checked before the built-in detect_kind_from_ext.
/// Keys are lowercase extensions without the leading dot.
#[serde(default)]
pub file_types: HashMap<String, String>,
```

Validation: at parse time, reject values that are not one of the known kind
strings (return a descriptive error).

### 2. `resolve_kind` helper (`crates/extract-types/src/index_line.rs` or `find-common`)

```rust
pub fn resolve_kind<'a>(ext: &str, overrides: &'a HashMap<String, String>) -> &'a str {
    if let Some(k) = overrides.get(ext) {
        return k.as_str();
    }
    detect_kind_from_ext(ext)
}
```

### 3. Thread through `scan.rs`

Replace the direct `detect_kind_from_ext(ext)` call in `process_file` (and any
other call sites) with `resolve_kind(ext, &scan_cfg.file_types)`.

---

## Files Changed

| File | Change |
|------|--------|
| `crates/common/src/config.rs` | Add `file_types: HashMap<String, String>` to `ScanConfig`; validate known kind values at parse time |
| `crates/extract-types/src/index_line.rs` | Add `resolve_kind` (or place in `find-common`) |
| `crates/client/src/scan.rs` | Replace `detect_kind_from_ext` calls with `resolve_kind` |
| `examples/client.toml` | Add commented `[scan.file_types]` block |
| `install.sh` | Same block in heredoc template |
| `packaging/windows/find-anything.iss` | Same block in `BuildToml()` |

---

## Testing

- **`resolve_kind` unit tests** — override table checked before built-in; unknown
  ext in neither table returns built-in default; built-in ext overridden by user
  config returns the override.
- **Config validation** — invalid kind value (`"video"`, `"typo"`) rejected at
  parse time with a descriptive error message; valid values round-trip through
  serde.
- **Integration** — scan a directory containing a file with a non-standard
  extension, configured with `[scan.file_types] myext = "text"`; assert the
  indexed `FileRecord.kind == "text"`.

---

## Breaking Changes

None. `file_types` defaults to an empty map; all existing behaviour is unchanged.
