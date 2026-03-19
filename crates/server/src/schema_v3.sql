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
    scanner_version  INTEGER NOT NULL DEFAULT 0,
    line_count       INTEGER
    -- canonical_file_id REMOVED in v3
);

-- Inner archive members use composite paths: "archive.zip::member.txt"
-- No separate column needed; path IS the identifier.

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
-- chunk_name in ZIP = "{block_id}.{chunk_number}"
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

CREATE INDEX IF NOT EXISTS idx_content_chunks_block_start
    ON content_chunks(block_id, start_line);

-- Inline content for small files (below inline_threshold_bytes server setting).
-- Kept separate from `files` to avoid row-width bloat on the heavily-scanned files table.
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

-- Note: No triggers - FTS5 population is managed manually by worker

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

-- Append-only activity log: records every add, modify, rename, and delete
-- event for outer files (no '::' composite paths).  Used by GET /api/v1/recent
-- so that deleted and renamed files remain visible for a configurable
-- retention window (server.activity_log_max_entries, default 10000).
CREATE TABLE IF NOT EXISTS activity_log (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    occurred_at INTEGER NOT NULL,
    action      TEXT    NOT NULL,  -- 'added', 'modified', 'deleted', 'renamed'
    path        TEXT    NOT NULL,  -- for 'renamed': the old path
    new_path    TEXT               -- only for 'renamed': the new path; NULL otherwise
);
CREATE INDEX IF NOT EXISTS idx_activity_log_occurred_at
    ON activity_log(occurred_at DESC);
