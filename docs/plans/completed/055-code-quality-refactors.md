# Code Quality Refactors (March 2026)

## Overview

Findings from a systematic code quality review of the find-anything codebase.
Covers argument bloat, coupling, missing tests, duplication, SOLID violations,
error handling, UI complexity, and oversized files.

Analysis date: 2026-03-10

---

## High Priority

### [H1] Argument bloat in `worker.rs` processing functions — `worker.rs:~250`

**Category:** Argument Bloat / SOLID

**Problem:** The inbox worker's core processing functions carry 11 parameters each:

```rust
async fn process_phase1(
    source: &str,
    path: &str,
    kind: &str,
    mtime: i64,
    size: u64,
    lines: &[IndexLine],
    db: &Database,
    archive_mgr: &ArchiveManager,
    activity: &ActivityLog,
    source_cfg: &SourceConfig,
    cfg: &AppConfig,
) -> ...
```

`process_phase2` has a similarly bloated signature. These are called from a
tight loop in the worker and any new cross-cutting concern (e.g., rate limiting,
per-source stats) forces a signature change that cascades through every call site.

**Risk:** Every new feature that needs context in the processing loop forces a
signature change. Test helpers must replicate the full argument list.

**Recommendation:** Introduce a `WorkerContext<'a>` struct in `worker.rs`:

```rust
struct WorkerContext<'a> {
    db: &'a Database,
    archive_mgr: &'a ArchiveManager,
    activity: &'a ActivityLog,
    source_cfg: &'a SourceConfig,
    cfg: &'a AppConfig,
}
```

Then `process_phase1(source, path, kind, mtime, size, lines, &ctx)` — 7 → fewer
args, and adding new context fields touches only the struct and its construction.

---

### [H2] `db/mod.rs` — 1,113 lines, zero unit tests

**Category:** Missing Tests / Oversized File

**Problem:** `crates/server/src/db/mod.rs` (1,113 lines) contains all SQLite
operations: schema migration, FTS5 population, chunk deduplication, pruning, and
tree queries. There is not a single `#[test]` block in the file.

The most dangerous untested paths are:
- `prune_orphan_chunks()` — deletes rows based on a JOIN; a wrong predicate
  silently drops content.
- `fts_insert()` / `fts_delete()` — contentless FTS5; inserting wrong rowids
  corrupts the index silently (queries return stale results or miss matches).
- `migrate_schema()` — runs destructive ALTER TABLE / CREATE TABLE; a
  regression here corrupts every existing database on upgrade.
- `tree_children()` — the `prefix_bump` range-scan relies on byte arithmetic;
  non-ASCII prefix paths could produce wrong bounds.

**Risk:** Silent data corruption, index drift, and upgrade regressions are
undetectable without a test DB.

**Recommendation:** Add an in-memory SQLite fixture (`:memory:`) and at minimum
test:
1. Round-trip: insert a file → FTS5 hit → delete file → FTS5 miss.
2. `prune_orphan_chunks` removes only orphans, not referenced chunks.
3. `tree_children` with ASCII, Unicode, and `::` composite paths.
4. Schema migration from v2 → v3 on a pre-seeded database.

These can all run without the archive layer using mock chunk references.

---

### [H3] `normalise_path_sep` / `normalise_root` duplicated in `scan.rs` and `watch.rs`

**Category:** Duplication

**Problem:** Both `crates/client/src/scan.rs` and `crates/client/src/watch.rs`
contain identical (or near-identical) implementations of:

```rust
fn normalise_path_sep(p: &str) -> String { ... }   // backslash → forward slash
fn normalise_root(root: &str) -> String { ... }     // ensure trailing slash
```

These are non-trivial: the separator logic must handle UNC paths on Windows and
not mangle `::` composite paths. Two copies means two places to fix bugs.

**Risk:** A Windows path-handling fix applied to `scan.rs` is not applied to
`watch.rs`, causing different normalisation behaviour between initial scan and
live-watch events. This has already caused at least one bug (plan 046).

