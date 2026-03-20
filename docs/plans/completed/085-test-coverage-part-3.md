# Test Coverage Part 3

## Overview

Continue improving line coverage from ~70% toward ~78%. Focuses on the largest
remaining holes: the raw file-serving route (0%), search param paths, the
watch event loop, and several extractor gaps.

**Baseline (after plan 084):** 69.69% lines / 75.70% functions
**Target:** ~77% lines

## Phases

### Phase 1 — `server/src/routes/raw.rs` (0% → ~65%)

The entire raw file-serving route is untested — 321 uncovered lines. Requires
a `TestServer` variant with a configured source path so that `get_raw` can
resolve files on disk.

**`TestServer` change:** Add `spawn_with_source_path(source, path)` helper
(or `spawn_with_extra_config(toml: &str)`) that appends extra TOML to the
server config, enabling `[sources.<name>]` with a `path =` key.

**Tests to add** in a new `crates/server/tests/raw.rs`:
- `raw_requires_auth` — 401 without token
- `raw_path_traversal_rejected` — `..` in path returns 400
- `raw_leading_slash_rejected` — `/etc/passwd` style returns 400
- `raw_source_not_configured_returns_404` — unknown source name
- `raw_file_not_found_returns_404` — source configured but file absent
- `raw_serves_file_content` — full round-trip: write file, GET /api/v1/raw, verify body
- `raw_content_type_inferred_from_extension` — `.txt`, `.html`, `.pdf`
- `raw_download_param_sets_attachment_disposition` — `?download=1`
- `parse_byte_range` unit tests inline in `raw.rs`:
  - valid `bytes=0-99` → `(0, Some(99))`
  - open-ended `bytes=500-` → `(500, None)`
  - multi-range with comma → `None`
  - missing `bytes=` prefix → `None`

### Phase 2 — `server/src/routes/search.rs` (65% → ~85%)

Remaining gaps are in parameter parsing error paths and less-used search
modes. Tests go in the existing `crates/server/tests/search_filters.rs` or a
new file `crates/server/tests/search_params.rs`.

**Tests to add:**
- `search_missing_q_returns_400` — query without `?q=` param
- `search_invalid_limit_returns_400` — `?q=x&limit=notanumber`
- `search_invalid_offset_returns_400` — `?q=x&offset=abc`
- `search_invalid_date_from_returns_400` — `?q=x&date_from=bad`
- `search_invalid_date_to_returns_400`
- `search_mode_document` — `?mode=document` returns grouped results
- `search_mode_regex` — `?mode=regex&q=hel.*lo` matches content
- `search_case_sensitive` — `?case_sensitive=1` doesn't match wrong case
- `search_date_from_filter` — only returns files with mtime ≥ date_from
- `search_date_to_filter` — only returns files with mtime ≤ date_to
- `search_requires_auth`

### Phase 3 — `client/src/watch.rs` (7% → ~45%)

The event loop uses `mpsc::Receiver<notify::Result<notify::Event>>` internally.
Extract the loop body into a testable inner function, then drive it with
synthetic events in tests.

**Refactor:**
- Extract `pub(crate) async fn run_event_loop(mut rx: mpsc::Receiver<notify::Result<Event>>, api: &ApiClient, source_map: SourceMap, batch_window: Duration, batch_limit: usize, scan: &ScanConfig)` from `run_watch`
- `run_watch` does setup (watcher, channel) then calls `run_event_loop`
- The extracted function has no dependency on `notify::Watcher` — it only processes events from the channel

**Pure-function unit tests** (inline in `watch.rs`):
- `collapse_*` — all 5 explicit transition-table cases + identity cases
- `accumulate_create_event` — Create event populates pending as Create
- `accumulate_update_event` — Modify(Data) → Update
- `accumulate_remove_event` — Remove → Delete
- `accumulate_error_event` — Err result is ignored gracefully
- `accumulate_collapses_create_then_delete`
- `find_source_returns_most_specific_root` — longest prefix wins
- `find_source_returns_none_for_unknown_path`
- `is_excluded_matches_glob`

