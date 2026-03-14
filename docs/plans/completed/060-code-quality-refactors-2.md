# Code Quality Refactors — Round 2 (March 2026)

## Overview

Findings from a second systematic code quality review of the find-anything
codebase. Covers duplication, argument bloat, coupling, missing tests, SOLID
violations, and UI complexity.

Analysis date: 2026-03-13

---

## High Priority

### [H1] Batch-flush condition copy-pasted three times — `scan.rs:488, 599, 607`

**Category:** Duplication

**Problem:** The 3-condition flush gate:
```rust
if ctx.batch.len() >= ctx.batch_size || ctx.batch_bytes >= ctx.batch_bytes_limit
    || (!ctx.batch.is_empty() && ctx.last_submit.elapsed() >= ctx.batch_interval)
```
appears verbatim at three different points inside `process_file`: once inside
the archive member loop (~line 489), once inside the non-archive file loop
(~line 600), and once after both branches close (~line 608).

**Risk:** Adding a new flush condition (e.g., a file-count limit) requires
updating all three sites. Missing one silently creates inconsistent batching
behaviour.

**Recommendation:** Add a `should_flush() -> bool` method on `ScanContext` and
call it at each site.

---

### [H2] `timed!` macro defined in both `worker/mod.rs` and `worker/archive_batch.rs`

**Category:** Duplication

**Problem:** The timing helper macro is duplicated. Each file defines its own
`macro_rules! timed` block. If the macro is extended (e.g., to emit structured
tracing spans), one copy is guaranteed to drift.

**Risk:** Silent semantic difference between phase-1 and archive-batch timing;
confusion when reading logs that appear to report the same metric differently.

**Recommendation:** Move the macro to a `crate::worker::util` or `crate::macros`
module and import it in both files.

---

### [H3] `SourceMap` in `watch.rs` is an anonymous 5-tuple — `watch.rs:36`

**Category:** Coupling / Readability

**Problem:** `type SourceMap = Vec<(PathBuf, String, String, GlobSet, Option<HashSet<String>>)>`.
All five fields are positional. Every call site destructures with patterns like
`(root, name, root_str, includes, _terminals)`, and callers must count positions
to understand what they're accessing. `build_source_map` constructs it with
positional `.push((root, src.name.clone(), root_str, includes, terminals))` —
there is no field name to verify ordering.

**Risk:** Swapping two fields of the same type (e.g., the two `String`s) is an
invisible bug. Adding a sixth field requires touching every destructure site in
`find_source`, `is_excluded`, `process_renames`, and `watch_tree` calls.

**Recommendation:** Replace the tuple with a named struct:
```rust
struct SourceEntry {
    root: PathBuf,
    name: String,
    root_str: String,
    includes: GlobSet,
    terminals: Option<HashSet<String>>,
}
```

---

### [H4] `handle_dir_rename` has 10 arguments with one unused — `watch.rs:734`

**Category:** Argument Bloat

**Problem:** `#[allow(clippy::too_many_arguments)]` suppresses the lint. The
function signature takes `(api, source_name, _old_dir, new_dir, old_rel_dir,
new_rel_dir, source_root, source_includes, global_scan, extractor_dir)`.
`_old_dir` is accepted but never used (the underscore prefix signals this
explicitly). `source_name`, `source_includes`, `source_root`, and `global_scan`
are all derivable from a `SourceEntry` (H3).

**Risk:** When `SourceEntry` is introduced (H3), this function still keeps the
dead parameter until explicitly cleaned up.

**Recommendation:** After H3, pass `&SourceEntry` for the source-related fields.
Remove `_old_dir` (it is never read). This brings the count to ~5.

---

### [H5] Exclusion logic duplicated between main flush loop and `process_renames` — `watch.rs:196` and `watch.rs:681`

**Category:** Duplication

**Problem:** Both the main event-flush loop and the single-file rename path
independently call `resolve_watch_config`, `build_globset`, and `is_excluded` in
the same sequence. The main loop also checks `source_includes` and per-directory
`.index` patterns; `process_renames` does a partial version (skips `.index`
check, uses direct `is_excluded`). The partial version means a file excluded by
a `.index` pattern could still receive a rename instead of falling back to
delete.

**Risk:** A filter rule added to the main loop path may silently not apply to
the rename path, creating inconsistent indexing behaviour.

**Recommendation:** Extract `is_path_included(abs_path, source_map, global_scan) -> bool`
and use it in both call sites.

---

## Medium Priority

### [M1] `read_chunk_lines_zip` is a redundant subset of `read_chunk_lines` — `db/mod.rs:144`

**Category:** Duplication

**Problem:** `read_chunk_lines_zip` handles only the ZIP case (both
`chunk_archive` and `chunk_name` non-null). `read_chunk_lines` covers both ZIP
and inline. The only current callers of `read_chunk_lines_zip` could call
`read_chunk_lines` with the same `Some/Some` arguments.

