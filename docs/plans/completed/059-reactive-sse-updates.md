# Reactive UI Updates via SSE

## Overview

The web UI is currently static — it only reflects the index state at the time of a user action. This plan makes three UI surfaces react in real-time to indexing changes by connecting to the existing `GET /api/v1/recent/stream` SSE endpoint:

1. **Tree view** — refresh expanded directories when files are added, removed, or renamed beneath them
2. **Search results** — show a "results may have changed" banner on add/modify; mark deleted-file results inline
3. **File viewer** — show a reload banner on modify; show a strikethrough header + DELETE badge for deleted files; show a rename notice for renamed files

**No server changes required.** The existing `/api/v1/recent/stream` endpoint already broadcasts `RecentFile` events (`added`, `modified`, `deleted`, `renamed`) for all indexed files.

---

## Design Decisions

### Re-use `GET /api/v1/recent/stream` as-is

`RecentFile` carries `{ source, path, action, new_path?, indexed_at }` — everything needed to decide what to refresh. We ignore the initial history snapshot (events with `indexed_at < connection_start_time`) to avoid spurious refreshes on reconnect.

### `fetch()` streaming instead of `EventSource`

`EventSource` doesn't support custom request headers. The rest of the API uses `Authorization: Bearer <token>`. Using `fetch()` with `ReadableStream` keeps auth consistent and avoids needing cookie-based auth just for SSE. The same SSE text format (`data: ...\n\n`) is parsed manually — it's simple enough to do inline.

### Reconnect with exponential back-off

Network blips and server restarts should reconnect automatically. Cap back-off at 30 s.

### One connection for the whole app

A single SSE connection shared across all components, managed in `liveUpdates.ts` as a Svelte store. Components subscribe to filtered events — no per-component connections.

### Tree: immediate re-fetch on invalidation

When a `TreeRow` with `expanded = true` has a file change in its direct children, re-fetch the directory listing silently (no loading spinner). The result replaces the existing children array; the expansion state is preserved.

Condition: an event invalidates a tree row whose `source` matches AND whose `prefix` exactly equals the **parent directory** of the changed path. "Parent directory" of `"docs/api/readme.txt"` is `"docs/api/"`. Only the immediately containing directory needs to re-fetch (not all ancestors) — the entry count of parent directories doesn't change just because a child's child changes.

Exception: renames where old and new paths have different parent directories invalidate two rows.

### Search results: banner + inline deletion marks, no auto-re-search

Auto-re-searching would be disorienting (results jump around while the user reads). Instead:
- When any `added`/`modified`/`renamed` event fires for the currently active source filter, show a dismissible banner: **"Index updated — click to refresh results"**. Clicking calls `doSearch()`.
- When a `deleted` event fires and `path` matches any result in the current `results` array, those result cards get a visual "deleted" state (greyed out, strikethrough on the file name) without a network request.

### File viewer: banner + overlay for deleted

- `modified`: show a sticky yellow banner "File updated — click to reload". Clicking re-calls `getFile()` and re-renders.
- `deleted`: show a red "DELETED" badge beside the filename in the header; draw strikethrough on the filename. File content stays visible (user can still read it). No auto-close.
- `renamed`: show an info banner "Renamed to `new_path`" with a link to navigate to the new path.

---

## Implementation

### Step 1 — `web/src/lib/liveUpdates.ts` (new)

Central SSE connection manager. Exports:

```typescript
export interface LiveEvent {
  source: string;
  path: string;
  action: 'added' | 'modified' | 'deleted' | 'renamed';
  new_path?: string;
  indexed_at: number;
}

// Readable store: each subscriber gets every live event as it arrives.
// Emits null when disconnected (can be used to show a "reconnecting" indicator).
export const liveEvent: Readable<LiveEvent | null>;

// Start the SSE connection. Returns a stop() function.
// Call once from the root component's onMount.
export function startLiveUpdates(getToken: () => string): () => void;
```

Internally:
1. Record `connectionStartTime = Date.now() / 1000` before connecting.
2. `fetch('/api/v1/recent/stream', { headers: { Authorization: 'Bearer ...' } })`.
3. Read the response body with `getReader()`. Parse SSE `data:` lines, skip `event:` and `:` heartbeat lines.
4. Deserialize each `data:` line as `RecentFile` JSON.
5. Drop events where `indexed_at < connectionStartTime` (initial history snapshot).
6. Push live events to the store via a writable internal store, exposed as `Readable`.
7. On fetch error or stream end: exponential back-off (1 s, 2 s, 4 s … 30 s) then reconnect.

