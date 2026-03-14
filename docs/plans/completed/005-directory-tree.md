# Directory Tree Explorer

## Overview

Add a file-explorer sidebar to the web UI that displays the indexed files as a
collapsible directory tree.  When a file is open in FileViewer, the tree
highlights the current file and lets the user navigate to siblings or other
files without going back to search.

The feature requires a new backend API endpoint (`GET /api/v1/tree`) plus
implementation of the existing `DirectoryTree.svelte` stub.

## Design Decisions

### Lazy, prefix-based loading

Instead of returning the entire file tree in one response (which could be
thousands of entries), the endpoint returns only the immediate children of a
given directory prefix.  The UI expands directories lazily by fetching their
children on demand.

### Range-scan SQL

The `files` table already stores full relative paths.  Given a prefix `foo/bar/`,
we can retrieve everything under it with:

```sql
SELECT path, size, kind, mtime FROM files
WHERE path >= ?1 AND path < ?2
```

where `?2` is produced by incrementing the last character of the prefix string
(`"foo/bar/" → "foo/bar0"`).  Rust code then strips the prefix, takes the first
path component, and groups results into virtual directory nodes vs. file nodes.

### Root prefix

An empty prefix (`""`) lists top-level entries.  The range becomes `"" ≤ path < "\xFF"`.

### DirEntry shape

Each entry carries enough information to render a tree row and, for files, to
open them in FileViewer without a second round-trip:

```
{ name, path, entry_type: "dir"|"file", kind?, size?, mtime? }
```

Directories have no `kind`/`size`/`mtime`.

### UI layout — collapsible sidebar

The sidebar slides in alongside the main content area (not a full-page overlay).
A toggle button in the topbar shows/hides it.  It is visible by default in the
`file` view, hidden by default in `results` and `empty` views.

The sidebar needs a `source` to know which database to query; it reads the
currently active source from the file view state.

## Implementation

### 1. Common API type (`crates/common/src/api.rs`)

Add:

```rust
#[derive(Debug, Serialize, Deserialize)]
pub struct DirEntry {
    pub name: String,
    pub path: String,
    pub entry_type: String,   // "dir" | "file"
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mtime: Option<i64>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct TreeResponse {
    pub entries: Vec<DirEntry>,
}
```

### 2. Database query (`crates/server/src/db.rs`)

Add `list_dir(conn, prefix) -> Result<Vec<DirEntry>>`:

- Compute range: if prefix is empty, use `("", "\u{FFFF}")`, else `(prefix, prefix_bump(prefix))`
- Run range-scan SELECT on `files`
- For each row, strip prefix, split on `/`, take first component
- If first component == remaining path (no slash): emit file node
- Otherwise: collect unique directory names
- Return dirs (sorted) then files (sorted), no duplicates

Helper `prefix_bump(s: &str) -> String`: replace last byte with `last_byte + 1`.
(Safe because path separators are `/` = 0x2F, well below 0xFF.)

### 3. Route handler (`crates/server/src/routes.rs`)

Add:

```rust
#[derive(Deserialize)]
pub struct TreeParams {
    pub source: String,
    pub prefix: Option<String>,   // default ""
}

pub async fn list_dir(State(state), headers, Query(params)) -> impl IntoResponse
```

- Auth check
- Validate source name
- Open source DB (or 404)
- Call `db::list_dir(conn, prefix)`
- Return `Json(TreeResponse { entries })`

Register at `GET /api/v1/tree`.

### 4. Router registration (`crates/server/src/main.rs`)

```rust
.route("/api/v1/tree", get(routes::list_dir))
```

### 5. TypeScript client (`web/src/lib/api.ts`)

Add types and function:

```typescript
export interface DirEntry {
    name: string;
    path: string;
    entry_type: 'dir' | 'file';
    kind?: string;
    size?: number;
    mtime?: number;
}

export interface TreeResponse {
    entries: DirEntry[];
}

export async function listDir(source: string, prefix = ''): Promise<TreeResponse>
```

### 6. `DirectoryTree.svelte` (`web/src/lib/DirectoryTree.svelte`)

Props:
- `source: string`
- `activePath: string | null` — currently open file (highlighted)

Behaviour:
- On mount, fetch root entries
- Render a list of rows: folder icon + name for dirs, file icon + name for files
- Click dir → toggle expanded; on first expand, fetch children lazily
- Click file → dispatch `open` event with `{ source, path, kind }`
- Active file row gets a highlight style
- Auto-expand ancestors of `activePath` when it changes

State: a recursive tree node store built locally from fetched entries.

### 7. `+page.svelte` integration (`web/src/routes/+page.svelte`)

- Add `showTree: boolean = false` state
- Add toggle button to both topbars (compact + results)
- In `file` view: default `showTree = true`; wrap content in a flex row:
  `[DirectoryTree sidebar] | [FileViewer]`
- Pass `source={fileSource}` and `activePath={filePath}` to `DirectoryTree`
- Handle `open` event from `DirectoryTree` the same as `openFile` from ResultList

## Files Changed

- `crates/common/src/api.rs` — add `DirEntry`, `TreeResponse`
- `crates/server/src/db.rs` — add `list_dir`, `prefix_bump`
- `crates/server/src/routes.rs` — add `TreeParams`, `list_dir` handler
- `crates/server/src/main.rs` — register route
- `web/src/lib/api.ts` — add `DirEntry`, `TreeResponse`, `listDir`
- `web/src/lib/DirectoryTree.svelte` — full implementation
- `web/src/routes/+page.svelte` — sidebar toggle + tree integration

## Testing

1. Run a scan against a directory with nested structure
2. `curl .../api/v1/tree?source=...` with various prefixes
3. Verify dirs and files are separated; no duplicates; empty prefix returns roots
4. Open a file in the web UI → tree sidebar appears with the file highlighted
5. Expand directories and navigate to another file via the tree

## Breaking Changes

None.  New endpoint only; existing endpoints and schema unchanged.
