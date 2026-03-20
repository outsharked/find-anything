# 084 — Test Coverage Part 2

## Overview

Continuation of plan 078. All phases in 078 are complete. Current coverage is
**65.36% lines / 72.11% functions** (measured 2026-03-19).

This plan targets the remaining high-value gaps: extractor libraries with <60% coverage,
server upload/compaction paths, and remaining admin route holes.

**Still out of scope:** binary `*_main.rs` entry points, `routes/raw.rs` (requires real
FS mount), `client/src/watch.rs` (async file-watch loop — not worth the harness cost),
`client/src/api.rs` (HTTP client — covered indirectly by server integration tests).

---

## Current State (2026-03-19)

| File | Lines | Functions | Priority |
|------|-------|-----------|----------|
| `extractors/media/src/lib.rs` | 44% | 60% | **High** — audio/video metadata extraction |
| `server/src/routes/admin.rs` | 43% | 54% | **High** — remaining admin routes uncovered |
| `server/src/upload.rs` | 45% | 70% | **High** — upload processing logic |
| `extractors/office/src/lib.rs` | 53% | 43% | **High** — Office doc extraction paths |
| `extractors/archive/src/lib.rs` | 60% | 74% | **Medium** — archive edge cases |
| `server/src/compaction.rs` | 74% | 78% | **Medium** — compaction logic gaps |
| `server/src/db/mod.rs` | 78% | 77% | **Medium** — DB operation error paths |
| `extractors/pdf/src/lib.rs` | 70% | 78% | **Medium** — PDF extraction paths |
| `extractors/text/src/lib.rs` | 84% | 84% | **Low** — encoding/binary-detect paths |
| `extractors/epub/src/lib.rs` | 80% | 86% | **Low** — malformed EPUB paths |

---

## Phase 1 — Media extractor (`extractors/media/src/lib.rs`: 44% → ~70%)

Inline unit tests in `crates/extractors/media/src/lib.rs`.

Use small in-memory byte slices where possible; avoid committing binary fixtures.

- `accepts_audio_extensions` — .mp3/.flac/.wav/.ogg/.m4a/.aac return true
- `accepts_video_extensions` — .mp4/.mkv/.avi/.mov/.webm return true
- `rejects_non_media_extensions` — .txt/.pdf/.exe rejected
- `empty_bytes_returns_ok_not_panic` — empty slice → Ok(vec[])
- `garbage_bytes_returns_ok_not_panic` — random bytes → Ok (no panic)
- `extract_title_from_id3_tag` — minimal ID3v2 frame → title field in output
- `extract_artist_from_id3_tag` — minimal ID3v2 frame → artist field in output
- `file_without_tags_returns_filename_line_only` — untagged mp3-shaped bytes → just filename line

---

## Phase 2 — Office extractor (`extractors/office/src/lib.rs`: 53% → ~70%)

Inline unit tests in `crates/extractors/office/src/lib.rs`.

- `accepts_office_extensions` — .docx/.xlsx/.pptx/.odt/.ods/.odp return true
- `rejects_non_office_extensions` — .txt/.pdf/.mp3 rejected
- `empty_bytes_returns_ok_not_panic`
- `garbage_bytes_returns_ok_not_panic`
- `minimal_docx_extracts_text` — smallest valid OOXML zip structure → text content returned
- `file_with_no_text_content_returns_empty` — valid zip but empty document.xml → Ok(vec[])
- `malformed_zip_returns_ok_not_panic` — truncated ZIP header → Ok(vec[]) not Err propagation

---

## Phase 3 — Upload path (`server/src/upload.rs`: 45% → ~70%)

Integration tests in `crates/server/tests/upload.rs` (new file).

- `test_upload_single_file_appears_in_search` — POST file to /upload, verify searchable
- `test_upload_requires_auth` — unauthenticated upload returns 401
- `test_upload_text_file_content_is_indexed` — uploaded .txt with content is full-text searchable
- `test_upload_replaces_existing_file` — upload same path twice → latest content wins
- `test_upload_with_source_param` — source= query param routes to correct DB

---

## Phase 4 — Remaining admin routes (`server/src/routes/admin.rs`: 43% → ~65%)

Add to `crates/server/tests/admin.rs`.

Identify which functions are uncovered (likely: `get_sources`, `delete_source`,
`reindex_source`, `get_indexing_errors`, `clear_indexing_errors`):

- `test_get_sources_returns_indexed_source` — after indexing, source appears in list
- `test_delete_source_removes_files` — delete source → files no longer searchable
- `test_get_indexing_errors_empty_initially` — fresh server has no errors
- `test_clear_indexing_errors_removes_all` — seed errors, clear, verify empty

---

## Phase 5 — Compaction logic (`server/src/compaction.rs`: 74% → ~85%)

Add inline unit tests or extend existing compaction integration tests in
`crates/server/tests/admin.rs`.

- `test_compact_reports_zero_when_nothing_to_remove` — freshly indexed content → no orphans
- `test_compact_after_delete_removes_chunks` — index + delete file → compact reclaims
- `test_compact_stats_bytes_freed_nonzero` — verify `bytes_freed` populated after orphan removal
- `test_dry_run_compact_returns_nonzero_counts_but_no_removal` — dry_run=true → counts but no actual deletion

---

## Phase 6 — Archive extractor edge cases (`extractors/archive/src/lib.rs`: 60% → ~75%)

Inline unit tests in `crates/extractors/archive/src/lib.rs`.

- `accepts_archive_extensions` — .zip/.tar/.gz/.tar.gz/.7z/.rar return true
- `rejects_non_archive_extensions`
- `empty_zip_returns_only_outer_path`
- `zip_with_single_text_member_extracts_content`
- `zip_with_binary_member_returns_member_path_only`
- `nested_zip_respects_max_depth` — depth limit prevents infinite recursion
- `malformed_zip_returns_outer_path_only` — corrupt inner zip → graceful fallback

---

## Phase 7 — PDF extractor (`extractors/pdf/src/lib.rs`: 70% → ~82%)

Inline unit tests in `crates/extractors/pdf/src/lib.rs`.

- `accepts_pdf_extension`
- `rejects_non_pdf_extensions`
- `empty_bytes_returns_ok`
- `garbage_bytes_returns_ok_not_panic`
- `truncated_pdf_header_returns_ok_not_panic` — `%PDF-1.4` with no body → no panic

---

## Files Changed

| File | Change |
|------|--------|
| `crates/extractors/media/src/lib.rs` | Inline unit tests |
| `crates/extractors/office/src/lib.rs` | Inline unit tests |
| `crates/extractors/archive/src/lib.rs` | Inline unit tests |
| `crates/extractors/pdf/src/lib.rs` | Inline unit tests |
| `crates/server/tests/upload.rs` | New file — upload endpoint integration tests |
| `crates/server/tests/admin.rs` | Add remaining admin route tests |
| `crates/server/src/compaction.rs` | Inline unit tests or extend admin integration tests |

---

## Expected Outcome

| File | Current | Target |
|------|---------|--------|
| `extractors/media/src/lib.rs` | 44% | ~70% |
| `extractors/office/src/lib.rs` | 53% | ~70% |
| `extractors/archive/src/lib.rs` | 60% | ~75% |
| `extractors/pdf/src/lib.rs` | 70% | ~82% |
| `server/src/upload.rs` | 45% | ~70% |
| `server/src/routes/admin.rs` | 43% | ~65% |
| `server/src/compaction.rs` | 74% | ~85% |
| **TOTAL** | **65.36% lines** | **~72% lines** |
