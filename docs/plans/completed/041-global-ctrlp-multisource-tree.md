# Global ctrl+p and Multi-Source Tree Sidebar

## Overview

Two related pain points with the current single-source constraint:

1. **ctrl+p only searches one source at a time.** `CommandPalette` hardcodes
   `activeSource = sources[0]`, so even when opened with no context it only
   lists files from the first source alphabetically.

2. **The tree sidebar is locked inside `FileView`**, visible only once a file is
   open and always scoped to that file's source. There is no way to browse the
   filesystem before searching, or to navigate between sources in the tree.

## Design Decisions

- `CommandPalette` loads all scoped sources in parallel using `Promise.all`.
  The `sources` prop already contains the correctly-scoped list — no logic
  change to how sources are determined.
- A new `MultiSourceTree` component wraps one `DirectoryTree` per source in a
  collapsible root node. Auto-expands the active source.
- The global sidebar (with resize handle) moves up to `+page.svelte` so it is
  visible in both the search view and the file view.
- `FileView` keeps its `showTree` prop and `treeToggle` event (for the button
  in its topbar), but no longer owns the sidebar DOM.
- `SearchView` gets a `showTree` prop and a matching tree-toggle button.

## Implementation

### Part 1 — CommandPalette (multi-source)
- Remove `activeSource` reactive variable.
- On open, load all sources in parallel.
- Merge per-source file lists into `allItems` with a `source` field.
- Show source badge per row when `sources.length > 1`.
- `confirm()` uses `item.source` instead of `activeSource`.

### Part 2 — MultiSourceTree component
- New `web/src/lib/MultiSourceTree.svelte`.
- Each source renders as a collapsible header + `DirectoryTree`.
- Auto-expands the `activeSource`.

### Part 3 — Lift sidebar to page level
- `sidebarWidth` and resize logic move from `FileView` to `+page.svelte`.
- `+page.svelte` renders `<aside>` + resize-handle + `MultiSourceTree` outside
  the view components when `showTree` is true.
- `FileView` sidebar/resize DOM removed; `showTree` prop kept for toggle button.
- `SearchView` gets `showTree` prop + tree-toggle button.

## Files Changed

- `web/src/lib/CommandPalette.svelte` — multi-source load + source badge
- `web/src/lib/MultiSourceTree.svelte` — **new**
- `web/src/routes/+page.svelte` — global sidebar at page level
- `web/src/lib/FileView.svelte` — remove sidebar/resize DOM
- `web/src/lib/SearchView.svelte` — add showTree prop + toggle button

## Testing

1. ctrl+p on fresh load → files from all sources appear with source badge.
2. ctrl+p with source filter → scoped to selected sources only.
3. Tree toggle from search view → sidebar visible with source roots.
4. Tree toggle from file view → sidebar persists when switching views.
5. Active file → its source auto-expands and file is highlighted.
6. Single source → no regression.
7. `pnpm run check` passes.
