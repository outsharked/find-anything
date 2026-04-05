# Code Quality Refactors — Round 3 (April 2026)

## Overview

Findings from a third systematic code quality review of the find-anything
codebase. Covers panic risk, implicit ordering dependencies, invariant
enforcement, duplication, argument bloat, missing tests, and UI complexity.

Analysis date: 2026-04-01

---

## High Priority

### [H1] Excessive Arc cloning in `spawn_blocking` closure — `worker/request.rs:60`
**Category:** Concurrency
**Problem:** `process_request_async` spawns a blocking task that moves clones of
`ctx` and `handles` into the closure. Before the recent refactor this was 8
individual clones; it still clones the entire struct (including all Arc fields)
by value. Each Arc clone is cheap individually but the pattern signals a design
smell: the blocking task holds an owned copy of data that isn't logically "its
own".
**Risk:** Adding a new field to `IndexerHandles` silently adds another clone.
Code readability suffers — maintainers must verify each field is actually needed
in the closure.
**Recommendation:** Pass `Arc<IndexerHandles>` so the closure captures a single
cheap Arc clone instead of cloning all fields individually.

---

### [H2] Broadcast send errors silently discarded — `worker/request.rs:367`
**Category:** Error Handling
**Problem:** Every activity log event is sent with `let _ = recent_tx.send(...)`.
If the broadcast channel has no receivers (e.g., all SSE clients disconnected),
the send fails silently. No log or metric indicates this.
**Risk:** Live activity updates stop reaching connected clients with no
visibility. Difficult to diagnose in production.
**Recommendation:** Log at `debug` level when send fails. If a failure counter
exists, increment it.

---

### [H3] Rowid encoding constant not centralised — `crates/server/src/db/`
**Category:** Coupling / Invariant Enforcement
**Problem:** The rowid encoding formula `file_id × 1_000_000 + line_number` is
load-bearing for all FTS5 queries and search result decoding. The multiplier
`1_000_000` is defined as `MAX_LINES_PER_FILE` but is also embedded directly
into SQL strings in `db/mod.rs` and `db/search.rs`. Changing the constant
without updating all SQL sites silently corrupts the index.
**Risk:** Any tuning of `MAX_LINES_PER_FILE` is a high-risk, multi-site refactor
with no compile-time safety net.
**Recommendation:** Replace all SQL literals with a call to `encode_fts_rowid()`
or expose a SQL scalar function that wraps the formula. Add a startup assertion
that the constant matches the value embedded in the schema.

---

### [H4] Worker-only write invariant not enforced by types — `crates/server/src/db/`
**Category:** Invariant Enforcement
**Problem:** The design invariant "only the worker writes to SQLite" is enforced
by convention, not the type system. Write functions in `db/mod.rs` are `pub(crate)`,
meaning any code in the server crate can call them. A future route handler could
accidentally write to the DB, bypassing the worker and creating write contention.
**Risk:** Silent correctness bug or SQLITE_BUSY errors under concurrent load.
Future maintainers may not know the invariant.
**Recommendation:** Add a prominent `// WRITE PATH — only call from worker/` comment
at the top of every write function in `db/mod.rs`. Longer term: move write
functions into `worker/db_writes.rs` so the module boundary enforces the
invariant structurally.

---

### [H5] DICOM-before-media dispatch order is load-bearing but undocumented — `dispatch/src/lib.rs:28`
**Category:** Implicit Ordering
**Problem:** The extractor dispatch order (PDF → DICOM → media → HTML → office
→ EPUB → PE → text → MIME fallback) is hard-coded and order-dependent. DICOM
must precede media because `accepts_bytes()` checks magic bytes at offset 128;
if media ran first, extensionless DICOM files might be misclassified. There is
no comment or type enforcement explaining this dependency.
**Risk:** Reordering extractors (e.g., adding a new one) silently breaks DICOM
detection for extensionless files.
**Recommendation:** Add a comment block at the top of `dispatch_from_bytes()`
explaining the ordering rationale for each step, especially the DICOM/media
dependency.

---

## Medium Priority

### [M1] Boolean parameter `skip_inner_delete` — `worker/pipeline.rs:40`
**Category:** Boolean Param
**Problem:** `process_file_phase1_fallback(conn, file, false, store)` — the
third argument is a bare `false` with no semantic label at the call site.
Readers must look up the signature to understand it.
**Risk:** Non-self-documenting call sites; boolean proliferation if more flags
are added.
**Recommendation:** Replace with an enum:
```rust
enum MemberCleanup { DeleteStaleMembers, PreserveMembers }
```
Call sites become `MemberCleanup::PreserveMembers`, which is self-documenting.

---

