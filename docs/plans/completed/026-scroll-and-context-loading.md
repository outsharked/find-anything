# Plan 026: Page-Scroll Architecture for Load-More and Lazy Context

## Overview

Plans 022–024 added infinite scroll via TanStack Virtual (later removed) and then a simplified
load-more approach. Three attempts at the simplified approach all failed with the same symptom:
the loading spinner never stops. Root cause: scroll trigger lives in `ResultList` but state
(`results`, `totalResults`) lives in `+page.svelte`; calling an async function prop across that
boundary makes Svelte's `$$invalidate` unreliable after `await`.

This plan fixes it by moving the load trigger (IntersectionObserver on a sentinel div) into
`+page.svelte`, right next to the state it modifies, and letting the page scroll naturally.

## Design

### Architecture

```
<body> scrolls naturally (no inner scroll container)
  .page (block, no height/overflow for results; flex+overflow only for file view)
    <SearchView>
      .topbar  (position: sticky; top: 0)     ← sticks to viewport
      .content
        result-meta ("X results")
        <ResultList>  ← pure display component, no scroll logic
          {#each results}
            <SearchResultItem>  ← IntersectionObserver for lazy context load
          {/each}
        </ResultList>
    </SearchView>
    <load-row>  ← shown when loadingMore, in +page.svelte
    <sentinel>  ← IntersectionObserver root=viewport, rootMargin=400px
```

### Key decisions

- **Sentinel + IO in `+page.svelte`**: All state (`results`, `totalResults`, `loadingMore`)
  and the load trigger live in the same component. No async function props, no cross-boundary
  `$$invalidate` issues.
- **`triggerLoad()` is synchronous**: Uses `.then().finally()` instead of `async/await`. Svelte
  compiles all assignments inside a component's `.then()` callbacks correctly.
- **Page scroll, sticky topbar**: Eliminates the inner `overflow-y: auto` div in ResultList.
  The browser handles scroll natively. `position: sticky` on the topbar is simpler than any
  Svelte-based solution.
- **`rootMargin: '400px'`**: Preemptively fires ~400px before the sentinel enters view, so next
  batch starts loading before the user reaches the bottom.
- **Batches of 50**: Each `triggerLoad()` fetches 50 results at `offset = results.length`.
  After each load the sentinel is pushed down by the new cards (~5000px), so the IO naturally
  fires again only when the user scrolls further.
- **`window.scrollTo(0, 0)` on new search**: Resets page scroll when a fresh search is
  performed, so results always start at top.
- **`ResultList` becomes pure**: Removes all loading logic, scroll handler, and inner scroll
  container. Only renders the result cards.
- **FileView unchanged**: `.page.file-view` class restores `height: 100vh; overflow: hidden;
  display: flex` when the file viewer is active.
- **Context lazy loading unchanged**: `SearchResult.svelte` keeps its existing
  IntersectionObserver (fires when card enters viewport, fetches context, shows placeholder
  until ready). This works correctly because the page scrolls natively.

## Files changed

| File | Change |
|------|--------|
| `web/src/routes/+page.svelte` | Add `loadingMore`, `hasMore`, `setupSentinel`, `triggerLoad()`; add sentinel/load-row to template; `.page` → `.page.file-view`; scroll reset |
| `web/src/lib/SearchView.svelte` | Remove `loadMore` prop; sticky topbar; remove flex/overflow constraints from `.content` |
| `web/src/lib/ResultList.svelte` | Remove all loading logic and scroll container; pure display |
| `web/src/lib/SearchResult.svelte` | No change (lazy context IO already works) |

## Testing

1. Search for a common term with > 50 results — verify first 50 load, scroll down, next 50
   load automatically before reaching bottom.
2. Search with < 50 results — verify no load-more spinner appears.
3. Search with > 500 results — verify batches load on scroll, eventually capped by server.
4. Type new search mid-scroll — verify results reset to top, old spinner gone.
5. Navigate to file view and back — verify sticky topbar and scroll all work.
6. Verify context placeholder → 5-line context loads as each card scrolls into view.
