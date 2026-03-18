# Schema v3: Content-Addressable Storage

## Overview

This overhaul replaces the current `lines`-based schema with a content-addressable
design that eliminates the "phantom canonical" bug class, dramatically reduces row
counts (~25× for the old `lines` table), and cleanly models content deduplication
via a junction table rather than a canonical/alias pointer.

Key changes at a glance:

- Remove `canonical_file_id` / alias system from `files` entirely
- Introduce `content_blocks` (hash → integer id) and `content_archives` (zip name →
  integer id) lookup tables
- Replace `lines` (one row per line) with `content_chunks` (one row per chunk per
  content block); `files.content_hash` joins directly to `content_blocks` — no
  separate `file_block` table needed
- Encode FTS5 rowid as `file_id * MAX_LINES_PER_FILE + line_number` — no
  `line_index` table needed
- Add `duplicates(content_hash, file_id)` junction table for explicit duplicate
  tracking
- No backwards compatibility — full re-index required

---

## Design Decisions

### 1. Remove canonical_file_id / alias system

`canonical_file_id` on the `files` table is removed entirely. The phantom-canonical
bug (plan 074/regression fix in pipeline.rs) is a direct consequence of
`ON DELETE SET NULL` promoting an alias to an "orphan canonical" with no content.
Since every file now stores its own content independently (or shares a content block
by hash), this class of bug cannot occur. The dedup query complexity in
`pipeline.rs` also vanishes.

### 2. Content-addressable block storage

Two new tables replace the ad-hoc `(chunk_archive, chunk_name)` TEXT columns that
previously sat in every row of the `lines` table:

```sql
content_blocks(id INTEGER PRIMARY KEY, content_hash TEXT UNIQUE NOT NULL)
content_archives(id INTEGER PRIMARY KEY, name TEXT UNIQUE NOT NULL)
```

A `content_block_id` is a stable integer identifying a unique body of content by
its hash. Chunk names in ZIPs become `{block_id}.chunk{chunk_number}` — short
integer-based names. When two files share the same content hash they share the same
`block_id` and therefore the same ZIP chunks; content is stored only once on disk.

### 3. Replace `lines` with `content_chunks` + `file_block`

Old `lines` table: one row per line of content — potentially hundreds of thousands
of rows per source file.

```sql
content_chunks(block_id, chunk_number, archive_id, start_line, end_line)
file_block(file_id, block_id)
```

One `content_chunks` row per chunk per content block (~25× fewer rows than
`lines`). The `block_id` is resolved at read/write time via
`JOIN content_blocks ON content_hash` — no separate mapping table needed.

`chunk_name` is never stored — derived as `format!("{}.chunk{}", block_id,
chunk_number)`. `line_offset_in_chunk` is computed as `line_number - start_line`.

### 4. FTS5 rowid encoding — no line_index table

```
FTS5 rowid = file_id * MAX_LINES_PER_FILE + line_number
MAX_LINES_PER_FILE = 1_000_000  (hardcoded constant)
```

This is hardcoded, NOT derived from `max_content_kb` config — FTS rowids must be
stable across config changes, and a degenerate file of 1M empty lines must not
overflow. Lines at or beyond this limit are skipped at index time with a
`tracing::warn!`. Decode: `file_id = rowid / MAX_LINES_PER_FILE`,
`line_number = rowid % MAX_LINES_PER_FILE`. Max safe file_id before i64 overflow:
~9.2 × 10¹² — effectively unlimited.

Eliminates the `line_index` join table entirely. FTS orphaned entries (from deleted
files) are naturally filtered by the `JOIN files f ON f.id = (rowid /
MAX_LINES_PER_FILE)` in search queries.

### 5. Duplicates junction table

```sql
duplicates(content_hash TEXT, file_id INTEGER REFERENCES files ON DELETE CASCADE)
PRIMARY KEY (content_hash, file_id)
```

A file appears in this table only when 2+ files share the same hash. Insert
invariant: when inserting file F with hash H, if any other file with hash H already
exists in `files`, insert `(H, F.id)` AND ensure `(H, existing_file_id)` rows
exist. On deletion: `ON DELETE CASCADE` removes F's entry; a GC pass or the
deletion handler removes singleton entries (no longer duplicates). User-facing:
new `GET /api/v1/duplicates` endpoint; optional `duplicate_paths` field on
`SearchResult`.

