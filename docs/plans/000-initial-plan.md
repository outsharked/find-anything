# find-anything — Architecture & Implementation Plan

## Purpose

A distributed full-content indexing and search system. Source machines run lightweight
clients that index their file trees (text files, archives, PDFs, media metadata) and
submit the data to a central server. Any number of machines can be indexed. A CLI tool
and a web UI query the central index, returning ranked fuzzy-search results with
in-context line previews.

---

## Decisions & Constraints

| Decision | Choice | Rationale |
|----------|--------|-----------|
| Storage | One SQLite DB per source | Isolation; easy backup/drop; avoids cross-source schema conflicts |
| Content | Full content indexed | Context retrieval requires stored lines; enables real search |
| Fuzzy engine | nucleo-matcher (same as text-scan) | Best-in-class, same dependency already in use |
| Auth | One bearer token per installation | Internal tool; simplicity wins |
| Concurrency (inotify during rescan) | Non-issue | Both paths upsert; last-writer-wins is correct |
| Archive content | Yes, index it | Already have the machinery in text-scan |
| Exclusions | Configurable glob patterns | node_modules etc. must be excluded |
| PDF extraction | pdf-extract crate (pure Rust) | No external dependency |
| Image metadata | EXIF via kamadak-exif | Pure Rust, well-maintained |
| Audio metadata | id3 / metaflac / mp4ameta | Format-specific crates |
| OCR | Post-MVP | tesseract-rs (wraps tesseract); opt-in, expensive |

---

## System Topology

```
┌─────────────────────────────────────────────────────────────┐
│                       Central Server                         │
│                                                              │
│   ┌────────────────┐       ┌─────────────────────────────┐  │
│   │  find-server   │◄─────►│  SQLite per source          │  │
│   │  (axum REST)   │       │  ~/.local/share/find-       │  │
│   └───────┬────────┘       │  anything/sources/{name}.db │  │
│           │                └─────────────────────────────┘  │
│    serves │                                                  │
│   ┌───────▼────────┐   ┌──────────────────────────────────┐ │
│   │  find (CLI)    │   │  find-web (SvelteKit)            │ │
│   └────────────────┘   └──────────────────────────────────┘ │
└────────────────────────────┬────────────────────────────────┘
                             │ HTTP + bearer token
               ┌─────────────┴──────────────┐
               │                            │
       ┌───────▼───────┐            ┌───────▼───────┐
       │  Machine A    │            │  Machine B    │
       │               │            │               │
       │  find-scan    │            │  find-scan    │  systemd timer
       │  (nightly)    │            │  (nightly)    │  (OnCalendar)
       │               │            │               │
       │  find-watch   │            │  find-watch   │  systemd service
       │  (inotify)    │            │  (inotify)    │  (always running)
       └───────────────┘            └───────────────┘
```

---

## Repository Structure

```
find-anything/
├── PLAN.md
├── README.md
├── Cargo.toml              ← workspace
├── crates/
│   ├── common/             ← shared: API types, fuzzy scorer, extractors, config
│   │   └── src/
│   │       ├── lib.rs
│   │       ├── api.rs      ← request/response types (serde)
│   │       ├── config.rs   ← client config schema (toml)
│   │       ├── fuzzy.rs    ← nucleo wrapper
│   │       ├── extract/
│   │       │   ├── mod.rs  ← Extractor trait + dispatch
│   │       │   ├── text.rs ← plain text / code files
│   │       │   ├── archive.rs ← zip/tar/gz/bz2/xz/7z (from text-scan)
│   │       │   ├── pdf.rs  ← pdf-extract
│   │       │   ├── image.rs ← EXIF via kamadak-exif
│   │       │   └── audio.rs ← id3 / metaflac / mp4ameta
│   ├── server/             ← find-server binary
│   │   └── src/
│   │       ├── main.rs
│   │       ├── db.rs       ← SQLite schema + all queries
│   │       ├── routes/
│   │       │   ├── mod.rs
│   │       │   ├── index.rs   ← PUT/DELETE file data endpoints
│   │       │   ├── search.rs  ← GET /search
│   │       │   └── context.rs ← GET /context
│   │       └── search.rs   ← FTS5 pre-filter + nucleo re-score
│   └── client/             ← find-scan + find-watch binaries
│       └── src/
│           ├── main.rs     ← clap dispatcher: scan | watch | register
│           ├── scan.rs     ← walk + mtime diff + extract + submit
│           ├── watch.rs    ← notify watcher → incremental submit
│           └── api.rs      ← reqwest client for server API
└── web/                    ← SvelteKit web UI
    ├── package.json
    ├── svelte.config.js
    └── src/
        ├── routes/
        │   ├── +page.svelte        ← main search page
        │   └── api/search/+server.ts ← proxy to find-server (adds token)
        └── lib/
            ├── SearchBox.svelte
            ├── ResultList.svelte
            └── ContextPanel.svelte
```

