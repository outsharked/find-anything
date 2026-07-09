# Plan 092 — File Viewer Virtualized Scrolling ("infinite scroll window")

## Overview

`FileViewer`'s paged mode (files over `fileViewPageSize`, default 2000 lines)
currently only ever *accumulates*: `loadForward()`/`loadBackward()` append or
prepend pages to `allContentLines`/`allLineOffsets`/`highlightedCode`, and
every line ever loaded stays mounted as a real `<tr>` in `CodeViewer` forever.
Scrolling through a large file leaves thousands of permanent DOM rows behind.

This plan replaces that accumulate-forever model with a **bounded window**:
at any time, `FileViewer` holds only the lines needed to cover the viewport
plus a small overscan buffer, positioned inside the scroll container using
spacer rows so the scrollbar behaves as if the whole file were present.
Scrolling past the edge of the window discards the far end and fetches a new
window, rather than growing forever. Opening a file at a specific line (search
result, `#L` deep link) becomes a direct operation — center a window on the
target and fetch it — instead of today's page-anchoring arithmetic.

No server changes are needed: `/api/v1/file?offset=&limit=` already supports
arbitrary ranges. This is a client-only restructure: `FileViewer.svelte`
becomes a shell that picks one of two sibling text-view components — the new
windowed path and the extracted legacy path — rather than hosting both
behaviors behind conditionals in a single component.

---

## Design Decisions

### Two rendering paths, two components — not one component full of branches

`FileViewer.svelte` is already ~1500 lines and hosts the toolbar, share
dialog, duplicates modal, media viewers, formatted (markdown/RTF/HTML)
views, archive browsing, live-update banners, *and* the paged text-loading
state machine. Threading a second, structurally different loading model
(windowed) through the same component with `if (windowed)` branches at every
scroll/load/measure site would make both paths harder to reason about and
regress.

**Decision:** split along the natural seam — "what is this file and which
viewer applies" vs. "how do I load and render text ranges":

- **`FileViewer.svelte` (shell)** — keeps everything that isn't text-range
  loading: initial `getFile` call, metadata/kind handling, toolbar, media
  and formatted viewers, share/duplicates, live updates. After the initial
  response it picks exactly one text-view component and passes the initial
  page down; it contains no scroll or pagination logic of its own.
- **`WindowedCodeView.svelte` (new)** — the virtualized path, used when
  `pagedMode && !wordWrap`. Owns the scroll container, window state,
  spacer math, row-height/offset measurement, debounced refetch, and
  `jumpToLine()`.
- **`PagedCodeView.svelte` (extracted legacy path)** — the
  accumulate-everything path, moved out of today's `FileViewer` intact:
  `allContentLines`/`allLineOffsets`, `loadForward`/`loadBackward`,
  `appendCodeState`/`prependCodeState`, load sentinels, scroll-prepend
  compensation. Used for word-wrap mode, and for small (non-paged) files —
  where it simply starts with everything loaded and both `noMore*` flags
  set, so no third component is needed.

**Shared building blocks**, used by both view components rather than
duplicated:

- `CodeViewer.svelte` stays the dumb table renderer for both; the spacer
  props are optional and only the windowed path passes them.
- A `fetchLineRange()` helper (new, in `pagination.ts` or a small
  `fileContent.ts`) wrapping `getFile` + the `adjustOffsets` normalization
  both paths currently need — today that logic is repeated at three call
  sites inside `FileViewer`.
- `virtualWindow.ts` math and `highlightFile()`.
- The content that renders *inside* the scroll container above the table
  (meta-panel, encrypted-PDF notice, markdown-too-large notice) is passed
  from the shell as a `header` **snippet** (Svelte 5), so both view
  components render identical header content without owning it. The
  windowed path's `contentOffsetPx` measurement covers whatever the
  snippet renders.
- Both components report `hasOverflow` upward via an `onOverflowChange`
  callback; the shell owns the word-wrap toggle and the latching behavior
  described below.

### Fixed-height virtualization, word-wrap falls back to legacy behavior