### 6. Inline storage unchanged

`file_content(file_id, content)` preserved for files below
`inline_threshold_bytes`. Inline files still have `content_hash` set (for duplicate
detection) but no `content_blocks` / `content_chunks` / `file_block` rows.

### 7. No backwards compatibility — full re-index required

Schema version bumps from current to next. Migration: delete `data_dir/sources/`,
restart server, run `find-scan --force`.

---

## New Schema (schema_v3.sql)

```sql
PRAGMA journal_mode=WAL;
PRAGMA foreign_keys=ON;

CREATE TABLE IF NOT EXISTS meta (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS files (
    id               INTEGER PRIMARY KEY AUTOINCREMENT,
    path             TEXT    NOT NULL UNIQUE,
    mtime            INTEGER NOT NULL,
    size             INTEGER,
    kind             TEXT    NOT NULL DEFAULT 'text',
    indexed_at       INTEGER,
    extract_ms       INTEGER,
    content_hash     TEXT,
    scanner_version  INTEGER NOT NULL DEFAULT 0
    -- canonical_file_id REMOVED
);

CREATE INDEX IF NOT EXISTS files_content_hash ON files(content_hash)
    WHERE content_hash IS NOT NULL;
CREATE INDEX IF NOT EXISTS idx_files_mtime ON files(mtime);

-- Maps content hash → integer ID used as chunk name prefix in ZIPs.
-- Separate from files: content can outlive any particular file.
CREATE TABLE IF NOT EXISTS content_blocks (
    id           INTEGER PRIMARY KEY AUTOINCREMENT,
    content_hash TEXT NOT NULL UNIQUE
);

-- One row per ZIP archive file on disk.
CREATE TABLE IF NOT EXISTS content_archives (
    id   INTEGER PRIMARY KEY AUTOINCREMENT,
    name TEXT NOT NULL UNIQUE   -- e.g. "content_00042.zip"
);

-- One row per chunk per content block.
-- chunk_name in ZIP = "{block_id}.chunk{chunk_number}"
-- line_offset_in_chunk = line_number - start_line  (computed, never stored)
CREATE TABLE IF NOT EXISTS content_chunks (
    block_id     INTEGER NOT NULL REFERENCES content_blocks(id),
    chunk_number INTEGER NOT NULL,
    archive_id   INTEGER NOT NULL REFERENCES content_archives(id),
    start_line   INTEGER NOT NULL,
    end_line     INTEGER NOT NULL,
    PRIMARY KEY (block_id, chunk_number)
);

CREATE INDEX IF NOT EXISTS idx_content_chunks_archive
    ON content_chunks(archive_id);

-- Inline content for small files (below inline_threshold_bytes).
CREATE TABLE IF NOT EXISTS file_content (
    file_id INTEGER PRIMARY KEY REFERENCES files(id) ON DELETE CASCADE,
    content TEXT NOT NULL
);

-- Duplicate tracking: populated only when 2+ files share a content_hash.
CREATE TABLE IF NOT EXISTS duplicates (
    content_hash TEXT    NOT NULL,
    file_id      INTEGER NOT NULL REFERENCES files(id) ON DELETE CASCADE,
    PRIMARY KEY (content_hash, file_id)
);

CREATE INDEX IF NOT EXISTS idx_duplicates_hash ON duplicates(content_hash);

-- Contentless FTS5 index.
-- rowid = file_id * MAX_LINES_PER_FILE + line_number
-- MAX_LINES_PER_FILE = 1_000_000 (hardcoded; see db/constants.rs)
CREATE VIRTUAL TABLE IF NOT EXISTS lines_fts USING fts5(
    content,
    content  = '',
    tokenize = 'trigram'
);

CREATE TABLE IF NOT EXISTS indexing_errors (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    path       TEXT    NOT NULL UNIQUE,
    error      TEXT    NOT NULL,
    first_seen INTEGER NOT NULL,
    last_seen  INTEGER NOT NULL,
    count      INTEGER NOT NULL DEFAULT 1
);

CREATE TABLE IF NOT EXISTS scan_history (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    scanned_at  INTEGER NOT NULL,
    total_files INTEGER NOT NULL,
    total_size  INTEGER NOT NULL,
    by_kind     TEXT    NOT NULL
);

CREATE TABLE IF NOT EXISTS activity_log (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    occurred_at INTEGER NOT NULL,
    action      TEXT    NOT NULL,
    path        TEXT    NOT NULL,
    new_path    TEXT
);

CREATE INDEX IF NOT EXISTS idx_activity_log_occurred_at
    ON activity_log(occurred_at DESC);
```