**Recommendation:** Move both functions to `crates/client/src/path_util.rs` (new
file, ~30 lines) and `pub use` them in both `scan.rs` and `watch.rs`. Add unit
tests for UNC paths, trailing slashes, and composite paths.

---

### [H4] `FileViewer.svelte` — 883 lines, mixed concerns

**Category:** Oversized File / SOLID (SRP)

**Problem:** `web/src/lib/FileViewer.svelte` is 883 lines handling:
- PDF rendering (PDF.js integration, page navigation, zoom)
- Image display
- Plain text / markdown display
- Archive member listing
- Office document rendering (iframe embed)
- File metadata header (shared across all kinds)

Each kind has its own reactive state, event handlers, and DOM sections
interleaved in one component.

**Risk:** Adding a new viewer kind (e.g., audio player, EPUB reader) means
editing a file that already has 7 concerns. A CSS change for the PDF viewer
risks breaking the image viewer layout.

**Recommendation:** Extract per-kind sub-components:
- `PdfViewer.svelte` (~150 lines) — PDF.js, pagination, zoom
- `ImageViewer.svelte` (~60 lines) — `<img>` + zoom
- `TextViewer.svelte` (~80 lines) — pre/code block, line highlighting
- `ArchiveMemberList.svelte` (~80 lines) — member tree
- `FileViewerHeader.svelte` (~50 lines) — shared metadata bar

`FileViewer.svelte` becomes a ~100-line dispatcher. The Svelte component
boundary also gives each viewer its own reactive scope, eliminating cross-kind
state leakage.

---

### [H5] `worker.rs` — 1,239 lines, five interleaved concerns

**Category:** Oversized File / SOLID (SRP)

**Problem:** `crates/server/src/worker.rs` (1,239 lines) handles:
1. Inbox polling loop (`poll_inbox`, `process_inbox_file`)
2. Per-file upsert pipeline (`process_phase1`, `process_phase2`)
3. Rename detection and path rewriting
4. Activity log writes (`ActivityLog`)
5. Archive queue management (deciding when to flush, which archive to target)

These concerns are entangled: rename detection mutates DB state, which is
guarded by the same lock that archive queue flushing acquires.

**Risk:** The file is approaching the point where a single logical change (e.g.,
plan 049 parallel inbox workers) requires understanding all 1,239 lines to avoid
introducing a race. The `ActivityLog` struct embedded in `worker.rs` is
inaccessible to tests without constructing the full worker.

**Recommendation:** Extract:
- `rename.rs` — rename detection logic and the `PendingRenames` map (~150 lines)
- `activity.rs` — `ActivityLog` struct and its DB writes (~100 lines)
- `pipeline.rs` — `process_phase1` / `process_phase2` (~300 lines)

`worker.rs` becomes the inbox polling loop that wires these together (~400 lines).
No new abstractions needed — just mechanical extraction into separate files with
`pub(super)` visibility.

---

## Medium Priority

### [M1] Route handlers read ZIP archives independently — `routes/search.rs`, `routes/context.rs`, `routes/file.rs`

**Category:** Duplication / Coupling

**Problem:** Each route handler that needs chunk content opens ZIP archives via
its own local `HashMap<ArchiveId, ZipArchive>` cache. The cache is per-request
and not shared across routes. The "open ZIP, look up member, read bytes" sequence
appears three times with subtly different error handling.

**Risk:** A fix to ZIP error handling (e.g., handling a corrupted archive
gracefully) must be applied in three places. The per-request cache means a
search result that spans 50 different archives pays the open-cost 50 times per
request.

**Recommendation:** Centralise chunk reading in `archive.rs` as
`ArchiveManager::read_chunk(archive_id, chunk_name) -> Result<Bytes>` with an
internal LRU cache (e.g., 32 open ZIPs). Routes call this single method.

---

### [M2] `::` path separator parsed in multiple locations

**Category:** Duplication

**Problem:** The composite path format `outer.zip::member.txt` is split on `::`
in at least:
- `db/mod.rs` — building `LIKE 'archive::%'` delete predicates
- `routes/tree.rs` — detecting whether a prefix is inside an archive
- `scan.rs` — filtering client-side deletion candidates
- `worker.rs` — recognising archive members during re-index