Virtualizing a table where every row has the same height is straightforward:
total scrollable height = `totalLines * rowHeight`, and the window's position
is `windowStart * rowHeight` pixels from the top. Virtualizing *variable*
height rows (which is what word-wrap produces — a long line wraps to N
screen-lines depending on container width) requires either measuring every
row after render and correcting spacer heights, or an estimate-then-correct
scheme like most virtual-list libraries use. That's a meaningfully bigger
undertaking for a mode (`wordWrap`) most users leave off for large files
specifically because it preserves column alignment.

**Decision:** virtualize only when `wordWrap` is off. When the user has
word-wrap on, the shell mounts `PagedCodeView` — today's
accumulate-everything behavior unchanged (same `allContentLines`
growing-array path, same unbounded DOM). Toggling word-wrap swaps the
mounted component (keyed on the mode), which naturally reloads around the
currently-visible line range — no in-place state migration between the two
models. Variable-height virtualization is explicitly out of scope (see
below) — revisit only if word-wrap-on-huge-files turns out to matter in
practice.

### All geometry in raw-line space; placeholder rows fill server gaps

The server (`get_file_lines_paged`) **silently skips raw lines whose stored
chunk lookup misses** — e.g. blank lines between TOML table entries in a
Cargo.lock that never got their own content chunk. This is the documented
reason `nextForwardOffset` (`pagination.ts`) advances by the raw range
consumed rather than the response length. A window request covering raw
range `[start, start+N)` can therefore return fewer than N lines, with gaps
visible in `line_offsets`.

If spacer heights were computed from *rendered row counts* while `totalLines`
counts *raw lines*, every gap would introduce vertical drift: line N would no
longer sit at `N * rowHeight`, `pixelOffsetToLine` would return wrong lines,
and jumps would land off-target — with the error accumulating over the file.

**Decision:** all windowing math operates in raw-line space, and the rendered
window fills gaps in `line_offsets` with **placeholder rows** (empty content,
real line number) so that rendered rows always equal the raw range covered.
With that invariant, `spacerBefore = windowStart * rowHeight` and
`spacerAfter = (totalLines - windowEnd) * rowHeight` are exact, where
`windowEnd` is the raw end of the fetched range (per `nextForwardOffset`
semantics), never a rendered-row count.

### Row height: measured once, spacers installed immediately

CSS controls `line-height`/`font-size`/cell padding for `.code-row`, and
those are one active-theme concern away from drifting out of sync with a
hardcoded pixel value. Instead: render the *first* window exactly like today
(no spacers), measure `.code-row`'s actual `getBoundingClientRect().height`
once content exists, then **immediately install the spacer rows** — before
any scrolling happens — so the scrollbar reflects the whole file from the
moment the file opens. This sidesteps the chicken-and-egg problem of needing
a rendered row to know the row height needed to size the spacers, without
leaving the viewer in a half-windowed state until the first scroll event.

Row height can drift after the initial measurement (browser zoom, font-size
changes, fallback fonts for emoji/CJK content rendering taller than the
mono font). `FileViewer` already runs a `ResizeObserver` on the scroll
container for the overflow check; the windowed path re-measures a rendered
row on the same observer callback and recomputes spacer heights if the
value changed. Odd individual rows (an emoji line) being a few pixels taller
is accepted slop — the spacers position the window, and small local error
doesn't accumulate because spacer heights are always recomputed from
`windowStart`, not summed incrementally.

### Spacer rows, not `transform`/absolute positioning

`CodeViewer` renders a `<table>` (real semantics: `id="line-N"` rows,
`scrollIntoView`, native find-in-page within the window). The standard table
virtualization technique — a spacer `<tr style="height: {n}px">` before and
after the rendered window — keeps that markup intact rather than switching
to a div/flex layout with manual positioning. Two spacer rows: one sized to
`windowStart * rowHeight` (raw lines above the window), one sized to
`(totalLines - windowEnd) * rowHeight` (raw lines below).

### Scroll geometry is invariant across window shifts

