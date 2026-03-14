# External Pluggable Extractors (`[scan.extractors]`)

## Overview

Extension → extractor routing is currently hardcoded in `find-client`
(`extractor_binary_for` in `subprocess.rs`).  Adding support for a new archive
format (e.g. `.lzh`, `.cab`, `.arc`) requires a code change and a new binary.

This plan makes the routing table configurable so power users can wire in
system tools via `client.toml`, exactly as they already do for text formatters.

Built-in behaviour is unchanged by default.  Users only need to specify what
they want to add or override.

---

## Design Decisions

### "Builtin" sentinel

The special string `"builtin"` refers to the existing built-in routing (the
`match` in `extractor_binary_for`).  It appears in the default config
documentation so users can see what the built-in mapping looks like and can
replace any entry with an external tool.

```toml
[scan.extractors]
zip  = "builtin"   # handled by find-extract-archive
pdf  = "builtin"   # handled by find-extract-pdf
docx = "builtin"   # handled by find-extract-office
# User-defined: pipe stdout from an external tool
lzh  = { mode = "stdout", bin = "lhasa", args = ["-q", "-c", "{file}"] }
# User-defined: extract to temp dir, then walk and index members
cab  = { mode = "tempdir", bin = "cabextract", args = ["-d", "{dir}", "{file}"] }
```

### Two execution modes for external tools

| Mode | How it works | When to use |
|---|---|---|
| `"stdout"` | Tool writes extracted content to **stdout**; we capture and index it as the file's content | Single-file compressed formats, text-output tools |
| `"tempdir"` | Tool extracts into a **temp directory** (`{dir}`); we walk and index the extracted files as members | Multi-file archives where the tool can't stream |

The `mode` field is purely about *how the extractor is invoked* — it has no
relation to the file kind taxonomy (`"text"`, `"archive"`, `"image"`, etc.).
The stored `kind` for externally extracted files is determined the same way as
for any other file: `detect_kind_from_ext` on the file's own extension, with
plan 064's `[scan.file_types]` override applied on top.

The one exception is `mode = "tempdir"`: the outer file is always stored as
`kind = "archive"` regardless of extension, because that is what allows the
tree browser to expand it into its members. If `detect_kind_from_ext` would
return something else for that extension, it is overridden.

Built-in extractors don't use `mode` — the existing `detect_kind_from_ext`
determines the stored kind for built-in files, unchanged.

### Placeholders

| Placeholder | Meaning |
|---|---|
| `{file}` | Absolute path to the input file (always available) |
| `{name}` | Filename only, no directory (useful for type-detection hints, as in formatters) |
| `{dir}` | Absolute path to the temp directory (only valid for `mode = "tempdir"`) |

A `{dir}` placeholder in a `mode = "stdout"` config is a configuration error.
It is logged as a warning when the file is processed (there is no scan-startup
validation pass); the placeholder is left as-is in the arg, which will likely
cause the tool to fail.

### Composite paths for `mode = "tempdir"` members

Members follow the same `::` convention as built-in archives:
`outer.cab::subdir/file.txt`, `outer.lzh::readme.txt`.  The tree browser,
Ctrl+P, and search all work identically regardless of whether extraction was
handled by a built-in or external tool.

### Formatters remain unchanged

`[scan.normalization.formatters]` is not changed. Formatters run *after*
extraction on already-extracted text — a separate concern.

---

## Config Format

```toml
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
# pdf  = "builtin"
# docx = "builtin"
# xlsx = "builtin"
# epub = "builtin"
# jpg  = "builtin"
# ...
#
# Example: add RAR support via unrar (useful on ARM where unrar_sys won't build)
# rar = { mode = "tempdir", bin = "unrar", args = ["e", "-y", "{file}", "{dir}"] }
#
# Example: add LZH support via lhasa
# lzh = { mode = "tempdir", bin = "lhasa", args = ["-x", "{file}", "-C", "{dir}"] }
#
# Example: add LZW-compressed files via uncompress
# lzw = { mode = "stdout", bin = "uncompress", args = ["-c", "{file}"] }
```

---

## Implementation

### 1. Config structs (`crates/common/src/config.rs`)

```rust
/// A single external extractor tool.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExternalExtractorConfig {
    /// How to invoke the tool.
    /// "stdout": capture stdout as file content.
    /// "tempdir": extract to a temp directory, walk and index the files.
    pub mode: ExternalExtractorMode,
    /// Absolute or PATH-relative path to the binary.
    pub bin: String,
    /// Args with {file}, {name}, {dir} placeholders.
    pub args: Vec<String>,
}

/// Controls how an external extractor is invoked.
/// This is about execution mechanics, not the file kind taxonomy.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ExternalExtractorMode {
    /// Tool writes extracted content to stdout.
    Stdout,
    /// Tool extracts files into a temp directory.
    TempDir,
}

/// Value in the [scan.extractors] table.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ExtractorEntry {
    /// Use built-in routing (the existing find-extract-* subprocess).
    Builtin(String),
    External(ExternalExtractorConfig),
}
```

