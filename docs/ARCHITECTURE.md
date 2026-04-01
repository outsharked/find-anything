# find-anything Architecture

## System Overview

find-anything is a two-process system for full-text indexing and search of local files.

```
find-scan ──POST /api/v1/bulk──▶ find-server ──▶ SQLite + blobs.db
                                      │
                              GET /api/v1/search
                                      │
                               web UI (SvelteKit)
```

| Binary | Role |
|--------|------|
| `find-server` | Receives indexed content, stores it, serves search queries |
| `find-scan`   | Walks the filesystem, extracts content, batches to server |
| `find-watch`  | inotify/FSEvents watcher; spawns extractor subprocesses and submits to server |
| `find-upload` | Chunked HTTP upload client; delegates extraction to find-scan on the server |

---

## Crate Structure

```
crates/
├── common/                   # Shared API types, config, fuzzy search
│                             # Deliberately lean: no extractor deps
├── extract-types/            # IndexLine, ExtractorConfig, SCANNER_VERSION
├── content-store/            # ContentStore trait + SqliteContentStore
├── server/                   # HTTP server, SQLite, blobs.db management
├── client/                   # find-scan binary; dispatches to extractor libs
└── extractors/
    ├── text/                 # Plain text, source code, Markdown + frontmatter
    ├── pdf/                  # PDF text extraction (pdf-extract)
    ├── media/                # Image EXIF, audio tags, video metadata (+ ffprobe)
    ├── html/                 # HTML tag stripping, title/description metadata
    ├── office/               # DOCX, XLSX, PPTX extraction
    ├── epub/                 # EPUB spine + metadata extraction
    ├── pe/                   # PE (Windows executable) metadata
    ├── dicom/                # DICOM medical image metadata extraction
    ├── dispatch/             # Unified bytes-based dispatch — single source of truth
    └── archive/              # ZIP / TAR / GZ / BZ2 / XZ / 7Z + orchestration
```

### Extractor crates

Each extractor is **both a library and a standalone binary**:

- **Library** – linked into `find-scan` for zero-overhead in-process extraction
- **Binary** – standalone CLI used by `find-watch` in subprocess mode

```
find-extract-text    [~2 MB]   gray_matter, serde_yaml, content_inspector
find-extract-pdf     [~10 MB]  pdf-extract
find-extract-media   [~15 MB]  kamadak-exif, id3, metaflac, mp4ameta, audio-video-metadata
find-extract-html    [~3 MB]   scraper (html5ever)
find-extract-office  [~5 MB]   calamine, quick-xml
find-extract-epub    [~3 MB]   quick-xml
find-extract-pe      [~2 MB]   goblin
find-extract-dicom   [~3 MB]   dicom-rs
find-extract-dispatch [~1 MB]  infer + all above extractor libs (unified dispatch)
find-extract-archive  [~6 MB]  zip, tar, flate2, bzip2, xz2, sevenz-rust2
                               + find-extract-dispatch (member delegation via dispatch)
```

Dependency diagram (runtime linkage in `find-scan`):

```
find-common
find-extract-types
    ↑
find-extract-{text, pdf, media, html, office, epub, pe, dicom}
    ↑
find-extract-dispatch   ← single source of truth for bytes-based dispatch
    ↑               ↑
find-extract-archive   find-client (find-scan)

find-server
  └─ find-common          (no extractors – lean binary)
  └─ find-content-store   (blobs.db management)
```

---

## Write Path (Indexing)

```
find-scan → POST /api/v1/bulk (gzip JSON) → inbox/{id}.gz on disk
                                                    │
                                   Phase 1: inbox worker (SQLite only)
                                      delete old FTS5 rows (reading old blob via content_store),
                                      upsert files table + insert FTS5 rows,
                                      write normalised .gz → inbox/to-archive/
                                                    │
                                            inbox/to-archive/{id}.gz
                                                    │
                                   Phase 2: archive worker
                                      parse gz, verify content_hash matches DB,
                                      content_store.put_overwrite(file_hash, blob)
                                      → stored in blobs.db
```

