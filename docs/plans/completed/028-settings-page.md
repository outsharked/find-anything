# Plan: Settings Page with Left Nav

## Context

The toolbar currently has two separate icon buttons — a grid/dashboard icon and a gear/settings icon — that each open modal overlays. The user wants to consolidate these into a single settings icon that navigates to a dedicated `/settings` route with a left nav sidebar containing "Preferences" and "Stats" sections.

---

## What We're Building

- Remove the dashboard grid icon from the toolbar in both `SearchView.svelte` and `FileView.svelte`
- Make the gear/settings icon slightly larger
- Clicking the gear navigates to a new `/settings` SvelteKit route (via `goto`)
- `/settings` is a full-page layout with:
  - A header bar with the app logo (links back to `/`) and a back button
  - A left sidebar nav with two items: **Preferences** and **Stats**
  - A main content area that renders the active section
  - Active section tracked via `?section=preferences` (default) or `?section=stats` URL param
- Settings and Dashboard content is extracted from their modal wrappers into plain content components

---

## Files Changed

| File | Change |
|------|--------|
| `web/src/lib/SearchView.svelte` | Remove dashboard button; enlarge gear icon; use `goto('/settings')` |
| `web/src/lib/FileView.svelte` | Same as SearchView |
| `web/src/routes/+page.svelte` | Remove `showSettings`, `showDashboard` state, handlers, and component mounts |
| `web/src/lib/Preferences.svelte` (new) | Settings content extracted from Settings.svelte (no modal wrapper) |
| `web/src/lib/StatsPanel.svelte` (new) | Dashboard content extracted from Dashboard.svelte (no modal wrapper) |
| `web/src/routes/settings/+page.svelte` (new) | Settings page: header + left nav + content area |
| `web/src/lib/Settings.svelte` | Delete (replaced by Preferences.svelte + settings route) |
| `web/src/lib/Dashboard.svelte` | Delete (replaced by StatsPanel.svelte + settings route) |

---

## Implementation

### 1. SearchView.svelte & FileView.svelte

Remove the dashboard button block entirely. Change the gear button:
- Import `goto` from `$app/navigation`
- Replace `dispatch('gear')` with `goto('/settings')`
- Remove `gear` and `dashboard` from the event dispatcher type
- Increase gear icon font-size from 16px to 20px (it's a `⚙` unicode char)

### 2. +page.svelte

- Remove `let showSettings = false` and `let showDashboard = false`
- Remove `on:gear` and `on:dashboard` handlers on `<SearchView>` and `<FileView>`
- Remove `import Settings` and `import Dashboard`
- Remove `<Settings open={showSettings} ...>` and `<Dashboard open={showDashboard} ...>` from the template

### 3. Preferences.svelte (new)

Extract the body content of `Settings.svelte` — the per-source base URL override form — into a standalone component with no modal/backdrop wrapper. Same logic and `profile` store usage, just rendered as a plain content panel.

### 4. StatsPanel.svelte (new)

Extract the body content of `Dashboard.svelte` — async stats fetch, source selector, summary cards, by-kind table, SVG chart — into a standalone component with no modal/backdrop wrapper.

### 5. web/src/routes/settings/+page.svelte (new)

```
┌──────────────────────────────────────────────────────────┐
│ ← find-anything                                          │  ← header
├────────────┬─────────────────────────────────────────────┤
│            │                                             │
│ Preferences│   [active section content]                  │
│ Stats      │                                             │
│            │                                             │
└────────────┴─────────────────────────────────────────────┘
```

- `export const params = {}` to satisfy SvelteKit prop passing (see `_params` pattern in +page.svelte)
- Read active section from `$page.url.searchParams.get('section') ?? 'preferences'`
- Left nav links update URL with `?section=preferences` / `?section=stats` using `replaceState` (no history push, just updates param in place)
- Header: logo text "find-anything" links to `/`; a `←` back button calls `history.back()` (falls back to `goto('/')` if no history)
- Conditionally renders `<Preferences>` or `<StatsPanel>` based on active section
- Styled to match existing design tokens (`--bg`, `--bg-secondary`, `--border`, `--text`, `--accent`)

---

## Verification

1. `pnpm run check` — zero errors/warnings
2. Click gear icon → navigates to `/settings?section=preferences`
3. Preferences content renders (source URL overrides work, saves to localStorage)
4. Click "Stats" in left nav → URL updates to `?section=stats`, stats content loads
5. Click "find-anything" logo → returns to `/`
6. Click `←` back button → returns to previous page
7. No dashboard or settings modals appear anywhere; no stale `showDashboard`/`showSettings` references
