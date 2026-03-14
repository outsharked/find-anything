# Plan 022: Result Pagination and Load More

## Context

Currently, search results show all matching documents up to a hardcoded limit of 50 results. For searches with large result sets (hundreds or thousands of matches), this creates two problems:

1. **Performance**: Loading and rendering hundreds of results causes UI lag
2. **User Experience**: The initial 50 results may not include what the user is looking for, but they have no way to see more

The API already supports pagination (via `offset` and `limit` parameters) and returns both the limited result set and the total count. This plan adds:

1. **Configurable initial limit** - Both server and client can configure default result limits
2. **"Load More" UI** - Shows remaining result count and loads complete set on click
3. **Loading state feedback** - Visual indication while fetching additional results

---

## Current State (from exploration)

### API Already Supports Pagination ✓

**`SearchResponse` structure** (`crates/common/src/api.rs` lines 146-151):
```rust
pub struct SearchResponse {
    pub results: Vec<SearchResult>,  // Limited by pagination
    pub total: usize,                 // Total matches (all sources)
}
```

**Search endpoint** (`crates/server/src/routes.rs` lines 317-442) accepts:
- `q`: search query (required)
- `mode`: "fuzzy" | "exact" | "regex" (default: "fuzzy")
- `source`: filter by source(s) (repeatable)
- `limit`: results per request (default: 50, max: 500)
- `offset`: skip N results (default: 0)

**Current defaults:**
- Server `default_limit`: 50 (`routes.rs` line 271)
- Server `max_limit`: 500 (`config.rs` line 175)
- UI hardcoded: `limit: 50` (`+page.svelte` line 250)

### No Pagination UI Currently

The web UI:
- Stores `totalResults` but doesn't display it prominently
- Shows `{totalResults} result{s}` but provides no way to load more
- Always requests exactly 50 results

---

## Design Decisions

### Load Strategy (User Preference)

**Choice: Load all remaining results at once**

