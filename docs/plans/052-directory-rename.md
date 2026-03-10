# 052 — Directory Rename Support + Storage Improvements

## Overview

Three related changes, all requiring a schema break, bundled together:

1. **Opaque chunk naming** — chunk names change from `{path}.chunk{N}.txt` to
   `{file_id}.{N}`, making chunk names stable across renames.
2. **Inline content storage** — files whose total content is below a configurable
   threshold are stored directly in SQLite instead of ZIP archives, eliminating ZIP
   overhead for small files and simplifying their delete/rename paths.
3. **Directory rename support** — a new `rename_paths` field in `BulkRequest` allows
   the watcher to rename files in the index without re-extracting content.

---

## Background

### Chunk names are opaque addresses, not semantic paths

Chunk names in ZIP archives currently embed the file's relative path:

```
docs/report.pdf.chunk0.txt
docs/report.pdf.chunk1.txt
```

However, the path portion serves **no functional purpose**. The `file_id` foreign key in
the `lines` table is what links a chunk back to its file. `chunk_name` is purely an opaque
key used to locate a ZIP entry — nothing derives meaning from the embedded path string.

**This plan changes chunk naming to `{file_id}.{N}`**, making chunk names stable
across file renames. A rename then becomes a pure DB operation; ZIP archives are never
touched.

### Include/exclude pattern complications for renames

When a directory is renamed, the new path may match different include/exclude patterns
than the old path:

- Files previously included may now be excluded (new name matches an exclude glob).
- Files previously excluded may now be included (old name matched an exclude glob).
- `.index` / `.noindex` control files move with the rename, but their *parent path* changes,
  which may change which rules apply.

A pure "rename all paths" bulk update is incorrect in the general case. The client must
re-evaluate inclusion rules for the new path and split affected files into three groups:

| Group | Action |
|-------|--------|
| Was included, still included | Rename paths in DB (no ZIP change) |
| Was included, now excluded | Delete from index |
| Was excluded, now included | Full index as new files |

### Schema break

All three changes require a schema version bump (`SCHEMA_VERSION` → 9). The server
already fails to start on a schema mismatch. No migration path; delete and re-index.

---

## Part 1 — Opaque Chunk Naming (`{file_id}.{N}`)

### archive.rs — chunk name generation

```rust
// Before:
let chunk_name = format!("{}.chunk{}.txt", chunk.file_path, chunk.chunk_number);

// After:
let chunk_name = format!("{}.{}", file_id, chunk.chunk_number);
```

`file_id` is the row ID returned after `INSERT OR REPLACE INTO files` in the worker,
already available at the point chunks are written.

### Human readability via ZIP entry comments

To preserve greppability for humans inspecting an orphaned archive, store the original
file path as a per-entry comment using `FileOptions::with_file_comment()`:

```rust
let options = FileOptions::default()
    // ... compression, etc. ...
    .with_file_comment(chunk.file_path.clone());
zip.start_file(chunk_name, options)?;  // chunk_name is "42.7"
```

The comment is stored in the ZIP central directory and is visible to standard ZIP tools
(`unzip -v`, 7-Zip info view, etc.) alongside the entry name. It carries no operational
meaning — purely a human aid. Set on every chunk so any entry is self-describing without
needing to find chunk 0 first. Copied through unchanged during archive rewrites.

### Compaction

Compaction's orphan detection builds a live-chunk set from all `(chunk_archive, chunk_name)`
pairs in `lines`. The naming change is transparent — compaction works correctly with the
new format.

---

## Part 2 — Inline Content Storage for Small Files

### Motivation

ZIP has ~100 bytes of structural overhead per entry (local file header + central directory
entry + entry name + comment). For a 50-byte config file the ZIP structure is larger than
the payload. Files below the inline threshold also compress poorly, so the ZIP size
advantage is minimal even before considering overhead.

Storing small file content directly in SQLite eliminates the ZIP overhead, speeds up reads
(SQLite page cache vs. ZIP file I/O), and removes the ZIP from the delete/rename paths for
these files entirely.

### Configuration

Add to `ServerConfig` (and `server.toml`):

```toml
# Files whose total extracted content is at or below this size (in bytes) are stored
# directly in SQLite rather than in ZIP archives. Set to 0 to disable inline storage.
inline_threshold_bytes = 256
```

Exposed via `GET /api/v1/settings` alongside other server config values.

The threshold is a server-side setting — clients are unaware of it. The decision is made
in the worker when processing each file.

### Schema change

Add a new `file_content` table rather than a column on `files`:

```sql
CREATE TABLE file_content (
    file_id INTEGER PRIMARY KEY REFERENCES files(id) ON DELETE CASCADE,
    content TEXT NOT NULL
);
```

Keeping content out of the `files` table preserves row density for the many queries
that scan `files` for metadata only (tree browsing, stats, compaction). Those queries
touch many rows sequentially; wider rows mean fewer rows per 4 KB B-tree page and more
I/O. Content reads, by contrast, are point lookups — one extra rowid lookup into
`file_content` is negligible.