With fixed row height and exact spacers, the container's total scroll height
never changes when the window moves — swapping the window while resizing both
spacers leaves every line at the same absolute pixel, so `scrollTop` needs
**no compensation on window shifts at all**. (This is a simplification over
the legacy `loadBackward()` capture-and-restore pattern, which exists because
prepending rows *without* spacers really does change the geometry.)

Compensation is needed at exactly two moments:

1. **The first-render → windowed transition**, when the spacers first appear
   and `scrollHeight` jumps to the full-file value.
2. **After a row-height re-measure** (zoom/font change), when all spacer
   heights are recomputed.

Both are handled the same way: recompute `scrollTop` from the line that was
at the top of the viewport before the change.

One wrinkle: `.code-container` holds content *above* the table — the
meta-panel, the encrypted-PDF notice, the markdown-too-large notice. All
pixel↔line conversions must offset by the table's position within the scroll
container (`table.offsetTop`), measured alongside the row height. The pure
math in `virtualWindow.ts` takes this as an explicit `contentOffsetPx`
parameter rather than assuming the table starts at scrollTop 0. The paged
"Load earlier lines" / "Loading…" sentinel elements are removed from the
windowed path entirely (the spacers replace them); they remain for the
word-wrap fallback.

### Jump-to-line goes through one path

Today the only jump is the initial open with a line selection (from a search
result or a `#L` deep link parsed once at startup in `appState.ts`):
`loadFile()` computes an anchor page containing the target line, loads it,
then `scrollToLine()` does a smooth `scrollIntoView`. There is no in-session
go-to-line UI and no live `hashchange` handling — nothing else to unify.

Under windowing, the initial-selection path becomes `jumpToLine(targetLine)`:
compute a window centered on the target via `computeWindowOffset()`, fetch
it, set `scrollTop` directly from the target's raw-line offset (plus
`contentOffsetPx`). No "is it already loaded" branching, no
`scrollIntoView` animation across mounted rows. Any future go-to-line
feature (Ctrl+G dialog, live hash editing) gets this entry point for free,
but building that UI is not part of this plan.

### Debounced, cancellable window fetches

Scrolling fires many events; fetching a new window on every tick would
flood the server during a fast scrollbar drag. Window-shift fetches are
debounced (~100ms after scroll settles) and a new fetch supersedes/cancels
any still in flight for a stale window. For small shifts (viewport still
overlapping the old window) the old rows stay visible, dimmed with the same
stale treatment used elsewhere in the app (`ResultList`'s
`.result-list.searching` opacity/blur pattern), until the new window swaps
in. For a long-distance scrollbar drag the viewport lands on spacer — blank
space until the debounced fetch fills it. That blank flash is standard
virtual-scrolling behavior and accepted here; the dimmed treatment only
applies when old content is actually under the viewport.

### `hasOverflow` reflects only the rendered window

The word-wrap toggle's visibility is gated on `hasOverflow`
(`scrollWidth > clientWidth`). Under windowing, `scrollWidth` reflects only
the currently-rendered lines, so the toggle could appear/disappear as the
widest line scrolls in and out of the window. To avoid flicker, the windowed
path latches `hasOverflow`: once any window has overflowed, it stays true
for the lifetime of the loaded file.

---

## Implementation

### Phase 1 — Pure windowing math, no behavior change

New `web/src/lib/virtualWindow.ts`, fully unit-testable, no Svelte/DOM
dependency. All line arguments are raw line numbers:

- `computeWindowOffset(centerLine, windowSize, totalLines): number` —
  clamps a window start so `[start, start+windowSize)` stays inside
  `[0, totalLines)` while centering on `centerLine`.
- `shouldRefetchWindow({ windowStart, windowEnd, viewportStartLine,
  viewportEndLine, overscan }): boolean` — whether the visible range (plus
  overscan) still fits inside the current window. Takes `windowEnd` (raw),
  not a rendered-row count.
- `lineToPixelOffset(line, rowHeight, contentOffsetPx): number` /
  `pixelOffsetToLine(px, rowHeight, contentOffsetPx): number` — spacer
  height and scroll-position math, accounting for content above the table.
