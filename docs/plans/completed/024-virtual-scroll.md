# Plan 024: Virtual Scroll for Search Results

## Overview

Replace the current "Load More" button with virtual/infinite scrolling for search
results. Instead of rendering all results in the DOM at once, only render the
items currently visible in the viewport, loading more from the server automatically
as the user scrolls to the bottom.

This eliminates the explicit "Load More" button and makes browsing large result
sets seamless while keeping DOM node counts low regardless of total result count.

---

## Background Research

### Library Evaluated: `@tanstack/svelte-virtual`

**Version:** `3.13.18`
**Peer deps:** `svelte: ^3.48.0 || ^4.0.0 || ^5.0.0` ✅ Compatible with our Svelte 4

**Why TanStack Virtual:**
- Headless UI — zero opinion on markup or styles; we keep full control
- Handles variable/dynamic row heights via `estimateSize` + `measureElement`
- Battle-tested in production (React, Vue, Svelte, Solid)
- Active TanStack ecosystem, well-maintained

**Alternatives considered:**
- `svelte-virtual-scroll-list` v1.3.0 — simpler API, but less ecosystem support
- `svelte-tiny-virtual-list` — zero deps, but manual `recomputeSizes()` required

### How TanStack Virtual Handles Dynamic Heights

TanStack uses a two-pass approach:

1. **Initial render**: `estimateSize: () => 45` gives every item a rough height
   so scroll calculations can proceed immediately.

2. **Measurement pass**: Each rendered item's DOM element is bound and passed
   to `measureElement()`. The virtualizer updates its internal size map with
   actual measured heights.

3. **Layout**: Items are rendered with `position: absolute` at `top: row.start`
   inside a container whose height is set to `virtualizer.getTotalSize()`. This
   creates a real scrollable area that browsers handle natively.

```typescript
// Core pattern from the TanStack Svelte dynamic example:
$: virtualizer = createVirtualizer<HTMLDivElement, HTMLDivElement>({
    count,                            // total item count
    getScrollElement: () => listEl,   // scrollable container ref
    estimateSize: () => 45,           // initial height estimate per item
});

$: virtualItems = $virtualizer.getVirtualItems(); // only visible items
```

Items render:
```svelte
<div style="position: relative; height: {$virtualizer.getTotalSize()}px">
    {#each virtualItems as row (row.index)}
        <div
            bind:this={itemEls[row.index]}
            style="position: absolute; top: {row.start}px; width: 100%"
            use:measureEl={row.index}
        >
            <SearchResultItem result={results[row.index]} ... />
        </div>
    {/each}
</div>
```

---

## Key Challenge: Variable Heights from Lazy Context Loading

Each `SearchResult` card has **dynamic height** because context lines load
lazily via `IntersectionObserver`:

- **Before context loads**: header + 1 snippet line ≈ 55–65px
- **After context loads**: header + up to 7 lines (3 before + match + 3 after) ≈ 130–160px

With virtual scroll, height changes after initial measurement break layout unless
we re-measure. Solution: **`ResizeObserver` per item**.

Bind a `ResizeObserver` to each rendered virtual item element. When its height
changes (context lines expand), re-call `measureElement()` so the virtualizer
updates its size map and recomputes positions for subsequent items.

```typescript
// Svelte action applied to each virtual item wrapper:
function measureEl(node: HTMLElement, index: number) {
    virtualizer.measureElement(node);          // initial measure
    const ro = new ResizeObserver(() => {
        virtualizer.measureElement(node);       // re-measure on height change
    });
    ro.observe(node);
    return { destroy: () => ro.disconnect() };
}
```

This is zero-cost for stable items (context already loaded), and automatically
corrects positions when context expands.

---

## Infinite Scroll Replacing "Load More"

Virtual scroll naturally enables infinite scroll: watch whether the last
rendered virtual item is near the bottom, and if more results exist on the
server, fetch the next batch.

```typescript
$: {
    const lastItem = virtualItems[virtualItems.length - 1];
    if (lastItem && lastItem.index >= results.length - 5 && hasMore && !isLoadingMore) {
        loadMore();
    }
}
```