**Risk:** Any fix to error handling in the ZIP-read path (e.g., a fallback on
corrupt chunk) must be applied in two places.

**Recommendation:** Delete `read_chunk_lines_zip`; update its callers to use
`read_chunk_lines`.

---

### [M2] Alias-lookup pattern verbatim-duplicated in `routes/search.rs` — `search.rs:213` and `search.rs:285`

**Category:** Duplication

**Problem:** Both the document-mode path and the regular-mode path perform
`canonical_ids → fetch_aliases_for_canonical_ids → merge aliases into results`
with near-identical code. Any change to alias handling (e.g., including
archive-member aliases) needs to be applied in both.

**Risk:** Alias behaviour diverges between search modes.

**Recommendation:** Extract:
```rust
fn attach_aliases(
    results: Vec<(SearchResult, i64)>,
    conn: &Connection,
) -> Result<Vec<SearchResult>>
```
and call it in both branches.

---

### [M3] `ArchiveManager::new_for_reading` constructs a zeroed `SharedArchiveState` — `archive.rs:188`

**Category:** SOLID (Interface Segregation)

**Problem:** Read-only consumers (all route handlers: `get_file`, `get_context`,
`context_batch`, `search`) must instantiate the full write-capable
`ArchiveManager` with a dummy `SharedArchiveState` that has zeroed counters and
empty mutex maps. The `total_archives` and `archive_size_bytes` counters on
these dummy instances are silently zero, which would corrupt stats if a
read-only instance were ever passed to stats-reporting code.

**Risk:** A future refactor that passes a read-only `ArchiveManager` to
stats-reading code would silently report zeros.

**Recommendation:** Introduce a `ChunkReader` trait:
```rust
pub trait ChunkReader {
    fn read_chunk(&self, chunk_ref: &ChunkRef) -> Result<String>;
}
```
Read-only callers take `&dyn ChunkReader`. `ArchiveManager` implements it. This
also makes unit-testing route logic easier (mock the reader).

---

### [M4] Dry-run counter update block duplicated in `scan.rs` — `scan.rs:228`

**Category:** Duplication

**Problem:** The counter increment block:
```rust
indexed += 1;
if is_new { new_files += 1; }
else if is_upgraded_file { upgraded += 1; }
else if !subdir_rescan { modified += 1; }
```
appears identically inside both the `if !opts.dry_run` branch and the `else`
branch, because the dry-run path still needs to count what *would* be indexed.

**Risk:** Adding a new counter category (e.g., tracking renamed files) requires
updating both branches.

**Recommendation:** Move the counter increments after the `if !opts.dry_run`
block and gate only the actual `process_file` call.

---

### [M5] `context_batch` bypasses `source_db_path()` helper — `context.rs:81`

**Category:** Coupling

**Problem:** `context_batch` performs inline source name validation
(`chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')`) and
db path construction (`data_dir.join("sources").join(format!("{}.db", item.source))`).
The `source_db_path()` helper in `routes/mod.rs` already does this. These are
currently consistent but could drift.

**Risk:** A rule change to source name validation applied to `source_db_path()`
silently misses `context_batch`.

**Recommendation:** Refactor `context_batch` to use `source_db_path()` for
per-item path resolution (or a variant that takes `&AppState` and a source name).

---

### [M6] `process_renames` in `watch.rs` has no tests — `watch.rs:534`

**Category:** Missing Tests

**Problem:** This 190-line function handles the most complex logic in `watch.rs`:
grouping events by parent directory, distinguishing directory renames from file
renames, the `first_seen_creates` interaction (create+rename → index as new),
and include/exclude filter re-evaluation for the new path. The collapse
transition table is well-tested; the rename pairing logic is not.

**Risk:** The "1 delete + 1 create in the same parent = rename" heuristic is
fragile. A batch with 2 deletes and 1 create in the same directory would
silently fall back to delete+index. There is no test catching this regression.

**Recommendation:** Add unit tests for:
- (a) 1:1 rename detected correctly
- (b) 2 deletes + 1 create → not treated as a rename
- (c) Cross-source rename not paired
- (d) Create+rename within one batch upgrades to `Create` kind

---

## Low Priority

### [L1] Eight interrelated date/NLP variables in `+page.svelte` — `+page.svelte:44`

**Category:** UI Complexity

**Problem:** `dateFromStr`, `dateToStr`, `dateFromTs`, `dateToTs`, `nlpResult`,
`nlpSuppressed`, `effectiveDateFrom`, `effectiveDateTo` all track aspects of the
same conceptual thing: "what date filter is active". `effectiveDateFrom` and
`effectiveDateTo` are computed in both `doSearch` (lines ~279-280) and
`handleClearNlpDate` (lines ~328-330) with duplicated logic
`dateFromTs ?? nlpResult?.dateFrom`.

**Risk:** A new NLP suppression rule needs updating in both computation sites.