Each site uses ad-hoc string splitting. The `::` contract is not encapsulated.

**Recommendation:** Add to `find-common/src/api.rs` (or a new `path.rs`):

```rust
pub fn composite_outer(path: &str) -> &str { ... }
pub fn composite_member(path: &str) -> Option<&str> { ... }
pub fn is_composite(path: &str) -> bool { path.contains("::") }
pub fn make_composite(outer: &str, member: &str) -> String { ... }
```

All call sites use these instead of raw string operations.

---

### [M3] Error swallowing in archive rewrite — `archive.rs:~400`

**Category:** Error Handling

**Problem:** The ZIP rewrite path (removing old chunks before writing new ones)
uses `let _ = zip.start_file(...)` in the copy loop, silently ignoring errors
from the underlying writer. A partial rewrite that hits a disk-full condition
would log nothing and leave a corrupt archive.

**Risk:** Disk-full during a large re-index produces a corrupt ZIP with no
logged error. The next read from that archive returns garbage or panics.

**Recommendation:** Propagate the error: replace `let _ = ...` with `?`. The
outer transaction will roll back the SQLite side; ensure the temporary file is
also cleaned up (`std::fs::remove_file` in an error branch).

---

### [M4] `scan.rs` archive sentinel logic untested — `scan.rs:~600`

**Category:** Missing Tests

**Problem:** `scan.rs` contains logic to detect when an outer archive has
changed (by comparing mtime/size) and trigger re-indexing of all its members.
The "sentinel" path that decides whether to re-scan members uses several
conditions around `is_archive`, `mtime_changed`, and `size_changed`. There are
no tests for this logic.

**What could go wrong:** A change that modifies the sentinel condition could
cause archives to never re-index (stale content) or always re-index (performance
regression). The composite-path filtering (`contains("::")`) that prevents inner
members from being treated as outer files is a single-character guard with no
test.

**Recommendation:** Extract the sentinel decision to a pure function
`fn needs_archive_rescan(old: &FileRecord, new: &FileRecord) -> bool` and unit
test with fixtures covering: mtime changed, size changed, both unchanged,
composite path input (should always return false).

---

### [M5] `watch.rs` accumulator collapse rules untested — `watch.rs:~300`

**Category:** Missing Tests

**Problem:** The watch accumulator collapses event sequences
(`Create + Modify → Create`, `Delete + Create → Modify`, directory rename
detection) with no unit tests. The collapse logic is a nested match over
`(existing_event, new_event)` pairs.

**What could go wrong:** An event ordering that arrives out of sequence (common
on network drives) could produce a `Delete` event for a file that was
subsequently re-created, causing the client to submit an unnecessary deletion
to the server.

**Recommendation:** Extract the accumulator to a struct with a
`fn apply(&mut self, event: WatchEvent)` method and test the full transition
table (Create→Modify, Delete→Create, Rename→Rename, etc.).

---

### [M6] `+page.svelte` — implicit state machine spread across reactive blocks

**Category:** UI Complexity

**Problem:** The main search page tracks view state through a combination of:
- `searchMode: 'search' | 'tree' | 'file'`
- `selectedResult`, `fileViewerOpen`, `treeRoot`, `ctrlPOpen`
- 4–5 additional booleans controlling sub-states of each mode

The transitions between states (e.g., clicking a result while in tree mode)
are implemented as imperative mutations scattered across `on:click` handlers
and reactive `$:` blocks. There is no central state machine — illegal state
combinations (e.g., `fileViewerOpen && ctrlPOpen`) are prevented only by
convention.

**Recommendation:** Encode the view state as a discriminated union:

```ts
type ViewState =
  | { mode: 'search'; results: SearchResult[] }
  | { mode: 'tree'; root: string; prefix: string }
  | { mode: 'file'; result: SearchResult; origin: 'search' | 'tree' }
  | { mode: 'ctrlp' }
```

A single `$state<ViewState>` replaces 6+ booleans. Impossible states become
unrepresentable. Event handlers call a `transition(next: ViewState)` function
that validates the transition before applying it.