The server-side pagination is already implemented (offset + limit). The UI
change is:
- Remove the "Load More" button
- Trigger `loadMore()` automatically when scrolled near the last loaded result
- Show a small loading indicator at the bottom while fetching

**Server request on scroll:**
- Initial load: `limit=50, offset=0`
- Each subsequent batch: `limit=50, offset=results.length`
- Results **appended** to existing array (not replaced)

This is a change from the current load-more which re-fetches from offset 0
with a higher limit. Infinite scroll needs true append-mode pagination.

---

## Implementation Plan

### Files to Change

| File | Change |
|------|--------|
| `web/package.json` | Add `@tanstack/svelte-virtual` dependency |
| `web/src/lib/ResultList.svelte` | Full rewrite to virtual list |
| `web/src/routes/+page.svelte` | `doSearch` append mode, scroll-triggered load |

No server changes needed — the API already supports `limit` and `offset`.

---

### Step 1: Install Dependency

```bash
cd web
pnpm add @tanstack/svelte-virtual
```

---

### Step 2: Rewrite `ResultList.svelte`

The component takes on responsibility for scroll management and infinite load
triggering. The `isLoadingMore` and `loadmore` event stay on the same interface
so `+page.svelte` changes are minimal.

```svelte
<script lang="ts">
    import { createVirtualizer } from '@tanstack/svelte-virtual';
    import { createEventDispatcher } from 'svelte';
    import type { SearchResult } from '$lib/api';
    import SearchResultItem from '$lib/SearchResult.svelte';

    export let results: SearchResult[] = [];
    export let totalResults = 0;
    export let isLoadingMore = false;
    export let searching = false;

    const dispatch = createEventDispatcher<{ open: SearchResult; loadmore: void }>();

    let listEl: HTMLDivElement;

    $: hasMore = results.length < totalResults;
    $: count = results.length + (hasMore ? 1 : 0);  // +1 for loader row

    $: virtualizer = listEl
        ? createVirtualizer<HTMLDivElement, HTMLDivElement>({
            count,
            getScrollElement: () => listEl,
            estimateSize: (i) => i === results.length ? 56 : 100,
            overscan: 5,
          })
        : null;

    $: virtualItems = $virtualizer?.getVirtualItems() ?? [];

    // Trigger load more when near end
    $: {
        const last = virtualItems[virtualItems.length - 1];
        if (last && last.index >= results.length - 3 && hasMore && !isLoadingMore) {
            dispatch('loadmore');
        }
    }

    function measureEl(node: HTMLElement, index: number) {
        $virtualizer?.measureElement(node);
        const ro = new ResizeObserver(() => $virtualizer?.measureElement(node));
        ro.observe(node);
        return { destroy: () => ro.disconnect() };
    }
</script>

<div class="result-list" class:searching>
    {#if results.length === 0 && !searching}
        <p class="empty">No results.</p>
    {:else}
        <div class="scroll-container" bind:this={listEl}>
            <div style="position: relative; height: {$virtualizer?.getTotalSize() ?? 0}px">
                {#each virtualItems as row (row.index)}
                    <div
                        use:measureEl={row.index}
                        style="position: absolute; top: {row.start}px; width: 100%"
                    >
                        {#if row.index < results.length}
                            <SearchResultItem
                                result={results[row.index]}
                                on:open={(e) => dispatch('open', e.detail)}
                            />
                        {:else}
                            <!-- Loader row -->
                            <div class="loader-row">
                                {#if isLoadingMore}
                                    <div class="spinner">
                                        <svg viewBox="0 0 24 24" fill="none">
                                            <circle cx="12" cy="12" r="10" stroke="currentColor" stroke-width="3" opacity="0.25"/>
                                            <path d="M12 2a10 10 0 0 1 10 10" stroke="currentColor" stroke-width="3" stroke-linecap="round"/>
                                        </svg>
                                    </div>
                                    <span>Loading more results...</span>
                                {/if}
                            </div>
                        {/if}
                    </div>
                {/each}
            </div>
        </div>

        {#if !hasMore && results.length > 0}
            <div class="all-loaded">
                ✓ Showing all {totalResults.toLocaleString()} results
            </div>
        {/if}
    {/if}
</div>
```