---

## New Constants

New file `crates/server/src/db/constants.rs`:

```rust
/// FTS5 rowid multiplier: rowid = file_id * MAX_LINES_PER_FILE + line_number.
/// Hardcoded — must not change after any index has been built.
/// A file with >= MAX_LINES_PER_FILE lines has its excess lines dropped from
/// the FTS index (logged as a warning).
pub const MAX_LINES_PER_FILE: i64 = 1_000_000;

pub fn encode_fts_rowid(file_id: i64, line_number: i64) -> i64 {
    debug_assert!(file_id < i64::MAX / MAX_LINES_PER_FILE,
        "file_id {file_id} would overflow FTS rowid");
    debug_assert!(line_number < MAX_LINES_PER_FILE,
        "line_number {line_number} would overflow FTS rowid");
    file_id * MAX_LINES_PER_FILE + line_number
}

pub fn decode_fts_rowid(rowid: i64) -> (i64, i64) {
    (rowid / MAX_LINES_PER_FILE, rowid % MAX_LINES_PER_FILE)
}
```

---

## Implementation

### Step 1: New schema file + version bump

- Create `crates/server/src/schema_v3.sql`
- Bump `SCHEMA_VERSION` in `db/mod.rs`
- Switch `include_str!("../schema_v2.sql")` to `include_str!("../schema_v3.sql")`
- Add `mod constants` to `db/mod.rs`; export `encode_fts_rowid` / `decode_fts_rowid`

### Step 2: archive.rs changes

The `Chunk` struct loses `file_id` / `file_path`, gains `block_id`. The
`chunk_lines()` function signature changes from `(file_id, file_path, lines)` to
`(block_id, lines)`. `ChunkResult` carries `start_line`/`end_line` per chunk
instead of per-line offsets.

### Step 3: pipeline.rs — write path

1. **Remove** the `canonical_file_id` dedup block (lines 71–109 in current file)
2. Upsert `files` row (simpler — no `canonical_file_id` column)
3. If `content_hash` is set: `INSERT OR IGNORE INTO content_blocks(content_hash)
   VALUES(?)` — ensures the block exists; `block_id` is resolved at read/archive
   time via `JOIN content_blocks ON content_hash`
4. Call `upsert_duplicate_tracking(tx, hash, file_id)` (new helper, see below)
5. Inline path: write `file_content`, insert FTS rows using
   `encode_fts_rowid(file_id, line_number)`
6. Deferred path: insert FTS rows immediately; `content_chunks` written in phase 2

**`upsert_duplicate_tracking`:**
```
1. SELECT id FROM files WHERE content_hash = ? AND id != file_id
2. If no rows: no action (file is unique)
3. If any rows: INSERT OR IGNORE INTO duplicates for each existing id + for file_id
```

### Step 4: archive_batch.rs — archive write path

1. Lookup `block_id` via `JOIN content_blocks ON content_hash` for this file's hash
2. Check if `content_chunks` already has rows for `block_id` — if yes, skip write
   (another file with same hash already archived the content)
3. If new: chunk lines, write to ZIP, upsert `content_archives`, insert
   `content_chunks(block_id, chunk_number, archive_id, start_line, end_line)`
4. No `UPDATE lines` — the `lines` table is gone

### Step 5: db/search.rs — FTS query changes

Remove the `JOIN lines` from all FTS queries. Decode `file_id` and `line_number`
from the FTS5 rowid directly:

```sql
SELECT f.path, f.kind, f.id, f.mtime, f.size,
       (lines_fts.rowid % 1000000) AS line_number
FROM lines_fts
JOIN files f ON f.id = (lines_fts.rowid / 1000000)
WHERE lines_fts MATCH ?1
LIMIT ?2
```

`RawRow` drops `chunk_archive`, `chunk_name`, `line_offset` fields. Content
retrieval moves entirely to the new `read_chunk_for_file` helper (see below).

Replace `fetch_aliases_for_canonical_ids` with `fetch_duplicates_for_file_ids`
that queries the `duplicates` table.

