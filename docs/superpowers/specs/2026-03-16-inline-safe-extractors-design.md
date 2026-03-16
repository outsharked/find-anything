# Inline Safe Extractors in find-scan

## Overview

Currently find-scan spawns a subprocess for every file extraction — including
text, HTML, media, and office files — even though these extractors are stable,
in-house or well-understood 3rd-party libraries that pose no panic risk. Only
PDF (custom fork, can panic on malformed data) and archive (streaming MPSC path,
recursive, complex) genuinely benefit from subprocess isolation.

This change inlines safe extractors directly into find-scan, eliminating
unnecessary IPC overhead, JSON serialization round-trips, and subprocess spawn
cost for the most common file types.

find-watch retains subprocess dispatch for everything except text, to keep its
memory footprint small while it runs as a persistent daemon.

## Extractors Inlined

| Extractor | find-scan | find-watch |
|-----------|-----------|------------|
| Text / code (via dispatch) | Inline | Inline |
| HTML | Inline | Subprocess |
| Media (images, audio, video) | Inline | Subprocess |
| Office (docx, xlsx, pptx, …) | Inline | Subprocess |
| PDF | Subprocess | Subprocess |
| Archive | Subprocess (streaming) | Subprocess (streaming) |
| EPUB | Subprocess | Subprocess |
| PE | Subprocess | Subprocess |
| Unknown / dispatch fallback | Subprocess | Subprocess |

## Design

### New Types

Replace the current two-type system (`ExtractorChoice` + string from
`extractor_binary_for`) with a single unified enum returned by `resolve_extractor`:

```rust
pub enum ExtractorRoute {
    Inline(InlineKind),
    Archive,
    Subprocess(String),
    External(ExternalExtractorConfig),
}

#[derive(PartialEq)]
pub enum InlineKind {
    Text,
    Html,
    Media,
    Office,
}
```

### Resolver

`resolve_extractor` gains two new parameters — `extractor_dir` (absorbed from
`extractor_binary_for`) and `inline_set: &[InlineKind]` (caller controls which
extractors are inlined):

```rust
pub fn resolve_extractor(
    path: &Path,
    scan: &ScanConfig,
    extractor_dir: &Option<String>,
    inline_set: &[InlineKind],
) -> ExtractorRoute
```

Resolution order inside the function:

1. User-configured `scan.extractors` entry → `External` (if not overridden to builtin)
2. Archive extensions (zip, tar, gz, bz2, xz, 7z, …) → `Archive` (always subprocess)
3. `.pdf` → `Subprocess("find-extract-pdf")` (always subprocess)
4. Extension matches an inline-eligible type **and** that `InlineKind` is in
   `inline_set` → `Inline(kind)`
5. Extension matches an inline-eligible type but not in `inline_set` → `Subprocess(binary)`
6. Everything else → `Subprocess("find-extract-dispatch")`

`extractor_binary_for()` is deleted; its extension→binary mapping is absorbed
into this function.

### Inline Dispatch

New function `extract_inline()` (in `subprocess.rs` or a new `inline.rs`):

```rust
pub fn extract_inline(
    kind: InlineKind,
    path: &Path,
    cfg: &ExtractorConfig,
) -> Vec<IndexLine> {
    let result = match kind {
        InlineKind::Text   => find_extract_dispatch::dispatch_from_path(path, cfg),
        InlineKind::Html   => find_extract_html::extract(path, cfg),
        InlineKind::Media  => find_extract_media::extract(path, cfg),
        InlineKind::Office => find_extract_office::extract(path, cfg),
    };
    match result {
        Ok(lines) => lines,
        Err(e) => {
            warn!("inline extraction failed for {}: {e:#}", path.display());
            vec![]
        }
    }
}
```

All extractor library functions return `anyhow::Result<Vec<IndexLine>>`. On
error, `extract_inline` logs a warning and returns an empty vec — matching the
behaviour of a subprocess `Failed` outcome (file is indexed by filename only).

Text routes through `find_extract_dispatch::dispatch_from_path` since dispatch
is the existing fallback for all text/code types and has no dedicated
single-purpose library. (`dispatch_from_bytes` is infallible but requires
pre-read content; `dispatch_from_path` is the path-based API.)

`dispatch_from_path` internally checks whether any specialist extractor (html,
media, office, pdf, etc.) claims the file. This is safe because the resolver's
ordering ensures no specialist-claimed extension ever reaches `InlineKind::Text`
— html, media, office, pdf, and archive are all matched earlier in resolution
order.

`extract_inline` is synchronous. CPU-bound extraction called directly from the
async `process_file()` will block the Tokio executor thread. This is an accepted
trade-off for this change; `spawn_blocking` wrapping is out of scope.

### process_file() Dispatch (scan.rs)

The existing three-way match becomes a five-arm match on `ExtractorRoute`:

- `External(stdout)` → `run_external_stdout()` (unchanged)
- `External(tempdir)` → `run_external_tempdir()` (unchanged)
- `Archive` → `start_archive_subprocess()` (unchanged)
- `Subprocess(bin)` → `extract_via_subprocess()` (unchanged)
- `Inline(kind)` → `extract_inline(kind, path, cfg)` (new arm)

### watch.rs

The extraction dispatch in watch.rs (currently lines 481–503) receives the same
treatment: call `resolve_extractor` with `inline_set = &[InlineKind::Text]` and
add the `Inline` arm calling `extract_inline()`. All other arms unchanged.

### Cargo Dependencies

`find-client/Cargo.toml` gains direct dependencies on the extractor libraries:

```toml
find-extract-html   = { path = "../../crates/extractors/html" }
find-extract-office = { path = "../../crates/extractors/office" }
```

`find-extract-dispatch` and `find-extract-media` are already direct dependencies
of `find-client` (dispatch via `dispatch_from_bytes` in subprocess.rs; media
for `detect_kind`). Only `find-extract-html` and `find-extract-office` are
genuinely new entries in `Cargo.toml`.

## Files Changed

| File | Change |
|------|--------|
| `crates/client/src/subprocess.rs` | Add `ExtractorRoute`, `InlineKind`, `extract_inline()`; extend `resolve_extractor`; delete `extractor_binary_for()` |
| `crates/client/src/scan.rs` | Update `process_file()` to match on `ExtractorRoute`; pass `inline_set` to resolver |
| `crates/client/src/watch.rs` | Update extraction dispatch; pass `inline_set = &[Text]` |
| `crates/client/Cargo.toml` | Add find-extract-html, find-extract-office deps (media and dispatch already present) |

## Testing

**Unit tests on `resolve_extractor`** (extend existing tests in subprocess.rs):
- `.html` + `inline_set = &[Html]` → `Inline(Html)`
- `.html` + `inline_set = &[]` → `Subprocess("find-extract-html")`
- `.pdf` + any inline_set → `Subprocess("find-extract-pdf")`
- `.zip` + any inline_set → `Archive`
- `.rar` with user-configured external → `External(...)`

**Unit tests on `extract_inline`**: call on a real file of each type, assert
non-empty lines returned. Fast, no subprocess overhead.

**Regression**: existing integration/scan tests cover end-to-end correctness;
output is identical (same library functions, just called in-process rather than
via JSON round-trip).

**Binary size sanity check**: `ls -lh` on find-scan and find-watch before/after
to confirm acceptable size increase (informational, not a gate).

## Non-Goals

- Inlining EPUB, PE — less common, not worth the binary size cost.
- Changing find-watch to inline anything beyond text — memory footprint concern
  for a persistent daemon.
- Inlining PDF or archive — subprocess isolation is the point for these.
