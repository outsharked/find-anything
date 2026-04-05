-- Schema v4: same as v3 but without content_blocks, content_archives,
-- content_chunks and their indexes.  Chunk metadata now lives in
-- data_dir/content.db, owned by find-content-store.
--
-- v14: file_content table dropped; files.content_hash renamed to files.file_hash.

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
    file_hash        TEXT,
    scanner_version  INTEGER NOT NULL DEFAULT 0,
    line_count       INTEGER
);

-- Inner archive members use composite paths: "archive.zip::member.txt"
-- No separate column needed; path IS the identifier.

CREATE INDEX IF NOT EXISTS files_file_hash ON files(file_hash)
    WHERE file_hash IS NOT NULL;
CREATE INDEX IF NOT EXISTS idx_files_mtime ON files(mtime);

-- Duplicate tracking: populated only when 2+ files share a file_hash.
CREATE TABLE IF NOT EXISTS duplicates (
    file_hash TEXT    NOT NULL,
    file_id   INTEGER NOT NULL REFERENCES files(id) ON DELETE CASCADE,
    PRIMARY KEY (file_hash, file_id)
);

CREATE INDEX IF NOT EXISTS idx_duplicates_hash   ON duplicates(file_hash);
CREATE INDEX IF NOT EXISTS idx_duplicates_file_id ON duplicates(file_id);

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
