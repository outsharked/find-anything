# Atomic Archive Deletion

## Overview

When indexed files are deleted, their content chunks must be removed from the
ZIP archives.  Currently `db::delete_files` only removes rows from SQLite;
chunk content is orphaned in the ZIPs indefinitely.

## Design

SQLite and ZIP files are separate storage systems, so true cross-system ACID
is not possible.  However, by keeping the SQLite transaction open until after
the ZIP rewrite completes, we get the next-best thing:

1. Open a SQLite transaction
2. Read the chunk refs (`chunk_archive`, `chunk_name`) for all lines belonging
   to the files being deleted
3. Delete the `files` rows within the open transaction (cascades to `lines`)
4. Rewrite the affected ZIP archives to remove those chunks
   — `ArchiveManager::remove_chunks` already does this atomically via
   temp-file → rename
5. **If ZIP rewrite fails** → drop the transaction (automatic rollback) →
   SQLite and ZIPs remain consistent
6. **If ZIP rewrite succeeds** → commit the transaction

### Remaining edge case

A crash in the narrow window between the ZIP rename completing and the SQLite
`COMMIT` will leave SQLite rolled back (rows still present) while the ZIP has
lost those chunks.  Subsequent searches for those files return empty content.
The next client scan will re-index the files and restore consistency.  This
window is acceptably small for this use case.

## Implementation

### `crates/server/src/db.rs`

- Add helper `collect_chunk_refs(tx, paths) -> Result<Vec<ChunkRef>>` that
  queries `lines` for all `(chunk_archive, chunk_name)` pairs belonging to the
  given file paths (before deleting them).
- Update `delete_files(conn, archive_mgr, paths)` to:
  - Open transaction
  - Call `collect_chunk_refs`
  - Delete rows
  - Call `archive_mgr.remove_chunks(refs)`
  - Commit

### `crates/server/src/routes.rs`

- `delete_files` handler: construct `ArchiveManager::new(data_dir)` and pass
  it into `db::delete_files`.

### `crates/server/src/archive.rs`

- Remove `#[allow(dead_code)]` from `remove_chunks` and `rewrite_archive` —
  they are now actively used.

## Files Changed

- `crates/server/src/db.rs` — add `collect_chunk_refs`, update `delete_files`
- `crates/server/src/routes.rs` — pass `ArchiveManager` into `db::delete_files`
- `crates/server/src/archive.rs` — remove `#[allow(dead_code)]` attributes

## Testing

1. Index a source, verify files appear in search
2. Delete a file from the source, re-run the client scan
3. Confirm the file no longer appears in search results
4. Inspect the ZIP archives to confirm no orphaned chunks remain for the
   deleted file
5. Restart the server mid-delete (simulate crash) and confirm the index
   remains consistent

## Breaking Changes

None. Internal change only; API is unchanged.
