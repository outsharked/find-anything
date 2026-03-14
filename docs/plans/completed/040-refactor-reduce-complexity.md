# 040 — Refactor: Reduce Complexity and Improve Separation of Concerns

## Overview

After several major feature releases (archive members, deduplication, subprocess extraction,
original file serving), complexity has accumulated in a small number of specific files.
The overall architecture is sound — the extractor plugin system, transaction boundaries,
and config structs are all well-designed. The goal of this refactor is targeted: address
the files that have outgrown a single responsibility, without changing any behaviour.

---

## Problem Areas

### 1. `crates/server/src/db.rs` — 1,748 lines (highest priority)

**Too many concerns in one file:**
- Schema DDL and 6 inline migrations
- File upsert/delete, including a 140-line `delete_one_path` state machine (3 branches:
  straight deletion, alias promotion with FTS re-indexing, and archive member cascade)
- Two separate search paths: `fts_candidates()` and `document_candidates()`
- Context/file retrieval helpers
- Directory tree listing (`list_dir`, `prefix_bump`, `split_composite_path`)
- Stats aggregation and scan history
- Indexing error tracking CRUD

**Duplication:** The chunk-read pattern (open a ZIP member, split by lines, cache) appears
in three separate places: `fts_candidates` (line ~646), `document_candidates` (line ~795),
and inside `delete_one_path` alias promotion (line ~357). Each is an independent copy.

### 2. `web/src/routes/+page.svelte` — 530 lines, ~45 state variables (high priority)

**True god component.** Auth, search orchestration, file viewing, URL/history management,
scroll-based pagination, keyboard shortcuts, and the command palette are all intertwined.

Two documented brittle invariants that must not be broken:
- `loadOffset` must advance by the server's response count, not the filtered client count
  (cross-page dedup means `results.length` grows by less than `resp.results.length`)
- The dedup filter in `triggerLoad` must never be removed (server re-ranks candidates
  across pages, causing the same item to appear in page N and page N+1)

These invariants live as comments in the component. They should be encapsulated in code.

### 3. `crates/server/src/routes/search.rs` — 328 lines (lower priority)

Four query modes (fuzzy / phrase / regex / document) share one handler. Mode-switching,
`SearchParams` parsing (~60 lines), parallel fan-out across sources, scoring, and result
dedup are all inline. Adding a new mode touches existing code throughout the function.

---

## What Is Fine (Do Not Touch)

- **`worker.rs`** (605 lines) — complexity is real and necessary; transactional guarantees
  and the archive stub pattern are correct and well-reasoned
- **`FileViewer.svelte`** (568 lines) — all responsibilities are file-display-related
- **`api.ts`** (323 lines) — clean, thin, one method per endpoint
- **`config.rs`** — well-architected, follows config-struct pattern
- **`archive.rs`** — focused ZIP management
- **Extractor crates** — plugin architecture working well

---

## Proposed Changes

### Priority 1: Split `db.rs` into sub-modules (pure reorganisation, no logic changes)

Create `crates/server/src/db/` as a directory module:

```
db/
  mod.rs       — schema, migrations, open(), file upsert/delete, context retrieval
  search.rs    — fts_candidates, document_candidates, build_fts_query
  tree.rs      — list_dir, prefix_bump, split_composite_path
  stats.rs     — get_stats, get_stats_by_ext, get_scan_history, append_scan_history,
                  upsert_indexing_errors, clear_errors_for_paths, get_indexing_errors
```

All public interfaces remain identical — callers use `db::list_dir`, `db::fts_candidates`,
etc. unchanged. This is a pure file split.

### Priority 2: Extract the chunk-read helper in `db.rs`

The three copies of "read chunk from ZIP, split by lines, cache result" should become
one private function:

```rust
fn read_chunk_lines<'a>(
    cache: &'a mut HashMap<(String, String), Vec<String>>,
    archive_dir: &Path,
    archive_name: &str,
    chunk_name: &str,
) -> Option<&'a Vec<String>>
```

Called from `fts_candidates`, `document_candidates`, and the alias-promotion path in
`delete_one_path`. Removes ~60 lines of duplication and ensures bug fixes apply everywhere.

### Priority 3: Extract `SearchStore` from `+page.svelte` (moderate effort)

Move the search state and its invariants into `web/src/lib/searchStore.ts`:

```typescript
// Owns: results, loadOffset, searching, noMoreResults, totalResults, searchId
// Exports: doSearch(), triggerLoad(), reset()
// Encapsulates: dedup logic, loadOffset/results.length distinction, race-prevention
```

The component binds to the store's readable state and calls its methods. The brittle
invariants move from comments into the code that enforces them.

### Priority 4: Extract history/URL management from `+page.svelte`

The `captureState`, `applyState`, `pushState`, `syncHash` cluster (~60 lines) becomes
`web/src/lib/history.ts`. The component imports `pushAppState(state)` and
`readAppState()` without knowing about SvelteKit's navigation APIs directly.

### Priority 5: Separate `SearchParams` parsing in `search.rs` (low priority)

Extract the `SearchParams` struct and its validation/parsing logic into
`crates/server/src/routes/search_params.rs`. The route handler imports the validated
type. Makes it easy to add a new query mode without touching the fan-out logic.

---

## Implementation Order

These are independent and can be done in any order. Suggested sequence:

1. **`db.rs` chunk helper** — smallest change, immediate duplication removal
2. **`db.rs` module split** — pure file reorganisation, verify `cargo test` passes unchanged
3. **`+page.svelte` `SearchStore`** — isolates the most brittle invariants
4. **`+page.svelte` history module** — smaller extraction, lower risk
5. **`search.rs` params** — lowest value, do last or skip

---

## Testing Strategy

- `cargo test --workspace` must pass unchanged after each step (no behaviour changes)
- `pnpm run check` must pass after each frontend step
- `pnpm test` must pass after each frontend step
- Manual smoke test: search, open file, expand archive in tree, load more results

---

## Files Changed

| File | Change |
|------|--------|
| `crates/server/src/db.rs` | Split into `db/mod.rs`, `db/search.rs`, `db/tree.rs`, `db/stats.rs`; extract chunk helper |
| `crates/server/src/routes/search.rs` | Extract `SearchParams` to `search_params.rs` |
| `web/src/lib/searchStore.ts` | New — search state + pagination invariants |
| `web/src/lib/history.ts` | New — URL/history state management |
| `web/src/routes/+page.svelte` | Simplified: imports store and history module |

---

## Non-Goals

- No behaviour changes
- No new features
- No changes to the public HTTP API
- No changes to the database schema
- Do not refactor `worker.rs` — its complexity is load-bearing
