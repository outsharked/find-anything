# Enhanced Results Navigation

## Overview

Three related improvements to file-view navigation:

1. **Deep link bug fix** — Hard-reloading a file-view URL reverts to search results
2. **Click-to-select line** — Click any line to set it as the highlighted line; URL updates
3. **Multi-line selection** — Ctrl+click (add/toggle), Shift+click (range); uses hash fragment

---

## Bug: Deep Link Restores as Results View

### Root Cause

`doSearch` unconditionally sets `view = 'results'` after the fetch completes (both in the
`try` and `catch` branches, `+page.svelte:204,207`). When `applyState` restores a `view=file`
state it sets `view = 'file'` and then calls `doSearch(push=false)` to populate the results
list — but `doSearch` overwrites `view` back to `'results'` when the async fetch finishes.

### Fix

In `doSearch`, only switch to results view on interactive searches:

```typescript
// Only change view when this is a live search, not a state restoration.
if (push) view = 'results';
```

This applies to both the `try` and `catch` branches. State-restore searches populate
`results` + `totalResults` in the background without clobbering the current view.

**File:** `web/src/routes/+page.svelte`

---

## Line Selection: URL Design

Move line selection out of the query string and into the **URL hash**, so that changing the
selected line is a lightweight `history.replaceState` (no full navigation) and the file-view
query params remain stable for sharing.

### Hash format

```
#L43           single line
#L43,50,60     non-contiguous lines
#L20-30        range (inclusive)
#L20-30,43     mixed: range + individual line
```

Parsing rules:
- Split on `,`
- Each token is either `N` (single) or `N-M` (range)
- All values are 1-based line numbers

### Effect on query string

Remove `line=` from query params (`buildUrl`, `restoreFromParams`, `AppState`). The `line`
param is gone; selection lives exclusively in the hash.

---

## State Type: `LineSelection`

Replace `fileTargetLine: number | null` with a structured type:

```typescript
// Exported from a new web/src/lib/lineSelection.ts utility module
export type LinePart = number | [number, number]; // number = single, tuple = range [start, end]
export type LineSelection = LinePart[];            // empty array = no selection

export function parseHash(hash: string): LineSelection { ... }
export function formatHash(sel: LineSelection): string { ... }   // → "#L43,50" etc.
export function selectionSet(sel: LineSelection): Set<number> {  // for O(1) membership test
    // expands ranges into a flat set (capped at e.g. 10 000 lines to avoid perf issues)
}
export function firstLine(sel: LineSelection): number | null { ... }
```

`formatHash` returns the empty string when `sel` is empty.

---

## Click Handling in `FileViewer`

### New prop

```typescript
export let selection: LineSelection = [];
```
(Replaces `targetLine: number | null`)

**Event dispatched:** `lineselect: { selection: LineSelection }`

### Click logic on `<tr>`

```typescript
function handleLineClick(lineNum: number, e: MouseEvent) {
    if (e.ctrlKey || e.metaKey) {
        // Toggle this line in the selection (add if absent, remove if present).
        selection = toggleLine(selection, lineNum);
    } else if (e.shiftKey && selection.length > 0) {
        // Extend from the last anchor point to lineNum.
        const anchor = firstLine(selection)!;
        selection = [anchor <= lineNum ? [anchor, lineNum] : [lineNum, anchor]];
    } else {
        // Plain click: set single line.
        selection = [lineNum];
    }
    dispatch('lineselect', { selection });
}
```

### Rendering

- `class:target` on `<tr>` changes to `class:target={highlightedSet.has(lineNum)}`
- `$: highlightedSet = selectionSet(selection)` (reactive)
- The `▶` arrow indicator: show on `firstLine(selection)` only (scroll target)
- On mount: `scrollToLine(firstLine(selection))` instead of `scrollToLine(targetLine)`
- Cursor on `<tr>`: `cursor: pointer`

### Scroll-to-first-line

`scrollToLine` scrolls to `firstLine(selection)` on initial mount and whenever
`selection` changes from outside (e.g. URL restore).

---

## `+page.svelte` Wiring

### State

```typescript
let fileSelection: LineSelection = [];  // replaces fileTargetLine
```

### Hash sync

```typescript
function syncHash() {
    const h = formatHash(fileSelection);
    // replaceState so line changes don't pollute browser history
    history.replaceState(history.state, '', h ? window.location.pathname + window.location.search + h : window.location.pathname + window.location.search);
}
```

Called from `handleLineSelect`:
```typescript
function handleLineSelect(e: CustomEvent<{ selection: LineSelection }>) {
    fileSelection = e.detail.selection;
    syncHash();
}
```

### `buildUrl`

Remove `line=` param entirely. `buildUrl` produces the query string only.
`pushState` calls `history.pushState(s, '', buildUrl(s) + formatHash(fileSelection))`.

### `restoreFromParams`

Read hash from `window.location.hash` (separately from search params):
```typescript
const hash = window.location.hash;
fileSelection = parseHash(hash);  // replaces fileTargetLine parse
```

### `AppState`

Replace `fileTargetLine: number | null` with `fileSelection: LineSelection`.
`captureState` / `applyState` updated accordingly.

### `openFile` (from search result click)

Set `fileSelection = result.line_number ? [result.line_number] : []`.

---

## Files Changed

| File | Change |
|---|---|
| `web/src/routes/+page.svelte` | Fix deep link bug; replace `fileTargetLine` with `fileSelection`; hash sync |
| `web/src/lib/FileViewer.svelte` | Replace `targetLine` prop with `selection: LineSelection`; click handlers |
| `web/src/lib/lineSelection.ts` | New utility: `LinePart`, `LineSelection`, parse/format/set helpers |

---

## Breaking Changes

None externally. The `?line=N` query param is removed from generated URLs; old shared links
with `?line=N` will open the file but without line highlighting (graceful degradation).

---

## Verification

1. `npm run check` — 0 errors
2. Hard-reload `?q=foo&view=file&fsource=X&path=bar.py&line=10` — file view shown (not results)
   - Note: `line=` in query string is now ignored; if sharing a new URL, line is in hash
3. Hard-reload `?q=foo&view=file&fsource=X&path=bar.py#L10` — file shown, line 10 highlighted
4. Click line 20 → `#L20` in URL, line 20 highlighted, scroll to it
5. Ctrl+click line 30 → `#L20,30`, both highlighted
6. Shift+click line 25 → `#L20-25` (range from first anchor to clicked line)
7. Ctrl+click already-selected line → deselects it
8. Browser Back/Forward still navigate between files correctly
