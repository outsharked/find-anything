# Date Range Search

## Overview

Allow search results to be filtered by file date. For regular files this is the
filesystem mtime. For archive members, use the member's internal timestamp if
the archive format provides one; fall back to the outer archive's filesystem
mtime otherwise.

Date filtering is a non-default, opt-in feature. It does not change the default
search path at all — the filter is only applied when the user provides a date
range.

---

## Design Decisions

### Member timestamps vs. outer archive mtime

Currently all archive members inherit the outer archive's filesystem mtime.
This means searching for "files from 2020" finds archives *touched* in 2020,
not necessarily files *created* in 2020. Most archive formats store per-entry
timestamps; we should use them.

**Strategy per format:**

| Format | Timestamp source | Notes |
|--------|-----------------|-------|
| ZIP | `ExtendedTimestamp` extra field (UTC unix i32) if present; otherwise `last_modified()` DOS datetime treated as UTC | ExtendedTimestamp is set by modern tools (Info-ZIP, Python zipfile, etc.). DOS datetime has 2-second resolution and no timezone — treat as UTC (best we can do). Skip if year ≤ 1980 (DOS epoch default, means "no date set"). |
| TAR / TGZ / TBZ2 / TXZ | `entry.header().mtime()` → `u64` unix seconds | Reliable, always present |
| 7z | `entry.has_last_modified_date` + `entry.last_modified_date()` → `NtTime` | Convert: `(nt.0 - NtTime::UNIX_EPOCH.0) / 10_000_000` |
| gz / bz2 / xz (single-file) | Use outer archive's filesystem mtime directly | These are single-file wrappers with no inner members; the decompressed content is the "member" |
| Nested archives | Each level uses its own internal timestamp | A zip-inside-a-tar gets the zip entry's timestamp; members of the inner zip get the inner zip's entry timestamps |

`MemberBatch.mtime: Option<i64>` — `None` signals "caller should use outer
archive mtime." `build_member_index_files` uses
`batch.mtime.unwrap_or(outer_mtime)`.

### SQL approach for filtering

Mtime filter goes directly in the SQL JOIN query, **not** as a Rust post-filter.
This means the `LIMIT` applies to post-filter rows, so the client always gets up
to `limit` results for any date range without inflating `scoring_limit`.

```sql
-- When date params are present, append:
AND f.mtime BETWEEN ?date_from AND ?date_to
```

SQLite walks FTS5 candidates in order, JOINs to `files`, filters mtime, and
stops when LIMIT is satisfied. For narrow date ranges with common queries this
may scan more FTS5 rows than usual, but it is bounded by the total FTS5 match
count and is acceptable since the feature is non-default.

An index on `files(mtime)` makes each per-row JOIN lookup fast.

### `fts_count` with date filtering

`fts_count` currently has no JOIN (pure FTS5 — very fast). When a date filter
is active, it needs the full JOIN to count only matching files. This is
acceptable; the count is just counting, not reading ZIP chunks. The index on
`mtime` keeps it reasonable.

### Non-default / UI

The date range filter is hidden by default. It appears as an expandable "Filter"
section near the search box. No changes to the default search path when no date
params are provided.

Date inputs accept ISO date strings (`YYYY-MM-DD`). The UI converts these to
unix timestamps at midnight UTC before sending to the API.

API params are optional unix timestamp integers: `date_from` and `date_to`.
Either may be omitted to produce an open-ended range.

---

## Implementation

### Step 1: Member timestamps in archive extractor

**`crates/extractors/archive/src/lib.rs`**

1. Add `mtime: Option<i64>` to `MemberBatch`.

2. In `zip_from_archive`: extract timestamp per entry:
   - First check `entry.extra_data_fields()` for
     `ExtraField::ExtendedTimestamp(ts)` → `ts.mod_time()` → `Option<i32>` unix
     seconds. Use if present.
   - Otherwise call `entry.last_modified()` → `zip::DateTime`. If
     `dt.year() > 1980`, convert via `dt.to_time()` (requires `zip` crate's
     `time` feature) → `OffsetDateTime::unix_timestamp()`. Otherwise `None`.
   - Set `batch.mtime = Some(unix_secs)` or `None`.
   - Enable `zip`'s `time` feature in `Cargo.toml` if not already present.