- `fillLineGaps(lines, lineOffsets, rangeStart, rangeEnd): { lines,
  lineOffsets }` — inserts empty placeholder rows for raw lines the server
  skipped, so rendered rows always equal `rangeEnd - rangeStart`.

No `FileViewer`/`CodeViewer` changes yet. This phase lands the algorithm and
its test coverage independently of the riskier rendering rewrite.

### Phase 2 — Extract the legacy path into `PagedCodeView` (pure refactor)

No behavior change; the app works identically after this phase.

- New `fetchLineRange()` helper wrapping `getFile` + the `adjustOffsets`
  normalization (currently repeated at three call sites in `FileViewer`).
- Move the scroll container, paged state (`allContentLines`,
  `allLineOffsets`, `forwardOffset`, `backwardOffset`, `noMore*`,
  `loading*`), `loadForward`/`loadBackward`, `appendCodeState`/
  `prependCodeState`, load sentinels, scroll-prepend compensation, and the
  overflow `ResizeObserver` into `PagedCodeView.svelte`.
- The shell passes the initial page data, a `header` snippet (meta-panel /
  notices), `selection`, `tabWidth`, `wordWrap`, and receives
  `onLineSelect` / `onOverflowChange`.
- Small (non-paged) files also render through `PagedCodeView` with
  everything loaded and both `noMore*` flags set.

### Phase 3 — `WindowedCodeView` (windowed rendering + jump-to-line)

- `CodeViewer.svelte`: accept optional `spacerBeforePx`/`spacerAfterPx`
  props, rendered as leading/trailing `<tr>` spacers around the existing
  keyed `{#each codeLines as line, i (lineOffsets[i] ?? i)}` loop (unchanged
  otherwise — the line-number keying from plan 055 still applies;
  placeholder rows carry real line numbers so keys stay unique).