Add to `ScanConfig`:
```rust
/// Extension → extractor override. "builtin" = use built-in routing.
#[serde(default)]
pub extractors: HashMap<String, ExtractorEntry>,
```

### 2. Extractor routing (`crates/client/src/subprocess.rs`)

`extractor_binary_for` is replaced by `resolve_extractor`, which returns an
`ExtractorChoice` enum:

```rust
pub enum ExtractorChoice {
    Builtin(String),                   // path to a find-extract-* binary (including dispatch)
    External(ExternalExtractorConfig), // user-configured external tool
}

pub fn resolve_extractor(path: &Path, scan: &ScanConfig) -> ExtractorChoice {
    let ext = path.extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();

    // 1. Check config override
    if let Some(entry) = scan.extractors.get(&ext) {
        match entry {
            ExtractorEntry::Builtin(_) => { /* fall through to built-in */ }
            ExtractorEntry::External(cfg) => return ExtractorChoice::External(cfg.clone()),
        }
    }

    // 2. Built-in routing (unchanged)
    match ext.as_str() {
        "zip" | "tar" | ... | "7z" => ExtractorChoice::Builtin(
            resolve_binary("find-extract-archive", &scan.extractor_dir)
        ),
        "pdf" => ExtractorChoice::Builtin(resolve_binary("find-extract-pdf", &scan.extractor_dir)),
        // ...
        // Unknown extensions fall through to find-extract-dispatch, which
        // handles plain text, HTML, and other formats it recognises, and
        // indexes filename-only for everything else.  This preserves the
        // existing behaviour for unrecognised extensions.
        _ => ExtractorChoice::Builtin(
            resolve_binary("find-extract-dispatch", &scan.extractor_dir)
        ),
    }
}
```

### 3. External extractor execution (`crates/client/src/subprocess.rs`)

Two new functions called from `process_file` in `scan.rs`:

#### `mode = "stdout"`
```
substitute placeholders in args ({file} → absolute path, {name} → filename)
spawn(bin, substituted_args)
capture stdout
split into lines → Vec<IndexLine>
store outer file with kind = detect_kind_from_ext(ext) in files table
```
Timeout and error handling mirror the existing built-in subprocess path.

#### `mode = "tempdir"`
```
create TempDir
substitute placeholders ({file} → path, {dir} → tempdir, {name} → filename)
spawn(bin, substituted_args)
wait for exit (with timeout)
walk tempdir → for each extracted file:
  member_rel   = path relative to tempdir (forward slashes)
  outer_name   = original file's name
  archive_path = "<outer_name>::<member_rel>"
  detect_kind_from_ext(member_rel) → member_kind
  dispatch_from_bytes(member_bytes, member_name) → Vec<IndexLine>
  emit IndexFile { path: archive_path, lines, kind: member_kind, ... }
clean up TempDir
store outer file with kind = "archive" in files table
```

#### Placeholder substitution (helper)
```rust
fn substitute_args(
    args: &[String],
    file: &Path,
    dir: Option<&Path>,
) -> Vec<String>
```
Replaces `{file}`, `{name}`, `{dir}` in each arg string.  `{dir}` is only
substituted when `dir` is `Some`; a `{dir}` placeholder in a `mode = "stdout"`
config logs a warning at scan startup.

### 4. `process_file` integration (`crates/client/src/scan.rs`)

Switch from calling `extractor_binary_for` to `resolve_extractor`:

```rust
match resolve_extractor(path, &scan_cfg) {
    ExtractorChoice::Builtin(bin) => run_subprocess(bin, path, cfg),
    ExtractorChoice::External(ext_cfg) => match ext_cfg.mode {
        ExternalExtractorMode::Stdout  => run_external_stdout(path, &ext_cfg, cfg),
        ExternalExtractorMode::TempDir => run_external_tempdir(path, &ext_cfg, cfg),
    },
}
```

### 5. Default config documentation

`examples/client.toml` gains a commented `[scan.extractors]` block listing all
built-in mappings.  `install.sh` and the Windows installer `BuildToml()` both
get the same block (per the "keep Linux and Windows in sync" rule in CLAUDE.md).

---

## Files Changed

| File | Change |
|------|--------|
| `crates/common/src/config.rs` | Add `ExternalExtractorConfig`, `ExternalExtractorMode`, `ExtractorEntry`; add `extractors` field to `ScanConfig` |
| `crates/client/src/subprocess.rs` | Replace `extractor_binary_for` with `resolve_extractor` returning `ExtractorChoice`; add `run_external_stdout`, `run_external_tempdir`, `substitute_args` |
| `crates/client/src/scan.rs` | Switch to `resolve_extractor`; call new execution paths |
| `examples/client.toml` | Add commented `[scan.extractors]` block |
| `install.sh` | Same block in heredoc template |
| `packaging/windows/find-anything.iss` | Same block in `BuildToml()` |
| `crates/client/tests/fixtures/find-extract-nndjson` | New bash script (tempdir mode), `chmod +x` |
| `crates/client/tests/fixtures/find-extract-nndjson-stdout` | New bash script (stdout mode), `chmod +x` |
| `crates/client/tests/fixtures/test.nndjson` | New fixture file |
| `crates/client/tests/external_extractor.rs` | New integration tests |
| `crates/extractors/archive/tests/fixtures/fixtures.zip` | Add `test.nndjson` member |
| `crates/extractors/archive/tests/extract.rs` | Add `nndjson_member_not_traversed` test |