### Step 2 — `web/src/routes/+page.svelte`

In `onMount`: call `startLiveUpdates(() => token)` and store the returned `stop` function. Call `stop()` in `onDestroy`.

Pass nothing down — components import `liveEvent` directly from `liveUpdates.ts`.

### Step 3 — `web/src/lib/api.ts`

No changes needed — the SSE connection is managed in `liveUpdates.ts` directly with `fetch()`.

### Step 4 — `web/src/lib/TreeRow.svelte`

```svelte
<script>
  import { liveEvent } from './liveUpdates';

  // Existing props: source, entry, activePath, onFileSelect, ...

  // Compute this row's prefix (the directory it lists children of)
  $: myPrefix = entry.entry_type === 'dir' ? entry.path + '/' : null;

  // React to live events
  $: if ($liveEvent && myPrefix && expanded && $liveEvent.source === source) {
    const ev = $liveEvent;
    const parentDir = dirOf(ev.path);
    const newParentDir = ev.new_path ? dirOf(ev.new_path) : null;
    if (parentDir === myPrefix || newParentDir === myPrefix) {
      refreshChildren(); // silent re-fetch, preserves expanded state
    }
  }

  async function refreshChildren() {
    const fresh = await listDir(source, myPrefix);
    children = fresh.entries;
  }

  function dirOf(path: string): string {
    const i = path.lastIndexOf('/');
    return i >= 0 ? path.slice(0, i + 1) : '';
  }
</script>
```

`refreshChildren()` re-fetches without resetting `expanded` or clearing `children` first, so the tree doesn't flicker.

Root-level entries (prefix `""`) are in `DirectoryTree.svelte` — it needs the same reactive block, re-fetching from `listDir(source, '')` when a top-level file changes.

### Step 5 — `web/src/lib/DirectoryTree.svelte`

Add the same `$: if ($liveEvent && ...)` reactive block that watches for `source` matches and re-fetches `roots` when `dirOf(ev.path) === ''` (i.e. a file was added/removed at the root level).

### Step 6 — `web/src/lib/DirListing.svelte`

`DirListing` shows a flat table of a directory's contents (separate from the tree sidebar). Apply the same invalidation logic: when a live event's parent directory matches the current `prefix` for the same `source`, call the existing `load(source, prefix)` function.

### Step 7 — `+page.svelte` — search result staleness

Add a reactive `resultsStale: boolean` flag:

```typescript
$: if ($liveEvent) {
  const ev = $liveEvent;
  const sourceMatches = selectedSources.length === 0 || selectedSources.includes(ev.source);
  if (sourceMatches && query) {
    if (ev.action === 'deleted') {
      // Mark any matching results as deleted (no re-search)
      deletedPaths = new Set([...deletedPaths, `${ev.source}:${ev.path}`]);
    } else {
      // add/modified/renamed — offer banner
      resultsStale = true;
    }
  }
}
```

Render the banner:
```svelte
{#if resultsStale}
  <div class="stale-banner">
    Index updated — <button on:click={() => { doSearch(); resultsStale = false; }}>refresh results</button>
    <button on:click={() => resultsStale = false}>✕</button>
  </div>
{/if}
```

Pass `deletedPaths: Set<string>` as a prop to `ResultList.svelte`. `ResultList` uses it to add a `deleted` CSS class to cards matching `"${source}:${path}"`.

### Step 8 — `web/src/lib/ResultList.svelte`

Accept `deletedPaths: Set<string>` prop. In the result group rendering:

```svelte
{#each groups as group (group.key)}
  {@const isDeleted = deletedPaths.has(`${group.hits[0].source}:${group.hits[0].path}`)}
  <div class="result-group" class:deleted={isDeleted}>
    <!-- existing content -->
  </div>
{/each}
```

CSS:
```css
.result-group.deleted {
  opacity: 0.5;
}
.result-group.deleted .result-filename {
  text-decoration: line-through;
}
```

### Step 9 — `web/src/lib/FileViewer.svelte`

Add reactive state and subscribe to live events:

```typescript
import { liveEvent } from './liveUpdates';
// props: source, path (archive_path for members)

type FileState = 'normal' | 'modified' | 'deleted' | 'renamed';
let fileState: FileState = 'normal';
let renamedTo: string | null = null;

$: if ($liveEvent && $liveEvent.source === source && $liveEvent.path === path) {
  const ev = $liveEvent;
  if (ev.action === 'deleted') {
    fileState = 'deleted';
  } else if (ev.action === 'modified') {
    fileState = 'modified';
  } else if (ev.action === 'renamed') {
    fileState = 'renamed';
    renamedTo = ev.new_path ?? null;
  }
}
```

In the template, above the file content:
```svelte
{#if fileState === 'deleted'}
  <div class="file-status deleted">
    ⚠ This file has been deleted from the index.
  </div>
{:else if fileState === 'modified'}
  <div class="file-status modified">
    File updated — <button on:click={reload}>reload</button>
  </div>
{:else if fileState === 'renamed' && renamedTo}
  <div class="file-status renamed">
    Renamed to <button on:click={() => dispatch('navigate', renamedTo)}>{renamedTo}</button>
  </div>
{/if}
```

The file header should reflect `fileState === 'deleted'` with a strikethrough and a red `DELETED` pill:
```svelte
<h2 class:line-through={fileState === 'deleted'}>
  {path}
  {#if fileState === 'deleted'}<span class="badge deleted-badge">DELETED</span>{/if}
</h2>
```

`reload` calls the existing `loadFile()` function and resets `fileState = 'normal'`.

For archive members (`archive_path` is set), live events only fire for outer files. The check should match the outer archive path, and on match, show a generic "outer archive updated — reload to see changes" banner.

---

## Files Changed

| File | Change |
|------|--------|
| `web/src/lib/liveUpdates.ts` | **New** — SSE connection manager, `liveEvent` store, `startLiveUpdates()` |
| `web/src/routes/+page.svelte` | Start/stop SSE; add `resultsStale` + `deletedPaths` state; stale banner |
| `web/src/lib/ResultList.svelte` | Accept `deletedPaths` prop; apply deleted CSS class to matching groups |
| `web/src/lib/TreeRow.svelte` | Subscribe to `liveEvent`; silent refresh when `myPrefix` is invalidated |
| `web/src/lib/DirectoryTree.svelte` | Subscribe to `liveEvent`; silent refresh when root-level files change |
| `web/src/lib/DirListing.svelte` | Subscribe to `liveEvent`; call `load()` when directory is invalidated |
| `web/src/lib/FileViewer.svelte` | Subscribe to `liveEvent`; show deleted/modified/renamed banners |

No Rust/server changes needed.

---

## Edge Cases

**Active-view path vs. outer archive path:** Live events only carry outer file paths (no `::` separators, by server design). A viewer showing an archive member (`outer.zip::member.txt`) should watch for events matching `source + "outer.zip"` and show a generic "archive updated" banner.

**Deleted file still in tree:** After a `deleted` event, `refreshChildren()` re-fetches the parent directory. The deleted file won't appear in the fresh listing, so it disappears from the tree naturally. No special handling needed.

**Rename across directories:** Invalidates two tree rows — the directory of `path` and the directory of `new_path`. Both get refreshed independently.

**Search result dedup key:** The `deletedPaths` set uses `"${source}:${path}"`. The search result dedup key includes `archive_path` and `line_number`, but deletion only affects the outer file — all result cards sharing that `source:path` prefix should be marked.

**No active search:** If `query` is empty, ignore all live events for the search surface (there are no results to mark stale or deleted).

**Multiple tabs:** Each tab manages its own SSE connection independently. No cross-tab coordination needed.

**Server restart / long disconnect:** On reconnect, `connectionStartTime` is reset. A brief period of events that occurred during the disconnect will be missed. This is acceptable — users can manually refresh if they notice stale state. A future improvement could send the last `indexed_at` seen as a query param to recover missed events.

---

## Testing

- Start server, open browser, expand a tree directory
- Use `find-scan` to add a file under that directory → tree updates without page reload
- Add file matching current search term → stale banner appears
- Open a file, re-index it → modified banner appears, reload works
- Delete a file from the filesystem and re-scan → deleted badge appears in file viewer; disappears from tree; search result is greyed out
- Rename a file → renamed banner in file viewer; both affected tree directories refresh
- Kill the server → UI should reconnect silently when server restarts

## Breaking Changes

None. Additive only.