---

## Database Schema (per-source SQLite)

Each source gets its own DB file at:
`{data_dir}/sources/{source-name}.db`

```sql
-- Source metadata (single-row config table)
CREATE TABLE meta (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
);
-- Keys: source_name, base_path, token_hash, last_scan (unix timestamp)

-- One row per indexed file (or archive)
CREATE TABLE files (
    id       INTEGER PRIMARY KEY,
    path     TEXT    NOT NULL UNIQUE,  -- relative to base_path
    mtime    INTEGER NOT NULL,         -- unix timestamp
    size     INTEGER NOT NULL,
    kind     TEXT    NOT NULL          -- 'text' | 'pdf' | 'archive' | 'image' | 'audio'
);

-- Every indexed line (text files, archive entries, PDF pages, metadata fields)
CREATE TABLE lines (
    id           INTEGER PRIMARY KEY,
    file_id      INTEGER NOT NULL REFERENCES files(id) ON DELETE CASCADE,
    archive_path TEXT,                 -- NULL for non-archive files
                                       -- inner entry path for archive content
    line_number  INTEGER NOT NULL,     -- 1-based; page number for PDFs;
                                       -- 0 for metadata pseudo-lines
    content      TEXT    NOT NULL
);

-- FTS5 trigram index over content — enables fast pre-filtering
CREATE VIRTUAL TABLE lines_fts USING fts5(
    content,
    content     = 'lines',
    content_rowid = 'id',
    tokenize    = 'trigram'
);

-- Keep FTS5 in sync
CREATE TRIGGER lines_ai AFTER INSERT ON lines BEGIN
    INSERT INTO lines_fts(rowid, content) VALUES (new.id, new.content);
END;
CREATE TRIGGER lines_ad AFTER DELETE ON lines BEGIN
    INSERT INTO lines_fts(lines_fts, rowid, content)
    VALUES ('delete', old.id, old.content);
END;
CREATE TRIGGER lines_au AFTER UPDATE OF content ON lines BEGIN
    INSERT INTO lines_fts(lines_fts, rowid, content)
    VALUES ('delete', old.id, old.content);
    INSERT INTO lines_fts(rowid, content) VALUES (new.id, new.content);
END;

CREATE INDEX lines_file_id ON lines(file_id);
CREATE INDEX lines_file_line ON lines(file_id, archive_path, line_number);
```

### Notes on special content types

**PDF files**: `pdf-extract` gives text per-page. Split by `\n`, store with
`line_number` = actual line within the extracted page text (reset per page is fine).
The `archive_path` column is repurposed as the page number string (`"page:3"`) so
context queries can distinguish pages cleanly.

**Image EXIF**: Each EXIF tag becomes one line:
```
[EXIF:Make] Canon
[EXIF:Model] EOS R5
[EXIF:DateTimeOriginal] 2024:01:15 14:30:22
[EXIF:GPSLatitude] 37.774900
[EXIF:ImageDescription] Sunset over the bay
```
`line_number = 0` for all metadata pseudo-lines (no meaningful line concept).

**Audio tags**: Same pattern:
```
[TAG:title] Purple Haze
[TAG:artist] Jimi Hendrix
[TAG:album] Are You Experienced
[TAG:year] 1967
[TAG:comment] Remastered 2010
```

---

## Content Extractor Trait

```rust
// crates/common/src/extract/mod.rs

pub struct ExtractedLine {
    pub archive_path: Option<String>,  // for archives / PDF pages
    pub line_number:  usize,
    pub content:      String,
}

pub trait Extractor {
    /// Returns true if this extractor handles the given path/mime.
    fn accepts(&self, path: &Path) -> bool;
    /// Extract lines from the file. May be called for archive entries too.
    fn extract(&self, path: &Path) -> anyhow::Result<Vec<ExtractedLine>>;
}
```