Key invariants:
- **All DB writes go through the inbox worker** — no route handler writes SQLite directly.
- The bulk route handler only writes a `.gz` file to `data_dir/inbox/` and returns `202 Accepted`.
- Within a `BulkRequest`, the worker processes **deletes first, then upserts** so renames work correctly.
- **Phase 1** (worker/request.rs) handles all SQLite writes synchronously. When re-indexing a modified
  file, it reads the old blob from the content store (via `file_hash`) and issues the FTS5 `'delete'`
  command for each old line before inserting new content — keeping the contentless FTS5 index clean.
  Empty lines are skipped in the delete pass (issuing `'delete'` with `""` corrupts FTS5 state).
  At the end it writes a normalised `.gz` to `inbox/to-archive/` and notifies the archive worker.
- **Phase 2** (worker/archive_batch.rs) reads from `to-archive/`, verifies each file's `content_hash`
  matches the current DB record (to skip stale batches), then calls `content_store.put_overwrite(key, blob)`.
  Always overwrites — extraction output may differ even when raw bytes are unchanged (e.g. SCANNER_VERSION bump).
  Line content is `trim_end()`-stripped before being stored in the blob.

---

## Two-Phase Inbox Processing

Inbox processing is split into two phases.

```
router loop (every 1 s)
  → scan inbox/, sort .gz files by mtime
  → try_send to indexing worker (capacity-1 channel, non-blocking)
  → track in-flight paths in HashSet to avoid re-dispatching

Phase 1 — inbox worker (SQLite only, no blob I/O):
  receive path → spawn_blocking(process_request) with timeout
    → deletes: read old blob from content_store, issue FTS5 'delete' per old line,
               delete files rows
    → upserts: insert/update files table, insert FTS5 rows
    → write normalised .gz to inbox/to-archive/
    → signal archive worker via Notify

Phase 2 — archive worker (blob I/O):
  wait for Notify (or timeout)
  loop until drained:
    read up to archive_batch_size .gz files from inbox/to-archive/ (sorted by mtime)
    for each file:
      → verify content_hash in gz matches DB (skip stale)
      → build blob: sort lines by line_number, trim_end, join with '\n'
      → content_store.put_overwrite(file_hash, blob) → writes to blobs.db
    → delete processed .gz files
```

### Timeout

The phase 1 `spawn_blocking` call is wrapped in `tokio::time::timeout`
(`inbox_request_timeout_secs`, default 1800 s / 30 min). If the blocking
thread hangs, the worker logs an error and moves the file to `inbox/failed/`.

---

## Content Storage (blobs.db)

All file content is stored in **`data_dir/blobs.db`** — a single SQLite database
managed by `SqliteContentStore` (`crates/content-store/`). There are no ZIP archives.

```
data_dir/
  blobs.db          ← all file content (content-addressable)
  sources/
    {source}.db     ← files table + FTS5 index (per source)
  inbox/            ← incoming bulk requests (temporary)
  inbox/to-archive/ ← awaiting phase 2 blob storage (temporary)
```

- Content is **content-addressable**: keyed by `file_hash` (blake3 of raw file bytes).
  Two files with identical bytes share one stored blob.
- Each blob is split into chunks of configurable size (default 1 KB). Each chunk
  records `(key, chunk_num, start_line, end_line, data_bytes)`. Chunk data is lines
  joined by `\n` with **no trailing newline**; `get_lines` uses `str::lines()` to
  reconstruct them, which naturally handles the empty-blob sentinel and preserves
  interior blank lines.
- Reads use a PK-indexed range query: `get_lines(key, lo, hi)` returns only the
  chunk(s) that overlap the requested line range — no full-blob load.
