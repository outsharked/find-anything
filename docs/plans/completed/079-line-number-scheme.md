# 079 — Reserved Line Number Scheme ✅ COMPLETE

## Overview

Introduce a three-zone line number convention that makes `line_number = 0`
exclusively the file path entry, eliminates the `starts_with("[PATH] ")` content
check in the filename-only search path, and fixes a pre-existing bug where
multiple metadata entries at `line_number = 0` shadow each other in content
retrieval.

## Current State (Broken)

All extractors write metadata at `line_number = 0`, the same slot as the
`[PATH]` entry added by `build_index_files`. This causes two problems:

1. **`filename_only` search** requires a `starts_with("[PATH] ")` content check
   to distinguish path entries from EXIF/audio/PE metadata — but with the
   search performance work (plan 079 predecessor) we no longer read content
   during search, making this check impossible.

2. **Shadowed metadata in retrieval**: FTS5 contentless tables accept duplicate
   rowids, so all 50 EXIF fields for a photo are indexed and *searchable*, but
   `read_chunk_for_file` uses `split('\n').nth(line_number)` on inline content
   and `chunk_offset = line_number - start_line` on ZIP chunks. Both assume
   each line_number maps to exactly one position, so only the first
   `line_number = 0` entry is ever *retrieved*. Clicking through to a matched
   EXIF field always shows the wrong metadata line.

## Proposed Scheme

| line_number | Content | Notes |
|------------|---------|-------|
| `0` | `[PATH] relative/path` | Always present, exactly one per file |
| `1` | All metadata concatenated | Present for files with metadata; `""` otherwise |
| `2+` | Content lines | 1-indexed to user: display = `line_number - 1` |

**Always-present line 1** — `build_index_files` guarantees every `IndexFile`
has a `line_number = 1` entry, inserting `""` if the extractor produced no
metadata. This maintains dense line numbering (no gaps) so the existing
`nth(line_number)` and `line_number - start_line` retrieval paths need no
changes.

## Constants

Add to `crates/extract-types/src/index_line.rs` (or a new `constants` module):

```rust
pub const LINE_PATH:          usize = 0;  // [PATH] entry — always present
pub const LINE_METADATA:      usize = 1;  // concatenated metadata — always present (may be "")
pub const LINE_CONTENT_START: usize = 2;  // first content line
```

All magic numbers (`== 0`, `> 0`, `!= 0`, `>= 1`) in extractors, client, and
server must be replaced with these constants.

## Changes by Layer

### Extractors — metadata consolidation + content offset

Each extractor must:
1. Concatenate all metadata fields into a **single** `IndexLine` at
   `LINE_METADATA` (1), space- or newline-separated. An image with 50 EXIF
   fields produces one searchable metadata line, not 50 separate lines at 0.
2. Shift content lines from `1+` to `LINE_CONTENT_START+` (2+).

| Extractor | Metadata change | Content change |
|-----------|----------------|----------------|
| `find-extract-text` | None (no metadata) | Lines 1→ to 2→ |
| `find-extract-pdf` | None (no metadata beyond path) | Lines 1→ to 2→ |
| `find-extract-media` | Concatenate all EXIF/audio/video fields → line 1 | No content lines |
| `find-extract-pe` | Move version info → line 1 | No content lines |
| `find-extract-office` | Move title/author/subject → line 1 | Lines 1→ to 2→ |
| `find-extract-epub` | Move title/author/description → line 1 | Lines 1→ to 2→ |
| `find-extract-html` | Move title/description/og tags → line 1 | Lines → start at 2 |
| `find-extract-archive` | `make_filename_line` produces line 0; no change | Member content 2→ |
| `find-extract-dispatch` | Adjust forwarded line numbers | Pass-through shift |

The `[FILE:mime]` pseudo-metadata currently written at `line_number = 0` moves
to line 1 and is concatenated with other metadata.

### `crates/client/src/batch.rs` — guarantee line 1; update content detection

**`build_index_files`** already adds `[PATH]` at line 0. After assembling
lines, add:

```rust
// Guarantee the metadata slot is always present (maintains dense line numbering).
if !all_lines.iter().any(|l| l.line_number == LINE_METADATA) {
    all_lines.push(IndexLine { line_number: LINE_METADATA, content: String::new(), .. });
}
```

**Content-vs-metadata detection** — two places use `line_number > 0` to test
"is this a content line?" (FileKind promotion logic in `build_member_index_files`
and the mime-detection branch). Change to `line_number >= LINE_CONTENT_START`.

**Archive member handling** — `content_lines.retain(|l| l.line_number != 0)`
strips the extractor's own filename line before adding the composite `[PATH]`.
After the change, line 1 (metadata) must be *kept*; only line 0 is stripped:
```rust
content_lines.retain(|l| l.line_number != LINE_PATH);
```

### `crates/client/src/scan.rs` — content detection

Same `> 0` → `>= LINE_CONTENT_START` fix for the FileKind promotion logic and
the mime-type check.

### `crates/server/src/normalize.rs` — content-line filter