Dispatch order (first match wins):
1. `ArchiveExtractor`  — zip / tar / tar.gz / tar.bz2 / tar.xz / 7z
2. `PdfExtractor`      — `.pdf`
3. `ImageExtractor`    — jpeg / png / tiff / heic / webp / raw formats
4. `AudioExtractor`    — mp3 / flac / ogg / m4a / aac / opus / wav
5. `TextExtractor`     — everything else that `content_inspector` says is text

Archives: re-run the extractor chain on each entry (recursive, 1 level deep).

### Post-MVP: OCR

An `OcrExtractor` would sit between `ImageExtractor` and `TextExtractor`. It requires
`tesseract` installed on the indexing machine. Opt-in via config:

```toml
[scan]
ocr = false  # default off; requires tesseract in PATH
```

For PDFs without embedded text: `PdfExtractor` can detect empty extraction and
fall back to rendering pages as images + OCR. The `pdfium-render` crate supports
this flow; it's more complex but correct for scanned PDFs.

---

## API Specification

All endpoints require `Authorization: Bearer <token>`.

### Index update endpoints (called by scan/watch clients)

```
GET    /api/v1/files
       ?source=workstation-home
       → [{path, mtime}]           — full file listing for deletion detection

PUT    /api/v1/files
       body: {source, files: [{path, mtime, size, kind, lines: [{archive_path,
                line_number, content}]}]}
       → 200                        — upsert batch (≤500 files per request)

DELETE /api/v1/files
       body: {source, paths: [string]}
       → 200                        — remove files + their lines

POST   /api/v1/scan-complete
       body: {source, timestamp}
       → 200                        — updates meta.last_scan
```

### Query endpoints (called by CLI + web UI)

```
GET    /api/v1/search
       ?q=<pattern>
       &mode=fuzzy|exact|regex      default: fuzzy
       &source=<name>               repeatable; omit = all sources
       &limit=<n>                   default: 50
       &offset=<n>                  default: 0
       → {results: [{source, path, archive_path, line_number, snippet, score}],
          total: n}

GET    /api/v1/context
       ?source=<name>
       &path=<rel-path>
       &archive_path=<inner-path>   optional
       &line=<n>
       &window=<n>                  default: 5 (lines before and after)
       → {lines: [{line_number, content}], file_kind: string}
```

---

## Search Pipeline (server-side)

```
query Q arrives at GET /api/v1/search
  │
  ├── for each source DB (parallel tokio tasks):
  │     ├── FTS5 trigram pre-filter:
  │     │     SELECT l.*, f.path, f.kind FROM lines_fts
  │     │     JOIN lines l ON l.id = lines_fts.rowid
  │     │     JOIN files f ON f.id = l.file_id
  │     │     WHERE lines_fts MATCH <query>
  │     │     LIMIT 2000
  │     │
  │     └── nucleo-matcher re-score candidates → Vec<ScoredResult>
  │
  ├── merge all per-source results
  ├── sort by score descending
  └── paginate → return top N
```

For `exact` and `regex` modes, skip nucleo and use SQL LIKE / regexp() directly.

---

## Client Configuration

`/etc/find-anything/client.toml` (or `~/.config/find-anything/client.toml`):

```toml
[server]
url   = "https://index.internal:8080"
token = "your-bearer-token-here"

[source]
name      = "workstation-home"          # unique name for this source on the server
base_path = "/home/user"                # root of what's being indexed

[[source.paths]]
path = "/home/user"

[[source.paths]]
path = "/mnt/projects"

[scan]
# Glob patterns (applied relative to each source path)
# Uses gitignore-style matching via the globset crate
exclude = [
    "**/.git/**",
    "**/node_modules/**",
    "**/target/**",          # Rust build artifacts
    "**/__pycache__/**",
    "**/.next/**",
    "**/dist/**",
    "**/.cache/**",
    "**/*.pyc",
    "**/*.class",
    "**/Thumbs.db",
    "**/.DS_Store",
]

max_file_size_kb = 1024     # skip files larger than this (default 1 MB)
follow_symlinks  = false
include_hidden   = false
ocr              = false    # post-MVP; requires tesseract

[scan.archives]
enabled = true
# Archive entries also respect max_file_size_kb and exclusion patterns
```

---

## Server Configuration

`/etc/find-anything/server.toml`:

```toml
[server]
bind    = "0.0.0.0:8080"
data_dir = "/var/lib/find-anything"     # contains sources/ subdirectory
token   = "your-bearer-token-here"      # single shared token

[search]
default_limit    = 50
max_limit        = 500
fts_candidate_limit = 2000              # max FTS5 rows before nucleo re-score
```

---

## Systemd Integration

### Nightly rescan (timer)

`/etc/systemd/system/find-scan.service`:
```ini
[Unit]
Description=find-anything index scan
After=network.target

[Service]
Type=oneshot
ExecStart=/usr/local/bin/find-scan --config /etc/find-anything/client.toml
User=find-anything
```

`/etc/systemd/system/find-scan.timer`:
```ini
[Unit]
Description=find-anything nightly scan

[Timer]
OnCalendar=*-*-* 02:00:00
Persistent=true     # run on next boot if the window was missed

[Install]
WantedBy=timers.target
```

Enable: `systemctl enable --now find-scan.timer`

### Full rescan (manual):
```bash
find-scan --full --config /etc/find-anything/client.toml
```

### inotify watcher (always running)

`/etc/systemd/system/find-watch.service`:
```ini
[Unit]
Description=find-anything filesystem watcher
After=network.target

[Service]
Type=simple
ExecStart=/usr/local/bin/find-watch --config /etc/find-anything/client.toml
User=find-anything
Restart=always
RestartSec=5

[Install]
WantedBy=multi-user.target
```

Enable: `systemctl enable --now find-watch.service`

---

## Incremental Scan Logic

```
find-scan [--full] --config <path>

1. Read config; connect to server

2. GET /api/v1/files?source=<name>
   → server_files: HashMap<path, mtime>

3. Walk local filesystem (respecting exclude globs, max_file_size_kb)
   → local_files: HashMap<path, mtime>

4. Compute:
   if --full:
       to_index  = all local_files
   else:
       last_scan = read from server (POST /api/v1/scan-complete records it)
       to_index  = { f | f ∈ local_files AND (mtime(f) > last_scan
                                               OR f ∉ server_files) }

   to_delete = server_files.keys() − local_files.keys()

5. For each file in to_index:
   a. Detect kind (text / pdf / archive / image / audio)
   b. Run appropriate Extractor → Vec<ExtractedLine>
   c. Accumulate into batch (≤500 files)
   d. PUT /api/v1/files (batch)

6. DELETE /api/v1/files with to_delete paths (batched)

7. POST /api/v1/scan-complete {source, timestamp: now()}
```

### inotify Watcher Logic

```
find-watch --config <path>

1. Set up notify::RecommendedWatcher on all source.paths
2. Loop:
   on CREATE / MODIFY event for path P:
     if P matches any exclude glob: skip
     if P is file and size ≤ max_file_size_kb:
       extract P → lines
       PUT /api/v1/files [{path: P, ...lines}]
   on REMOVE event for path P:
     DELETE /api/v1/files [P]
   on RENAME event (old, new):
     DELETE /api/v1/files [old]
     extract new → lines
     PUT /api/v1/files [{path: new, ...lines}]
```

Debounce rapid events (e.g. editor saves): coalesce events within 500ms.
Use `notify`'s `PreciseEvents` (inotify) for rename tracking.

---

## Web UI (SvelteKit)

### Search experience
- Search box with 150ms debounce → live results as you type (fuzzy mode default)
- Mode switcher: Fuzzy / Exact / Regex
- Source filter chips (multi-select)
- Results show: source badge, file path, line number, match snippet

### Context panel
- Click any result → right panel slides in
- Shows ±10 lines of context with the match line highlighted
- Fetched from `GET /api/v1/context`
- Syntax highlighting via `highlight.js` or `shiki` (language detected from file extension)
- For PDF results: shows "Page N" header
- For metadata results (EXIF, audio tags): shows all tags for that file

### Stack
- SvelteKit (SSR + static adapter for simplicity)
- TypeScript
- `highlight.js` for syntax highlighting
- Minimal CSS (no heavy framework; perhaps pico.css for baseline)

### Deployment
The web server talks to find-server using the same bearer token, which it reads from
an environment variable. The web server itself can be unauthenticated (for internal
network use) or protected via nginx basic auth / SSO.

---

## Key Rust Dependencies