- WAL mode + a read-connection pool (`SqliteContentStore`) allow unlimited concurrent
  readers while a single write mutex serialises puts.
- Compaction (`POST /api/v1/admin/compact`) deletes blobs whose key no longer appears
  in any source DB's `files.file_hash` column, then VACUUMs.

There is **no** separate `lines` table. The FTS5 rowid encodes both the `file_id`
and `line_number` arithmetically:

```
rowid = file_id × 1_000_000 + line_number
```

This lets the search query decode file and line position from the FTS result without
a JOIN to an auxiliary table.

---

## ContentStore Abstraction

The `ContentStore` trait (`crates/content-store/src/store.rs`) is the interface between
the server worker and blob storage:

```rust
pub trait ContentStore: Send + Sync {
    fn put(&self, key: &ContentKey, blob: &str) -> anyhow::Result<bool>;
    fn put_overwrite(&self, key: &ContentKey, blob: &str) -> anyhow::Result<bool>;
    fn delete(&self, key: &ContentKey) -> anyhow::Result<()>;
    fn get_lines(&self, key: &ContentKey, lo: usize, hi: usize)
        -> anyhow::Result<Option<Vec<(usize, String)>>>;
    fn contains(&self, key: &ContentKey) -> anyhow::Result<bool>;
    fn compact(&self, live_keys: &HashSet<ContentKey>, dry_run: bool) -> anyhow::Result<CompactResult>;
    fn storage_stats(&self) -> Option<(u64, u64)>;
}
```

- `put` is idempotent: returns `Ok(false)` if the key already exists (no re-write).
- `put_overwrite` deletes then puts — used by phase 2 to handle SCANNER_VERSION bumps where
  the raw file bytes (and therefore the key/hash) are unchanged but the extracted content changes.
- `get_lines(key, lo, hi)` returns `(position, line_content)` pairs for all lines in `[lo, hi]`.

The concrete implementation is `SqliteContentStore`, which stores all data in `blobs.db`:
- **Writes**: single `Mutex<Connection>` serialises all puts/deletes.
- **Reads**: elastic pool of read-only connections (up to `DEFAULT_MAX_READ_CONNECTIONS = 100`);
  WAL mode allows unlimited concurrent readers.
- Chunk data may optionally be gzip-compressed (controlled by a config flag; off by default).

---

## Read Path (Search)

```
GET /api/v1/search → FTS5 query → decode (file_id, line_number) from rowid
                   → JOIN files → fetch content via content_store.get_lines(file_hash, lo, hi)
                   → return matched lines + snippets
```

Context retrieval (`/api/v1/context`, `/api/v1/file`) uses the same
`content_store.get_lines` path. A per-request cache avoids re-fetching the same
chunk for files with many matched lines.

---

## Archive Members as First-Class Files

Archive members use **composite paths** with `::` as separator:

```
taxes/w2.zip::wages.pdf          (member of a ZIP)
data.tar.gz::report.txt          (member of a tarball)
outer.zip::inner.zip::file.txt   (nested archives)
```

- Each member has its own `file_id` in the `files` table
- `::` is reserved — cannot appear in regular file paths
- Members get their `kind` detected from their own filename (not the outer archive)
- Deletion: `DELETE FROM files WHERE path = 'x' OR path LIKE 'x::%'` cleans up members
- Re-indexing: server deletes `path LIKE 'archive::%'` members **only when the outer file arrives with `mtime=0`** (the start sentinel — see below)
- Client filters `::` paths from deletion detection (outer files only)

**Tree browsing**: `GET /api/v1/tree?prefix=archive.zip::` lists archive members.
Archive files (`kind="archive"`) expand in the tree like directories.

### Archive indexing protocol: mtime=0 sentinel

Indexing an archive is a multi-step process that can be interrupted mid-way. To ensure
the index is always in a consistent state, the client uses a two-phase commit:

**Phase 1 — start:** The outer archive file is submitted with `mtime=0`.
The server recognises `mtime=0` as the start-of-indexing sentinel and deletes all
existing inner members (`path LIKE 'archive::%'`) before writing the outer file row.

**Phase 2 — members:** Archive members are streamed and submitted in normal batches.

**Phase 3 — completion:** After all members are flushed, the client sends a second
upsert for the outer file with its real mtime. The server updates the outer file's
mtime **without** deleting members (deletion only fires on `mtime=0`).

**Interrupted scan recovery:** If the scan process is killed between phases 1 and 3,
the outer file retains `mtime=0` in the database. Any real file mtime is always > 0, so
the next scan sees a mtime mismatch and re-indexes the archive from scratch — no manual
intervention required.

---

## Archive Extractor: Member Delegation

The archive extractor acts as an **orchestrator**: it decompresses archive members one
at a time and delegates each member's bytes to `find-extract-dispatch`, which applies
the same priority-ordered extraction pipeline used for regular files.

```
archive.zip
  ├── report.pdf   → dispatch_from_bytes() → find_extract_pdf
  ├── notes.txt    → dispatch_from_bytes() → find_extract_text
  ├── photo.jpg    → dispatch_from_bytes() → find_extract_media
  ├── document.docx→ dispatch_from_bytes() → find_extract_office
  ├── page.html    → dispatch_from_bytes() → find_extract_html
  ├── nested.zip   → recursive extraction (in-memory via Cursor)
  ├── data.log.gz  → decompress in-memory, dispatch as text
  └── qjs (ELF)   → dispatch_from_bytes() → MIME fallback → [FILE:mime] application/x-elf
```

**Dispatch priority order** (identical for archive members and regular files):
PDF → DICOM → Media → HTML → Office → EPUB → PE → Text → MIME fallback

**MIME fallback**: For unrecognised binary content, dispatch emits a `line_number=0` line
`[FILE:mime] <mime>` (e.g. `application/x-elf`). The caller uses this to set the file's
`kind` accurately instead of falling back to `"unknown"`.

**Supported archive formats**: ZIP, TAR, TAR.GZ, TAR.BZ2, TAR.XZ, GZ, BZ2, XZ, 7Z

**Depth limiting**: Controlled by `scan.archives.max_depth` (default: 10). When exceeded,
only the filename is indexed and a warning is logged.

**iWork members** (`.pages`, `.numbers`, `.key`): detected by `is_iwork_ext()`. The extractor opens the member bytes as a ZIP and extracts the embedded `preview.jpg` / `preview-web.jpg`, emitting it as a child entry (e.g. `doc.pages::preview.jpg`, `kind=image`). The outer `.pages` entry gets `kind=archive`. Text lives in binary `.iwa` protobuf files and requires Tika — see `server_only` delegation below.

**`server_only` delegation for archive members**: When a member's extension is in `ExtractorConfig::server_only_exts` (populated from extensions configured as `"server_only"` in `scan.extractors`), the extractor writes the raw member bytes to a temp file (`fa-member-XXXXXXXX/<leaf>`) and sets `MemberBatch::delegate_temp_path` instead of extracting inline. `scan.rs` then uploads the temp file to the server via the normal upload path (using the composite path, e.g. `outer.zip::doc.pages`) and deletes the temp dir. The server runs `find-scan` on the uploaded file, which applies the server-side extractor config (e.g. Tika for `.pages`). The `server_only_exts` list is passed as arg[6] to the `find-extract-archive` subprocess.

---

## Content Extraction: Memory Model

**All extraction is fully in-memory.** There is no true byte-level streaming.

| Path | Code | Memory behaviour |
|------|------|-----------------|
| Regular file | `dispatch_from_path` → `std::fs::read(path)` | Whole file buffered into `Vec<u8>` |
| Archive member | `read_to_end(&mut bytes)` inside extractor loop | Whole member buffered into `Vec<u8>` |
| Nested archive | Recursive `find_extract_archive::extract_from_bytes` | Member bytes already in `Vec<u8>` |