3. In `tar_streaming`: `entry.header().mtime().ok().map(|t| t as i64)` →
   `batch.mtime`.

4. In `sevenz_process_entry`: if `entry.has_last_modified_date`, convert
   `entry.last_modified_date()` (`NtTime`) to unix seconds:
   ```rust
   let unix = (entry.last_modified_date().0
       .saturating_sub(sevenz_rust2::NtTime::UNIX_EPOCH.0)) / 10_000_000;
   Some(unix as i64)
   ```
   Otherwise `None`.

5. Single-file `Gz`/`Bz2`/`Xz` path (`single_compressed`): these have no inner members — the decompressed content is the file. The `mtime` is passed in from the caller (outer archive's filesystem mtime) and used directly; no change needed here.

6. Nested archive recursion (`handle_nested_archive`): the recursion path
   produces `MemberBatch` items for inner members. Inner members set their own
   `mtime` from the inner archive's entry metadata, same as above.

### Step 2: Thread mtime through the client

**`crates/client/src/scan.rs`**

In `build_member_index_files`, change:
```rust
for file in build_member_index_files(rel_path, mtime, size, member_batch.lines, content_hash) {
```
to:
```rust
let member_mtime = member_batch.mtime.unwrap_or(mtime);
for file in build_member_index_files(rel_path, member_mtime, size, member_batch.lines, content_hash) {
```

No other client changes needed.

### Step 3: Schema — add mtime index

**`crates/server/src/schema_v2.sql`** (or wherever the schema lives):

```sql
CREATE INDEX IF NOT EXISTS idx_files_mtime ON files(mtime);
```

Add this to the schema file and ensure the server applies it on startup (either
in the schema init path or as a separate `PRAGMA`-free `CREATE INDEX IF NOT
EXISTS` call in `db::open`).

### Step 4: Search query changes

**`crates/server/src/db/search.rs`**

Add a `DateFilter` struct (or two `Option<i64>` params) to `fts_candidates`,
`fts_count`, and `document_candidates`.

```rust
pub struct DateFilter {
    pub from: Option<i64>, // unix timestamp seconds, inclusive
    pub to: Option<i64>,   // unix timestamp seconds, inclusive
}
```

Build the mtime WHERE clause fragment dynamically:

```rust
fn mtime_clause(f: &DateFilter) -> (&'static str, &'static str) {
    match (f.from.is_some(), f.to.is_some()) {
        (true,  true)  => ("AND f.mtime >= ?N AND f.mtime <= ?M", ...),
        (true,  false) => ("AND f.mtime >= ?N", ...),
        (false, true)  => ("AND f.mtime <= ?N", ...),
        (false, false) => ("", ...),
    }
}
```

In practice, since rusqlite uses positional `?` params, build the SQL string
once and push the corresponding param values conditionally. A simple approach:
always pass both bounds, using `0` for missing `from` and `i64::MAX` for missing
`to`, and always include `AND f.mtime BETWEEN ?N AND ?M`. When both are absent,
skip adding the clause entirely.

**`fts_candidates`**: add `AND f.mtime BETWEEN ?3 AND ?4` after the existing
WHERE clause when a filter is active.

**`fts_count`**: when a filter is active, add the JOIN to `lines` and `files`
plus the mtime condition. When no filter: keep the existing fast no-JOIN query.

**`document_candidates`**: filter `qualifying_ids` by mtime after the
per-token intersection step (a single `SELECT id FROM files WHERE id IN (...)
AND mtime BETWEEN ? AND ?` against the candidate set), rather than modifying the
per-token FTS5 queries.

### Step 5: API route

**`crates/server/src/routes/search.rs`**

Parse two optional query params from the URL: `date_from` and `date_to` (unix
timestamp integers). Add them to `SearchParams` and thread down to the db
functions as a `DateFilter`.

**`crates/common/src/api.rs`** (if `SearchParams` is defined there — otherwise
stays in `routes/search.rs`): no change needed; `SearchParams` is not part of
the shared API types.

### Step 6: Web UI

**Replace `SourceSelector` with a new `AdvancedSearch.svelte` component.**

The existing `SourceSelector.svelte` (a dropdown in the topbar) is removed. In
its place, an "Advanced" button opens a popout panel containing both the sources
selector and the date range filter. This consolidates all search filters in one
place.

**`web/src/lib/AdvancedSearch.svelte`** (new, replaces `SourceSelector.svelte`):

- Button in the topbar labelled "Advanced" (or a sliders/filter icon).
- Shows an active indicator (accent border + count badge) when any filter is
  active: sources filtered OR either date set.
- Clicking opens a popout panel (same `clickOutside` pattern as the current
  `SourceSelector` dropdown), containing:
  - **Sources** section: "All sources" reset button + per-source checkboxes
    (same behaviour as current `SourceSelector`)
  - **Date range** section: two `<input type="date">` fields labelled "From"
    and "To", both optional
  - A "Clear filters" link/button when any filter is active
- Dispatches a single `change` event with the combined filter state:
  ```ts
  { sources: string[]; dateFrom?: number; dateTo?: number }
  ```
- Dates are converted to unix timestamps at midnight UTC before being included
  in the event.

**`web/src/lib/SearchView.svelte`**:

- Replace `<SourceSelector>` with `<AdvancedSearch>`.
- Propagate `selectedSources`, `dateFrom`, `dateTo` from parent state.
- Replace the `sourceChange` event with a combined `filterChange` event
  carrying `{ sources, dateFrom, dateTo }`.

**`web/src/routes/+page.svelte`**:

- Replace `selectedSources` + `sourceChange` handler with a unified `filters`
  state object `{ sources: string[], dateFrom?: number, dateTo?: number }`.
- Pass `dateFrom` / `dateTo` through to the search call.

**`web/src/lib/api.ts`**:

- Add `dateFrom?: number` and `dateTo?: number` to the search params type.
- Include them as `&date_from=...&date_to=...` in the search URL when set.

---

## Files Changed

| File | Change |
|------|--------|
| `crates/extractors/archive/src/lib.rs` | Add `mtime: Option<i64>` to `MemberBatch`; extract per-entry timestamps for ZIP, TAR, 7z |
| `crates/extractors/archive/Cargo.toml` | Enable `zip` crate's `time` feature |
| `crates/client/src/scan.rs` | Use `member_batch.mtime.unwrap_or(outer_mtime)` in `build_member_index_files` call |
| `crates/server/src/db/search.rs` | Add `DateFilter`; thread mtime clause into `fts_candidates`, `fts_count`, `document_candidates` |
| `crates/server/src/routes/search.rs` | Parse `date_from`/`date_to` params; construct `DateFilter`; pass to db functions |
| `crates/server/src/schema_v2.sql` (or `db::open`) | `CREATE INDEX IF NOT EXISTS idx_files_mtime ON files(mtime)` |
| `web/src/lib/api.ts` | Add optional `dateFrom`/`dateTo` params to search call |
| `web/src/lib/AdvancedSearch.svelte` (new) | Popout panel with sources selector + date range filter, replaces `SourceSelector` in the topbar |
| `web/src/lib/SourceSelector.svelte` | Deleted — functionality absorbed into `AdvancedSearch.svelte` |
| `web/src/lib/SearchView.svelte` | Swap `<SourceSelector>` for `<AdvancedSearch>`; replace `sourceChange` event with `filterChange` |
| `web/src/routes/+page.svelte` | Unified filter state; pass dates to search |

---

## Testing

- Unit test: ZIP entry with `ExtendedTimestamp` → correct unix timestamp in `MemberBatch`
- Unit test: ZIP entry with only DOS datetime (year > 1980) → plausible unix timestamp
- Unit test: ZIP entry with default DOS date (year 1980) → `mtime: None`
- Unit test: TAR entry → `mtime` set from header
- Unit test: 7z entry with `has_last_modified_date` → correct unix timestamp
- Integration: scan a test archive, check DB mtime for a member matches entry timestamp
- Integration: search with `date_from`/`date_to` that includes/excludes known files
- Integration: search with no date params → no regression in results or performance

## Breaking Changes

None. `MemberBatch.mtime` is a new optional field (default `None`); existing
serialized batches in flight during upgrade will deserialize with `mtime: None`
and fall back to outer archive mtime, which was the previous behaviour.
