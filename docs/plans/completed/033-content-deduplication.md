# Plan 033: Content Deduplication

## Overview

Files indexed from backups and archives often appear multiple times — e.g. a PDF exists
directly on disk **and** inside one or more backup `.tar.gz` archives. Currently each
copy is extracted, chunked, and stored independently in the ZIP archives, and all
copies appear as separate search results. This plan adds blake3 content hashing to
eliminate duplicate ZIP storage and surface a clean "primary + aliases" result in
search.

## Design Decisions

- Each file gets a **blake3 hash of its raw bytes**, computed client-side.
- The server's `files` table gains two nullable columns: `content_hash` and
  `canonical_file_id`.
- The **first** file indexed with a given hash is the **canonical** — it owns the ZIP
  chunks and `lines`/FTS rows.
- Every subsequent file with the same hash becomes an **alias** — a `files` row with
  `canonical_file_id` set and **no lines or chunks written**.
- **Search results** carry a new `aliases: Vec<String>` field listing other paths with
  identical content. Empty when there are no duplicates (omitted from JSON).
- Deletion of an alias is cheap (delete the `files` row, no ZIP changes). Deletion of
  a canonical **promotes** the first alias: the existing ZIP chunks (named after the
  old canonical's path) are re-pointed to the promoted file by re-inserting `lines`
  rows with the same `(chunk_archive, chunk_name)` tuples — no ZIP rewrite needed.
- **No backward compatibility**: existing DBs should be wiped and re-indexed after
  deployment. `content_hash` is required; files where hashing fails (e.g. permission
  error) set it to `None` and are treated as always-canonical (no dedup attempted).

## Schema Migration (v5)

```sql
ALTER TABLE files ADD COLUMN content_hash      TEXT;
ALTER TABLE files ADD COLUMN canonical_file_id INTEGER REFERENCES files(id) ON DELETE SET NULL;

CREATE INDEX IF NOT EXISTS files_content_hash ON files(content_hash)
    WHERE content_hash IS NOT NULL;
CREATE INDEX IF NOT EXISTS files_canonical ON files(canonical_file_id)
    WHERE canonical_file_id IS NOT NULL;

PRAGMA user_version = 5;
```

## Key Logic

### `process_file()` — dedup check (worker.rs)

```
if file.content_hash is Some(hash):
    canonical ← SELECT id FROM files
                 WHERE content_hash = hash
                   AND canonical_file_id IS NULL
                   AND path != file.path
                 LIMIT 1

    if canonical found:
        upsert files row with canonical_file_id = canonical.id, content_hash = hash
        return early  ← skip ALL chunk/lines/FTS writes

fall through → normal flow (write chunks, lines, FTS, set canonical_file_id = NULL)
```

### `delete_files()` — canonical promotion (db.rs)

When deleting path P where `canonical_file_id IS NULL` (P is canonical):

```
aliases ← SELECT id, path FROM files WHERE canonical_file_id = P.id ORDER BY id

if aliases is empty:
    normal deletion (remove chunks from ZIP, delete files row → cascade lines, delete FTS)

else:
    new_canonical ← aliases[0]

    1. Fetch canonical's lines: (line_number, chunk_archive, chunk_name, offset)
    2. Read chunk content from ZIP for each line (for FTS re-insertion)
    3. Manually delete FTS entries for canonical's line ids (contentless FTS5 requires this)
    4. DELETE canonical's files row (cascades → deletes its lines rows)
    5. UPDATE files SET canonical_file_id = NULL WHERE id = new_canonical.id
    6. UPDATE files SET canonical_file_id = new_canonical.id WHERE canonical_file_id = P.id
    7. INSERT lines for new_canonical with same (chunk_archive, chunk_name) as old canonical's lines
    8. INSERT FTS entries for new_canonical's new line ids

    ← NO ZIP rewrite; old chunk names (e.g. "old/path.chunk0.txt") remain valid in the ZIP,
      now referenced by new_canonical's lines rows
```

When deleting an alias: just `DELETE FROM files WHERE path = ?` — no chunks to touch.

## Files Changed

| File | Change |
|------|--------|
| `Cargo.toml` (workspace) | Add `blake3 = "1"` to `[workspace.dependencies]` |
| `crates/client/Cargo.toml` | Add `blake3 = { workspace = true }` |
| `crates/extractors/archive/Cargo.toml` | Add `blake3 = { workspace = true }` |
| `crates/common/src/api.rs` | `IndexFile.content_hash`, `SearchResult.aliases` |
| `crates/client/src/scan.rs` | Hash raw bytes for direct files; propagate to `IndexFile`; update channel type to `MemberBatch` |
| `crates/client/src/batch.rs` | Update `build_member_index_files` to accept and set `content_hash` |
| `crates/extractors/archive/src/lib.rs` | Hash member bytes; emit `MemberBatch`; expose hash through streaming channel |
| `crates/server/src/db.rs` | `migrate_v5()`; update `check_schema_version()`; overhaul `delete_files()` with promotion; add `fetch_aliases_for_canonical_ids()`; add `file_id` to `CandidateRow` |
| `crates/server/src/worker.rs` | Dedup early-exit in `process_file()` |
| `crates/server/src/routes/search.rs` | Alias lookup; populate `SearchResult.aliases` |
| `web/src/lib/api.ts` | Add `aliases?: string[]` to `SearchResult` |
| `web/src/lib/SearchResult.svelte` | Render aliases (collapsed by default) |
| `docs/plans/033-content-deduplication.md` | This plan file |

## Edge Cases

- **Hash fails** (permission error, I/O): `content_hash = None`; dedup skipped; file treated as always-canonical.
- **Hash changes on canonical**: falls through to normal re-index; aliases get stale until their own next scan, which self-heals.
- **Alias becomes canonical**: if re-scanned with a different hash, `INSERT ... ON CONFLICT DO UPDATE SET canonical_file_id = NULL` clears alias status.
- **Circular references**: impossible — dedup lookup requires `canonical_file_id IS NULL`, so an alias is never the target of another alias.
- **Archive re-indexing bulk delete**: when an outer archive is re-indexed, all inner members are bulk-deleted. Former aliases of inner canonical members become empty canonicals (via ON DELETE SET NULL). They self-heal on the next scan of their source archive.

## Testing

1. Index a file directly and as an archive member in the same source → second entry has
   `canonical_file_id` set, zero `lines` rows, no new ZIP entry.
2. Search for content → one result with `aliases` field populated.
3. Delete canonical → alias is promoted; search still returns the alias; same content.
4. Delete alias → canonical unchanged; search result no longer shows alias.
5. `cargo test --workspace` passes; `mise run clippy` passes.
