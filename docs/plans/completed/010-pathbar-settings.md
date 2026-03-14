# PathBar + User Settings

## Overview

The right-pane currently has two redundant rows when viewing a file:
1. `<Breadcrumb>` — clickable path segments for directory navigation (already handled by the left tree)
2. `FileViewer.viewer-header` — back button, source badge, raw path, line count

Replace both with a single `PathBar` showing the full resource URL (base URL + path) as a clickable
link that opens a new tab. Add a settings gear to the topbar to let users override the base URL
per source (since network paths differ by consumer).

---

## 1 — API: Extend `list_sources` to return `SourceInfo`

**Current**: `GET /api/v1/sources` → `string[]`
**New**: `GET /api/v1/sources` → `SourceInfo[]`

```rust
// crates/common/src/api.rs
pub struct SourceInfo {
    pub name: String,
    pub base_url: Option<String>,
}
```

`list_sources` in `crates/server/src/routes.rs`:
- For each `.db` file found, call `db::open()` + `db::get_base_url()` (already exists at db.rs:214)
- Return `Vec<SourceInfo>` sorted by name
- `get_base_url` is a single `SELECT value FROM meta WHERE key = 'base_url'` — negligible cost

**Files:** `crates/common/src/api.rs`, `crates/server/src/routes.rs`

---

## 2 — Profile: Add source base URL overrides

```ts
// web/src/lib/profile.ts
interface UserProfile {
  sidebarWidth?: number;
  sourceBaseUrls?: Record<string, string>;  // add this
}
```

No other changes needed — the store already auto-saves every mutation.

**Files:** `web/src/lib/profile.ts`

---

## 3 — New `PathBar.svelte` component

Replaces both `<Breadcrumb>` and the `FileViewer.viewer-header`.

**Props:**
```ts
export let source: string;
export let path: string;
export let archivePath: string | null = null;
export let baseUrl: string | null = null;  // effective resolved URL
```
**Events:** `back`

**Rendering:**
- Left side: `← results` button (dispatches `back`)
- Source badge (small pill)
- Path display:
  - If `baseUrl` set: construct `href = baseUrl.trimEnd('/') + '/' + path.trimStart('/')`, render as `<a href={href} target="_blank" rel="noopener noreferrer">` showing the full URL
  - If no `baseUrl`: plain `<span>` with monospace path (no click)
- For archive entries: append `::archivePath` to displayed text (but not to the URL)
- Overflow: text-overflow ellipsis, full URL in `title` attribute for hover tooltip

**Files:** `web/src/lib/PathBar.svelte` (new)

---

## 4 — New `Settings.svelte` component

Modal overlay (same pattern as `CommandPalette`).

**Props:** `open: boolean`, `sources: SourceInfo[]`
**Events:** `close`

**Layout:**
```
┌─────────────────────────────────┐
│ Settings                      ✕ │
├─────────────────────────────────┤
│ Base URL overrides              │
│ ┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄ │
│ source-name                     │
│  Server: file:///mnt/share      │
│  Override: [_________________]  │
│  [Clear]                        │
│ ...                             │
└─────────────────────────────────┘
```

- For each source: show name, server-configured base_url (read-only hint in muted text),
  and an `<input>` bound to `$profile.sourceBaseUrls[source.name]`
- `<input>` on:change writes directly to the profile store
- Clear button removes the key from `sourceBaseUrls`
- Dismiss: ESC, click backdrop, or ✕ button

**Files:** `web/src/lib/Settings.svelte` (new)

---

## 5 — Wire up `+page.svelte`

**State changes:**
- `sources: string[]` → `sources: SourceInfo[]`
- Add `showSettings = false`
- Derived: `serverBaseUrls: Record<string, string>` from `sources` array
- `function effectiveBaseUrl(src: string): string | null` — `$profile.sourceBaseUrls?.[src] ?? serverBaseUrls[src] ?? null`

**Topbar:** Add gear button (⚙) at far right in both topbar variants:
```svelte
<button class="gear-btn" on:click={() => (showSettings = !showSettings)}>⚙</button>
```

**Viewer-wrap:** Replace `<Breadcrumb>`:
```svelte
<PathBar
  source={fileSource}
  path={panelMode === 'dir' ? currentDirPrefix : filePath}
  archivePath={fileArchivePath}
  baseUrl={effectiveBaseUrl(fileSource)}
  on:back={backToResults}
/>
```

**Chip components** that previously consumed `sources: string[]` — pass `sources.map(s => s.name)`.

**Settings modal** rendered outside `.page` div (like `<CommandPalette>`):
```svelte
<Settings open={showSettings} {sources} on:close={() => (showSettings = false)} />
```

**Files:** `web/src/routes/+page.svelte`, `web/src/lib/FileViewer.svelte`,
`web/src/lib/api.ts` (update `listSources` type + add `SourceInfo` interface)

---

## 6 — Remove `Breadcrumb.svelte`

After replacing all usages, delete `web/src/lib/Breadcrumb.svelte`.

**Note:** `panelMode === 'dir'` and `DirListing` are kept — PathBar shows the directory
path above the listing.

---

## Files Changed

| File | Change |
|---|---|
| `crates/common/src/api.rs` | Add `SourceInfo { name, base_url }` struct |
| `crates/server/src/routes.rs` | Update `list_sources` to return `Vec<SourceInfo>` |
| `web/src/lib/api.ts` | Add `SourceInfo` interface; update `listSources()` return type |
| `web/src/lib/profile.ts` | Add `sourceBaseUrls` field to `UserProfile` |
| `web/src/lib/PathBar.svelte` | New: path display with clickable URL + back button |
| `web/src/lib/Settings.svelte` | New: settings modal with per-source base URL overrides |
| `web/src/routes/+page.svelte` | Wire settings gear, pass SourceInfo, use PathBar |
| `web/src/lib/FileViewer.svelte` | Remove `.viewer-header` (PathBar replaces it) |
| `web/src/lib/Breadcrumb.svelte` | Delete |

---

## Breaking Changes

`GET /api/v1/sources` response changes from `string[]` to `SourceInfo[]`. Only the web
client calls this endpoint (via the SvelteKit proxy).

---

## Verification

1. `npm run check` in `web/` — 0 errors
2. Start server + dev client; confirm:
   - Sources list loads correctly
   - File view shows single path bar (no breadcrumb, no FileViewer header)
   - With `base_url` configured: clicking path opens new tab at correct URL
   - Without `base_url`: path shown as plain text
   - Settings gear opens modal; base URL override immediately changes PathBar link
   - Clearing override reverts to server base_url
   - Back button in PathBar returns to results
   - Browser Back/Forward still work
3. `cargo check` — 0 errors