### Step 6: db/mod.rs — content read helper

New `read_chunk_for_file(conn, archive_mgr, file_id, line_number)`:

1. Check `file_content` (inline) → split on newline, index by position
2. Lookup `block_id` via `JOIN files → content_blocks ON content_hash`
3. Query `content_chunks WHERE block_id = ? AND start_line <= ? AND end_line >= ?`
4. Lookup archive name from `content_archives`
5. Derive `chunk_name = format!("{}.chunk{}", block_id, chunk_number)`
6. Read ZIP chunk; index by `line_number - start_line`

Cache key: `(archive_name, chunk_name)` — compatible with existing
`HashMap<(String, String), Vec<String>>` per-request cache.

### Step 7: Deletion + duplicate cleanup

After `DELETE FROM files` (which cascades to `file_block`, `duplicates`,
`file_content`), clean up singleton duplicate entries inside the same transaction:

```sql
DELETE FROM duplicates
WHERE content_hash IN (
    SELECT content_hash FROM duplicates
    GROUP BY content_hash HAVING COUNT(*) = 1
)
```

### Step 8: Compaction / GC

Replace the `SELECT DISTINCT chunk_archive, chunk_name FROM lines` live-chunk query
with:

```sql
SELECT ca.name,
       cb.id || '.chunk' || cc.chunk_number AS chunk_name
FROM content_chunks cc
JOIN content_blocks cb ON cb.id = cc.block_id
JOIN content_archives ca ON ca.id = cc.archive_id
```

Add GC sweeps:

```sql
-- Orphaned content_blocks (last referencing file deleted)
DELETE FROM content_blocks
WHERE id NOT IN (SELECT block_id FROM file_block);

-- Orphaned content_chunks (content_block was deleted; cascade handles this,
-- but explicit cleanup ensures no dangling archive references remain)

-- Singleton duplicates
DELETE FROM duplicates
WHERE content_hash IN (
    SELECT content_hash FROM duplicates
    GROUP BY content_hash HAVING COUNT(*) = 1
);
```

---

## Files Changed

| File | Change |
|------|--------|
| `crates/server/src/schema_v3.sql` | **New** — full v3 schema |
| `crates/server/src/db/constants.rs` | **New** — `MAX_LINES_PER_FILE`, encode/decode helpers |
| `crates/server/src/db/mod.rs` | `SCHEMA_VERSION` bump; include schema_v3; add constants; new `read_chunk_for_file`; delete duplicate cleanup; remove old chunk-ref collector |
| `crates/server/src/db/search.rs` | Remove `lines` JOIN; decode rowid; replace `fetch_aliases_for_canonical_ids` |
| `crates/server/src/worker/pipeline.rs` | Remove canonical dedup; add content_block upsert; add `upsert_duplicate_tracking`; rewrite FTS insert; remove `lines` INSERT |
| `crates/server/src/worker/archive_batch.rs` | Check `content_chunks` instead of `lines.chunk_archive`; upsert `content_archives`; insert `content_chunks` instead of UPDATE lines |
| `crates/server/src/archive.rs` | `Chunk` / `ChunkResult` structs; `chunk_lines()` signature |
| `crates/server/src/compaction.rs` | New live-chunk query; orphaned block / singleton duplicate GC |
| `crates/server/src/routes/search.rs` | Duplicate path population |
| `crates/common/src/api.rs` | Remove `canonical_file_id`; add `duplicate_paths: Vec<String>` to `SearchResult` (or stub empty) |
| `crates/server/tests/*.rs` | Update test helpers and assertions throughout |

---

## Testing Strategy

### Unit tests (pipeline.rs)

- `content_hash_registers_duplicate_pair` — index A then B with same hash; both in `duplicates`
- `duplicate_cleanup_on_delete` — delete A; B removed from `duplicates` (singleton)
- `no_duplicate_entry_for_unique_hash` — unique hash; no `duplicates` row
- `fts_rowid_encoding_roundtrip` — encode/decode several `(file_id, line_number)` pairs
- `deferred_storage_inserts_content_block` — verify `content_blocks` row exists for the file's content_hash after indexing

### Unit tests (archive_batch.rs)

- `content_chunks_written_after_archive_phase`
- `dedup_block_skipped_if_already_archived` — two files same hash; second is no-op
- `content_archives_upserted`