`file_content` is only populated for inline files. ZIP-stored files have no row here.

The `lines` rows for inline files store NULL `chunk_archive` and `chunk_name` as the
signal to the read path to fetch from `file_content` instead of a ZIP. `line_number`
and `line_offset_in_chunk` (byte offset into the content string) are still populated
normally.

### Worker write path

After content extraction, check total content size:

```rust
if total_content_bytes <= config.inline_threshold_bytes && config.inline_threshold_bytes > 0 {
    // Store inline: UPDATE files SET inline_content = ?, clear chunk refs
    // Insert lines rows with NULL chunk_archive/chunk_name
} else {
    // Existing ZIP path
}
```

When a file transitions across the threshold on re-index (grew or shrank):
- **Inline → ZIP:** write new chunks to ZIP; set `inline_content = NULL` on the `files` row.
- **ZIP → Inline:** remove old chunks from ZIP (existing delete path); set `inline_content`.

The existing "delete old chunks before writing new ones" logic in the worker handles both
transitions correctly — it just also needs to clear/set `inline_content` accordingly.

### Read path

`read_chunk_lines()` in `db/mod.rs` currently takes `(chunk_archive, chunk_name)` and
opens a ZIP. The signature gains a `file_id` parameter:

```rust
fn read_chunk_lines(
    cache: &mut ChunkCache,
    archive_mgr: &ArchiveManager,
    file_id: i64,
    chunk_archive: Option<&str>,
    chunk_name: Option<&str>,
) -> Result<Vec<String>>
```

If `chunk_archive` is `None`, fetch content with:

```sql
SELECT content FROM file_content WHERE file_id = ?
```

then split into lines and return directly. Otherwise proceed with the existing ZIP path.

The SQL queries that join `lines` already select `file_id`; no additional join is
needed at the query level.

### Delete path

For inline files, deletion is a plain `DELETE FROM files WHERE path = ?`. The `lines` rows
and the `file_content` row both cascade via FK. No ZIP involvement. No `ChunkRef` is
returned for inline files.

### Compaction

The live-chunk set is built from `lines` rows where `chunk_archive IS NOT NULL`. Inline
files are naturally excluded — they have no ZIP entries to check for orphaning.

---

## Part 3 — Rename Support (Files and Directories)

### New API field: `rename_paths`

Add to `BulkRequest` in `crates/common/src/api.rs`:

```rust
pub struct PathRename {
    pub old_path: String,
    pub new_path: String,
}

pub struct BulkRequest {
    // ... existing fields ...
    #[serde(default)]
    pub rename_paths: Vec<PathRename>,
}
```

Each entry is a single top-level file rename. The client expands a directory rename into
one `PathRename` per included file. Files that become excluded go into `delete_paths`;
files that become newly included go into `files`.

### Processing order in the worker

```
deletes → renames → upserts
```

Renames after deletes handles the edge case where the rename destination conflicts with a
file being deleted in the same batch (e.g. an `a` ↔ `b` swap via a temp name).

### Server-side: `rename_files()` in `db/mod.rs`

```rust
pub fn rename_files(conn: &Connection, renames: &[PathRename]) -> Result<()>
```

For each rename within a single transaction:

1. Look up `file_id` for `old_path`.
2. If `new_path` already exists in `files`, skip (race with periodic scan — new path
   already indexed, rename is a no-op).
3. `UPDATE files SET path = ? WHERE path = ?`
4. Rename archive members: update all rows where `path LIKE 'old_path::%'` to replace
   the prefix with `new_path`.
5. `UPDATE fts_files SET path = ? WHERE path = ?` for the file and each member.

No `lines` update needed. No ZIP rewrite needed. Inline content needs no update either —
it lives on the `files` row which was already updated.

### FTS path column

The `fts_files` table has a `path` column used for filename search. FTS5 `UPDATE` is a
delete+insert internally but the SQL surface accepts it:

```sql
UPDATE fts_files SET path = ? WHERE rowid = ?
```

Issued once per renamed file (not per line), keyed on `files.id`.

### Client-side detection (watch.rs)

The `notify` crate emits `Modify(Name(From))` and `Modify(Name(To))` events. The current
code treats each independently (absent → Delete, exists → Update with re-extraction).
Two detection strategies are used depending on whether the renamed path is a file or a
directory.

#### Single file renames — debounce-window pairing

Both `From` and `To` events for a file rename arrive within the same debounce window.
After the window expires, before flushing accumulated events, scan the map for pairs:

- A path with `AccumulatedKind::Delete` (no longer exists on disk) paired with a path
  with `AccumulatedKind::Update` (now exists on disk) where **both arrived in the same
  flush cycle** and the Update path did not exist at the start of the window.

If the include/exclude rules pass for the new path, emit a `PathRename` entry. If the
new path is now excluded, emit a plain delete for the old path and nothing for the new.
If the old path was excluded (not indexed), emit a normal Update for the new path.

