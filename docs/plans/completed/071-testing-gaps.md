# Testing Gaps Analysis and Remediation

## Implementation Status

| Phase | Status | Description |
|-------|--------|-------------|
| Phase 1 | ✅ Complete | CI vitest, archive write-path, `process_file_phase1`, `fts_candidates`/`fts_count` SQL tests |
| Phase 2 | ✅ Complete | Route handler test harness (extend existing `TestServer` in `tests/helpers/`) |
| Phase 3 | ✅ Complete | Worker pipeline (`request.rs`, `pipeline.rs`, `archive_batch.rs`) |
| Phase 4 | ✅ Complete | TypeScript/component layer (`ResultList` dedup, `FuzzyScorer`) |
| Phase 5 | ✅ Complete | Upload TUS, PDF panic-path, external formatter, compaction, lineSelection.ts |

---

## Overview

Analysis of test coverage across all layers: Rust unit and integration tests,
server route handlers, indexing pipeline, TypeScript logic, and Svelte
components. The goal is to identify and prioritise the highest-risk untested
paths and produce a roadmap for closing the gaps incrementally.

---

## Current Test Infrastructure

| Layer | How Run | In CI? |
|-------|---------|--------|
| Rust unit + integration | `cargo test --workspace` / `mise run test` | Yes |
| Web (vitest) | `pnpm test` in `web/` | **No** |
| Web type-check + build | `pnpm run check` + `pnpm run build` | Yes |
| Client binary integration | `cargo test -p find-client` (`#[ignore]` by default for watch) | Yes (scan only) |

**Key structural problem:** The vitest suite has 168+ tests across 5 files but
is never run in CI. Any regression in TypeScript logic is invisible to CI.

### Test fixtures available

- `crates/extractors/archive/tests/fixtures/` — real ZIP/TGZ with inner archives
  in every supported format.
- In-memory SQLite (`rusqlite::Connection::open_in_memory()`) used by DB tests.
- `tempfile::TempDir` used by filesystem tests.
- No HTTP-level test harness exists anywhere in the codebase.

---

## What Is Well-Tested (summary)

- `db/tree.rs` — directory listing, composite path splitting, nested archives
- `db/search.rs` — `build_fts_query` string construction
- `db/links.rs` — link create/resolve/expire/sweep
- `db/mod.rs` — delete, rename, FTS insert, activity log, last-scan
- `archive.rs` — number parsing, chunk splitting, archive number allocation
- `normalize.rs` — line wrapping, JSON/TOML pretty-print, extension detection
- `worker/mod.rs` — `is_outer_archive`, filename helpers
- `compaction.rs` — config parsing, time-string parsing
- `walk.rs` — directory traversal, exclude globs, `.noindex`, hidden files
- `scan.rs` — `needs_reindex`, `include_dir_prefixes`, `dir_allowed`
- `batch.rs` — `build_index_files`, archive member construction, byte budget
- `watch.rs` — `AccumulatedKind::collapse` state machine
- `subprocess.rs` — arg substitution, extractor resolution, relay-line parsing
- `path_util.rs` — path separator normalisation
- Web: `filePath.test.ts`, `imageMeta.test.ts`, `commandPaletteLogic.test.ts`,
  `nlpQuery.test.ts`, `searchPrefixes.test.ts` — thorough logic coverage

---

## Gap Inventory and Prioritisation

### Priority 1 — Critical (data correctness and security)

#### P1-A: HTTP route handlers — zero test coverage

Every route handler is completely untested at the HTTP level. This is the
primary API surface consumed by clients and the web UI.

Highest-risk routes, in order:

| Route file | Why critical |
|-----------|-------------|
| `routes/search.rs` (416 lines) | Most complex handler. Assembles results, deduplicates, scores with nucleo, formats snippets, handles all search modes (fuzzy/exact/regex/document/filename), pagination. A regression here silently returns wrong results. |
| `routes/bulk.rs` | Write path entry point — decodes gzipped `BulkRequest`, writes to inbox. Malformed input or auth bypass here corrupts the index. |
| `routes/admin.rs` (629 lines) | Pause/resume/retry/clear inbox, compact, delete_source, update-check/apply. Destructive operations with no test coverage. |
| `routes/auth` (`check_auth` in `mod.rs`) | Token validation. No test verifies that invalid tokens are rejected (401), that valid tokens are accepted, or that the session cookie path works. |
| `routes/file.rs` | File content retrieval, pagination params, link-code bypass. |
| `routes/context.rs` | Context window retrieval from ZIP archives. |
| `routes/tree.rs` | Directory listing HTTP layer. |
| `routes/links.rs` | Link create/resolve HTTP layer. |
| `routes/upload.rs` (179 lines) | TUS resumable upload state machine. |
| `routes/recent.rs` | SSE stream. |
| `routes/settings.rs`, `routes/stats.rs`, `routes/raw.rs` | Lower risk but still dark. |

