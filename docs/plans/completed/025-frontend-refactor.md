# Plan 025: Frontend Refactor — Simplicity, Decoupling, Single Responsibility

## Overview

The frontend has accumulated complexity across sessions. `+page.svelte` has grown
into a 648-line god component owning eight distinct concerns. The search result
display stacks five async mechanisms (IntersectionObserver → debounce → fetch →
ResizeObserver → virtualizer remeasure), making bugs nearly impossible to isolate.
When investigating the context display issue (plan 024 follow-up) we needed to hold
six files and ~2,600 lines in context simultaneously.

This plan refactors the frontend in priority order to reduce cognitive load, improve
debuggability, and keep each file under ~200 lines with a clear single purpose.

---

## Changes

### 1. Split `+page.svelte` into three components

**Current:** 648-line god component owning all app state, two views with near-duplicate
topbars, URL serialization, history management, sidebar resize, file/dir navigation,
command palette, settings, and search.

**After:**

```
web/src/routes/+page.svelte        ~120 lines  coordinator only
web/src/lib/SearchView.svelte      ~200 lines  search topbar + results + pagination
web/src/lib/FileView.svelte        ~220 lines  file topbar + sidebar + viewer/dir
web/src/lib/appState.ts            ~120 lines  URL state, history, AppState type
```

**`appState.ts`** owns the `AppState` interface, `captureState`, `buildUrl`,
`pushState`, `syncHash`, `applyState`, `restoreFromParams`. Exported as a plain
module (not a store) — called by the coordinator.

**`+page.svelte`** becomes a thin shell:
- Loads sources on mount
- Instantiates `appState`, restores from URL, listens for `popstate`
- Renders either `<SearchView>` or `<FileView>` based on `view`
- Renders `<CommandPalette>` and `<Settings>` as portals
- Handles Ctrl+P

**`SearchView.svelte`** owns:
- The search topbar (logo, search box, source selector, gear)
- `doSearch`, `loadMore`, `isLoadingMore`, `searchError`
- `<ResultList>` + result count
- Emits `open(SearchResult)` upward

**`FileView.svelte`** owns:
- The compact topbar
- Sidebar (tree toggle, resize handle, `<DirectoryTree>`)
- Path bar
- `<FileViewer>` or `<DirListing>` depending on panel mode
- Emits `back()` upward

The coordinator passes `fileSource`, `currentFile`, `fileSelection`, `panelMode`,
`currentDirPrefix` as props to `FileView` and receives mutations back via events.

---

### 2. Replace per-card context loading with a batch fetch

**Current:** Each `SearchResultItem` mounts an `IntersectionObserver`, waits 150 ms,
fires `GET /api/v1/context`, populates `contextLines`. Five async hand-offs per card.
When results are virtualised and cards remount, the whole chain re-runs.

**After:** Context is batch-fetched once, immediately after search results arrive, using
the existing `POST /api/v1/context-batch` endpoint.

- `doSearch` (in `SearchView`) fires `contextBatch` for the first page of results right
  after the search response arrives. Context is attached to each result before
  `ResultList` renders.
- On `loadMore`, batch-fetch context for the new page of results.
- `SearchResult.svelte` receives context as a prop: `contextStart`, `contextMatchIndex`,
  `contextLines`. No `onMount`, no `IntersectionObserver`, no `setTimeout`, no
  `ResizeObserver` needed inside the card.