- New `WindowedCodeView.svelte`: window state (`windowLines`,
  `windowLineOffsets`, `windowStart`, `windowEnd`), measured `rowHeightPx` /
  `contentOffsetPx`. `highlightedCode`/`codeLines` derive from `windowLines`
  only; each window load runs `fillLineGaps()` then calls
  `highlightFile(windowLines, path)` once (bounded cost — nothing to
  concatenate onto, so the append/prepend incremental highlighting from
  plan 055 isn't needed on this path).
- After the first window renders: measure row height and table offset,
  install spacers immediately, apply the one-time scroll compensation for
  the transition.
- A single debounced `handleScroll()` computes the visible line range from
  `scrollTop`, checks `shouldRefetchWindow()`, and on a miss computes a new
  `windowStart` via `computeWindowOffset()`, fetches it, and swaps
  `windowLines` (replace, not append). No scroll compensation on the swap —
  geometry is invariant. No load sentinels on this path.
- `jumpToLine(targetLine)`: compute a centered window, fetch, set
  `scrollTop` from the target's raw-line offset. The
  initial-open-with-selection path routes through it, replacing the
  anchor-page arithmetic and `scrollIntoView` used by the legacy path.
- The shell mounts `WindowedCodeView` when `pagedMode && !wordWrap`,
  `PagedCodeView` otherwise, keyed on the mode so toggling word-wrap
  remounts cleanly.

### Phase 4 — Shell cleanup and verification

- Confirm `FileViewer.svelte` retains no scroll/pagination state or
  branches — only the initial fetch, viewer selection, and everything
  non-text (toolbar, media, share, live updates).
- `appendCodeState`/`prependCodeState` and the `nextForwardOffset` call
  sites (plan 055, `pagination.ts`) now live only in `PagedCodeView` — do
  not delete `nextForwardOffset`, just confirm both call sites are
  accounted for.

---

## Files Changed

| File | Change |
|------|--------|
| `web/src/lib/virtualWindow.ts` (new) | Pure windowing math: centering, refetch decision, pixel/line conversion, gap filling |
| `web/src/lib/virtualWindow.test.ts` (new) | Unit tests for the above |
| `web/src/lib/WindowedCodeView.svelte` (new) | Virtualized path: window state, spacer math, measurement, debounced refetch, `jumpToLine()`, latched `hasOverflow` reporting |
| `web/src/lib/PagedCodeView.svelte` (new) | Legacy accumulate path extracted intact from `FileViewer`: load forward/backward, sentinels, prepend compensation |
| `web/src/lib/FileViewer.svelte` | Becomes the shell: initial fetch + metadata + toolbar + media/formatted viewers; mounts exactly one text-view component; no scroll/pagination logic |
| `web/src/lib/CodeViewer.svelte` | Optional spacer `<tr>` props around the existing keyed row loop; shared by both paths |
| `web/src/lib/pagination.ts` | `fetchLineRange()` helper (shared); `nextForwardOffset` stays, now only called from `PagedCodeView` |
| `web/src/routes/+page.svelte` | No change expected — it only writes the `#L` hash on selection change; the initial hash is parsed once in `appState.ts`, which also doesn't change |

---

## Testing

- Unit tests (Vitest) for every `virtualWindow.ts` function: window at file
  start/end, `totalLines` smaller than `windowSize`, centering near either
  boundary, overscan threshold edge cases, `contentOffsetPx` round-tripping,
  and `fillLineGaps` with gaps at the start/middle/end of a range and with
  no gaps.
- Existing `pagination.test.ts` (`nextForwardOffset`) stays green
  unmodified — it covers the legacy path, which is intentionally unchanged.
  Unit tests for the new `fetchLineRange()` helper (offset adjustment,
  fallback numbering when `line_offsets` is absent).
- After Phase 2 (extraction), a full manual/Playwright pass of today's
  behavior — paged scroll both directions, small files, word-wrap, meta
  panels — before any windowing code lands, since that phase claims "no
  behavior change".
- Live Playwright verification against a real large paged file — including
  one known to have server-skipped lines (a Cargo.lock-style file with blank
  lines between entries) to exercise the gap-filling path:
  - Scroll down and up through a multi-thousand-line file; confirm
    `document.querySelectorAll('.code-row').length` stays roughly constant
    (bounded) rather than growing with scroll depth.
  - Confirm the scrollbar thumb position/size reflects position in the whole
    file immediately on open, before any scrolling has happened.
  - On the gappy file, confirm line numbers stay pixel-aligned deep into the
    file: jump/scroll to a late line and check the row labelled N is actually
    the one under the viewport position for N (no cumulative drift).
  - Open a search result / `#L` deep link targeting a far line; confirm it
    lands on the correct line with no visible content flash/blank gap.
  - Toggle word-wrap on a large file mid-session: falls back to the legacy
    path without error (DOM growth resumes as today — expected, not a
    regression for that mode).
  - Rapid scrollbar-drag scrolling doesn't flood the server with requests
    (debounce holds) and doesn't leave the window visibly stuck on stale
    content past the debounce window.

---

## Breaking Changes

None. Client-side only; no server API, schema, or config changes.

---

## Out of Scope

- **Variable-height virtualization for word-wrap mode.** Falls back to the
  existing unbounded-DOM behavior instead of measuring/correcting per-row
  heights. Revisit only if this turns out to matter in practice for large
  wrapped files.
- **In-session go-to-line UI (Ctrl+G dialog, live `hashchange` handling).**
  Neither exists today; `jumpToLine()` is the natural entry point if either
  is built later, but no new UI is part of this plan.
- **Ctrl+F / browser native find-in-page across the whole file.** With
  windowing, native find-in-page only searches the currently-rendered
  window's DOM text, not lines that have been scrolled past and discarded.
  This matches how virtualized editors generally behave (VS Code, Monaco)
  but is a real, user-visible change from today's behavior, where every
  line ever scrolled through remains real, findable DOM text. Worth
  surfacing to users if it comes up (e.g. a "search may not find distant
  matches" hint), but no in-app full-text-within-viewer search is being
  built here — free-text search across the file already exists via the
  app's own search feature.
- **Copy/select-all across virtualized boundaries.** Selecting text that
  spans outside the currently-rendered window only captures what's actually
  in the DOM, same caveat as find-in-page.
- **Server-side changes.** The existing `/api/v1/file?offset=&limit=` API
  is sufficient as-is.