---

## Testing

### Test format — `.nndjson` (Named NDJSON)

The integration tests use a synthetic format invented for this purpose.  It
requires no installed dependencies; the extractor is a short bash script
checked into the repository.

**Format** — one member per line, `<member-filename>:<content>`.  Only the
*first* colon is the separator; colons in content are preserved.

```
item1.json:{"greeting": "hello world"}
notes.txt:this is a plain text note inside the archive
empty.txt:
```

**Extractor — `crates/client/tests/fixtures/find-extract-nndjson`**
(`chmod +x`; `mode = "tempdir"`):

```bash
#!/usr/bin/env bash
# find-extract-nndjson — extract a .nndjson archive to a temp directory.
# Usage: find-extract-nndjson <file> <outdir>
# Each line: <member-filename>:<content>  (first colon is the separator)
set -euo pipefail
file="$1"
outdir="$2"
while IFS= read -r line || [ -n "$line" ]; do
    [ -z "$line" ] && continue
    name="${line%%:*}"
    content="${line#*:}"
    printf '%s\n' "$content" > "$outdir/$name"
done < "$file"
```

**Extractor — `crates/client/tests/fixtures/find-extract-nndjson-stdout`**
(`chmod +x`; `mode = "stdout"`):

```bash
#!/usr/bin/env bash
# Variant: strip member names, write all content to stdout (mode = "stdout")
while IFS= read -r line || [ -n "$line" ]; do
    [ -z "$line" ] && continue
    printf '%s\n' "${line#*:}"
done < "$1"
```

**Fixture — `crates/client/tests/fixtures/test.nndjson`**:

```
item1.json:{"greeting": "hello world"}
notes.txt:this is a plain text note inside the archive
```

### Unit tests

**`substitute_args`** — given template `["{file}", "{dir}", "{name}"]` and
concrete paths, assert the returned `Vec<String>` is correct. The helper does
pure string substitution and is unaware of mode; the caller (`run_external_stdout`)
is responsible for warning when `{dir}` appears in args for a `mode = "stdout"`
extractor.

**`resolve_extractor`** — `"builtin"` sentinel falls through to built-in
binary; external entry returns `ExtractorChoice::External`; unknown extension
returns `ExtractorChoice::None`; explicit override of a built-in extension
returns the external config.

### Integration tests (`crates/client/tests/external_extractor.rs`)

All tests derive the script path from `CARGO_MANIFEST_DIR` — no `PATH`
manipulation required.  All tests are `#[cfg(unix)]`-gated.

**`mode = "tempdir"` — members indexed as composite paths**

```
config:
  nndjson = { mode = "tempdir",
              bin  = "<fixtures>/find-extract-nndjson",
              args = ["{file}", "{dir}"] }

input:  test.nndjson

assert: outer file kind = "archive"
        "test.nndjson::item1.json" indexed, content contains "hello world"
        "test.nndjson::notes.txt"  indexed, content contains "plain text note"
        no other "test.nndjson::" paths
```

**`mode = "stdout"` — whole file indexed as a single text document**

```
config:
  nndjson = { mode = "stdout",
              bin  = "<fixtures>/find-extract-nndjson-stdout",
              args = ["{file}"] }

input:  test.nndjson

assert: outer file kind = detect_kind_from_ext("nndjson")  (→ "unknown" / fallback)
        content lines contain "hello world" and "plain text note"
        no composite paths
```

**Error handling — extractor exits non-zero**

Configure a one-line script `exit 1`; assert the result is recorded as an
`IndexingFailure` with a non-empty error string and no content lines emitted.

**`.nndjson` member in `fixtures.zip` — not traversed by archive extractor**

Add `test.nndjson` to `fixtures.zip`.  In `extract.rs`:

```rust
#[test]
fn nndjson_member_not_traversed() {
    // find-extract-archive knows nothing about .nndjson; it should appear as
    // an opaque top-level member, never as composite "test.nndjson::..." paths.
    let lines = extract(&fixtures_zip(), &default_cfg()).unwrap();
    assert!(has_path(&lines, "test.nndjson"));
    assert!(!any_path_contains(&lines, "test.nndjson::"));
}
```

### Windows note

External extractor support on Windows (`.bat`, `.ps1`, or WSL path) is
deferred.  All integration tests are `#[cfg(unix)]`-gated.

---

## Breaking Changes

None. All new config keys are optional; unspecified keys inherit built-in
behaviour. `extractor_binary_for`'s rename to `resolve_extractor` is internal.
