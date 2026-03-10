PRAGMA journal_mode=WAL;
PRAGMA foreign_keys=ON;

CREATE TABLE IF NOT EXISTS meta (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS files (
    id                INTEGER PRIMARY KEY AUTOINCREMENT,
    path              TEXT    NOT NULL UNIQUE,
    mtime             INTEGER NOT NULL,
    size              INTEGER,
    kind              TEXT    NOT NULL DEFAULT 'text',
    indexed_at        INTEGER,
    extract_ms        INTEGER,
    content_hash      TEXT,
    canonical_file_id INTEGER REFERENCES files(id) ON DELETE SET NULL,
    scanner_version   INTEGER NOT NULL DEFAULT 0
);

-- Inner archive members use composite paths: "archive.zip::member.txt"
-- No separate column needed; path IS the identifier.

CREATE INDEX IF NOT EXISTS files_content_hash ON files(content_hash)
    WHERE content_hash IS NOT NULL;
CREATE INDEX IF NOT EXISTS files_canonical ON files(canonical_file_id)
    WHERE canonical_file_id IS NOT NULL;
CREATE INDEX IF NOT EXISTS idx_files_mtime ON files(mtime);

CREATE TABLE IF NOT EXISTS lines (
    id                   INTEGER PRIMARY KEY AUTOINCREMENT,
    file_id              INTEGER NOT NULL REFERENCES files(id) ON DELETE CASCADE,
    line_number          INTEGER NOT NULL,
    chunk_archive        TEXT,    -- NULL for inline-stored files; "content_00001.zip" for ZIP-stored
    chunk_name           TEXT,    -- NULL for inline-stored files; "{file_id}.{chunk_number}" for ZIP-stored
    line_offset_in_chunk INTEGER NOT NULL   -- line index within chunk (0-indexed)
);

CREATE INDEX IF NOT EXISTS lines_file_id   ON lines(file_id);
CREATE INDEX IF NOT EXISTS lines_file_line ON lines(file_id, line_number);
CREATE INDEX IF NOT EXISTS lines_chunk     ON lines(chunk_archive, chunk_name)
    WHERE chunk_archive IS NOT NULL;

-- Inline content for small files (below inline_threshold_bytes server setting).
-- Only populated when chunk_archive/chunk_name are NULL in the lines table.
-- Kept separate from `files` to avoid row-width bloat on the heavily-scanned files table.
CREATE TABLE IF NOT EXISTS file_content (
    file_id INTEGER PRIMARY KEY REFERENCES files(id) ON DELETE CASCADE,
    content TEXT NOT NULL
);

-- FTS5 table with content='' (no content storage, index only)
CREATE VIRTUAL TABLE IF NOT EXISTS lines_fts USING fts5(
    content,
    content       = '',  -- Don't store content, only build index
    tokenize      = 'trigram'
);

-- Note: No triggers - FTS5 population is managed manually by worker
-- Worker will INSERT INTO lines_fts(rowid, content) after reading from ZIP

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

-- Chunk references queued for removal by the archive thread.
-- Phase 1 (indexing thread) inserts rows here; phase 2 (archive thread) drains them.
CREATE TABLE IF NOT EXISTS pending_chunk_removes (
    id           INTEGER PRIMARY KEY,
    archive_name TEXT NOT NULL,
    chunk_name   TEXT NOT NULL
);
