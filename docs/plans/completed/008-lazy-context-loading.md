# Lazy Context Loading

## Overview

Search for common terms (e.g. "test") is slow because the search handler eagerly
enriches every result with surrounding context lines (`?context=N`), causing
additional ZIP archive reads on top of the `fts_candidates` reads used for scoring.
For a query with many hits this means many extra ZIP reads on the hot search path.

Fix in two parts:

1. **Remove context enrichment from `GET /api/v1/search`** — search returns only
   the matched line and its metadata (source, path, line number, score).  No snippet
   column is added to the database; the `fts_candidates` ZIP reads remain (needed
   for fuzzy/regex scoring) but the per-result context enrichment is removed.

2. **New `POST /api/v1/context-batch` endpoint** + **IntersectionObserver** in
   the web UI so surrounding context is only fetched for results the user can
   actually see.

## Design

### Part 1 — Remove context from search

The `?context=N` query parameter is removed from `GET /api/v1/search`.  The
handler no longer calls `db::get_context` for each result.  `SearchResult.snippet`
is still populated (it is the matched line, read during `fts_candidates` for
scoring), but `context_lines` is always empty from the server.

No schema changes.  No worker changes.

### Part 2 — Batch context endpoint

```
POST /api/v1/context-batch
Content-Type: application/json

{ "requests": [
    { "source": "code", "path": "src/main.rs", "line": 42, "window": 3 },
    { "source": "code", "path": "lib/foo.rs",  "line": 17, "window": 3 }
  ]
}
```

Response:
```json
{ "results": [
    { "source": "code", "path": "src/main.rs", "line": 42,
      "lines": [{ "line_number": 40, "content": "..." }, ...],
      "file_kind": "text" },
    ...
  ]
}
```

The server resolves each request using the existing `get_context` logic in db.rs.
ZIP reads happen only here, not during search.

### Web UI changes

- Remove `context: 3` from the `search()` call in `+page.svelte`.
- `SearchResult.svelte` adds an `IntersectionObserver`; when the result card
  enters the viewport it calls `getContext` (the existing single-item endpoint)
  and fills in the context lines reactively.
- Results display the matched snippet immediately (from the search response) and
  show surrounding context lines once loaded.

## Files Changed

### Backend

- `crates/common/src/api.rs` — add `ContextBatchRequest`, `ContextBatchItem`,
  `ContextBatchResult`, `ContextBatchResponse` types; remove `context_lines` comment
  on `SearchResult`
- `crates/server/src/routes.rs` — remove `context` param from `SearchParams`,
  remove context enrichment block, add `context_batch` handler
- `crates/server/src/main.rs` — register `POST /api/v1/context-batch`

### Web UI

- `web/src/lib/api.ts` — add `ContextBatchItem`, `ContextBatchResult`,
  `ContextBatchResponse` types and `contextBatch()` function; remove `context`
  from `SearchParams`
- `web/src/routes/+page.svelte` — remove `context: 3` from search params
- `web/src/lib/SearchResult.svelte` — add `IntersectionObserver`, call
  `getContext` for visible rows, display context when loaded

## Testing

1. Search for a common term — verify response time is fast
2. Search results display the matched line immediately
3. Scroll through results — verify context lines appear as rows enter viewport
4. Verify context is correct (same as before for exact/regex/fuzzy)

## Breaking Changes

- `GET /api/v1/search` no longer accepts `?context=N`.  Any external client using
  it must switch to `POST /api/v1/context-batch` for context retrieval.