### Unit tests (db/search.rs)

- `fts_candidates_decodes_file_id_from_rowid`
- `search_excludes_deleted_file_fts_orphans` — delete file; verify JOIN filter works

### Integration tests

- `test_schema_v3_basic_index_and_search`
- `test_duplicate_detection_via_junction_table`
- `test_content_block_sharing` — two files same hash; one set of `content_chunks`

---

## Migration Steps

1. Stop `find-server`
2. Delete `{data_dir}/sources/` (all `.db` files and ZIP archives)
3. Start new `find-server`
4. Run `find-scan --force` on all sources

---

## Potential Challenges

**FTS rowid overflow** — `file_id * 1_000_000` overflows `i64` at `file_id > 9.2 ×
10¹²`. `debug_assert!` at insert time. In practice AUTOINCREMENT won't approach
this.

**Lines exceeding MAX_LINES_PER_FILE** — Extra lines are silently dropped from FTS
with a `tracing::warn!`. The `max_content_kb` limit already makes this unlikely but
the hard limit must be enforced regardless of config.

**Archive batch race — same hash, two files** — Use `INSERT OR IGNORE INTO
content_chunks`; the existing `source_lock` serialises within a source. Cross-source
collisions are idempotent: writing the same chunk twice is harmless because
`append_to_zip_with_comment` removes the old entry before writing.

**Test helper scope** — Every test that seeds the `lines` table (many in
pipeline.rs, search.rs, archive_batch.rs, integration tests) must be updated. This
is the highest-effort part of the implementation.

## Implementation Order

1. `schema_v3.sql` + `constants.rs` + `db/mod.rs` schema version bump
2. `archive.rs` struct changes + all callers
3. `pipeline.rs` write path (largest change; run tests here before continuing)
4. `archive_batch.rs`
5. `db/search.rs` + `db/mod.rs` read helpers
6. `compaction.rs`
7. `routes/search.rs` duplicate population
8. `common/src/api.rs` type changes
9. All tests

---

## Detailed File Walkthroughs

### archive.rs — exact current signatures and what changes

**Current structs:**
```rust
pub struct Chunk {
    pub file_id: i64,
    pub file_path: String,  // used only as ZIP entry comment
    pub chunk_number: usize,
    pub content: String,
}

pub struct LineMapping {
    pub line_number: usize,
    pub chunk_number: usize,
    pub offset_in_chunk: usize,  // 0-indexed position within the chunk's lines
}

pub struct ChunkResult {
    pub chunks: Vec<Chunk>,
    pub line_mappings: Vec<LineMapping>,
}

pub fn chunk_lines(file_id: i64, file_path: &str, lines: &[(usize, String)]) -> ChunkResult
```

**v3 replacements:**
```rust
pub struct Chunk {
    pub block_id: i64,      // replaces file_id
    pub chunk_number: usize,
    pub content: String,
    // file_path/comment dropped; ZIP entry has no comment in v3
}

pub struct ChunkRange {
    pub chunk_number: usize,
    pub start_line: usize,  // first line_number stored in this chunk
    pub end_line: usize,    // last line_number stored (inclusive)
}

pub struct ChunkResult {
    pub chunks: Vec<Chunk>,
    pub ranges: Vec<ChunkRange>,  // one per chunk, replaces per-line LineMapping
}

pub fn chunk_lines(block_id: i64, lines: &[(usize, String)]) -> ChunkResult
```

**Chunk name changes:** `append_chunks` currently produces `chunk_name =
format!("{}.{}", chunk.file_id, chunk.chunk_number)`. In v3 this becomes
`format!("{}.{}", chunk.block_id, chunk.chunk_number)`. No other change to
`append_chunks`, `remove_chunks`, or `read_chunk`.

**ZIP entry comment:** currently stores `chunk.file_path` as the ZIP entry
comment (useful for debugging). In v3 this can be dropped (empty string) since
the block_id in the chunk name is sufficient for debugging.

**`LineMapping` to `ChunkRange`:** the v1 `offset_in_chunk` is computed at read
time as `line_number - start_line`. `ChunkRange` only needs to record the line
range, not each individual line.

---

### archive_batch.rs — exact current flow and what changes

**Current flow (per source, per file):**