**Recommendation:** Derive `effectiveDateFrom`/`effectiveDateTo` in a single
reactive statement:
```ts
$: effectiveDateFrom = dateFromTs ?? (nlpSuppressed ? undefined : nlpResult?.dateFrom);
$: effectiveDateTo   = dateToTs   ?? (nlpSuppressed ? undefined : nlpResult?.dateTo);
```
Remove the inline recomputation in `doSearch` and `handleClearNlpDate`.

---

### [L2] `regex_to_fts_terms` has no tests — `routes/search.rs:92`

**Category:** Missing Tests

**Problem:** The function extracts literal character runs from a regex for FTS5
pre-filtering. Corner cases are tricky: `\{`, `\\`, backslash at end of string,
nested character classes (`[^abc]`), escaped parentheses. There are zero tests.

**Risk:** A regex like `class\{` would emit `class` and nothing else (escaped
brace stripped), causing over-broad FTS pre-filtering and returning unrelated
results.

**Recommendation:** Add table-driven unit tests covering: escaped chars, empty
literal segments, adjacent specials, backslash at EOF.

---

### [L3] `ChunkRef.archive_name` stores filename only, hiding path reconstruction — `archive.rs:175`

**Category:** Coupling

**Problem:** `ChunkRef` stores `archive_name: String` which is just the filename
component (`content_00123.zip`). Every reader must call `parse_archive_number`
then `archive_path_for_number` to get the actual `PathBuf`. There is a hidden
fallback: if `parse_archive_number` returns `None`, the path falls back to
`sources_dir/archive_name`. This fallback is undocumented at the `ChunkRef` level.

**Risk:** New code constructing a `ChunkRef` with a full path would silently hit
the legacy fallback and look in the wrong directory.

**Recommendation:** Document the filename-only convention on `ChunkRef.archive_name`;
add `ChunkRef::archive_path(&self, state: &SharedArchiveState) -> PathBuf` to
centralise reconstruction logic.

---

## Summary Table

| # | Severity | Category | File | One-line description |
|---|----------|----------|------|---------------------|
| H1 | High | Duplication | `scan.rs:488,599,607` | Batch-flush condition copy-pasted 3× in `process_file` |
| H2 | High | Duplication | `worker/mod.rs`, `worker/archive_batch.rs` | `timed!` macro defined twice |
| H3 | High | Coupling | `watch.rs:36` | `SourceMap` is an untyped 5-tuple with no field names |
| H4 | High | Argument Bloat | `watch.rs:734` | `handle_dir_rename` has 10 params, one unused |
| H5 | High | Duplication | `watch.rs:196`, `watch.rs:681` | Exclusion logic duplicated between flush loop and rename path |
| M1 | Medium | Duplication | `db/mod.rs:144` | `read_chunk_lines_zip` is a redundant subset of `read_chunk_lines` |
| M2 | Medium | Duplication | `routes/search.rs:213,285` | Alias-lookup pattern duplicated between document and regular mode |
| M3 | Medium | SOLID | `archive.rs:188` | Read-only `ArchiveManager` silently zeroes all counters |
| M4 | Medium | Duplication | `scan.rs:228` | Dry-run counter block duplicated inside both branches |
| M5 | Medium | Coupling | `context.rs:81` | `context_batch` bypasses `source_db_path()` helper |
| M6 | Medium | Missing Tests | `watch.rs:534` | `process_renames` (190 lines) has no unit tests |
| L1 | Low | UI Complexity | `+page.svelte:44` | 8 date/NLP variables; effective dates recomputed in 2 places |
| L2 | Low | Missing Tests | `routes/search.rs:92` | `regex_to_fts_terms` has no tests |
| L3 | Low | Coupling | `archive.rs:175` | `ChunkRef.archive_name` filename-only convention undocumented |

---

## Suggested Implementation Order

1. **H2** — Move `timed!` macro: trivial, zero behaviour change.
2. **H1** — `ScanContext::should_flush()`: 1 new method, 3 call-site simplifications.
3. **M4** — Deduplicate dry-run counter block: mechanical, same file as H1.
4. **H3** — `SourceEntry` struct: rename refactor, enables H4 and H5.
5. **H4** — Remove unused `_old_dir`, shrink `handle_dir_rename`: follows H3.
6. **H5** — Extract `is_path_included`: small helper, fixes the filter inconsistency.
7. **M1** — Delete `read_chunk_lines_zip`: 1 function removed, callers updated.
8. **M2** — Extract `attach_aliases`: small helper in `routes/search.rs`.
9. **M5** — Route `context_batch` through `source_db_path()`: 5-line change.
10. **M6** — Add `process_renames` tests: most valuable test addition in this list.
11. **M3** — `ChunkReader` trait: larger refactor, do after M1/M2 settle the read path.
12. **L1** — Reactive date derivation in `+page.svelte`: safe UI-only change.
13. **L2** — `regex_to_fts_terms` tests: easy to add in isolation.
14. **L3** — Document/centralise `ChunkRef` path reconstruction.