When user clicks "Load More", fetch all remaining results in a single request (up to server's `max_limit`). This provides:
- **Simple UX**: One click to see everything
- **No pagination complexity**: Straightforward implementation
- **Good for most use cases**: Works well for up to ~thousands of results
- **Server enforces safety**: `max_limit` (500) prevents unbounded requests

Alternative considered but rejected: Batch loading (fetch 50-100 at a time). More complex, requires multiple clicks, only beneficial for extremely large result sets (10k+), which are rare with fuzzy search.

### Configuration Approach

**Server-side** (`ServerAppConfig`):
```toml
[search]
default_limit = 50    # Initial results shown
max_limit = 500       # Maximum results per request
```

**Client-side**: No additional config needed. The UI will:
1. Request `default_limit` results initially (50)
2. On "Load More" click, request `max_limit` results (500) starting from offset 0

This keeps the client simple - no config changes required.

### UI Design

**Location**: Below the result list, before any footer content

**Display logic**:
```typescript
hasMore = results.length < totalResults
remainingCount = totalResults - results.length
```

**Visual design**:
```
┌─────────────────────────────────────────────┐
│ [Result 1]                                  │
│ [Result 2]                                  │
│ ...                                         │
│ [Result 50]                                 │
├─────────────────────────────────────────────┤
│ ⋯ 1,234 more results · Load All             │  ← Clickable
└─────────────────────────────────────────────┘
```

**Loading state** (while fetching):
```
┌─────────────────────────────────────────────┐
│ ⋯ Loading 1,234 more results...   [spinner]│
└─────────────────────────────────────────────┘
```

**After loading** (all results shown):
```
┌─────────────────────────────────────────────┐
│ [Result 1]                                  │
│ ...                                         │
│ [Result 1,284]                              │
├─────────────────────────────────────────────┤
│ ✓ Showing all 1,284 results                 │
└─────────────────────────────────────────────┘
```

---

## Implementation Plan

### 1. No Server Changes Needed ✓

The server already:
- Returns `{ results, total }` in `SearchResponse`
- Accepts `limit` and `offset` parameters
- Enforces `max_limit` cap (500)
- Handles pagination correctly

**No code changes required.**

### 2. Web UI Changes

**Files to modify:**
- `web/src/lib/ResultList.svelte` - Add "Load More" component
- `web/src/routes/+page.svelte` - Add `loadMore()` function
- `web/src/lib/api.ts` - Already supports pagination parameters ✓

#### A. Update `web/src/routes/+page.svelte`

**Add state tracking:**
```typescript
let isLoadingMore = false;  // Loading state for "Load More"
```

**Modify `doSearch()` to track whether this is initial search or load-more:**
```typescript
async function doSearch(
	q: string,
	m: string,
	srcs: string[],
	push = true,
	append = false  // NEW: true when loading more
) {
	if (q.trim().length < 3) {
		results = [];
		totalResults = 0;
		searchError = null;
		return;
	}

	if (append) {
		isLoadingMore = true;
	} else {
		searching = true;
	}

	searchError = null;
	if (push) pushState();

	try {
		// When loading more, request max_limit (500) with offset 0
		// This gives us the full result set up to server's safety limit
		const limit = append ? 500 : 50;
		const offset = 0;

		const resp = await search({ q, mode: m, sources: srcs, limit, offset });

		if (append) {
			results = resp.results;  // Replace with full set
		} else {
			results = resp.results;
		}

		totalResults = resp.total;
		if (push) view = 'results';
	} catch (e) {
		searchError = String(e);
		if (push) view = 'results';
	} finally {
		if (append) {
			isLoadingMore = false;
		} else {
			searching = false;
		}
	}
}
```

**Add `loadMore()` function:**
```typescript
function loadMore() {
	doSearch(query, mode, selectedSources, false, true);
}
```

**Pass props to ResultList:**
```svelte
<ResultList
	{results}
	{totalResults}
	{isLoadingMore}
	searching={isSearchActive}
	on:open={openFile}
	on:loadmore={loadMore}
/>
```

#### B. Update `web/src/lib/ResultList.svelte`

**Add props:**
```typescript
export let results: SearchResult[] = [];
export let totalResults = 0;
export let isLoadingMore = false;
export let searching = false;
```

**Add "Load More" section:**
```svelte
<div class="result-list" class:searching>
	<!-- Existing result containers... -->

	{#if hasMore}
		<div class="load-more">
			{#if isLoadingMore}
				<div class="loading">
					<div class="spinner">
						<svg viewBox="0 0 24 24" fill="none">
							<circle cx="12" cy="12" r="10" stroke="currentColor" stroke-width="3" opacity="0.25"/>
							<path d="M12 2a10 10 0 0 1 10 10" stroke="currentColor" stroke-width="3" stroke-linecap="round"/>
						</svg>
					</div>
					<span>Loading {remainingCount.toLocaleString()} more results...</span>
				</div>
			{:else}
				<button on:click={handleLoadMore} class="load-more-btn">
					<span class="ellipsis">⋯</span>
					<span class="count">{remainingCount.toLocaleString()} more results</span>
					<span class="action">· Load All</span>
				</button>
			{/if}
		</div>
	{:else if results.length > 0 && results.length === totalResults}
		<div class="all-loaded">
			<span class="checkmark">✓</span> Showing all {totalResults.toLocaleString()} results
		</div>
	{/if}
</div>
```

**Add computed values and handler:**
```typescript
$: hasMore = results.length < totalResults;
$: remainingCount = totalResults - results.length;

function handleLoadMore() {
	dispatch('loadmore');
}
```

**Add styles:**
```css
.load-more {
	padding: 16px 12px;
	text-align: center;
	border-top: 1px solid var(--border);
}

.load-more-btn {
	display: flex;
	align-items: center;
	justify-content: center;
	gap: 6px;
	padding: 10px 20px;
	background: var(--bg-secondary);
	border: 1px solid var(--border);
	border-radius: var(--radius);
	color: var(--text-muted);
	cursor: pointer;
	font-size: 14px;
	transition: all 0.15s ease;
	width: 100%;
}

.load-more-btn:hover {
	background: var(--bg-hover);
	border-color: var(--accent-muted);
	color: var(--text);
}

.load-more-btn .ellipsis {
	font-size: 20px;
	line-height: 1;
	color: var(--accent);
}

.load-more-btn .count {
	font-weight: 500;
	color: var(--text);
}

.load-more-btn .action {
	color: var(--accent);
}

.loading {
	display: flex;
	align-items: center;
	justify-content: center;
	gap: 10px;
	color: var(--text-muted);
	font-size: 14px;
}

.loading .spinner {
	width: 16px;
	height: 16px;
}

.loading .spinner svg {
	width: 100%;
	height: 100%;
	color: var(--accent);
	animation: spin 0.8s linear infinite;
}

.all-loaded {
	padding: 16px 12px;
	text-align: center;
	color: var(--text-dim);
	font-size: 13px;
	border-top: 1px solid var(--border);
}

.all-loaded .checkmark {
	color: var(--accent);
}

@keyframes spin {
	from { transform: rotate(0deg); }
	to { transform: rotate(360deg); }
}
```

#### C. Update result metadata display

In `+page.svelte`, update the result count display to be more informative:

**Before:**
```svelte
<div class="result-meta">{totalResults} result{totalResults !== 1 ? 's' : ''}</div>
```

**After:**
```svelte
<div class="result-meta">
	{#if results.length < totalResults}
		Showing {results.length.toLocaleString()} of {totalResults.toLocaleString()} results
	{:else}
		{totalResults.toLocaleString()} result{totalResults !== 1 ? 's' : ''}
	{/if}
</div>
```

---

## Files Changed

| File | Change |
|------|--------|
| `web/src/routes/+page.svelte` | Add `isLoadingMore` state, modify `doSearch()` to support append mode, add `loadMore()` handler, update result count display |
| `web/src/lib/ResultList.svelte` | Add "Load More" button, loading state, and completion message; add event dispatcher for `loadmore` |
| `web/src/lib/api.ts` | No changes needed - already supports `limit` and `offset` ✓ |

**No server-side changes needed.**

---

## Edge Cases and Considerations

### 1. Max Limit Cap (500 results)

**Scenario**: User searches for "the" and gets 10,000 results.

**Behavior**:
- Initial load: Shows 50 results, "9,950 more results · Load All"
- After clicking: Fetches 500 results (server max), shows "9,500 more results" still visible
- User can click again to load next 500

**Alternative**: Hide "Load More" after hitting max and show "Showing first 500 of 10,000 results"

**Decision**: Implement basic version first (keep clicking to load more 500 at a time). Can refine UX later if needed.

### 2. Result Ordering Stability

**Question**: If results are re-fetched with different limit, will order change?

**Answer**: No. Results are sorted by score once on the server (line 437 in `routes.rs`), then paginated. Order is stable within a single search query.

### 3. Search While Loading More

**Behavior**: If user modifies search query while `isLoadingMore` is true, the new search should cancel the in-flight request and start fresh.

**Implementation**: The `doSearch()` function already handles this correctly - new searches set `searching = true` and replace results regardless of `isLoadingMore` state.

### 4. State Restoration from URL

When restoring search from browser history, always start with initial limit (50). Don't persist "loaded all" state in URL params.

---

## Verification

### Test Cases

**1. Small result set (< 50 results)**
```
1. Search for a unique term
2. Verify no "Load More" button appears
3. Verify "✓ Showing all N results" appears if N > 0
```

**2. Medium result set (50-500 results)**
```
1. Search for a common term
2. Verify initial load shows 50 results
3. Verify "X more results · Load All" button appears
4. Click "Load All"
5. Verify loading spinner appears
6. Verify all results load (up to 500)
7. Verify button disappears and "✓ Showing all N results" appears
```

**3. Large result set (> 500 results)**
```
1. Search for very common term (e.g., "the")
2. Verify initial load shows 50 results
3. Click "Load All"
4. Verify 500 results load
5. Verify "Load More" button still appears with remaining count
6. Click again
7. Verify next 500 load (offset 500)
```

**4. Search while loading**
```
1. Search for common term
2. Click "Load All"
3. Immediately type new search query
4. Verify new search replaces loading state
5. Verify no duplicate results
```

**5. Result count formatting**
```
1. Search with 1,234 results
2. Verify comma formatting: "1,234 more results"
3. Verify singular/plural: "1 result" vs "2 results"
```

### Manual Testing

```bash
# Start server
cd crates/server
cargo run -- --config ../../config/server.toml

# Start web UI dev server
cd web
pnpm run dev

# Test scenarios:
# - Search for "" (few results)
# - Search for "function" (medium results)
# - Search for "a" or "the" (large result set)
```

### Type Checking

```bash
cd web
pnpm run check  # Should pass with no errors
```

---

## Future Enhancements (Not in This Plan)

1. **Virtualized scrolling**: For extremely large result sets (1000+), render only visible results using virtual scrolling library
2. **Infinite scroll**: Alternative to "Load More" button - auto-load on scroll to bottom
3. **Jump to page**: "Show results 100-150" navigation
4. **Configurable client limit**: Add `search.initial_limit` to client config (currently hardcoded 50)
5. **Result caching**: Cache fetched results to avoid re-fetching on back/forward navigation
6. **Search within results**: Client-side filtering of already-loaded results

---

## Summary

This plan adds a simple, effective "Load More" mechanism that:
- ✓ Requires **no server changes** (API already supports it)
- ✓ Loads all results on single click (up to server's 500 limit)
- ✓ Shows clear loading state and completion feedback
- ✓ Handles edge cases (large result sets, search while loading)
- ✓ Uses existing API patterns and UI components
- ✓ Minimal code changes (2 files, ~100 lines total)