### [M2] Archive member `::` separator used without a constant — `batch.rs`, `db/mod.rs`, `scan.rs`, `watch.rs`
**Category:** Duplication / Magic String
**Problem:** The `::` separator for composite archive paths is constructed with
`format!("{}::{}", outer, member)` and split with `split_composite()` in multiple
files. The separator string itself is not a named constant — it is a string
literal in each location.
**Risk:** If the separator ever needs to change, every construction and split
site must be found and updated. Easy to miss one.
**Recommendation:** The `split_composite` function already lives in
`find_common::path` — verify the module also exports a `COMPOSITE_SEP: &str =
"::"` constant and that all format sites use it.

---

### [M3] `LINE_METADATA` / `LINE_CONTENT_START` values used as raw integers — `worker/pipeline.rs`, `batch.rs`
**Category:** Magic Constants
**Problem:** The reserved line number scheme defines `LINE_PATH = 0`,
`LINE_METADATA = 1`, `LINE_CONTENT_START = 2` in `find_extract_types`. Some
sites in `pipeline.rs` and `batch.rs` construct `IndexLine` with
`line_number: 0` or `line_number: 1` directly instead of using the constants.
**Risk:** If the scheme ever changes, numeric literal sites are missed.
**Recommendation:** Grep for `line_number: 0` and `line_number: 1` outside test
code and replace each with the named constant.

---

### [M4] Batch accumulation logic not shared between `scan.rs` and `watch.rs`
**Category:** Duplication
**Problem:** Both modules independently manage a `Vec<IndexFile>` batch with
size/byte/interval flush conditions and call `submit_batch()`. The flush
condition (`batch.len() >= batch_size || batch_bytes >= limit || elapsed >= interval`)
is similar in both but not identical.
**Risk:** A fix to batch flushing (e.g., adding a new flush condition) must be
applied in two places. Divergence is likely over time.
**Recommendation:** `ScanContext` in `scan.rs` already encapsulates this logic.
Consider exposing `BatchContext` from `batch.rs` and having `watch.rs` use it
directly.

---

### [M5] No test for outer archive sentinel and member deletion — `worker/pipeline.rs:53`
**Category:** Missing Tests
**Problem:** When a re-indexed outer archive is processed, `process_file_phase1`
deletes all `LIKE 'path::%'` rows before inserting new members. This path is
not covered by any test — neither the deletion nor the case where
`skip_inner_delete = true` (used for failed archives).
**Risk:** A regression here silently orphans archive members or incorrectly
deletes members that should be preserved.
**Recommendation:** Add tests:
- `test_outer_archive_reindex_deletes_stale_members`
- `test_outer_archive_fallback_preserves_members`

---

### [M6] No test for watch event collapse state machine — `watch.rs:59`
**Category:** Missing Tests
**Problem:** The `collapse()` function (Create+Modify→Create, Delete+Create→Modify,
etc.) has no unit tests. The `Delete + Update → Update` transition is
particularly non-obvious.
**Risk:** Changes to the event loop or accumulator logic can silently break
collapse semantics.
**Recommendation:** Add a parameterized test table covering all documented
transitions with expected outcomes.

---

### [M7] FTS cleanup silently skipped when content store unavailable — `worker/pipeline.rs:125`
**Category:** Implicit Ordering / Observability
**Problem:** On re-index, old FTS entries are cleaned up only if the content
store has the old blob. If the archive phase hasn't completed yet (content store
doesn't have it), cleanup is silently skipped and stale FTS entries accumulate.
No log records this.
**Risk:** Over time, FTS table grows with unreachable entries. No visibility into
how often this occurs.
**Recommendation:** Log at `debug` when FTS cleanup is skipped:
`"skipping FTS cleanup for {path}: old content not yet archived"`.

---

## Low Priority

### [L1] Oversized files — `scan.rs`, `watch.rs`, `+page.svelte`
**Category:** Code Organization
**Problem:**
- `crates/client/src/scan.rs` ~1360 lines: scan loop, context struct, batch accumulation, process_file, archive streaming all mixed.
- `crates/client/src/watch.rs` ~1330 lines: event loop, rename detection, batch submission, config loading all mixed.
- `web/src/routes/+page.svelte` ~840 lines: search, file view, history, live updates.
**Recommendation:**
- Extract `ScanContext` and batch logic from `scan.rs` into `scan_context.rs`.
- Extract `process_renames` and rename detection from `watch.rs` into `watch_rename.rs`.
- Split `+page.svelte` into `<SearchPanel>` and `<FileViewPanel>` components.

---

### [L2] Formatter subprocess timeout hardcoded — `normalize.rs:~217` ✅ DONE
**Category:** Configuration
**Problem:** External formatters had hardcoded timeouts (`BATCH_FORMATTER_TIMEOUT` 60s,
`PER_FILE_FORMATTER_TIMEOUT` 10s) with `#[cfg(test)]` overrides.
**Resolution:** Added `batch_formatter_timeout_secs` (default 60) and
`per_file_formatter_timeout_secs` (default 10) to `NormalizationSettings` in `config.rs`.
Constants removed; values threaded through `apply_batch_formatter` /
`apply_batch_formatter_per_file`. Test `batch_cfg` helper uses 2s to keep
tests fast.

---

