# Pluggable Extractors and File Type Config

## Overview

Two related pieces of hardcoded dispatch that should move to config:

1. **Extension → kind mapping** (`detect_kind_from_ext`) — determines whether a
   file is `"text"`, `"archive"`, `"image"`, `"document"`, etc. Currently a
   large `match` in `find-extract-types`. Users with unusual extensions (`.myformat`,
   `.conf.bak`, `.sql.gz`) can't tell the indexer how to treat their files.

2. **Extension → extractor routing** (`extractor_binary_for`) — determines which
   subprocess handles each file. Currently hardcoded in `find-client`. Adding
   support for new archive formats (e.g. `.lzh`, `.arc`, `.lzw`, `.cab`) requires
   a code change and a new binary. Power users should be able to wire in system
   tools like `lhasa`, `cabextract`, `unace` via config, exactly as they already
   do for formatters.

The goal is: built-in behaviour stays unchanged by default, but everything is
expressed through the same config layer that users can read and override.

---

## Design Decisions

### "Builtin" sentinel

Built-in extractors are indicated by the special value `"builtin"`.  This lets
the default config (shown in `examples/client.toml`) document what the built-in
mapping looks like, while allowing users to replace any entry with an external
tool.

```toml
[scan.extractors]
zip  = "builtin"   # handled by find-extract-archive
rar  = "builtin"   # handled by find-extract-archive
pdf  = "builtin"   # handled by find-extract-pdf
docx = "builtin"   # handled by find-extract-office
# User-defined: pipe stdout from an external tool
lzh  = { kind = "text", bin = "lhasa", args = ["-q", "-c", "{file}"] }
# User-defined: extract to temp dir, then walk and index
cab  = { kind = "archive", bin = "cabextract", args = ["-d", "{dir}", "{file}"] }
```

### Two extractor kinds for external tools

| Kind | How it works | When to use |
|---|---|---|
| `"text"` | Tool writes decompressed/extracted text to **stdout**; we index that as the file's content | Single-file compressed formats, text-output tools |
| `"archive"` | Tool extracts to a **temp directory** (`{dir}`); we walk and index that dir | Multi-file archives where the tool can't stream |

Built-in extractors don't need a `kind` — the routing logic already knows what
each built-in subprocess produces.

### Placeholders

| Placeholder | Meaning |
|---|---|
| `{file}` | Absolute path to the input file (always available) |
| `{name}` | Filename only, no directory (useful for type-detection hints, as in formatters) |
| `{dir}` | Absolute path to a temp directory (only for `kind = "archive"`) |

### File type config

```toml
[scan.file_types]
# Override or extend the built-in extension → kind table.
# Keys are lowercase extensions without the leading dot.
# Valid kinds: "text", "archive", "image", "audio", "video",
#              "document", "pdf", "binary", "unknown"
myformat = "text"
backup   = "text"   # treat .backup files as text
```

Built-in `detect_kind_from_ext` still applies as a fallback; the config table
is checked first. Keys not present in the user's config are inherited from the
built-in defaults — users only need to specify what they want to change.

### Interaction between the two tables

`file_types` controls the *kind* stored in the `files` table (used by the UI
for icons and rendering). `extractors` controls which subprocess is invoked for
content extraction. They are independent:

- A `.lzh` file can have `kind = "archive"` in `file_types` *and* an entry in
  `extractors` that invokes `lhasa`.
- An extension can have a `file_types` entry without an `extractors` entry (it
  will fall through to `find-extract-dispatch` as today).

### Formatters remain unchanged

The existing `[scan.normalization.formatters]` config is not changed. Formatters
run *after* extraction and act on the extracted text — they are a separate
concern from extraction.

---

## Config Format

### `client.toml`

```toml
# ── File type overrides ──────────────────────────────────────────────────────
# Uncomment and modify to override or extend the built-in extension → kind table.
# [scan.file_types]
# myformat = "text"
# backup   = "text"

# ── Extractor overrides ──────────────────────────────────────────────────────
# The built-in extractors are listed below for reference.
# Set an extension to "builtin" to use the built-in extractor (default).
# Replace with a tool config to use an external command instead.
#
# [scan.extractors]
# zip  = "builtin"
# tar  = "builtin"
# gz   = "builtin"
# bz2  = "builtin"
# xz   = "builtin"
# tgz  = "builtin"
# tbz2 = "builtin"
# txz  = "builtin"
# 7z   = "builtin"
# rar  = "builtin"
# pdf  = "builtin"
# jpg  = "builtin"
# ...
# docx = "builtin"
# xlsx = "builtin"
# epub = "builtin"
#
# Example: add support for LZH archives via lhasa
# lzh = { kind = "archive", bin = "lhasa", args = ["-x", "{file}", "-C", "{dir}"] }
#
# Example: add support for LZW-compressed files via uncompress
# lzw = { kind = "text", bin = "uncompress", args = ["-c", "{file}"] }
```

---

## Implementation

### 1. Config structs (`crates/common/src/config.rs`)