Normalization operates only on content lines. Several places filter with
`line_number > 0` or `line_number != 0` to exclude the path entry. These must
change to `line_number >= LINE_CONTENT_START` so the metadata line (1) is also
excluded from word-wrap and formatter passes.

### `crates/server/src/db/constants.rs` — updated FTS constants

`SQL_FTS_FILENAME_ONLY` (`rowid % 1_000_000 == 0`) is unchanged — it already
correctly selects `line_number = 0`.

Add:
```rust
/// FTS rowid condition matching the metadata line (line_number = 1).
pub const SQL_FTS_METADATA_ONLY: &str = "(lines_fts.rowid % 1000000) = 1";

/// FTS rowid condition matching content lines (line_number >= 2).
pub const SQL_FTS_CONTENT_ONLY:  &str = "(lines_fts.rowid % 1000000) >= 2";
```

### `crates/server/src/routes/search.rs` — simplify filename_only

Replace:
```rust
candidates.retain(|c| c.content.starts_with("[PATH] "));
```
With:
```rust
candidates.retain(|c| c.line_number == LINE_PATH);
```
No content read required — line_number is already available from the FTS
rowid without touching ZIPs.

### `crates/server/src/worker/pipeline.rs` — update filename_only_file / outer_archive_stub

`filename_only_file` and `outer_archive_stub` create synthetic `IndexFile`
values with a single `line_number = 0` entry. These should additionally include
an empty `line_number = 1` entry to maintain density.

### Schema version

Bump `SCHEMA_VERSION` from 3 to 4. Existing databases with the old line
numbering are incompatible — the server will refuse to start and instruct the
user to delete `data_dir/sources/` and re-run `find-scan`.

No SQL table changes are needed. The schema SQL file gains no new tables or
indexes; only the version constant changes.

### Display line numbers

Content was previously at `line_number = 1, 2, 3, ...` (displayed as
"line 1, 2, 3"). After the change, content starts at `line_number = 2`
(displayed as "line 1, 2, 3" → `display = line_number - 1`).

Add `content_line_start: 2` to `GET /api/v1/settings` so clients can
compute `display_line = line_number - (content_line_start - 1)` without
hardcoding the offset.

The web UI must update any line number display to subtract 1 for
`line_number >= 2`.

## Files Changed

| File | Change |
|------|--------|
| `crates/extract-types/src/index_line.rs` | Add `LINE_PATH`, `LINE_METADATA`, `LINE_CONTENT_START` constants |
| `crates/extractors/text/src/lib.rs` | Content 1→ to 2→ |
| `crates/extractors/pdf/src/lib.rs` | Content 1→ to 2→ |
| `crates/extractors/media/src/lib.rs` | Concatenate all fields → line 1 |
| `crates/extractors/pe/src/lib.rs` | Move version info → line 1 |
| `crates/extractors/office/src/lib.rs` | Metadata → line 1; content 1→ to 2→ |
| `crates/extractors/epub/src/lib.rs` | Metadata → line 1; content 1→ to 2→ |
| `crates/extractors/html/src/lib.rs` | Metadata → line 1; content → 2→ |
| `crates/extractors/archive/src/lib.rs` | Member content 1→ to 2→ |
| `crates/extractors/dispatch/src/lib.rs` | Pass-through line number shift |
| `crates/client/src/batch.rs` | Guarantee line 1; `> 0` → `>= 2`; archive member retain fix |
| `crates/client/src/scan.rs` | `> 0` → `>= 2` for FileKind/mime detection |
| `crates/server/src/normalize.rs` | `> 0` / `!= 0` → `>= 2` throughout |
| `crates/server/src/worker/pipeline.rs` | Add empty line 1 to synthetic IndexFiles |
| `crates/server/src/db/constants.rs` | Add `SQL_FTS_METADATA_ONLY`, `SQL_FTS_CONTENT_ONLY`; update comments |
| `crates/server/src/routes/search.rs` | `filename_only` retain uses `line_number == 0` |
| `crates/common/src/api.rs` | Add `content_line_start` to settings response |
| `crates/server/src/routes/settings.rs` | Return `content_line_start: 2` |
| `crates/server/src/db/mod.rs` | `SCHEMA_VERSION` 3 → 4 |
| `web/src/...` | Display `line_number - 1` for content lines |
| Test helpers + fixtures | Update `make_text_bulk` and inline test line numbers |

## Testing

- All 143 existing lib unit tests must pass after the extractor and server changes.
- `batch.rs` unit tests update to verify line 0 = `[PATH]`, line 1 = metadata
  (or `""`), line 2 = first content line.
- `normalize.rs` unit tests verify normalization still applies only to content
  lines (>= 2) and leaves lines 0 and 1 untouched.
- New `db/search.rs` unit test: `filename_only` filter returns only line 0
  entries without inspecting content.
- Integration: index an image (has metadata), a text file (no metadata), and an
  archive member; verify search results and context retrieval are correct for all
  three.

## Breaking Changes

Full re-index required. Delete `data_dir/sources/` and run `find-scan --force`.
Clients older than this version will see `MIN_CLIENT_VERSION` rejection if any
API changes are made (none are strictly required, but `content_line_start` is
added to settings).