```
1. SELECT id FROM files WHERE path = ?             → file_id (or skip if missing)
2. SELECT COUNT(*) FROM lines
   WHERE file_id = ? AND chunk_archive IS NOT NULL  → already_archived check
3. SELECT COUNT(*) FROM file_content WHERE file_id = ? → inline check
4. SELECT COUNT(*) FROM lines WHERE file_id = ?    → has_lines check
5. line_data = file.lines (content from the .gz request)
6. chunk_lines(file_id, path, line_data)           → ChunkResult
7. archive_mgr.append_chunks(chunks)               → Vec<ChunkRef>
8. [ZIP I/O happens here]
9. UPDATE lines SET chunk_archive=?, chunk_name=?, line_offset_in_chunk=?
   WHERE file_id=? AND line_number=?               → one UPDATE per line
```

**v3 flow (per source, per file):**

```
1. SELECT id, content_hash FROM files WHERE path = ?      → file_id + hash
2. SELECT id FROM content_blocks WHERE content_hash = ?   → block_id
3. SELECT COUNT(*) FROM content_chunks WHERE block_id = ? → already_archived check
4. SELECT COUNT(*) FROM file_content WHERE file_id = ?    → inline check
   (if inline, skip — no ZIP needed)
5. line_data = file.lines (content from the .gz request, same as now)
6. chunk_lines(block_id, line_data)                       → ChunkResult
7. archive_mgr.append_chunks(chunks)                      → Vec<ChunkRef>
8. [ZIP I/O happens here]
9. INSERT OR IGNORE INTO content_archives(name) VALUES(?)
   SELECT id FROM content_archives WHERE name = ?         → archive_id per ChunkRef
10. INSERT INTO content_chunks(block_id, chunk_number, archive_id, start_line, end_line)
    VALUES(?, ?, ?, ?, ?)                                  → one INSERT per chunk (not per line)
```

The `line_data` sourced from `file.lines` in the request is the same in v3 —
the archive phase still reads content from the `.gz` payload rather than
re-reading from the database. This is unchanged.

The `UPDATE lines` batch (currently one UPDATE per line) is replaced by one
`INSERT INTO content_chunks` per chunk (~25× fewer writes).

**`seed_db()` test helper (archive_batch tests) — must be rewritten:**

Current:
```rust
fn seed_db(data_dir, source, path) -> (Connection, i64) {
    // INSERT INTO files (..., canonical_file_id) VALUES (...)
    // INSERT INTO lines (file_id, line_number, chunk_archive=NULL, ...)
    // INSERT INTO lines_fts(rowid, content) VALUES (last_insert_rowid(), ...)
    // returns (conn, file_id)
}
```

v3 replacement:
```rust
fn seed_db(data_dir, source, path, content_hash) -> (Connection, i64, i64) {
    // INSERT INTO files (...) VALUES (...)  -- no canonical_file_id
    // INSERT OR IGNORE INTO content_blocks(content_hash) VALUES(?)
    // SELECT id FROM content_blocks WHERE content_hash = ? → block_id
    // INSERT INTO lines_fts(rowid, content) VALUES(encode_fts_rowid(file_id, 0), path)
    // INSERT INTO lines_fts(rowid, content) VALUES(encode_fts_rowid(file_id, 1), content)
    // returns (conn, file_id, block_id)
}

fn read_chunk_ranges(conn, block_id) -> Vec<(i64, i64)>  // (start_line, end_line) per chunk
```

**`read_line_refs()` test helper — replaced:**

Current reads `(chunk_archive, chunk_name) FROM lines WHERE file_id = ?`.
Replacement reads `(start_line, end_line, archive_id) FROM content_chunks WHERE block_id = ?`.

---

### compaction.rs — exact current flow and what changes

**Current `build_referenced_set`:**
```rust
// Opens each *.db in sources/, runs:
db::collect_all_chunk_refs(&conn)
// which executes:
"SELECT DISTINCT chunk_archive, chunk_name FROM lines WHERE chunk_archive IS NOT NULL"
// Returns Vec<ChunkRef { archive_name, chunk_name }>
// Inserts (archive_name, chunk_name) pairs into a HashSet
```

