# Plan 081 — Content Store Abstraction ✅ COMPLETE

## Overview

Isolate the ZIP-based content storage behind a `ContentStore` trait in a new
dedicated crate (`crates/content-store/`). The subsystem gets its own SQLite
database (`data_dir/content.db`) and owns all ZIP archive I/O. Nothing outside
the crate can see chunks, archive paths, or block IDs.

The inline `file_content` storage (small files stored directly in the source
DB) is out of scope — it stays exactly as-is.

## Design Decisions

**Why a trait?** The ZIP subsystem is tightly coupled throughout the server:
`SharedArchiveState` lives in `AppState`, `ArchiveManager` is created in every
route handler, and the chunk metadata tables (`content_blocks`, `content_chunks`,
`content_archives`) are mixed into each source-specific SQLite DB. A trait
boundary makes it possible to swap the backend and test components in isolation.

**Public surface (three items only):**
- `ContentKey` — opaque key (blake3 hex hash wrapped in `Arc<str>`)
- `ContentStore` trait — `put`, `delete`, `get_lines`, `contains`, `compact`, `archive_stats`
- `ZipContentStore` — concrete impl, constructed once, used via `Arc<dyn ContentStore>`

**Separate `content.db`:** Moving chunk metadata out of each source DB eliminates
the three-way JOIN (`content_blocks → content_chunks → content_archives`) on every
read. The new schema has only `blobs`, `archives`, `chunks`.

**ZIP member naming:** Changed from `{block_id}.{chunk_num}` to
`{key_prefix_16}.{chunk_num}` (first 16 hex chars of blake3 hash). Eliminates
the `content_blocks` integer indirection entirely. Old archives become orphans on
schema bump and are deleted at first compaction — acceptable per project's
no-backwards-compat schema policy.

**Schema version:** 12 → 13. Old v12 DBs rejected; full re-index required.

## Implementation

Each step leaves the workspace compiling.

1. Create `crates/content-store/` with trait, key, and stub `ZipContentStore`
   that compiles but panics at runtime.
2. Move `SharedArchiveState`, `ArchiveManager`, `chunk_lines` into the new
   crate. Keep a re-export shim in `server/src/archive.rs` temporarily.
3. Implement `content.db` schema + `ZipContentStore::open()`.
4. Implement `put`, `contains`, `get_lines`, `delete`.
5. Implement `compact`.
6. Update `AppState` → `Arc<dyn ContentStore>`.
7. Update `worker/archive_batch.rs` → `content_store.put()`.
8. Update `worker/pipeline.rs` — remove `content_blocks` insert.
9. Update `db/mod.rs` read path (remove `ArchiveManager` param, use `ContentStore`).
10. Update route handlers (`file.rs`, `context.rs`, `search.rs`).
11. Update `compaction.rs` — delegate to `content_store.compact()`.
12. Update `routes/admin.rs` — simplify `delete_source` (no eager chunk removal).
13. Bump `SCHEMA_VERSION` 12→13, create `schema_v4.sql`.
14. Update `db/stats.rs: get_files_pending_content` to use `content_store.contains()`.
15. Delete `server/src/archive.rs` and remove all remaining `ArchiveManager` /
    `SharedArchiveState` imports from the server crate.
16. `cargo test --workspace` + `mise run clippy`.

## Files Changed

| File | Change |
|------|--------|
| `crates/content-store/` | **New crate** (entire directory) |
| `crates/server/src/archive.rs` | **Deleted** (moved into new crate) |
| `crates/server/src/worker/archive_batch.rs` | Replace ZIP/SQL logic with `content_store.put()` |
| `crates/server/src/worker/pipeline.rs` | Remove `content_blocks` insert; update `has_chunks` check |
| `crates/server/src/db/mod.rs` | Remove `ArchiveManager` param; use `ContentStore` for reads |
| `crates/server/src/routes/file.rs` | Remove `ArchiveManager::new_for_reading` |
| `crates/server/src/routes/context.rs` | Same |
| `crates/server/src/routes/search.rs` | Same |
| `crates/server/src/compaction.rs` | Delegate to `content_store.compact()` |
| `crates/server/src/routes/admin.rs` | Simplify `delete_source` |
| `crates/server/src/lib.rs` | `AppState`: `archive_state` → `content_store` |
| `crates/server/src/schema_v4.sql` | **New**: schema_v3 minus content tables |
| `crates/server/src/db/mod.rs` | `SCHEMA_VERSION` 12→13, apply schema_v4 |
| `crates/server/src/db/stats.rs` | `get_files_pending_content` uses `ContentStore` |
| `crates/server/Cargo.toml` | Add `find-content-store` dependency |
| `Cargo.toml` (workspace) | Add `crates/content-store` to members |

## Testing

- **Unit tests in `crates/content-store/`**: round-trip put/get, idempotency,
  delete, `contains`, line-range boundary conditions, compact removes orphans,
  concurrent put same key.
- **Existing server integration tests**: `TestServer`-based tests in
  `crates/server/tests/` must continue to pass (file content readable after
  indexing, context retrieval, compaction, delete-source).
- **New integration test** `crates/server/tests/content_store.rs`: index a
  file, confirm `content_store.contains()` returns true; compact removes a
  deleted file's content.
- **Clippy**: `mise run clippy` clean.

## Breaking Changes

Schema version bumps 12 → 13. Existing databases must be deleted and re-indexed.
No client API changes.