Key style rules:
```css
.scroll-container {
    height: calc(100vh - 120px);  /* fills available vertical space */
    overflow-y: auto;
    overflow-x: hidden;
}

.result-list.searching .scroll-container {
    opacity: 0.5;
    transition: opacity 0.2s ease-in-out;
}
```

---

### Step 3: Update `+page.svelte`

**Switch load-more to append mode** (results accumulate, not replace):

```typescript
async function loadMore() {
    if (isLoadingMore || !hasMore) return;
    isLoadingMore = true;
    try {
        const resp = await search({
            q: query,
            mode,
            sources: selectedSources,
            limit: 50,
            offset: results.length,   // true offset pagination
        });
        results = [...results, ...resp.results];  // APPEND
        totalResults = resp.total;
    } catch (e) {
        searchError = String(e);
    } finally {
        isLoadingMore = false;
    }
}
```

**Reset results on new search** (results replace, not append):

```typescript
async function doSearch(q, m, srcs, push = true) {
    if (q.trim().length < 3) {
        results = [];
        totalResults = 0;
        return;
    }
    searching = true;
    searchError = null;
    if (push) pushState();
    try {
        const resp = await search({ q, mode: m, sources: srcs, limit: 50, offset: 0 });
        results = resp.results;       // REPLACE on new search
        totalResults = resp.total;
        if (push) view = 'results';
    } catch (e) {
        searchError = String(e);
    } finally {
        searching = false;
    }
}
```

Remove `isLoadingMore` from `doSearch` — it's now only used by `loadMore()`.

**Remove result-meta "Showing X of Y" display** (virtual scroll makes this
less useful; total is shown in the existing `result-meta` line):

```svelte
<!-- Keep this, already informative enough: -->
<div class="result-meta">
    {totalResults.toLocaleString()} result{totalResults !== 1 ? 's' : ''}
</div>
```

---

## Height Estimate Strategy

| Item state | Approx height |
|-----------|---------------|
| Snippet only (before context loads) | 60–70px |
| 7 context lines (after context loads) | 140–160px |
| Loader row | 56px |

Initial `estimateSize: () => 100` is a reasonable midpoint. The `ResizeObserver`
will correct individual items as context loads, ensuring layout remains accurate.
`overscan: 5` keeps 5 items above/below viewport rendered, so context loading
fires before items are visible (smooth experience).

---

## Edge Cases

### New Search Resets Scroll Position
When `doSearch` fires, `results` is replaced. The virtualizer should reset scroll
to top. Achieve this by destroying and recreating the virtualizer (key the
component on the search query) or calling `virtualizer.scrollToIndex(0)`.

### Search While Loading More
If `doSearch` fires while `isLoadingMore` is true, the new search wins:
- `results` is replaced → array length changes → virtualizer recalculates
- In-flight `loadMore` response is ignored (stale closure on old `results` ref)
- This is safe — Svelte's reactivity ensures `results` always reflects latest state

### Empty Results During Blur
The blur effect (`searching` class) should still apply to the scroll container
and its virtual items via a CSS transition on opacity, same as the current
`result-container` approach.

### Server `max_limit`
Each batch requests exactly 50 items. The server's `max_limit: 500` is never
hit because we always request exactly 50. No special casing needed.

---

## Files Changed

| File | Change |
|------|--------|
| `web/package.json` | Add `@tanstack/svelte-virtual` |
| `web/src/lib/ResultList.svelte` | Virtual scroll implementation |
| `web/src/routes/+page.svelte` | Append-mode loadMore, remove isLoadingMore from doSearch |

No changes to `SearchResult.svelte`, `api.ts`, or any Rust code.

---

## Verification

```bash
# Install dep
cd web && pnpm add @tanstack/svelte-virtual

# Type-check
pnpm run check  # should pass with no errors

# Manual test scenarios:
# 1. Search with <50 results → no infinite scroll trigger, "✓ all shown" appears
# 2. Search with >50 results → scroll to bottom → next 50 auto-load
# 3. Rapid typing → new search replaces results, scroll resets to top
# 4. Very tall results (7 context lines) → layout stays correct after context loads
# 5. 500+ results → multiple batches load correctly with correct offsets
```

---

## Version Bump

Patch bump to v0.2.3.