Unmatched Delete events (no corresponding Update in the same window) remain plain
deletes. Unmatched Update events remain plain updates with re-extraction. This means
the pairing is best-effort — correctness is preserved even if a pair is missed.

**Pairing heuristic:** a Delete+Update pair is treated as a rename when:
1. The deleted path no longer exists.
2. The updated path now exists and is a file.
3. Both events were accumulated in the same debounce flush (tracked by a per-event
   arrival timestamp or a generation counter on the accumulator).

No mtime/size matching is needed — if a file is renamed and simultaneously modified,
treating it as a rename is still correct; the content will be re-extracted on the next
update event if needed. (In practice, rename-then-immediately-modify arrives as a
separate subsequent event after the debounce window.)

#### Directory renames — on-disk walk

On a `Modify(Name(_))` event for a path that no longer exists and was a directory (or
when a new directory appears that has no prior event in the window), trigger a
**directory rename scan**:

1. Query the server via `GET /api/v1/tree?source=X&prefix=old_dir/` to get indexed files
   under the old prefix.
2. Walk `new_dir/` on disk.
3. Match files by relative sub-path: `old_dir/foo.txt` ↔ `new_dir/foo.txt`.
4. Re-evaluate include/exclude rules (source globs + `.index`/`.noindex` files) for each
   `new_dir/` path.
5. Build one `BulkRequest`:
   - `rename_paths`: matched files that remain included
   - `delete_paths`: unmatched old paths + files now excluded
   - `files`: files under `new_dir/` with no old counterpart, or newly included

**Fallback:** If `new_dir/` is not found on disk, emit plain deletes for all indexed
paths under `old_prefix`.

---

## Files Changed

| File | Change |
|------|--------|
| `crates/server/src/schema_v2.sql` | Add `file_content` table; bump schema version comment |
| `crates/server/src/db/mod.rs` | Bump `SCHEMA_VERSION` to 9; remove old migration branches; add `rename_files()`; update `read_chunk_lines()` for inline path via `file_content`; update delete to skip ZIP for inline files |
| `crates/server/src/archive.rs` | Change chunk name format to `{file_id}.{N}`; add file-path ZIP entry comments |
| `crates/server/src/worker.rs` | Inline threshold check; pass `file_id` to chunk writing; process `rename_paths` after deletes; handle inline↔ZIP transitions |
| `crates/server/src/compaction.rs` | Filter `chunk_archive IS NOT NULL` when building live-chunk set |
| `crates/common/src/config.rs` | Add `inline_threshold_bytes: u64` to `ServerConfig` |
| `crates/common/src/api.rs` | Add `PathRename`; add `rename_paths` to `BulkRequest`; expose `inline_threshold_bytes` in settings response |
| `crates/client/src/watch.rs` | Detect directory rename events; build rename `BulkRequest` |

---

## Testing

1. **Chunk naming:** After indexing a file, `lines.chunk_name` values match `{file_id}.{N}`.
2. **ZIP comments:** `unzip -v` on an archive shows the original file path as the entry comment.
3. **Inline storage:** Index a file ≤ threshold; verify `files.inline_content` is populated,
   `lines.chunk_archive` and `chunk_name` are NULL, and the file appears in search results.
4. **Inline threshold = 0:** All files go to ZIP; no inline content written.
5. **Threshold transition (grow):** Re-index a file that has grown past the threshold; verify
   it moves from inline to ZIP, old inline content is cleared, search still works.
6. **Threshold transition (shrink):** Re-index a file that has shrunk below the threshold;
   verify it moves from ZIP to inline, old chunks removed from ZIP.
7. **Inline delete:** Delete an inline file; verify no ZIP rewrite occurs.
8. **Single file rename:** Rename a large file (above inline threshold); verify it
   appears under the new path without re-extraction (no extraction log entry).
9. **Single file rename, now excluded:** Rename a file so the new name matches an
   exclude glob; verify it is removed from the index, not renamed.
10. **Single file rename, missed pair:** Simulate unpaired events (drop the To event);
    verify fallback to plain delete + re-index produces a correct final state.
11. **Directory rename correctness:** Rename a directory; verify all files appear under
    the new path in search results and old paths are gone, without re-extraction.
12. **Include/exclude on directory rename:** Rename a directory so the new name matches
    an exclude glob; verify files are removed from the index rather than renamed.
13. **Newly included on rename:** Rename an excluded directory to an included name;
    verify files are indexed as new entries.
14. **Archive members:** Index a ZIP inside the renamed directory; verify inner members
    appear under the new outer path.
15. **Schema guard:** Start the server against a v8 database; verify it refuses to start.

---

## Breaking Changes

- **Schema v8 → v9:** All existing databases are incompatible and must be deleted.
  The server refuses to start and prints a clear error. Re-run `find-scan` to re-index.
- **`MIN_CLIENT_VERSION`:** No bump needed. Rename is an optimisation; old clients
  continue to produce correct results via delete+re-index.
