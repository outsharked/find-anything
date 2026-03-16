# File Content Pagination

## Overview

Large text files and text extracts (logs, source files, extracted PDFs) can
crash or freeze the browser when the full content is returned in a single
`/api/v1/file` response. This plan adds paginated line loading using the same
intersection-observer / load-more scroll pattern already used for search
results, with a configurable page-size threshold that controls when pagination
kicks in.

Both paths through the file viewer — original text files and text extracts —
use the same `/api/v1/file` endpoint, so a single change to that endpoint and
its TypeScript wrapper covers both.

---

## Design Decisions

### Single endpoint, new optional params

Add `offset` and `limit` to `FileParams`. When `limit` is absent the endpoint
returns all lines (backward compatible). When `limit` is present, the server
returns only `lines[offset .. offset+limit]` for content lines
(`line_number > 0`).

Metadata lines (`line_number = 0` — file path, EXIF data, duplicate aliases)
are always returned in full regardless of pagination because they are small and
the client needs them to render the metadata panel and duplicate-path list.

### `total_lines` becomes a real count

Currently `FileResponse.total_lines` is just `lines.len()`. With pagination the
field must reflect the true count of content lines for the file so the UI can
know whether there are more pages. This requires a `SELECT COUNT(*)` alongside
the content query (fast: the `lines` table has an index on `file_id`).

### Threshold lives in server settings

A new `file_view_page_size: usize` field (default 2000) is added to
`ServerSettings` / `ServerAppSettings` and included in the `GET
/api/v1/settings` response. The web UI reads it from the settings it already
fetches on startup, so the threshold is consistent across all clients and
adjustable per deployment without a UI rebuild.

When `total_lines <= file_view_page_size` the UI fetches everything in one
request (existing behaviour, no visible change). When `total_lines >
file_view_page_size` the UI enters paged mode.

### Page-start anchoring to the active selection

When a file is opened from a search result the user wants to see the matched
line, which may be deep in a large file. To avoid starting at line 1 and
making the user scroll for thousands of lines:

- The first fetch uses `offset = floor(selection_start / page_size) * page_size`
  so the selected line is in the first returned page.
- Lines *before* the anchor page are not back-filled automatically; a
  "Load earlier lines" sentinel at the top of the list lets the user scroll
  upward if needed.

When there is no selection (file opened from the tree or Ctrl+P), the first
fetch starts at `offset = 0`.

### Scroll-based load-more (same pattern as search results)

An `IntersectionObserver` watches a sentinel `<div>` appended after the last
rendered line. When it enters the viewport the next page is fetched and its
lines are appended. A second sentinel above the first rendered line triggers
backward loading (only present when the anchor page is not page 0).

`loadOffset` tracks the server cursor for the forward direction;
`backwardOffset` tracks the cursor for the backward direction (the start line
of the earliest page loaded so far). Both advance independently.

### No virtual/windowed rendering (for now)

Removing already-rendered lines from the DOM as new ones are added is
substantially more complex (fixed row heights, scroll position management).
The current approach appends lines indefinitely. For the typical case
(< 50 000 lines of text) this is fine; very long files will still be slower
than virtual scrolling but will not crash immediately on open.

Virtual scrolling can be added later as a follow-up (plan 024 already
sketches this).

---

## Implementation

### 1. Config (`crates/common/src/config.rs`)

Add to `ServerAppSettings`:
```rust
/// Max lines returned per /api/v1/file page (0 = unlimited / legacy).
#[serde(default = "default_file_view_page_size")]
pub file_view_page_size: usize,
fn default_file_view_page_size() -> usize { 2000 }
```

### 2. API types (`crates/common/src/api.rs`)

**`FileParams`** — add two optional fields:
```rust
pub offset: Option<usize>,   // first content line to return (default 0)
pub limit: Option<usize>,    // max content lines to return (default: all)
```

**`FileResponse`** — `total_lines` already exists; document that it now
always reflects the true count, not `lines.len()`.

**`SettingsResponse`** — add:
```rust
pub file_view_page_size: usize,
```

### 3. DB query (`crates/server/src/db/mod.rs`)

New function (or extend `get_file_lines`):
```rust
pub async fn get_file_lines_paged(
    conn: &Connection,
    file_id: i64,
    offset: usize,
    limit: Option<usize>,
) -> Result<(Vec<ContextLine>, usize)>  // (content_lines, total_count)
```

- Metadata lines (`line_number = 0`): always fetched separately with no
  offset/limit — these rows are rare and tiny.
- Content lines: `SELECT ... WHERE file_id = ? AND line_number > 0 ORDER BY
  line_number LIMIT ? OFFSET ?`
- Total count: `SELECT COUNT(*) FROM lines WHERE file_id = ? AND line_number > 0`
- Returned tuple: `(metadata_lines + content_page, total_content_count)`
  — `total_lines` in the response = total content count.

### 4. Route handler (`crates/server/src/routes/file.rs`)

- Parse `offset` / `limit` from `FileParams` (both `Option<usize>`).
- Call the paged DB function.
- `FileResponse.total_lines` = the COUNT result.

### 5. Settings route (`crates/server/src/routes/settings.rs`)