---

## Low Priority

### [L1] `db/mod.rs` — `unwrap_or_default()` on FTS5 rowid lookup

**Category:** Error Handling

**Problem:** The FTS5 content-rowid lookup uses `unwrap_or_default()` which
returns `0` on failure. Rowid 0 is a valid SQLite rowid and could cause the FTS5
delete to corrupt an unrelated entry.

**Recommendation:** Return `Err(...)` on lookup failure rather than a default.

---

### [L2] Extractor `main.rs` files are near-identical boilerplate

**Category:** Duplication

**Problem:** All 9 extractor `main.rs` files contain identical stdin→stdout
dispatch logic with only the extractor function name differing. This is 9 copies
of ~40 lines of identical error-handling boilerplate.

**Recommendation:** Add a macro or shared function in `find-extract-types`:
```rust
pub fn run_extractor<F>(extract_fn: F) where F: Fn(...) -> ... { ... }
```
Each `main.rs` becomes ~5 lines.

---

### [L3] `batch.rs` — batch size limit not enforced on content length

**Category:** Missing Tests / Error Handling

**Problem:** `batch.rs` splits submissions by file count but does not enforce a
byte-size limit on the gzip-compressed payload. A single file with 10 MB of
extracted text would be sent as a single batch, potentially hitting server
memory limits or nginx body-size restrictions.

**Recommendation:** Add a byte-budget check alongside the count check. Test with
a synthetic file that exceeds the byte budget but not the count limit.

---

## Summary Table

| # | Severity | Category | File | One-line description |
|---|----------|----------|------|---------------------|
| H1 | High | Argument Bloat | `worker.rs:~250` | process_phase1/phase2 carry 11 params; needs WorkerContext struct |
| H2 | High | Missing Tests | `db/mod.rs` | 1,113 lines of SQLite logic with zero unit tests |
| H3 | High | Duplication | `scan.rs`, `watch.rs` | normalise_path_sep / normalise_root duplicated in both files |
| H4 | High | Oversized File / SRP | `FileViewer.svelte` | 883 lines, 6 viewer kinds in one component |
| H5 | High | Oversized File / SRP | `worker.rs` | 1,239 lines, 5 interleaved concerns |
| M1 | Medium | Duplication | `routes/search.rs`, `context.rs`, `file.rs` | ZIP chunk open/read duplicated in 3 route handlers |
| M2 | Medium | Duplication | multiple | `::` composite path split at 4+ ad-hoc sites |
| M3 | Medium | Error Handling | `archive.rs:~400` | `let _ =` swallows ZIP writer errors in rewrite path |
| M4 | Medium | Missing Tests | `scan.rs:~600` | archive sentinel re-index decision logic untested |
| M5 | Medium | Missing Tests | `watch.rs:~300` | accumulator event-collapse rules untested |
| M6 | Medium | UI Complexity | `+page.svelte` | view state spread across 6+ booleans, no state machine |
| L1 | Low | Error Handling | `db/mod.rs` | unwrap_or_default on FTS5 rowid could corrupt index |
| L2 | Low | Duplication | `*/main.rs` (9 files) | identical extractor boilerplate in all 9 main.rs files |
| L3 | Low | Missing Tests | `batch.rs` | byte-size limit not enforced; only count limit checked |

---

## Implementation Priority Order

1. **H3** — Easiest win: 30-line new file, immediate duplication eliminated.
2. **H1** — `WorkerContext` struct: mechanical refactor, zero behaviour change.
3. **M2** — Composite path helpers: small new module, fixes a latent bug class.
4. **M3** — Error propagation in archive rewrite: 1-line fix with high safety value.
5. **H2** — DB test suite: significant effort but eliminates the highest-risk untested surface.
6. **H4** — FileViewer split: pure UI refactor, no logic changes.
7. **H5** — worker.rs split: mechanical file extraction, enables plan 049.
8. **M4, M5** — Test extraction: requires function extraction first (subset of H1/H5 work).
9. **M6** — View state machine: most invasive UI change, do after H4.