**Approach:** The codebase already has a `TestServer` helper in
`crates/server/tests/helpers/mod.rs` that binds a real `TcpListener` on port 0,
spawns the full server with a `TempDir`-backed `AppState`, and wraps a
`reqwest` client with auth headers. It includes `wait_for_idle()` (polls until
the inbox worker drains) and `post_bulk()` (gzip-encodes and POSTs a
`BulkRequest`). No new dependencies or in-process test harness needed.

Phase 2 extends this existing infrastructure:
- Add an unauthenticated `reqwest::Client` variant to `TestServer` for 401 tests
- Add source-seeding helpers as needed
- Write new test files in `crates/server/tests/` alongside the existing ones

#### P1-B: Indexing worker pipeline — completely untested

`worker/request.rs` (387 lines) and `worker/pipeline.rs` (266 lines) contain
the Phase 1 processing: decode inbox `.gz` files, run deletes/renames/upserts,
call normalise, write `to-archive/` output. A bug here silently drops or
corrupts indexed data.

Specific untested paths:
- `process_file_phase1` and `process_file_phase1_fallback` — the content-hash
  dedup path (`content_hash` aliasing when content is unchanged), stale-mtime
  guard, and archive member delete-then-replace.
- The full inbox processing loop: picking up a `.gz`, running Phase 1, verifying
  the `to-archive/` output written.

`worker/archive_batch.rs` (303 lines) contains Phase 2: read `to-archive/`
batches, coalesce chunk work, call `append_chunks`/`remove_chunks`/`rewrite_archive`,
update chunk refs in SQLite. ZIP rewrite failure recovery (rollback on error) is
completely untested.

**Risk:** A failing rewrite silently loses data. The atomicity guarantee
(SQLite transaction stays open across the ZIP rewrite, rollback on failure) has
no test.

#### P1-C: `ArchiveManager` write operations — no tests

`append_chunks`, `remove_chunks`, `read_chunk`, and `rewrite_archive` in
`archive.rs` have no unit tests. These are the lowest-level storage operations.
The existing `archive.rs` tests cover only parsing/arithmetic helpers.

**Approach:** Use real temp files (`TempDir`) rather than mocks. Write a chunk,
read it back, remove it, verify the ZIP entry list.

#### P1-D: `db/mod.rs` — write and read path gaps

`upsert_file_phase1` (called for every indexed file) has no test. Neither do
`read_chunk_lines`, `get_file_record`, `get_file_content`, `get_context_lines`.
These are the hot paths for both indexing and serving search results.

---

### Priority 2 — High (search quality, core functionality)

#### P2-A: `FuzzyScorer` — no tests

`fuzzy.rs` wraps nucleo for fuzzy matching and scoring. No test verifies that
it returns relevant results, handles empty input, or scores correctly relative
to exact matches. A regression here degrades search quality invisibly.

#### P2-B: `db/search.rs` — only string helpers tested, not SQL

`fts_candidates`, `fts_count`, `document_candidates` — the complex FTS5 JOIN
queries with date filtering, kind filtering, and chunk reading — are completely
untested. The existing tests only verify `build_fts_query` output strings.

**Note:** These functions require an in-memory SQLite DB with the schema and
FTS5 index populated. The existing `db/mod.rs` test pattern shows how to do
this.

#### P2-C: `client/src/scan.rs` — full scan loop untested

`run_scan` and `scan_single_file` — which orchestrate the complete scan cycle
(query server for existing files, compute diffs, build batches, submit,
process deletions) — have no tests. `hash_file` is also untested.

The existing client integration tests (`crates/client/tests/`) cover the binary
end-to-end, but the unit-level logic of the scan loop is dark.

#### P2-D: Web vitest not in CI

