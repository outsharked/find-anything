# Archive Members as First-Class Files

## Overview

Currently a ZIP file is indexed as a single `files` entry; its inner text files are stored as
`lines` rows distinguished only by `archive_path`. This means:

- Ctrl+P cannot find inner files by name
- The directory tree cannot browse into a ZIP
- Search results surface `archive_path` as a secondary detail, not the primary file identity

This plan makes inner archive members first-class citizens using **composite paths** — the
`::` separator encodes nesting directly in the `files.path` column, requiring no schema change.

---

## Design: Composite Paths

Inner archive members are identified by a composite path using `::` as a separator:

```
files table:
  path="taxes/w2.zip"                    ← the ZIP itself (outer file)
  path="taxes/w2.zip::wages.pdf"         ← inner member
  path="taxes/w2.zip::letter.txt"        ← inner member
  path="taxes/w2.zip::inner.tar.gz::report.txt" ← nested archive member
```

Each inner member has its own `file_id`, its own chunk storage, and is independently
retrievable via `GET /api/v1/file?path=taxes/w2.zip::wages.pdf`.

### Why `::` over an `archive_member` column

An `archive_member` column would require a schema migration and thread `archive_member`
through every API. With composite paths:

- **No schema change** — `files.path TEXT NOT NULL UNIQUE` works as-is
- **Nested archives work naturally** — `outer.zip::inner.tar.gz::file.txt` without needing
  additional columns per nesting level
- **Simpler queries** — every operation is just `WHERE path = ?1`
- **Single source of truth** — the path IS the identifier

The `::` separator is reserved (documented as unsupported in plain filenames). It is
extremely rare in real paths on all platforms.

### `archive_path` backward compatibility

`SearchResult.archive_path` is preserved for the web UI. The server derives it from
`files.path` by splitting on the first `::`:
- `taxes/w2.zip::wages.pdf` → `path="taxes/w2.zip"`, `archive_path="wages.pdf"`

External API endpoints (`GET /api/v1/file`, `GET /api/v1/context`) still accept an
`archive_path` query param and combine it: `full_path = path + "::" + archive_path`.

---

## Archive Depth Limit

Nested archives (zip-in-zip) are extracted recursively. To prevent infinite recursion from
malicious zip bombs, extraction is limited by `scan.archives.max_depth` (default: 10).

When the limit is exceeded, a warning is logged and only the filename is indexed for
that entry. Configurable in `config.toml`:

```toml
[scan.archives]
max_depth = 10   # default; set to 1 to disable nested archive extraction
```

---

## Schema Migration (v2 → v3)

The `lines` table previously had an `archive_path TEXT` column; this is now redundant
(the path is in `files.path`). Migration strategy:

- Server detects v2 schema by checking `pragma_table_info('lines')` for `archive_path`
- Drops all tables and recreates with the new schema
- User must re-run `find-scan` to rebuild the index

---

## Delete Semantics

When `delete_paths = ["taxes/w2.zip"]`, the server deletes:
```sql
DELETE FROM files WHERE path = 'taxes/w2.zip' OR path LIKE 'taxes/w2.zip::%'
```
The `ON DELETE CASCADE` on `lines.file_id` handles line cleanup automatically.

The client filters `::` paths out of the deletion detection map — inner members are always
managed server-side (re-indexed when the outer ZIP's mtime changes).

---

## Re-index Semantics

When `taxes/w2.zip` has a changed mtime, the client sends:
1. `IndexFile { path: "taxes/w2.zip", kind: "archive", lines: [path line] }`
2. `IndexFile { path: "taxes/w2.zip::wages.pdf", kind: "text", lines: [...] }`
3. `IndexFile { path: "taxes/w2.zip::letter.txt", kind: "text", lines: [...] }`

The server, on processing the outer archive file (kind="archive", no "::" in path),
first deletes all inner member rows (`path LIKE 'taxes/w2.zip::%'`), then processes
each `IndexFile` in the batch.

---

## Tree / Browse

`GET /api/v1/tree?source=X&prefix=taxes/` returns `taxes/w2.zip` as a regular file
with `kind="archive"`. The UI recognises `kind === 'archive'` and renders an expand
toggle instead of an open button.

Clicking the toggle calls `listArchiveMembers(source, "taxes/w2.zip")`, which hits:
```
GET /api/v1/tree?source=X&prefix=taxes/w2.zip::
```
This does a range scan `WHERE path >= 'taxes/w2.zip::' AND path < 'taxes/w2.zip:;'`,
returning inner members as file leaves. Nested archives within the ZIP appear as
expandable nodes themselves.

---

## `list_files` (Ctrl+P)

`GET /api/v1/files?source=X` returns all rows including composite paths. The
`CommandPalette` displays `taxes/w2.zip::wages.pdf` as `"taxes/w2.zip → wages.pdf"`.
Selecting it dispatches `{ path: "taxes/w2.zip", archivePath: "wages.pdf" }` to open
the correct file.

The client-side deletion detection filters out `::` paths:
```rust
let server_files = api.list_files(source).await?
    .into_iter()
    .filter(|f| !f.path.contains("::"))  // only outer files
    .collect();
```

---

## Files Changed

| File | Change |
|---|---|
| `crates/common/src/api.rs` | `IndexFile.path` doc updated; `DirEntry` entry_type note |
| `crates/common/src/config.rs` | `ArchiveConfig.max_depth` field added |
| `crates/common/src/extract/archive.rs` | `ArchiveExtractor { max_depth }` struct; recursive nested extraction with depth limit |
| `crates/common/src/extract/mod.rs` | `extract()` takes `max_archive_depth` param |
| `crates/server/src/schema_v2.sql` | Removed `lines.archive_path` column |
| `crates/server/src/db.rs` | v2→v3 migration; `delete_files` cascades to `::` members; `fts_candidates` splits composite path; all context/file queries simplified (no `archive_path` filter); `list_dir` handles `::` prefix |
| `crates/server/src/worker.rs` | `process_file` clears inner members when re-indexing outer archive; removed `archive_path` from line insert |
| `crates/server/src/routes.rs` | All endpoints combine `path + archive_path` → composite path |
| `crates/client/src/scan.rs` | `build_index_files()` groups archive lines into separate `IndexFile` per member; filters `::` from deletion detection |
| `web/src/lib/api.ts` | `listArchiveMembers()` helper |
| `web/src/lib/CommandPalette.svelte` | Displays `::` paths as `zip → member`; dispatches `archivePath` on select |
| `web/src/lib/TreeRow.svelte` | `kind === 'archive'` entries expand like directories via `listArchiveMembers` |
| `web/src/routes/+page.svelte` | `openFileFromTree` and `handlePaletteSelect` handle `archivePath` |

---

## Breaking Changes

- `lines.archive_path` column removed (schema migration required — re-run find-scan)
- `GET /api/v1/tree` now skips composite paths in regular directory listings
- `archive_path` query param on file/context endpoints still works (backward compat)

## Verification

1. Index a ZIP containing multiple text files; verify inner files appear in Ctrl+P as `zip → member`
2. Search for content from an inner file; verify result shows archive_path correctly
3. Open a search result from a ZIP; verify FileViewer shows only that inner file's content
4. Click a ZIP in the directory tree; verify it expands to show inner members
5. Click an inner member; verify FileViewer opens correctly
6. Re-index the same ZIP after content change; verify old chunks removed cleanly
7. Delete a ZIP from the filesystem; verify all inner member rows removed on next scan
8. Create a zip-within-a-zip; verify depth limit log message appears and filename is indexed
