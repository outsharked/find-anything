# UX Improvements

## Overview

Seven UX improvements to the web UI, ranging from a quick bug fix and CSS
tweak through to browser history and a VS Code-style Ctrl+P file picker.

---

## 1 — User profile (localStorage)

A lightweight client-side profile stores per-user preferences so settings
survive page reloads.  Full user accounts are deferred to a later release.

**Shape:**
```ts
interface UserProfile {
  sidebarWidth?: number;   // px, default 240
  // future: theme, defaultSource, ...
}
```

**Implementation:**
- `web/src/lib/profile.ts` — Svelte `writable` store initialised from
  `localStorage.getItem('find-anything.profile')`.  `store.subscribe` writes
  back on every change so callers just do `$profile.sidebarWidth = 320`.
- No server involvement; entirely client-side.

**Roadmap note:**  Add "User accounts (cloud profile sync)" to the long-term
section of `ROADMAP.md`.

---

## 2 — Resizable sidebar

The directory tree sidebar should be draggable to any width, with the chosen
width persisted in the user profile.

**Implementation:**
- Replace the fixed `width: 240px` on `.sidebar` with a reactive binding to
  `$profile.sidebarWidth ?? 240`.
- Add a narrow drag-handle `<div class="resize-handle">` between `.sidebar`
  and `.viewer-wrap` in `+page.svelte`.
- On `mousedown` on the handle, listen for `mousemove` on `document` and
  update `$profile.sidebarWidth` in real time; remove listener on `mouseup`.
- Clamp width between 120 px and 600 px.

**Files changed:** `web/src/routes/+page.svelte`, `web/src/lib/profile.ts`
(new).

---

## 3 — Fix: clicking a file in the left nav does nothing (bug)

`TreeRow.svelte` dispatches `{ path, kind }` when a file is clicked.
`DirectoryTree.svelte` forwards the event verbatim, so `source` is missing
from the payload.  `openFileFromTree` in `+page.svelte` reads
`e.detail.source`, which is `undefined`, leaving `fileSource` blank and the
`FileViewer` unable to load anything.

**Fix:** intercept the 'open' event in `DirectoryTree.svelte` before
forwarding it, and inject the `source` prop:

```svelte
<!-- DirectoryTree.svelte -->
function handleOpen(e) {
  dispatch('open', { ...e.detail, source });
}
```

**Files changed:** `web/src/lib/DirectoryTree.svelte`.

---

## 4 — Full-width search results

The results panel has `max-width: 960px; margin: 0 auto` which looks fine
on small screens but wastes space on wide monitors.  Remove the cap so
results span the full content area (matching the detail/file view).

**Files changed:** `web/src/routes/+page.svelte` (`.content` CSS rule).

---

## 5 — Browser history

Use `history.pushState` / `popstate` to encode the full UI state in the URL
so the browser Back/Forward buttons work and results are shareable.

**URL scheme (query params):**

| Param | Values | Meaning |
|---|---|---|
| `q` | string | search query |
| `mode` | fuzzy/exact/regex | search mode |
| `source` | repeatable | selected source filters |
| `path` | string | open file path |
| `file_source` | string | source owning the open file |
| `line` | int | target line in file |
| `dir` | string | directory prefix in dir-listing view |
| `panel` | file/dir | right-panel mode |

**Behaviour:**
- On mount: read params from `window.location.search` and restore state
  (re-run search, open file, etc.).
- On any navigation action (search change, open file, open dir): call
  `history.pushState` with the new params.
- Add a `popstate` listener that restores state from `event.state` (or
  re-parses `window.location.search` as fallback).
- No SvelteKit `goto` needed — stay in the SPA and manipulate history
  directly to avoid full page reloads.

**Files changed:** `web/src/routes/+page.svelte` (significant refactor of
state management).

---

## 6 — Source chips on their own row

With many indexed sources the current horizontal topbar gets crowded.  Move
`SourceChips` out of the topbar into a dedicated strip immediately below it.

**Before:** `logo | chips | search-box` all in one `<div class="topbar">`

**After:**
```
┌──────────────────────────────────────────┐
│ logo          [tree-toggle]   [search-box]│  ← topbar (unchanged height)
├──────────────────────────────────────────┤
│ [code]  [docs]  [secrets]  …             │  ← source-bar (shown only when
└──────────────────────────────────────────┘    sources.length > 0)
```

The source bar is a separate `<div class="source-bar">` with
`overflow-x: auto` so it scrolls horizontally if there are many sources,
rather than wrapping into multiple lines.

**Files changed:** `web/src/routes/+page.svelte`.

---

## 7 — Ctrl+P fuzzy file picker

A VS Code-style command palette that opens on Ctrl+P (Cmd+P on macOS),
lets the user type to fuzzy-search filenames within the currently active
source, and jumps to the selected file.

**UX:**
- Overlay modal with a single text input, focused immediately on open.
- Results list below the input showing `source / path` rows, limited to ~15.
- Arrow keys move selection; Enter opens the file; Escape closes.
- Click outside closes.

**Data source:**
- On first open (per source), fetch `GET /api/v1/files?source=X` — this
  already returns `[{ path, mtime }]` for every indexed file.
- Cache the list in memory for subsequent opens.
- Filter client-side using the same character-subsequence scoring as nucleo
  (a simple character-sequence match is sufficient here since all paths are
  in memory).

**Scope:** restricted to the current source shown in the sidebar
(`fileSource`), or all sources if no file is open.  The existing
`CommandPalette.svelte` placeholder should be implemented.

**Files changed/created:**
- `web/src/lib/CommandPalette.svelte` — implement the placeholder.
- `web/src/routes/+page.svelte` — wire up Ctrl+P keydown listener and pass
  state into `CommandPalette`.

---

## Implementation Order

| # | Item | Complexity | Depends on |
|---|------|------------|------------|
| 3 | Nav file-click bug fix | trivial | — |
| 4 | Full-width results | trivial | — |
| 6 | Source chips row | small | — |
| 1 | User profile store | small | — |
| 2 | Resizable sidebar | medium | 1 |
| 7 | Ctrl+P file picker | medium | — |
| 5 | Browser history | large | — |

Items 3, 4, 6 can be done in a single pass.  Items 1 + 2 naturally go
together.  Item 7 builds on the existing CommandPalette stub.  Item 5 is
the largest and most self-contained; it can be done last without blocking
the others.

## Files Changed

| File | Changes |
|---|---|
| `web/src/lib/profile.ts` | **new** — localStorage-backed Svelte store |
| `web/src/lib/CommandPalette.svelte` | implement Ctrl+P file picker |
| `web/src/lib/DirectoryTree.svelte` | inject `source` into forwarded 'open' event |
| `web/src/routes/+page.svelte` | sidebar resize, full-width CSS, source bar,  browser history, Ctrl+P wiring |
| `ROADMAP.md` | add user accounts to long-term section |

## Breaking Changes

None.  All changes are purely client-side and additive.