**v3 replacement:**
```rust
// Same loop over *.db files, but query changes to:
"SELECT ca.name, cb.id || '.' || cc.chunk_number
 FROM content_chunks cc
 JOIN content_blocks cb ON cb.id = cc.block_id
 JOIN content_archives ca ON ca.id = cc.archive_id"
// This produces the same (archive_name, chunk_name) pairs
// chunk_name format: "{block_id}.{chunk_number}" — matches what append_chunks writes
```

Everything downstream (`scan_wasted_space`, `compact_archives`,
`rewrite_without`) is **unchanged** — they operate on ZIP files using the
`(archive_name, chunk_name)` set and don't touch the database schema directly.

**`collect_all_chunk_refs` in `db/mod.rs`** — this function is called from
both `compaction.rs` and `routes/admin.rs` (source-delete route). Update both
call sites when replacing the query.

**`seed_db_with_chunk_ref()` compaction test helper — must be rewritten:**

Current:
```rust
fn seed_db_with_chunk_ref(conn, chunk_archive, chunk_name) {
    // INSERT INTO files ...
    // INSERT INTO lines (file_id, 1, chunk_archive, chunk_name, 0)
    // INSERT INTO lines_fts(rowid, content) VALUES (last_insert_rowid(), 'hello')
}
```

v3 replacement:
```rust
fn seed_db_with_chunk_ref(conn, archive_name, block_id, chunk_number, start_line, end_line) {
    // INSERT INTO files ...
    // INSERT OR IGNORE INTO content_blocks(id, content_hash) VALUES(block_id, 'hash')
    // INSERT OR IGNORE INTO content_archives(name) VALUES(archive_name)
    // SELECT id FROM content_archives WHERE name = ? → archive_id
    // INSERT INTO content_chunks(block_id, chunk_number, archive_id, start_line, end_line)
    // INSERT INTO lines_fts(rowid, content) VALUES(encode_fts_rowid(file_id, 1), 'hello')
}
```

The compaction test ZIP entries currently use names like `"test.txt.chunk0.txt"`.
In v3 they should use `"42.0"` format (block_id=42, chunk_number=0) to match
what the live code writes.

---

### FTS orphan handling — the re-index problem

**Current behaviour:** when a file is re-indexed, `pipeline.rs` runs `DELETE
FROM lines WHERE file_id = ?`. This removes all `lines` rows, making their
`id` values (the FTS5 rowids) orphaned in `lines_fts`. The search query
`JOIN lines l ON l.id = lines_fts.rowid` returns no rows for orphaned rowids —
they are silently filtered. The FTS index accumulates dead entries over time
but correctness is maintained by the JOIN.

**v3 problem:** the search query becomes `JOIN files f ON f.id = (rowid /
MAX_LINES_PER_FILE)`. The file still exists (same `file_id`), so old FTS
entries from a previous index **survive the JOIN filter**. If a file is
re-indexed with different content, old line numbers (e.g. lines 50–100 that
no longer exist) would still produce FTS matches. Attempting to fetch content
for those line numbers via `content_chunks` would either return nothing (line
range gone) or wrong content.

**Solution: delete old FTS entries on re-index.**

FTS5 contentless delete requires both rowid AND the original content:
```sql
INSERT INTO lines_fts(lines_fts, rowid, content) VALUES('delete', ?, ?)
```

This is already done in `db/mod.rs` (e.g. line 573) for the alias-promotion
path. For re-indexing, we need the old content to delete old FTS entries.

**Practical approach for `pipeline.rs` phase 1:**

When `Phase1Outcome::Modified` is detected (file already existed), fetch the
old line count from a new `line_count INTEGER` column on `files`, then delete
the old FTS range by reading old content from ZIP:

```
if modified:
    old_line_count = files.line_count for this file_id
    for line_number in 0..old_line_count:
        old_rowid = encode_fts_rowid(file_id, line_number)
        old_content = read_chunk_for_file(file_id, line_number)  // reads ZIP
        tx.execute("INSERT INTO lines_fts(lines_fts, rowid, content)
                    VALUES('delete', ?, ?)", [old_rowid, old_content])?;
```

This is expensive (reads from ZIP) but correct. To avoid this cost, add a
`line_count INTEGER` column to `files` and use a **range-delete trick**:
delete FTS rowids for `file_id` without providing content, then do a periodic
GC rebuild of the FTS index. For a trigram index, accumulating stale trigrams
causes false positive search hits that are then filtered at the content
retrieval step — degraded precision but no incorrect results.