**API change:** The existing `contextBatch` function in `api.ts` is already correct.
`SearchView` calls it with `window: 2` (two lines before + match + two after = 5 total,
matching the user's requirement).

**SearchResult.svelte becomes a pure display component** — props in, HTML out, no async.

---

### 3. Remove TanStack Virtual from `ResultList`

**Current:** Virtual scroll with `@tanstack/svelte-virtual`, `use:measureEl` Svelte
action, `ResizeObserver` per item, absolute positioning. Necessary in principle for
very large lists but adds significant complexity for the common case (20–100 results).

**After:** Plain CSS scroll. `ResultList.svelte` becomes a simple `{#each}` loop in a
scrollable container. The "load more" trigger switches from virtualiser proximity to an
`IntersectionObserver` on a sentinel element at the bottom of the list.

```svelte
<div class="scroll-container">
  {#each results as result (result.path + result.line_number)}
    <div class="result-pad">
      <SearchResultItem {result} on:open=... />
    </div>
  {/each}
  {#if hasMore}
    <div use:sentinel />   <!-- IntersectionObserver triggers loadMore -->
  {/if}
</div>
```

For the batch sizes being used (initial 50, pages of 20), this renders comfortably
without any virtualisation. If performance becomes a concern at very high counts,
virtualisation can be re-added as a targeted optimisation with its own plan.

---

### 4. Split `routes.rs` into handler modules

**Current:** 708-line file containing all HTTP handlers.

**After:**

```
crates/server/src/routes/
  mod.rs        router construction, shared helpers (auth, AppState access)
  search.rs     GET /search
  context.rs    GET /context, POST /context-batch
  file.rs       GET /file
  tree.rs       GET /tree, GET /sources
  bulk.rs       POST /bulk (write path)
```

Each module stays under ~150 lines. `compact_lines` and other helpers move to the
module that uses them.

---

### 5. Extract `MarkdownViewer.svelte` from `FileViewer.svelte`

**Current:** `FileViewer.svelte` is 422 lines, mixing file fetch, highlight, line
selection, word wrap, and markdown rendering.

**After:** A `MarkdownViewer.svelte` component receives `rawContent: string` and
`isFormatted: boolean`, handles the `marked` rendering, and owns the format toggle
button. `FileViewer` drops ~60 lines and the `marked` import.

---

## Files Changed

| File | Change |
|------|--------|
| `web/src/routes/+page.svelte` | Shrinks to ~120-line coordinator |
| `web/src/lib/SearchView.svelte` | New — search topbar + results |
| `web/src/lib/FileView.svelte` | New — file topbar + sidebar + viewer |
| `web/src/lib/appState.ts` | New — URL/history state management |
| `web/src/lib/SearchResult.svelte` | Becomes pure display component, no async |
| `web/src/lib/ResultList.svelte` | Remove TanStack Virtual, use sentinel scroll |
| `web/src/lib/FileViewer.svelte` | Extract markdown block |
| `web/src/lib/MarkdownViewer.svelte` | New — markdown render + format toggle |
| `crates/server/src/routes.rs` | Split into `routes/` module directory |
| `crates/server/src/routes/mod.rs` | New — router, shared helpers |
| `crates/server/src/routes/search.rs` | New |
| `crates/server/src/routes/context.rs` | New |
| `crates/server/src/routes/file.rs` | New |
| `crates/server/src/routes/tree.rs` | New |
| `crates/server/src/routes/bulk.rs` | New |

---

## Implementation Order

1. **`appState.ts`** — extract URL/history logic (no visual change, pure refactor)
2. **`SearchView.svelte`** + **`FileView.svelte`** — split `+page.svelte`; verify visually identical behaviour
3. **Batch context + simplify `SearchResult.svelte`** — removes 5-layer async, fixes context display correctly
4. **Remove TanStack Virtual from `ResultList`** — simplify scroll
5. **Split `routes.rs`** — server-side cleanup
6. **Extract `MarkdownViewer.svelte`** — last, lowest risk

Steps 1–4 should be done together as they're tightly coupled on the frontend.
Step 5 is independent and can be done at any time.
Step 6 is a small cleanup that can follow whenever convenient.

---

## Testing

After each step:
- Search works, results display with context (2 lines before + match + 2 after)
- Browser back/forward restores correct view and query
- File opens from result click and from Ctrl+P
- Directory tree navigates and opens files
- "Load more" appends results
- Settings and source selector function correctly
- `pnpm run check` passes (TypeScript)

After step 5:
- `cargo check -p find-server` passes

---

## Non-Goals

- No new features
- No change to API contracts
- No change to the Rust extractor or write path
- No CSS/visual changes