```rust
/// A single external extractor tool.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExternalExtractorConfig {
    /// "text" (stdout) or "archive" (extract to temp dir)
    pub kind: ExternalExtractorKind,
    /// Absolute or PATH-relative path to the binary.
    pub bin: String,
    /// Args with {file}, {name}, {dir} placeholders.
    pub args: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ExternalExtractorKind { Text, Archive }

/// Value in the [scan.extractors] table.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ExtractorEntry {
    Builtin(String),        // "builtin"
    External(ExternalExtractorConfig),
}
```

Add to `ScanConfig`:
```rust
/// Extension → kind overrides (checked before built-in detect_kind_from_ext).
#[serde(default)]
pub file_types: HashMap<String, String>,

/// Extension → extractor override. "builtin" = use built-in routing.
#[serde(default)]
pub extractors: HashMap<String, ExtractorEntry>,
```

### 2. Kind resolution (`find-extract-types` / `find-common`)

Add a function:
```rust
pub fn resolve_kind<'a>(ext: &str, overrides: &'a HashMap<String, String>) -> &'a str {
    if let Some(k) = overrides.get(ext) { return k.as_str(); }
    detect_kind_from_ext(ext)
}
```

Thread `ScanConfig` (or just `file_types`) through to the call sites in
`scan.rs` that call `detect_kind_from_ext`.

### 3. Extractor routing (`crates/client/src/subprocess.rs`)

`extractor_binary_for` gains a `scan: &ScanConfig` parameter:

```rust
pub fn extractor_binary_for(
    path: &Path,
    scan: &ScanConfig,
) -> ExtractorChoice {
    let ext = /* ... */;

    // 1. Check config override
    if let Some(entry) = scan.extractors.get(&ext) {
        match entry {
            ExtractorEntry::Builtin(_) => { /* fall through to built-in */ }
            ExtractorEntry::External(cfg) => return ExtractorChoice::External(cfg.clone()),
        }
    }

    // 2. Built-in routing (unchanged from today)
    let name = match ext.as_str() { /* ... existing match ... */ };
    ExtractorChoice::Builtin(resolve_binary(name, &scan.extractor_dir))
}
```

### 4. External extractor execution (`crates/client/src/subprocess.rs`)

Two new execution paths called from `process_file` in `scan.rs`:

#### `kind = "text"`
```
spawn(bin, args[{file}=path]) → capture stdout → split into lines → IndexLines
```
Same as `find-extract-dispatch` stdout today.

#### `kind = "archive"`
```
create TempDir
spawn(bin, args[{file}=path, {dir}=tempdir])
wait for exit
walk tempdir → for each file:
  member_rel = relative path within tempdir
  archive_path = "<outer_filename>::<member_rel>"   ← composite path
  detect_kind, extract content → IndexLines
clean up TempDir
```

Composite paths follow the same `::` convention as the built-in archive
extractor — `outer.lzh::member.txt`, `outer.cab::subdir/file.txt` — so the
tree browser, Ctrl+P, and search all work identically regardless of whether
extraction was handled by a built-in or an external tool.

The "archive" path reuses the existing member-extraction helpers from
`find-extract-archive` (via the dispatch crate) — it just replaces the
"iterate archive members in memory" step with "iterate files in a temp dir".

### 5. `detect_kind_from_ext` stays as-is for now

It remains the built-in fallback.  Longer-term it could itself be generated
from the default config, but that's a separate refactor.

### 6. Default config documentation

`examples/client.toml` gains a commented `[scan.extractors]` block listing all
built-in mappings.  `install.sh` and the Windows installer `BuildToml()` both
get the same block (per the "keep Linux and Windows in sync" rule).

---

## Files Changed

- `crates/common/src/config.rs` — add `ExternalExtractorConfig`, `ExtractorEntry`,
  `file_types`, `extractors` to `ScanConfig`
- `crates/client/src/subprocess.rs` — `extractor_binary_for` accepts config,
  returns `ExtractorChoice`; new `run_external_text_extractor` and
  `run_external_archive_extractor`
- `crates/client/src/scan.rs` — thread `ScanConfig` through to kind resolution
  and extractor choice; call new execution paths
- `crates/extract-types/src/index_line.rs` — add `resolve_kind` (or move to
  `find-common` alongside the config)
- `examples/client.toml` — document new config sections
- `install.sh` — add commented `[scan.extractors]` block
- `packaging/windows/find-anything.iss` — same block in `BuildToml()`

---

## Testing

- Unit tests for `resolve_kind` with override table
- Unit tests for `extractor_binary_for` with `"builtin"` and external entries
- Integration test: configure a `kind = "text"` extractor pointing at a simple
  shell script (`echo hello`), scan a file with that extension, verify content
  is indexed
- Integration test: configure a `kind = "archive"` extractor pointing at `tar`,
  scan a `.tar` with the built-in disabled, verify members are indexed

---

## Breaking Changes

None for existing users — all new config keys are optional with sane defaults,
and unspecified keys inherit built-in behaviour.
`extractor_binary_for`'s signature change is internal.