"Streaming" in the current architecture means **iterating archive members one at a
time** — while one member is being extracted, the rest of the archive has not been
read. Each individual member is still fully buffered.

### Memory cap (`max_size_kb`)

`ExtractorConfig::max_content_kb` (derived from `scan.archives.max_member_size_mb` in
`scan.toml`) is the per-member memory limit:

- **Regular files**: skipped entirely if `fs::metadata().len() > max_size_kb * 1024`
  (checked in `dispatch_from_path` before reading).
- **Archive members**: guarded by `take(max_size_kb * 1024 + 1)` as a hard cap on
  the actual `read_to_end` call, independent of what the archive header reports.
  If a member exceeds the limit, only its filename line is indexed and the
  remainder of the member stream is drained to keep the decompressor in sync.

The `take()` guard is the critical safeguard against OOM. Some archive formats
(notably solid 7z blocks) report `entry.size() = 0` for all entries, so a
pre-read size check alone cannot prevent allocating the full decompressed member.

---

## Extractor Binary Protocol

Each extractor binary is invoked by `find-watch` in subprocess mode:

```bash
find-extract-text   [--max-size-kb N] <file-path>          → JSON array of IndexLine
find-extract-pdf    [--max-size-kb N] <file-path>          → JSON array of IndexLine
find-extract-media  <file-path> [max-content-kb] [ffprobe-path]  → JSON array of IndexLine
find-extract-archive <file-path> [max-size-kb] [max-depth] → JSON array of IndexLine
find-extract-dicom  <file-path>                            → JSON array of IndexLine
```

**IndexLine** fields:
- `line_number` — 0 = metadata (see convention below); 1+ = content lines
- `content` — text content of the line
- `archive_path` — member path within archive (deprecated, None for new output)

**`line_number=0` prefix convention**: every metadata row must begin with a bracketed
tag so entries are unambiguously identifiable. The standard tags are:

| Prefix | Produced by | Example |
|--------|-------------|---------|
| `[PATH] ` | all (scan.rs / batch.rs) | `[PATH] photos/vacation.jpg` |
| `[EXIF:tag] ` | find-extract-media (images) | `[EXIF:Make] Canon` |
| `[IMAGE:key] ` | find-extract-media (basic image header) | `[IMAGE:width] 1920` |
| `[IMAGE] ` | find-extract-media (fallback) | `[IMAGE] no metadata available` |
| `[TAG:key] ` | find-extract-media (audio tags) | `[TAG:title] Hey Jude` |
| `[VIDEO:key] ` | find-extract-media (video via ffprobe) | `[VIDEO:codec] h264` |
| `[DICOM:tag] ` | find-extract-dicom | `[DICOM:PatientName] Doe^John` |
| `[PE:key] ` | find-extract-pe | `[PE:ProductName] Notepad` |
| `[FILE:mime] ` | find-extract-dispatch (MIME fallback) | `[FILE:mime] image/jpeg` |
| `[fa:duplicate] ` | server (search results) | `[fa:duplicate] /other/path/file.txt` |

The `[PATH]` row is always present and is the canonical "findable by filename" entry.

**`[VIDEO:key]` tags** are produced when `ffprobe_path` is configured in `[scan]`.
Keys include: `format`, `codec`, `resolution`, `fps`, `audio_codec`, `audio_channels`, `duration`.

**SCANNER_VERSION** is currently `7` (defined in `crates/extract-types/src/index_line.rs`).
The server forces re-extraction of files whose stored `scanner_version` is below the
current value, ensuring new metadata tags are indexed when the extractor is updated.

---

## Directory Tree

`GET /api/v1/tree?source=X&prefix=foo/bar/` uses a **range-scan** on the `files` table:

```sql
WHERE path >= 'foo/bar/' AND path < 'foo/bar0'
```

`prefix_bump` increments the last byte of the prefix to get the upper bound.
Results are grouped server-side into virtual directory nodes and file nodes.
Only immediate children are returned; the UI lazy-loads subdirectories on expand.

`GET /api/v1/tree/expand?source=X&path=foo/bar/file.txt` returns all ancestor directory
listings needed to reveal a specific file path in the tree — used by the web UI to
expand the tree to the currently-open file without multiple round trips.

---

## find-upload → find-scan Delegation

`find-upload` sends files to the server as chunked PATCH uploads. When the
final chunk arrives, the server delegates extraction to `find-scan` rather than
running extractors inline:

1. Server creates a UUID temp dir (`$TMPDIR/find-upload-<uuid>/`) and places
   the file at `<temp_root>/<rel_path>`.
2. Server writes a minimal `<temp_root>.toml` with the source name, temp root
   path, and scan settings.
3. Server spawns `find-scan --config <temp.toml> <abs_path>` and awaits completion.
4. find-scan submits the result via the normal `/api/v1/bulk` path.
5. A Drop guard cleans up the temp dir and TOML unconditionally on exit.

**Config responsibility split:**
- `subprocess_timeout_secs` and `max_content_size_mb` come from the server's
  `[scan]` config block (`ServerScanConfig`) — never from the client.
- `include`, `exclude`, `exclude_extra` are forwarded from the client via
  `UploadScanHints`.

---

## Server Routes

The server's HTTP handlers live in `crates/server/src/routes/`, split by concern:

| File | Endpoints |
|------|-----------|
| `routes/mod.rs` | Shared helpers (`check_auth`, `source_db_path`, `compact_lines`); `GET /api/v1/metrics` |
| `routes/search.rs` | `GET /api/v1/search` — fuzzy / exact / regex modes, multi-source parallel query |
| `routes/context.rs` | `GET /api/v1/context`, `POST /api/v1/context-batch` |
| `routes/file.rs` | `GET /api/v1/file`, `GET /api/v1/files` |
| `routes/tree.rs` | `GET /api/v1/sources`, `GET /api/v1/tree`, `GET /api/v1/tree/expand` |
| `routes/bulk.rs` | `POST /api/v1/bulk` — writes gzip to inbox, returns 202 immediately |
| `routes/view.rs` | `GET /api/v1/view` — unified image/DICOM viewer (serves inline image bytes) |
| `routes/raw.rs` | `GET /api/v1/raw` — raw file download (with optional `?convert=png`) |
| `routes/links.rs` | `POST /api/v1/links`, `GET /api/v1/links/{code}` — share links with expiry |
| `routes/upload.rs` | `POST /api/v1/upload`, `PATCH /api/v1/upload/{id}`, `HEAD /api/v1/upload/{id}` |
| `routes/admin.rs` | `GET/DELETE /api/v1/admin/inbox`, `POST /api/v1/admin/inbox/retry`, `POST /api/v1/admin/inbox/pause`, `POST /api/v1/admin/inbox/resume`, `GET /api/v1/admin/inbox/show`, `POST /api/v1/admin/compact`, `DELETE /api/v1/admin/source`, `GET /api/v1/admin/update/check`, `POST /api/v1/admin/update/apply` |
| `routes/settings.rs` | `GET /api/v1/settings` |
| `routes/stats.rs` | `GET /api/v1/stats`, `GET /api/v1/stats/stream` |
| `routes/errors.rs` | `GET /api/v1/errors` |
| `routes/recent.rs` | `GET /api/v1/recent`, `GET /api/v1/recent/stream` |
| `routes/session.rs` | `POST /api/v1/auth/session`, `DELETE /api/v1/auth/session` |

---

## Web UI Structure

The SvelteKit frontend (`web/src/`) follows a coordinator + view component pattern:

```
routes/+page.svelte     — thin coordinator: owns all state, no layout code
lib/
  appState.ts           — pure functions: buildUrl(), restoreFromParams(), AppState type
  SearchView.svelte     — search topbar + ResultList + error display
  FileView.svelte       — file topbar + sidebar (DirectoryTree) + viewer panel
  ResultList.svelte     — pure display component; renders result cards, no scroll logic
  SearchResult.svelte   — single result card with lazy-loaded context lines
  FileViewer.svelte     — full file display (text, markdown, binary, image, PDF, SVG, RTF)
  DirectImageViewer.svelte — unified image viewer with adjustment panel (invert, flip, brightness/contrast)
  VideoViewer.svelte    — video player with codec warning banner
  api.ts                — typed fetch wrappers for all server endpoints
```

**State management**: All mutable state (query, results, file path, view mode, etc.) lives
in `+page.svelte`. Child components receive props and emit typed Svelte events upward.

**Page scroll architecture**: The page scrolls naturally (no inner scroll container in
`ResultList`). The search topbar is `position: sticky; top: 0`. A `window` scroll listener
in `+page.svelte` calls `triggerLoad()` when within 600 px of the bottom.

**Pagination**: `+page.svelte` fetches the next batch (limit 50, offset = current length)
and deduplicates by `source:path:archive_path:line_number` before appending. A new search
resets `results` and scrolls to top.

**Context lines**: `SearchResult` fetches context lazily via `IntersectionObserver` — only
when the card scrolls into view — to avoid a burst of N requests on initial load.

**URL / history**: `buildUrl` encodes `q`, `mode`, `source[]`, `path`, and `panelMode`
into query params. `restoreFromParams` reconstructs `AppState` from `URLSearchParams`.
`history.pushState` / `replaceState` are called directly in `+page.svelte`.

**Notable UI features:**
- Mobile-responsive layout
- Command palette (Ctrl+P) for file navigation — includes archive members as `zip → member`
- Directory tree with lazy-load via `GET /api/v1/tree` + `GET /api/v1/tree/expand`
- Share button (creates links via `POST /api/v1/links`)
- Duplicates modal (files with same content hash)
- `source:` search prefix with typeahead
- SVG viewer, RTF viewer, DICOM viewer, unified image viewer
- Image adjustment panel (invert, flip, brightness, contrast)
- Video viewer with codec warning banner
- `findanything://` protocol handler (for `find-upload`)

---

## Key Files