**Integration tests** (new `crates/client/tests/watch_loop.rs` or inline):
These call `run_event_loop` directly with a fake channel + real `TestServer`:
- `watch_create_event_indexes_file` — send Create event for a real file, verify searchable
- `watch_delete_event_removes_file` — send Remove event, verify no longer returned
- `watch_update_indexes_new_content` — send Modify, verify content updated
- `watch_channel_close_ends_loop` — drop sender, loop exits cleanly

### Phase 4 — `extractors/pe/src/lib.rs` (62% → ~85%)

**Tests to add** (inline in `pe/src/lib.rs`):
- `accepts_pe_extensions` — .exe, .dll, .sys, .scr, .cpl, .ocx, .drv, .efi all return true
- `accepts_rejects_non_pe` — .txt, .rs, .pdf return false
- `empty_bytes_returns_empty` — `extract_from_bytes(b"", ...)` → `Ok([])`
- `garbage_bytes_returns_ok` — random bytes → `Ok(...)` (no panic)
- `minimal_mz_no_version_info_returns_empty` — valid MZ header with no version resource
- `extract_version_info_from_real_exe` (if a fixture is available)

### Phase 5 — `extractors/text/src/lib.rs` (84% → ~95%)

**Tests to add** (inline in `text/src/lib.rs`):
- `accepts_known_text_extensions` — .txt, .rs, .md, .json, .yaml, .toml, .py, .js
- `accepts_rejects_binary_extensions` — .exe, .zip, .png return false
- `markdown_frontmatter_extracted` — `---\ntitle: Test\n---\n# Body` produces metadata line
- `markdown_body_content_indexed` — body lines below frontmatter are included
- `plain_text_content_indexed` — non-markdown lines numbered from LINE_CONTENT_START
- `content_truncated_at_max_kb` — large input truncated without error

### Phase 6 — `extractors/epub/src/lib.rs` (81% → ~90%)

EPUB files are ZIP archives with a specific structure. Build a minimal valid
EPUB in memory for testing.

**Tests to add** (inline in `epub/src/lib.rs`):
- `accepts_epub_extension` — .epub returns true; .txt returns false
- `minimal_epub_extracts_metadata` — build in-memory EPUB with container.xml,
  OPF, and a spine XHTML; verify title/creator appear in output
- `epub_body_text_indexed` — paragraph text appears in content lines
- `corrupt_epub_returns_error` — random bytes → `Err`
- `empty_epub_zip_returns_error` — empty ZIP → error (no container.xml)

### Phase 7 — `server/src/routes/admin.rs` (56% → ~70%)

Several admin route branches remain uncovered. Add to existing
`crates/server/tests/admin.rs`:
- `test_compaction_runs_dry_run` — POST /api/v1/admin/compact?dry_run=true
- `test_delete_source_removes_data` — POST /api/v1/admin/source/delete
- `test_compact_requires_auth`

## Files Changed

- `crates/server/tests/helpers/mod.rs` — add `spawn_with_extra_config`
- `crates/server/tests/raw.rs` — new file
- `crates/server/tests/search_params.rs` — new file (or extend search_filters.rs)
- `crates/server/src/routes/raw.rs` — inline `parse_byte_range` unit tests
- `crates/client/src/watch.rs` — extract `run_event_loop`; inline unit tests
- `crates/extractors/pe/src/lib.rs` — inline tests
- `crates/extractors/text/src/lib.rs` — inline tests
- `crates/extractors/epub/src/lib.rs` — inline tests
- `crates/server/tests/admin.rs` — additional tests

## Testing

```
cargo test --test raw
cargo test --test search_params
cargo test -p find-client
cargo test -p find-extract-pe
cargo test -p find-extract-text
cargo test -p find-extract-epub
cargo test --test admin
cargo llvm-cov --html
```