**Recommended approach:** accept stale FTS entries for the initial
implementation (same as current behaviour). Add post-retrieval filtering: if
`content_chunks` has no entry covering a returned line number, skip that
result. Add a TODO for proper FTS deletion (reading old ZIP content) as a
follow-up. Add `line_count` to `files` table now so the periodic GC rebuild
can determine the range to wipe.

Add `line_count INTEGER` to `files` schema. Update it in `pipeline.rs` at
INSERT/UPDATE time. During compaction GC, rebuild FTS for any file whose
`line_count` changed significantly.

---

### `pending_chunk_removes` table — drop it

The `pending_chunk_removes` table exists in `schema_v2.sql` but is **not used
anywhere in the current Rust code** (no INSERT, SELECT, or reference to it in
any `.rs` file). It was presumably planned as a two-phase chunk removal queue
but was never implemented. Drop it from `schema_v3.sql` — no code changes
needed.

---

### db/mod.rs `read_chunk_lines_zip` — current signature

The current read helpers used by search/context/file routes:

```rust
fn read_chunk_lines_zip(
    cache: &mut HashMap<(String, String), Vec<String>>,
    archive_mgr: &ArchiveManager,
    archive_name: &str,
    chunk_name: &str,
) -> Vec<String>
// Returns all lines in the chunk as Vec<String>
// Cache key: (archive_name, chunk_name)

fn read_chunk_lines(
    cache: &mut HashMap<(String, String), Vec<String>>,
    conn: &Connection,
    archive_mgr: &ArchiveManager,
    file_id: i64,
    line_number: i64,
) -> Option<String>
// Looks up (chunk_archive, chunk_name, line_offset_in_chunk) from lines table
// then calls read_chunk_lines_zip, indexes into result
```

**v3 replacement `read_chunk_for_file`:**

```rust
pub fn read_chunk_for_file(
    cache: &mut HashMap<(String, String), Vec<String>>,
    conn: &Connection,
    archive_mgr: &ArchiveManager,
    file_id: i64,
    line_number: i64,
) -> Option<String> {
    // 1. Check file_content (inline) → split on '\n', index by line_number
    // 2. SELECT cb.id, cc.chunk_number, ca.name, cc.start_line
    //    FROM files f
    //    JOIN content_blocks cb ON cb.content_hash = f.content_hash
    //    JOIN content_chunks cc ON cc.block_id = cb.id
    //      AND cc.start_line <= line_number AND cc.end_line >= line_number
    //    JOIN content_archives ca ON ca.id = cc.archive_id
    //    WHERE f.id = file_id
    // 3. chunk_name = format!("{}.{}", block_id, chunk_number)
    // 4. cache lookup → read_chunk_lines_zip (unchanged)
    // 5. index by (line_number - start_line)
}
```

The cache key `(archive_name, chunk_name)` and `read_chunk_lines_zip` itself
are **unchanged**. Only the lookup of which archive/chunk to read changes.

**All call sites** of `read_chunk_lines` in `db/mod.rs`, `db/search.rs`, and
route handlers become calls to `read_chunk_for_file` with the same cache.

---

### db/search.rs `RawRow` — current struct and what changes

```rust
// Current (approximate — uses anonymous tuple in practice)
struct RawRow {
    file_id: i64,
    path: String,
    kind: FileKind,
    mtime: i64,
    size: Option<i64>,
    line_number: i64,
    chunk_archive: Option<String>,   // REMOVED in v3
    chunk_name: Option<String>,      // REMOVED in v3
    line_offset_in_chunk: i64,       // REMOVED in v3
}
```

In v3, `chunk_archive`, `chunk_name`, and `line_offset_in_chunk` are dropped
from the SQL result. Content is fetched post-query via `read_chunk_for_file`
using only `(file_id, line_number)`.

The five FTS query sites in `db/search.rs` all have the pattern:
```sql
FROM lines_fts
JOIN lines l ON l.id = lines_fts.rowid
JOIN files f ON f.id = l.file_id
WHERE lines_fts MATCH ...
```
Each becomes:
```sql
FROM lines_fts
JOIN files f ON f.id = (lines_fts.rowid / 1000000)
WHERE lines_fts MATCH ...
```
With `line_number = lines_fts.rowid % 1000000` computed in the SELECT.