**This is a quick win.** The 168-test vitest suite catches regressions in
`filePath`, `searchPrefixes`, `nlpQuery`, `commandPaletteLogic`, and
`imageMeta` — but it is never run in CI. A broken `searchPrefixes.ts` after a
refactor would not block a merge.

Fix: add `pnpm test --run` to the `web` CI job in `.github/workflows/ci.yml`.

---

### Priority 3 — Medium (important but lower blast radius)

#### P3-A: Load-more dedup logic in `ResultList.svelte` — no tests

The CLAUDE.md explicitly documents the risk: duplicate search result keys cause
an infinite request loop. The dedup filter in `triggerLoad` and the
`loadOffset` advancement rule are specified precisely in CLAUDE.md but have no
test. A future refactor that removes or breaks the dedup would not be caught
until runtime.

**Approach:** Extract the pagination/dedup logic into a plain TypeScript
function (e.g. `mergePage(existing, incoming, offset)`) and test it with vitest.

#### P3-B: TypeScript state modules — no tests

`appState.ts`, `treeStore.ts`, `liveUpdates.ts`, `lineSelection.ts`,
`settingsStore.ts` — all stateful modules — have no tests. These are harder to
test because they use Svelte stores, but pure transformation functions within
them (e.g. tree node insertion/collapse logic) could be extracted and tested.

#### P3-C: PDF extractor — only 2 tests

The PDF extractor uses a forked library specifically to avoid a panic in
`type1_encoding_parser::get_encoding_map`. This panic-avoidance path has no
test. A bad PDF (malformed Type1 fonts) exercising this path would fail silently
or crash in production without detection.

**Approach:** Add a fixture PDF with malformed Type1 font data and assert it
returns a result (any result) rather than panicking.

#### P3-D: `server/src/upload.rs` and `client/src/upload.rs` — no tests

The TUS resumable upload state machine (server side: init, PATCH, status,
completion; client side: chunked PATCH loop) has no tests. This feature is used
for large file uploads and would be exercised rarely enough that breakage might
go unnoticed.

#### P3-E: `compaction.rs` — scan logic untested

`scan_wasted_space` (walks archives, compares SQLite chunk refs against ZIP
entries, computes wasted bytes) and `run_compaction` have no tests. Config
parsing and time arithmetic are tested, but the actual compaction logic is dark.

#### P3-F: `normalize.rs` — external formatter path untested

`try_external_formatters` (spawning a subprocess, timeout handling,
empty-output fallback) and `wait_with_timeout` are untested. A misconfigured
formatter that hangs could block the indexing pipeline indefinitely.

---

### Quality Issues in Existing Tests

| Issue | File | Fix |
|-------|------|-----|
| `test_subfolder_calculation` only tests arithmetic, not actual code | `archive.rs` | Replace with a test that calls `archive_path_for_number` and checks the returned `PathBuf` |
| `duration_until_next` test only bounds-checks, doesn't verify correctness | `compaction.rs` | Assert the next occurrence of a specific time from a known instant |
| `build_fts_query` tests don't exercise the actual SQL | `db/search.rs` | Add in-DB integration tests alongside string tests |
| Web tests not in CI | `.github/workflows/ci.yml` | Add `pnpm test --run` step |

---

## Recommended Implementation Order

### Phase 1 — Immediate wins (low effort, high value)

1. **Add `pnpm test --run` to CI** — 5-minute change, immediately catches all
   TypeScript logic regressions.

2. **`ArchiveManager` write-path unit tests** — Use `TempDir`. Test
   `append_chunks` → `read_chunk` round-trip, `remove_chunks` correctness,
   `rewrite_archive` under both success and failure conditions. No HTTP layer
   needed.

3. **`db/mod.rs` `upsert_file_phase1` tests** — The existing `db/mod.rs` test
   pattern (in-memory SQLite, populated schema) makes this straightforward. Add
   tests for insert, update, mtime guard, and content-hash dedup.

4. **`db/search.rs` SQL query tests** — Add tests for `fts_candidates` and
   `document_candidates` using the in-memory DB pattern with seeded FTS data.

### Phase 2 — Route handler test harness