Include `file_view_page_size: cfg.server.file_view_page_size` in
`SettingsResponse`.

### 6. TypeScript API client (`web/src/lib/api.ts`)

Update `getFile`:
```typescript
async getFile(
  source: string,
  path: string,
  archivePath: string | null,
  offset?: number,
  limit?: number,
): Promise<FileResponse>
```

Add `file_view_page_size: number` to the `Settings` TypeScript type.

### 7. `FileViewer.svelte`

Replace the single `getFile()` call on mount with a paged loading flow:

**State:**
```typescript
let pagedMode = false;
let contentLines: ContextLine[] = [];   // accumulated content lines
let metaLines: ContextLine[] = [];      // line_number=0 lines (always complete)
let totalLines = 0;
let forwardOffset = 0;    // next page start for downward loading
let backwardOffset = 0;   // start of earliest page loaded (for upward loading)
let loadingForward = false;
let loadingBackward = false;
let noMoreForward = false;
let noMoreBackward = false;
```

**`loadFile()` (replaces current `getFile()`):**
1. Compute `anchorOffset`:
   - If `selection` is set: `Math.floor(selection[0].start / pageSize) * pageSize`
   - Otherwise: `0`
2. Fetch `getFile(source, path, archivePath, anchorOffset, pageSize)`
3. Separate metadata lines from content lines
4. If `total_lines <= pageSize` (and `anchorOffset === 0`): single-page mode
   (identical to current behaviour)
5. Else: paged mode — set `pagedMode = true`, set `forwardOffset = anchorOffset
   + resp.lines.length`, set `backwardOffset = anchorOffset`

**Sentinels:** Two `<div>` elements watched by `IntersectionObserver`:
- `#sentinel-bottom`: visible when `pagedMode && !noMoreForward`
  → calls `loadForward()`
- `#sentinel-top`: visible when `pagedMode && backwardOffset > 0`
  → calls `loadBackward()`

**`loadForward()`:**
- Fetch `getFile(..., forwardOffset, pageSize)`
- Append content lines to `contentLines`
- Advance `forwardOffset += resp.lines.length`
- Set `noMoreForward = forwardOffset >= totalLines`

**`loadBackward()`:**
- Compute `prevOffset = Math.max(0, backwardOffset - pageSize)`
- Fetch `getFile(..., prevOffset, backwardOffset - prevOffset)`
- Prepend content lines to `contentLines`
- Set `backwardOffset = prevOffset`
- Set `noMoreBackward = backwardOffset === 0`

### 8. `CodeViewer.svelte`

No change needed if `FileViewer` passes the full accumulated `contentLines`
array as its `lines` prop each time it updates. The existing rendering loop
and selection highlighting both work on absolute `line_number` values, which
are stable across pages.

---

## Files Changed

| File | Change |
|------|--------|
| `crates/common/src/config.rs` | Add `file_view_page_size` to `ServerAppSettings` |
| `crates/common/src/api.rs` | Add `offset`/`limit` to `FileParams`; add `file_view_page_size` to `SettingsResponse` |
| `crates/server/src/db/mod.rs` | Add/extend `get_file_lines` to support offset + limit + COUNT |
| `crates/server/src/routes/file.rs` | Pass offset/limit to DB; set `total_lines` from COUNT |
| `crates/server/src/routes/settings.rs` | Include `file_view_page_size` in response |
| `web/src/lib/api.ts` | Add `offset`/`limit` params to `getFile`; add `file_view_page_size` to `Settings` type |
| `web/src/lib/FileViewer.svelte` | Replace single fetch with paged load flow + sentinels |

`CodeViewer.svelte`, `ImageViewer.svelte`, `MarkdownViewer.svelte` — no changes.
`ContextViewer.svelte` (`/api/v1/context`) — no changes (already windowed).

---

## Testing

1. **Small file (< page_size):** Open any small file — behaviour identical to
   today, no sentinels visible.
2. **Large text file:** Index a file with > 2000 lines (e.g. a large log or
   source file). Open it. Verify only the first 2000 lines load initially.
   Scroll to the bottom; verify next page appends. Continue until end.
3. **Selection mid-file:** Search for a term that matches line 5000 of a large
   file. Click the result. Verify the file opens with the matched line visible
   in the first loaded page, with backward-load sentinel at the top.
4. **Config override:** Set `file_view_page_size = 100` in server config.
   Verify pagination triggers on smaller files.
5. **Archive member:** Open a large `.txt` member inside a ZIP. Verify
   pagination works the same way (same endpoint).
6. **Extracted PDF text:** Open a large PDF whose text extract exceeds
   page_size. Verify same pagination behaviour.

---

## Breaking Changes

None. The `offset`/`limit` params are optional with sensible defaults.
`total_lines` is already in the response; its semantics change only for large
paged responses (it was previously always equal to `lines.len()`; now it
reflects the true total). No client version bump required.

---

## Future Work

- Virtual/windowed rendering (plan 024) to cap DOM size for very large files.
- Expose page size as a per-user setting in the Settings page.
- Keyboard shortcut to jump to a specific line number (bypasses scroll
  loading).
