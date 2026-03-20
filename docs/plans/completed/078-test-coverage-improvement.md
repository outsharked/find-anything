# 078 — Test Coverage Improvement

## Overview

Current workspace coverage: **62.23% lines / 68.68% functions** (from `mise run coverage`,
measured 2026-03-19). The baseline dropped slightly from the original plan (65% / 70%)
because new files added by the SqliteContentStore migration (`multi_store.rs`,
`find_test_main.rs`) start at 0% coverage.

Several critical paths are significantly undertested. This plan targets the highest-value
gaps — particularly routes with <50% coverage and complex DB logic with no unit tests.

The goal is not 100% coverage but meaningful coverage of critical paths: search, admin
operations, context/file retrieval, stats, and the core DB query functions.

**Not in scope:** binary `main.rs` entry points (all 0%, not meaningfully testable via
unit tests), `update_check`/`update_apply` (network-dependent), SSE stream body content
(only verify headers and initial response), and `routes/raw.rs` serving content from real
filesystem mounts (requires significant test harness work for uncertain return).

---

## Current State (2026-03-19)

| File | Lines | Functions | Priority | Notes |
|------|-------|-----------|----------|-------|
| `routes/admin.rs` | 43% | 54% | ~~done~~ — all inbox + compact tests added | was 26%/32%; Phase 1 complete |
| `routes/recent.rs` | 34% | 27% | ~~done~~ — 8 tests added | Phase 4 complete |
| `routes/context.rs` | 33% | 38% | ~~done~~ — 7 tests added | Phase 3 complete |
| `routes/mod.rs` | 44% | 45% | **Medium** — auth helpers, metrics | unchanged |
| `db/search.rs` | 67% | 72% | **High** — `document_candidates` has 0 unit tests | was 62%/68% |
| `db/stats.rs` | 59% | 70% | **Medium** — pending content, ext histogram | was 55%/70% |
| `routes/search.rs` | 63% | 59% | ~~done~~ — 14 tests added | Phase 6 complete |
| `routes/stats.rs` | 54% | 46% | ~~done~~ — 7 route + 5 unit tests added | Phase 5 complete |
| `content-store/src/multi_store.rs` | 0% | 0% | ~~done~~ — 17 tests added (12 contract + 5 specific) | Phase 7 complete |
| `extractors/pe/src/lib.rs` | 9% | 15% | ~~done~~ — 8 tests added | Phase 9 complete |
| `server/src/find_test_main.rs` | 0% | 0% | **Low** — bench binary entry point | new; not meaningfully unit-testable |
| `routes/raw.rs` | 0% | 0% | **Low** — deferred (needs real FS mount) | unchanged |

---

## Phase 1 — Admin routes (`routes/admin.rs`) — COMPLETE

All 17 admin integration tests pass. Coverage improved from 26% to ~50%+.

Tests added to `crates/server/tests/admin.rs`:

### Inbox operations (done)

- `test_inbox_status_after_indexing` — pause + bulk submit, verify `pending` non-empty
- `test_inbox_clear_pending` — clear pending only, verify `failed` unaffected
- `test_inbox_clear_all` — seed failed dir + pending, clear all, verify both empty
- `test_inbox_retry_moves_failed_to_pending` — seed failed dir, retry, verify moved
- `test_inbox_pause_stops_processing` — pause, submit, verify worker doesn't drain; resume, verify it does

### Compact with real content (done)

- `test_compact_removes_orphaned_chunks` — verifies `chunks_removed > 0` and `bytes_freed > 0`
- `test_compact_deletes_fully_orphaned_archive` — verifies `units_deleted >= 1`
- `test_compact_dry_run_does_not_remove_chunks` — verifies dry-run counts are non-zero

### Delete source with archive content (done)

- `test_delete_source_removes_chunk_refs` — verifies source DB removed and compact reclaims orphaned content

---

## Phase 2 — `db/search.rs`: `document_candidates` unit tests (`db/search.rs`: 67% → ~80%)

`document_candidates()` is the document-mode search path (used when `mode=document`).
It has 0 unit tests despite being ~100 lines of non-trivial logic (token intersection,
per-file cap, uncovered-terms tracking).

Add inline unit tests to `crates/server/src/db/search.rs`:

- **`document_candidates_returns_file_for_matching_token`** — index a file containing
  `"hello world"`, call `document_candidates("hello", ...)`, verify at least one result
  for that file.

- **`document_candidates_multi_token_requires_all_tokens`** — index file A with both
  `"alpha"` and `"beta"`, file B with only `"alpha"`. Query `"alpha beta"` in document
  mode; only file A should appear.