### [L3] Path prefix not escaped in LIKE patterns — `db/search.rs:~158`
**Category:** Input Validation
**Problem:** User-supplied `path_prefix` is used in SQL LIKE clauses without
escaping `%` or `_`. A prefix containing these characters could expand the
result set unexpectedly.
**Risk:** Low impact (filtering only), but a subtle correctness bug.
**Recommendation:** Escape `%` and `_` in prefix before passing to LIKE, or use
the range-scan pattern (`path >= prefix AND path < prefix_bump`) already used in
the tree endpoint.

---

### [L4] Search result state scattered across 7+ variables — `+page.svelte:73`
**Category:** UI Complexity
**Problem:** Search result state uses `results`, `totalResults`, `resultsCapped`,
`searching`, `searchError`, `resultsStale`, `deletedPaths` as separate reactive
variables. Live update handling (`$liveEvent`) updates some of these in one
reactive block and others inline.
**Recommendation:** Group into a single object: `let searchState = { results,
total, capped, error, stale, deletedPaths }` with a single reactive update path.

---

### [L5] Dispatch order rationale undocumented inline — `dispatch/src/lib.rs`
**Category:** Code Clarity
**Problem:** Each extractor in the dispatch chain is tried in sequence, but only
a top-level comment names the order. The rationale for why each step comes where
it does (e.g., HTML before text, PDF before everything) is not explained.
**Recommendation:** Add a numbered comment above each `if` block explaining
briefly why it occupies that position.

---

## Suggested Implementation Order

| Priority | Item | Effort | Value |
|----------|------|--------|-------|
| 1 | **H5** — Document dispatch order in dispatch/src/lib.rs | Tiny | Prevents regressions |
| 2 | **M1** — `MemberCleanup` enum for `skip_inner_delete` | Small | Readability |
| 3 | **M2** — Verify/add `COMPOSITE_SEP` constant | Small | DRY |
| 4 | **M3** — Replace raw `0`/`1` line numbers with constants | Small | DRY |
| 5 | **H2** — Log broadcast send failures | Small | Observability |
| 6 | **H4** — Add write-path comments to db/mod.rs write functions | Small | Invariant documentation |
| 7 | **H3** — Centralise rowid encoding in SQL | Medium | Correctness safety |
| 8 | **M5** — Archive sentinel tests | Medium | Regression prevention |
| 9 | **M6** — Watch collapse state machine tests | Medium | Regression prevention |
| 10 | **M4** — Share BatchContext between scan and watch | Medium | DRY |
| 11 | **H1** — `Arc<IndexerHandles>` to reduce clone noise | Small | Clarity |
| 12 | **L2** — Configurable formatter timeout ✅ | Small | Usability |
| 13 | **L3** — Escape LIKE prefix | Small | Correctness |
| 14 | **L1** — Split oversized files | Large | Maintainability |
| 15 | **L4** — Consolidate search state in +page.svelte | Medium | UI clarity |

---

## Summary Table

| # | Severity | Category | File | One-line description |
|---|----------|----------|------|---------------------|
| H1 | High | Concurrency | worker/request.rs:60 | `IndexerHandles` cloned by value into spawn_blocking |
| H2 | High | Error Handling | worker/request.rs:367 | Broadcast send errors silently discarded |
| H3 | High | Invariant | db/mod.rs, db/search.rs | Rowid multiplier embedded in SQL strings, not centralised |
| H4 | High | Invariant | db/mod.rs | Worker-write-only invariant enforced only by convention |
| H5 | High | Implicit Ordering | dispatch/src/lib.rs:28 | Load-bearing DICOM-before-media dispatch order undocumented |
| M1 | Medium | Boolean Param | worker/pipeline.rs:40 | `skip_inner_delete: bool` unreadable at call sites |
| M2 | Medium | Magic String | batch.rs, db/mod.rs | `::` archive separator not a named constant |
| M3 | Medium | Magic Constants | worker/pipeline.rs, batch.rs | LINE_PATH/LINE_METADATA used as raw `0`/`1` integers |
| M4 | Medium | Duplication | scan.rs, watch.rs | Batch accumulation and flush logic not shared |
| M5 | Medium | Missing Tests | worker/pipeline.rs:53 | Archive sentinel member deletion untested |
| M6 | Medium | Missing Tests | watch.rs:59 | Event collapse state machine untested |
| M7 | Medium | Observability | worker/pipeline.rs:125 | FTS cleanup silently skipped when content unavailable |
| L1 | Low | Code Size | scan.rs, watch.rs, +page.svelte | Three files over 800+ lines need decomposition |
| L2 | Low | Configuration | normalize.rs | Formatter subprocess timeout hardcoded at 5s | ✅ Done |
| L3 | Low | Input Validation | db/search.rs | LIKE path prefix not escaped for `%`/`_` |
| L4 | Low | UI Complexity | +page.svelte:73 | Search result state split across 7+ variables |
| L5 | Low | Code Clarity | dispatch/src/lib.rs | Dispatch order rationale not explained inline |