| Crate | Version | Purpose |
|-------|---------|---------|
| `axum` | 0.8 | HTTP server |
| `tokio` | 1 | Async runtime |
| `rusqlite` | 0.31 | SQLite (with `bundled` feature) |
| `reqwest` | 0.12 | HTTP client (indexer → server) |
| `notify` | 6 | Filesystem watching (inotify/FSEvents) |
| `nucleo-matcher` | 0.3 | Fuzzy re-scoring (same as text-scan) |
| `globset` | 0.4 | Exclusion pattern matching |
| `serde` + `serde_json` | 1 | Serialization |
| `clap` | 4 | CLI argument parsing |
| `toml` | 0.8 | Config file parsing |
| `walkdir` | 2 | Directory traversal |
| `zip` | 2 | ZIP archives |
| `tar` + `flate2` + `bzip2` + `xz2` | — | TAR variants |
| `sevenz-rust` | 0.6 | 7-Zip archives |
| `pdf-extract` | 0.7 | PDF text extraction |
| `kamadak-exif` | 0.5 | JPEG/TIFF EXIF metadata |
| `id3` | 1 | MP3 ID3 tags |
| `metaflac` | 0.2 | FLAC Vorbis comments |
| `mp4ameta` | 0.4 | M4A/AAC metadata |
| `content_inspector` | 0.2 | Binary/text detection |
| `anyhow` | 1 | Error handling |
| `tracing` | 0.1 | Structured logging |
| `tokio-rusqlite` | 0.5 | Async SQLite wrapper |

### Post-MVP OCR

| Crate | Purpose |
|-------|---------|
| `tesseract-rs` or `leptess` | Rust bindings to tesseract OCR engine |
| `pdfium-render` | Render PDF pages to images (for scanned PDFs) |
| `image` | Image decoding for pre-processing before OCR |

OCR is expensive (seconds per page) so it should run in a low-priority background
thread pool with a configurable concurrency limit, and results should be cached by
file content hash to avoid re-OCRing unchanged files.

---

## Implementation Phases

### Phase 1 — Core pipeline (get data flowing)
1. Cargo workspace + crate scaffolding
2. `common`: `ExtractedLine`, API request/response types (serde)
3. `common/extract`: `TextExtractor` (plain text only, skip archives/PDF/media for now)
4. `server`: SQLite schema (`db.rs`), PUT/DELETE/GET-files endpoints, basic auth middleware
5. `client/scan.rs`: walk + mtime diff + text extraction + batch submit

**Checkpoint**: can index a directory of text files and see them in the DB.

### Phase 2 — Search
1. `server/search.rs`: FTS5 trigram pre-filter + nucleo re-score
2. `server`: GET /search and GET /context endpoints
3. `client/find` (or a `find` binary in a 4th crate): CLI query tool
   ```
   find "pattern" [--source X] [--mode fuzzy|exact|regex] [--limit N]
   ```

**Checkpoint**: end-to-end: index files, query, get ranked results with context.

### Phase 3 — Rich extractors
1. `ArchiveExtractor` (port from text-scan)
2. `PdfExtractor`
3. `ImageExtractor` (EXIF)
4. `AudioExtractor` (ID3 / Vorbis / MP4)

### Phase 4 — Live watching
1. `client/watch.rs` using `notify`
2. Debounce logic
3. systemd service + timer files (in `deploy/` directory)

### Phase 5 — Web UI
1. SvelteKit scaffold
2. Search box + result list
3. Context panel with syntax highlighting
4. Source filter chips

### Phase 6 — Operations
1. `find-scan register` subcommand (create source, get token)
2. Server-side logging (`tracing` + `tracing-subscriber`)
3. DB vacuuming / compaction endpoint
4. README and setup guide

### Post-MVP
- OCR (tesseract) for images and text-less PDFs
- Result deduplication (same content in multiple sources)
- Search history / saved queries in web UI
- Webhook notifications on new indexed content
- Export index to JSON

---

## Open Questions (resolved for now, revisit if needed)

- **Token rotation**: currently one static token. If needed later, add a
  `tokens` table and expiry logic.
- **Large repos**: if a source DB grows beyond ~10 GB, evaluate partitioning by
  top-level directory. For now one-DB-per-source is sufficient.
- **Search ranking**: pure nucleo score. Could add recency bias (recently modified
  files rank higher) or frequency bias later.
- **Streaming results**: current API returns full result set. If result counts are
  large, add server-sent events or cursor-based pagination.