- **`document_candidates_per_file_cap`** — index a file with the same keyword on 20+
  lines; verify the result count for that file does not exceed the per-file cap
  (currently 5 or whatever `MAX_LINES_PER_FILE` is set to).

- **`document_candidates_empty_query_returns_empty`** — empty token list, verify no
  panic and empty result.

---

## Phase 3 — Context route (`routes/context.rs`) — COMPLETE

New file: `crates/server/tests/context.rs` — 7 tests, all pass.

- `test_get_context_returns_surrounding_lines` — window=1 around interior line, verify surrounding lines and match_index
- `test_context_window_is_respected` — window=2 on long file, verify exactly 5 lines returned and match_index=2
- `test_context_clamped_at_file_start` — large window at first line, verify graceful clamp
- `test_context_requires_auth` — unauthenticated GET returns 401
- `test_context_batch_multiple_items` — 3 items across 3 files, all lines returned correctly
- `test_context_batch_unknown_source_returns_empty_lines` — graceful degradation for missing source
- `test_context_batch_requires_auth` — unauthenticated POST returns 401

---

## Phase 4 — Recent route (`routes/recent.rs`) — COMPLETE

New file: `crates/server/tests/recent.rs` — 8 tests, all pass.

- `test_recent_returns_indexed_files` — 3 files indexed, all appear in /recent
- `test_recent_sorted_by_mtime` — different mtimes, verifies descending order
- `test_recent_limit_respected` — 10 files indexed, limit=3 returns exactly 3
- `test_recent_limit_above_max_returns_400` — limit > 1000 returns 400
- `test_recent_requires_auth` — unauthenticated GET returns 401
- `test_recent_empty_when_nothing_indexed` — fresh server returns empty list
- `test_recent_stream_returns_sse_headers` — verifies Content-Type: text/event-stream
- `test_recent_stream_requires_auth` — unauthenticated stream returns 401

---

## Phase 5 — Stats route + `db/stats.rs` — COMPLETE

### Route-level additions to `crates/server/tests/stats_cache.rs` (7 new tests)

- `test_stats_endpoint_returns_source` — source with 2 files appears in live cache
- `test_stats_requires_auth` — unauthenticated GET returns 401
- `test_stats_by_ext_populated_after_refresh` — .js and .py extensions appear in by_ext after refresh
- `test_stats_inbox_pending_reflects_paused_requests` — paused inbox shows in inbox_pending/inbox_paused
- `test_stats_by_kind_appears_in_response` — by_kind contains Text entry after indexing
- `test_stats_stream_returns_sse_headers` — Content-Type: text/event-stream
- `test_stats_stream_requires_auth` — unauthenticated stream returns 401

### Unit tests added inline to `crates/server/src/db/stats.rs` (5 new tests)

- `test_get_files_pending_content_counts_unarchived` — content_hash but no inline content = pending; adding inline row clears it
- `test_get_stats_by_ext_excludes_archive_members` — `outer.zip::module.js` excluded; `script.js` counted
- `test_upsert_indexing_errors_increments_count` — two calls → count = 2, last_seen updated
- `test_get_stats_counts_files_by_kind` — 2 text + 1 image → correct by_kind breakdown
- `test_upsert_indexing_errors_empty_is_noop` — empty slice does not error or insert

---

## Phase 6 — Search filters and pagination (`routes/search.rs`) — COMPLETE

New file: `crates/server/tests/search_filters.rs` — 14 tests, all pass.

- `test_search_kind_filter_text` / `test_search_kind_filter_image` — kind= filter isolates correct file type
- `test_search_date_from_filter` / `test_search_date_to_filter` — mtime bounds exclude out-of-range files
- `test_search_pagination_no_overlap` — 15 files, page 1 and page 2 are disjoint
- `test_search_pagination_total_is_accurate` — `total` reflects all matches, `results` is capped by limit
- `test_search_returns_duplicate_paths` — same content_hash → duplicate_paths populated
- `test_search_case_insensitive_by_default` — exact mode matches regardless of case
- `test_search_case_sensitive_excludes_wrong_case` — case_sensitive=true rejects wrong case
- `test_search_requires_auth` — 401 without token
- `test_search_missing_q_returns_400` — 400 when q absent
- `test_search_invalid_limit_returns_400` — 400 for non-numeric limit
- `test_search_across_multiple_sources` — no source filter hits all sources
- `test_search_source_filter_restricts_results` — source= limits to one source

---