5. **Extend the existing `TestServer`** in `crates/server/tests/helpers/mod.rs`
   (already uses port-0 `TcpListener`, `reqwest`, `wait_for_idle`, `post_bulk`).
   Add an unauthenticated client variant. Write smoke tests for:
   - Auth rejection (invalid token → 401)
   - `GET /api/v1/settings` round-trip
   - `POST /api/v1/bulk` → `GET /api/v1/search` round-trip (the most valuable
     single integration test)
   - `GET /api/v1/tree` basic listing
   - `GET /api/v1/file` with pagination params

6. **Extend route tests** to cover: search modes (exact, regex, filename),
   search result deduplication, link create/resolve, file pagination edge cases,
   admin operations (delete_source, compact).

### Phase 3 — Worker pipeline tests

7. **`worker/request.rs` tests** — Construct a `BulkRequest`, gzip-encode it,
   write it to a temp inbox dir, invoke the processing function directly, and
   assert the `to-archive/` output. Test delete, rename, and upsert paths.

8. **`worker/archive_batch.rs` tests** — Drive Phase 2 from the `to-archive/`
   output produced by Phase 1 tests. Assert chunk refs in SQLite and ZIP entry
   lists. Verify the rollback path by injecting a write failure.

9. **`worker/pipeline.rs` tests** — Test `process_file_phase1` for the
   content-hash dedup and stale-mtime guard cases directly.

### Phase 4 — TypeScript and component coverage

10. **Extract and test `ResultList` dedup/pagination logic** — Move the
    merge/dedup function out of the component into `pagination.ts`, add vitest
    tests for the duplicate-key prevention and `loadOffset` advancement rules.

11. **`FuzzyScorer` tests** — Verify that exact prefix matches score higher than
    partial matches, that empty input is handled, that the scorer is consistent
    across multiple calls.

12. **PDF panic-path fixture** — Obtain or generate a minimal PDF with malformed
    Type1 font data; add it to `crates/extractors/pdf/tests/fixtures/` and
    assert extraction does not panic.

### Phase 5 — Remaining gaps

13. Upload TUS state machine (server + client) tests.
14. External formatter path in `normalize.rs` (mock subprocess or temp script).
15. `compaction.rs` `scan_wasted_space` tests.
16. Tree store and line selection TypeScript module tests.

---

## Files to Create or Modify

| File | Change |
|------|--------|
| `.github/workflows/ci.yml` | Add `pnpm test --run` to web CI job |
| `crates/server/src/archive.rs` | Add write-path unit tests (`#[cfg(test)]`) |
| `crates/server/src/db/mod.rs` | Add `upsert_file_phase1` and read-path tests |
| `crates/server/src/db/search.rs` | Add SQL-level FTS query tests |
| `crates/server/src/fuzzy.rs` | Add `FuzzyScorer::score` unit tests |
| `crates/server/tests/` (new) | `TestServer` fixture + route handler integration tests |
| `crates/server/src/worker/request.rs` | Add inbox processing unit tests |
| `crates/server/src/worker/pipeline.rs` | Add phase1 dedup and mtime guard tests |
| `crates/server/src/worker/archive_batch.rs` | Add phase2 ZIP I/O and rollback tests |
| `crates/server/src/compaction.rs` | Add `scan_wasted_space` tests |
| `web/src/lib/pagination.ts` (new) | Extract dedup/merge logic from `ResultList.svelte` |
| `web/src/lib/pagination.test.ts` (new) | Vitest tests for dedup and `loadOffset` logic |
| `crates/extractors/pdf/tests/fixtures/` | Malformed Type1 font PDF fixture |
| `crates/extractors/pdf/src/lib.rs` | Add panic-path test |

---

## Testing Strategy Notes

- **No mocks for storage.** Use real `TempDir`-backed files and in-memory SQLite
  throughout. The codebase's existing test philosophy avoids mocks for data
  storage, and the integration test suite has validated this approach.
- **Route tests use the existing `TestServer` helper** (`tests/helpers/mod.rs`),
  which already binds a port-0 `TcpListener` and wraps a `reqwest` client.
  No new dependencies needed. New test files go in `crates/server/tests/`
  alongside the existing ones.
- **Worker pipeline tests drive real functions, not spawned workers.** Call
  `process_inbox_file(path, &state)` directly rather than spinning up the
  background polling loop.
- **Vitest for pure logic; no component rendering tests.** Mounting Svelte
  components in tests requires significant infrastructure (jsdom + Svelte
  compiler). Prefer extracting pure TypeScript functions and testing those
  instead.