| File | Purpose |
|------|---------|
| `crates/common/src/api.rs` | All HTTP request/response types |
| `crates/common/src/config.rs` | Client + server config structs |
| `crates/extract-types/src/index_line.rs` | `IndexLine`, `SCANNER_VERSION` |
| `crates/extract-types/src/extractor_config.rs` | `ExtractorConfig` (max_content_kb, ffprobe_path, etc.) |
| `crates/content-store/src/store.rs` | `ContentStore` trait |
| `crates/content-store/src/sqlite_store/mod.rs` | `SqliteContentStore` implementation |
| `crates/extractors/text/src/lib.rs` | Text + Markdown frontmatter extraction |
| `crates/extractors/pdf/src/lib.rs` | PDF extraction (with catch_unwind) |
| `crates/extractors/media/src/lib.rs` | Image EXIF, audio tags, video metadata (ffprobe) |
| `crates/extractors/html/src/lib.rs` | HTML text extraction + metadata |
| `crates/extractors/office/src/lib.rs` | DOCX / XLSX / PPTX extraction |
| `crates/extractors/epub/src/lib.rs` | EPUB spine + metadata extraction |
| `crates/extractors/pe/src/lib.rs` | PE (Windows executable) metadata |
| `crates/extractors/dicom/src/lib.rs` | DICOM medical image metadata |
| `crates/extractors/dispatch/src/lib.rs` | Unified bytes-based dispatch + `mime_to_kind` |
| `crates/extractors/archive/src/lib.rs` | Archive format iteration + orchestration |
| `crates/client/src/extract.rs` | Top-level dispatcher: archive vs. dispatch_from_path |
| `crates/client/src/scan.rs` | Filesystem walk, batch building, submission |
| `crates/server/src/worker.rs` | Inbox polling loop + phase 1 request processing |
| `crates/server/src/worker/archive_batch.rs` | Phase 2: reads to-archive/ gz, stores blobs in content_store |
| `crates/server/src/db.rs` | All SQLite operations (files table, FTS5, tree queries) |
| `crates/server/src/routes/mod.rs` | HTTP route helpers + `GET /api/v1/metrics` |
| `crates/server/src/routes/tree.rs` | `GET /api/v1/tree`, `GET /api/v1/tree/expand` |
| `crates/server/src/routes/upload.rs` | Upload HTTP route handlers (POST/PATCH/HEAD) |
| `crates/server/src/upload.rs` | Upload state management + find-scan delegation |
| `crates/server/src/schema_v2.sql` | DB schema |
| `web/src/lib/api.ts` | TypeScript API client |
| `web/src/lib/appState.ts` | URL serialisation + AppState type |
| `web/src/routes/+page.svelte` | Main page — coordinator, owns all state |
| `web/src/lib/SearchView.svelte` | Search topbar + result list |
| `web/src/lib/FileView.svelte` | File topbar + sidebar + viewer panel |
| `web/src/lib/DirectImageViewer.svelte` | Unified image viewer with adjustment panel |

---

## Key Invariants

- **`line_number = 0`** rows are metadata. Every such row carries a bracketed prefix
  tag (`[PATH]`, `[EXIF:…]`, `[TAG:…]`, `[VIDEO:…]`, `[DICOM:…]`, `[PE:…]`, `[IMAGE:…]`,
  `[FILE:mime]`, `[fa:duplicate]`) that identifies its type. The `[PATH] <relative-path>`
  row is always present, ensuring every file is findable by name even if content extraction
  yields nothing.
- **FTS5 index is contentless** (`content=''`); all content lives in `blobs.db`.
  FTS5 is populated manually by the worker at insert time. The source DB has no content
  column — only `(file_id, line_number)` encoded in the FTS5 rowid.
- **`archive_path` on `IndexLine`** is deprecated (schema v3) — composite paths in
  `files.path` replaced it. For backward compatibility, API endpoints still accept an
  `archive_path` query param.
- **The `files` table is per-source** — one SQLite DB per source name, stored at
  `data_dir/sources/{source}.db`. `blobs.db` is shared across all sources.
- **PDF extraction** wraps `pdf-extract` in `std::panic::catch_unwind` because the
  library panics on malformed PDFs rather than returning errors. Uses a fork at
  `https://github.com/jamietre/pdf-extract` pinned by git rev in
  `crates/extractors/pdf/Cargo.toml`.
- **Locked / inaccessible files are skipped gracefully.** Three defences are layered in
  `dispatch_from_path` and `process_file`:
  1. **Known binary extension — no I/O at all.** `find_extract_text::is_binary_ext_path`
     recognises extensions like `.vhdx`, `.vmdk`, `.vdi`, `.ova`, `.iso` and returns
     early before calling `File::open`.
  2. **Sniff-before-read for unknown extensions.** For files not claimed by a specialist
     extractor, `dispatch_from_path` reads only 512 bytes first.
  3. **I/O errors → skip with warning.** Any `File::open` or `read` error returns
     `Ok(vec![])` and logs a warning.
- **ffprobe integration**: When `ffprobe_path` is set in `[scan]`, `find-extract-media`
  runs ffprobe exclusively for video files and emits `[VIDEO:…]` metadata tags. The binary
  protocol accepts `ffprobe_path` as a positional argument: `find-extract-media <path> [max-content-kb] [ffprobe-path]`.