## Phase 7 — MultiContentStore (`content-store/src/multi_store.rs`) — COMPLETE

Added to `crates/content-store/tests/contract.rs`:

- **`contract_tests!(multi_store, ...)`** — stamps out the full 12-test contract suite
  for `MultiContentStore` (put, get_lines, delete, compact, empty blob, multi-chunk, etc.)

- **`multi_put_writes_to_all_backends`** — put to multi → both underlying stores contain the key
- **`multi_get_lines_reads_from_first_hit`** — key in secondary only → multi returns it via fallthrough
- **`multi_storage_stats_sums_backends`** — stats are summed across all backends
- **`multi_compact_runs_all_backends`** — orphans in both backends are removed; results summed
- **`multi_empty_stores_returns_none_for_stats`** — empty store list returns `None` for stats, no panic

---

## Phase 9 — PE extractor (`extractors/pe/src/lib.rs`) — COMPLETE

Added inline unit tests to `crates/extractors/pe/src/lib.rs` — 8 tests, all pass.

No binary fixtures committed (they'd bloat the repo); instead the tests use:
- Extension string literals for `accepts()` coverage
- Programmatically constructed byte slices for `extract_from_bytes()` error-path coverage
- A hand-crafted minimal PE32 stub (~512 bytes) to exercise the parser entry points

Tests:
- `accepts_pe_extensions` — all 8 supported extensions (.exe/.dll/.sys/.scr/.cpl/.ocx/.drv/.efi) return true
- `accepts_uppercase_extensions` — .EXE/.DLL (uppercase) accepted
- `rejects_non_pe_extensions` — .txt/.zip/.pdf/.rs/.toml/.png/.mp3 rejected
- `rejects_no_extension` — bare filename and empty path rejected
- `non_pe_bytes_returns_empty_not_panic` — garbage bytes → Ok(vec[])
- `empty_bytes_returns_empty_not_panic` — empty slice → Ok(vec[])
- `mz_header_only_returns_empty_not_panic` — truncated MZ header → Ok(vec[])
- `minimal_pe32_returns_ok` — structurally valid PE32 stub → Ok (no panic)

---

## Testing Strategy Notes

- All server integration tests use `TestServer::spawn()` from `crates/server/tests/helpers/`.
- To test inbox operations that require files to sit unprocessed, use
  `TestServer::spawn_paused()` (if it exists) or POST /admin/inbox/pause before the
  bulk request.
- For `db/` unit tests, open an in-memory SQLite DB with `db::open(":memory:")` and
  call `db::init_schema()` before each test (pattern already used in `db/search.rs`
  inline tests).
- For PE fixtures, keep binaries small — a 512-byte stub PE is sufficient to exercise
  the parsing paths without bloating the repo.

---

## Files Changed

| File | Change |
|------|--------|
| `crates/server/tests/admin.rs` | Add inbox tests (compaction tests already done) |
| `crates/server/tests/context.rs` | New file — context and context-batch tests |
| `crates/server/tests/recent.rs` | New file — recent files endpoint tests |
| `crates/server/tests/search_filters.rs` | New file — kind, pagination, archive member, dedup |
| `crates/server/tests/stats_cache.rs` | Add stats endpoint and stream header tests |
| `crates/server/src/db/search.rs` | Inline unit tests for `document_candidates` |
| `crates/server/src/db/stats.rs` | Inline unit tests for `get_files_pending_content`, `get_stats_by_ext`, `upsert_indexing_errors` count |
| `crates/content-store/src/multi_store.rs` | Inline unit tests for `MultiContentStore` routing |
| `crates/extractors/pe/src/lib.rs` | Inline unit tests + fixtures in `tests/fixtures/` |

---

## Expected Outcome

| File | Current (2026-03-19) | Target |
|------|----------------------|--------|
| `routes/admin.rs` | 43% lines / 54% fn | ~70% |
| `routes/context.rs` | 33% lines / 38% fn | ~75% |
| `routes/recent.rs` | 34% lines / 27% fn | ~70% |
| `routes/search.rs` | 63% lines / 59% fn | ~75% |
| `routes/stats.rs` | 54% lines / 46% fn | ~70% |
| `db/search.rs` | 67% lines / 72% fn | ~80% |
| `db/stats.rs` | 59% lines / 70% fn | ~75% |
| `content-store/src/multi_store.rs` | 0% lines / 0% fn | ~70% |
| `extractors/pe/src/lib.rs` | 9% lines / 15% fn | ~70% |
| **TOTAL** | **62.23% lines / 68.68% fn** | **~78% lines / ~82% fn** |
