# find-anything Architecture

## System Overview

find-anything is a two-process system for full-text indexing and search of local files.

```
find-scan ──POST /api/v1/bulk──▶ find-server ──▶ SQLite + ZIP archives
                                      │
                              GET /api/v1/search
                                      │
                               web UI (SvelteKit)
```

| Binary | Role |
|--------|------|
| `find-server` | Receives indexed content, stores it, serves search queries |
| `find-scan`   | Walks the filesystem, extracts content, batches to server |

---

## Crate Structure

```
crates/
├── common/                   # Shared API types, config, fuzzy search
│                             # Deliberately lean: no extractor deps
├── server/                   # HTTP server, SQLite, ZIP archive management
├── client/                   # find-scan binary; dispatches to extractor libs
└── extractors/
    ├── text/                 # Plain text, source code, Markdown + frontmatter
    ├── pdf/                  # PDF text extraction (pdf-extract)
    ├── media/                # Image EXIF, audio tags, video metadata
    ├── html/                 # HTML tag stripping, title/description metadata
    ├── office/               # DOCX, XLSX, PPTX extraction
    ├── epub/                 # EPUB spine + metadata extraction
    ├── pe/                   # PE (Windows executable) metadata
    ├── dispatch/             # Unified bytes-based dispatch — single source of truth
    └── archive/              # ZIP / TAR / GZ / BZ2 / XZ / 7Z + orchestration
```

### Extractor crates

Each extractor is **both a library and a standalone binary**:

- **Library** – linked into `find-scan` for zero-overhead in-process extraction
- **Binary** – standalone CLI for future use by `find-scan-watch` (subprocess mode)

```
find-extract-text    [~2 MB]   gray_matter, serde_yaml, content_inspector
find-extract-pdf     [~10 MB]  pdf-extract
find-extract-media   [~15 MB]  kamadak-exif, id3, metaflac, mp4ameta, audio-video-metadata
find-extract-html    [~3 MB]   scraper (html5ever)
find-extract-office  [~5 MB]   calamine, quick-xml
find-extract-epub    [~3 MB]   quick-xml
find-extract-pe      [~2 MB]   goblin
find-extract-dispatch [~1 MB]  infer + all above extractor libs (unified dispatch)
find-extract-archive  [~6 MB]  zip, tar, flate2, bzip2, xz2, sevenz-rust2
                               + find-extract-dispatch (member delegation via dispatch)
```

Dependency diagram (runtime linkage in `find-scan`):

```
find-common
    ↑
find-extract-{text, pdf, media, html, office, epub, pe}
    ↑
find-extract-dispatch   ← single source of truth for bytes-based dispatch
    ↑               ↑
find-extract-archive   find-client (find-scan)

find-server
  └─ find-common          (no extractors – lean binary)
```

---

## Write Path (Indexing)

```
find-scan → POST /api/v1/bulk (gzip JSON) → inbox/{id}.gz on disk
                                                    │
                                   Phase 1: indexing thread (SQLite only)
                                      delete old rows, queue chunk removes,
                                      upsert files/lines/FTS5 (chunk_archive=NULL)
                                                    │
                                            inbox/to-archive/{id}.gz
                                                    │
                                   Phase 2: archive thread (ZIP I/O)
                                      remove old chunks from ZIPs,
                                      chunk content → append to content_NNNNN.zip,
                                      UPDATE lines SET chunk_archive=...
```

Key invariants:
- **All DB writes go through the inbox worker** — no route handler writes SQLite directly.
- The bulk route handler only writes a `.gz` file to `data_dir/inbox/` and returns `202 Accepted`.
- Within a `BulkRequest`, the worker processes **deletes first, then upserts** so renames work correctly.

---

## Two-Phase Inbox Processing

Inbox processing is split into two phases handled by two independent threads.
Files stay in `inbox/` until phase 1 completes; the router never pre-claims
them into a separate directory.

```
router loop (every 1 s)
  → scan inbox/, sort .gz files by mtime
  → try_send to indexing worker (capacity-1 channel, non-blocking)
  → track in-flight paths in HashSet to avoid re-dispatching

Phase 1 — indexing thread (SQLite only, no ZIP I/O):
  receive path → spawn_blocking(process_request_phase1) with timeout
    → delete_files_phase1: remove DB rows, queue old chunk refs to
      pending_chunk_removes table for phase 2
    → upsert files/lines/FTS5 with NULL chunk_archive (deferred)
    → inline small files (≤ inline_threshold_bytes) directly to file_content
  → move .gz from inbox/ to inbox/to-archive/
  → signal archive thread via Notify

Phase 2 — archive thread (ZIP I/O + SQLite update):
  wait for Notify (or 60 s timeout)
  sleep 5 s to let queue accumulate
  loop until drained:
    read up to archive_batch_size .gz files from inbox/to-archive/
    per source:
      → take_pending_chunk_removes (clears pending removes table)
      → rewrite ZIPs to remove old chunks
      → coalesce upserts (last-writer-wins per path)
      → append new content chunks to ZIPs
      → UPDATE lines SET chunk_archive=... in a single transaction
    → delete processed .gz files
```

### Why two phases?

SQLite WAL mode uses POSIX `fcntl` byte-range locks to coordinate writers.
A critical POSIX property is that **a process cannot conflict with its own
locks**: when two file descriptors in the same process try to acquire the same
byte-range lock, the OS grants both immediately (same PID = same owner).
This means two connections from the same process to the same SQLite file do
not mutually exclude each other at the OS level, undermining WAL's write
serialisation regardless of `busy_timeout`.

The two-phase design avoids this by ensuring only one thread ever writes to a
given source DB at a time — enforced by a per-source `Mutex` in application
memory (see below), not by OS file locks.

### Per-source write lock (`SharedArchiveState::source_locks`)

`SharedArchiveState` holds a `Mutex<HashMap<String, Arc<Mutex<()>>>>` — one
inner mutex per source name. Both the indexing thread and the archive thread
**must hold this mutex for the duration of any SQLite write transaction** to
that source's DB.

```
indexing thread                        archive thread
───────────────────────────────────    ──────────────────────────────────
acquire source_lock("music")           // phase 2a: drain pending removes
  delete_files_phase1(...)             acquire source_lock("music")
  upsert files/lines/FTS5               take_pending_chunk_removes(...)
release source_lock("music")          release source_lock("music")
                                       // ZIP I/O (no lock held)
                                       rewrite ZIPs, append new chunks
                                       // phase 2b: update line refs
                                       acquire source_lock("music")
                                         UPDATE lines SET chunk_archive=...
                                       release source_lock("music")
```

The lock is **released between** the two archive-thread write segments so ZIP
I/O (potentially seconds) does not block the indexing thread. A plain
`std::sync::Mutex` is used — no timeout, no filesystem involvement — so it
waits indefinitely with no overhead and is immune to POSIX same-process lock
semantics.

### ZIP archive allocation (`SharedArchiveState`)

The archive thread owns a single **in-progress ZIP archive** for appending.
Archive numbers are allocated from a shared `AtomicU32` counter, so the
archive thread and any concurrent admin operation never write to the same
archive simultaneously on the append path.

The only other contention point is **rewriting** a sealed archive to remove
old chunks. This is serialised via a per-archive `Mutex` in
`SharedArchiveState::rewrite_locks`. `SharedArchiveState` is stored in
`AppState` and shared with admin routes (e.g. `DELETE /api/v1/admin/source`)
so rewrite locks are globally coordinated.

### Timeout

The phase 1 `spawn_blocking` call is wrapped in `tokio::time::timeout`
(`inbox_request_timeout_secs`, default 1800 s / 30 min). If the blocking
thread hangs, the worker logs an error and moves the file to `inbox/failed/`.

---

## Content Storage (ZIP Archives)

File content is stored in rotating ZIP archives, not inline in SQLite.

```
data_dir/sources/content/
  0000/content_00001.zip
  0000/content_00002.zip
  ...
  0001/content_01000.zip
  ...
```

- Folder: `content/{archive_num / 1000:04}` (4-digit zero-padded subfolder)
- Archive: `content_{archive_num:05}.zip` (5-digit zero-padded)
- Target: ~10 MB per archive (measured by compressed on-disk size)
- Maximum: 9,999 × 1,000 = 9,999,000 archives (~99.99 TB)

Each file's content is split into ~1 KB chunks:
- Chunk name: `{relative_path}.chunk{N}.txt`
- The `lines` table stores `(chunk_archive, chunk_name, line_offset_in_chunk)`
- No content inline in SQLite — all content lives in ZIPs

---

## Read Path (Search)

```
GET /api/v1/search → FTS5 query → candidate rows (chunk_archive, chunk_name, line_offset)
                   → read chunk from ZIP (cached per request)
                   → return matched lines + snippets
```

Context retrieval (`/api/v1/context`, `/api/v1/file`) reads chunks the same way, with a
per-request `HashMap` cache to avoid re-reading the same chunk file twice.

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

This also applies to the server-side error fallback path: if the worker fails to process
an outer archive, it writes a stub with `mtime=0` (via `outer_archive_stub`) so the
archive is retried on the next scan.

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
  ├── document.docx→ dispatch_from_bytes() → find_extract_office   ← same as regular files
  ├── page.html    → dispatch_from_bytes() → find_extract_html      ← same as regular files
  ├── nested.zip   → recursive extraction (in-memory via Cursor)
  ├── data.log.gz  → decompress in-memory, dispatch as text
  └── qjs (ELF)   → dispatch_from_bytes() → MIME fallback → [FILE:mime] application/x-elf
```

**Dispatch priority order** (identical for archive members and regular files):
PDF → Media → HTML → Office → EPUB → PE → Text → MIME fallback

**MIME fallback**: For unrecognised binary content, dispatch emits a `line_number=0` line
`[FILE:mime] <mime>` (e.g. `application/x-elf`). The caller uses this to set the file's
`kind` accurately instead of falling back to `"unknown"`.

**Supported archive formats**: ZIP, TAR, TAR.GZ, TAR.BZ2, TAR.XZ, GZ, BZ2, XZ, 7Z

**Depth limiting**: Controlled by `scan.archives.max_depth` (default: 10). When exceeded,
only the filename is indexed and a warning is logged.

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

`ExtractorConfig::max_size_kb` (derived from `scan.archives.max_member_size_mb` in
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

### Future: streaming extractor API

The intended long-term improvement is to allow extractors to accept `impl Read`
in addition to `&[u8]`, so large members can be piped through without buffering.
See the Roadmap section "Memory-Safe Archive Extraction (Streaming)".

---

## Extractor Binary Protocol

Each extractor binary can be invoked standalone (for future use by `find-scan-watch`):

```bash
find-extract-text   [--max-size-kb N] <file-path>   → JSON array of IndexLine
find-extract-pdf    [--max-size-kb N] <file-path>   → JSON array of IndexLine
find-extract-media  [--max-size-kb N] <file-path>   → JSON array of IndexLine
find-extract-archive <file-path> [max-size-kb] [max-depth]  → JSON array of IndexLine
```

**IndexLine** fields:
- `line_number` — 0 = metadata (see convention below); 1+ = content lines
- `content` — text content of the line
- `archive_path` — member path within archive (None for regular files)

**`line_number=0` prefix convention**: every metadata row must begin with a bracketed
tag so entries are unambiguously identifiable. The standard tags are:

| Prefix | Produced by | Example |
|--------|-------------|---------|
| `[PATH] ` | all (scan.rs / batch.rs) | `[PATH] photos/vacation.jpg` |
| `[EXIF:tag] ` | find-extract-media (images) | `[EXIF:Make] Canon` |
| `[IMAGE:key] ` | find-extract-media (basic image header) | `[IMAGE:width] 1920` |
| `[IMAGE] ` | find-extract-media (fallback) | `[IMAGE] no metadata available` |
| `[TAG:key] ` | find-extract-media (audio tags) | `[TAG:title] Hey Jude` |
| `[PE:key] ` | find-extract-pe | `[PE:ProductName] Notepad` |
| `[FILE:mime] ` | find-extract-dispatch (MIME fallback) | `[FILE:mime] image/jpeg` |

The `[PATH]` row is always present and is the canonical "findable by filename" entry.
Consumers that need only the path row filter on `content LIKE '[PATH] %'`.

---

## Directory Tree

`GET /api/v1/tree?source=X&prefix=foo/bar/` uses a **range-scan** on the `files` table:

```sql
WHERE path >= 'foo/bar/' AND path < 'foo/bar0'
```

`prefix_bump` increments the last byte of the prefix to get the upper bound.
Results are grouped server-side into virtual directory nodes and file nodes.
Only immediate children are returned; the UI lazy-loads subdirectories on expand.

---

## Server Routes

The server's HTTP handlers live in `crates/server/src/routes/`, split by concern:

| File | Endpoints |
|------|-----------|
| `routes/mod.rs` | Shared helpers (`check_auth`, `source_db_path`, `compact_lines`); `GET /api/v1/metrics` |
| `routes/search.rs` | `GET /api/v1/search` — fuzzy / exact / regex modes, multi-source parallel query |
| `routes/context.rs` | `GET /api/v1/context`, `POST /api/v1/context-batch`; returns `{start, match_index, lines[], kind}` |
| `routes/file.rs` | `GET /api/v1/file`, `GET /api/v1/files` |
| `routes/tree.rs` | `GET /api/v1/sources`, `GET /api/v1/tree` |
| `routes/bulk.rs` | `POST /api/v1/bulk` — writes gzip to inbox, returns 202 immediately |

`check_auth` and `source_db_path` are `pub(super)` so only submodules can call them.

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
  FileViewer.svelte     — full file display (text, markdown, binary, image, PDF)
  api.ts                — typed fetch wrappers for all server endpoints
```

**State management**: All mutable state (query, results, file path, view mode, etc.) lives
in `+page.svelte`. Child components receive props and emit typed Svelte events upward.

**Page scroll architecture**: The page scrolls naturally (no inner scroll container in
`ResultList`). The search topbar is `position: sticky; top: 0`. A `window` scroll listener
in `+page.svelte` calls `triggerLoad()` when within 600 px of the bottom.

**Pagination**: `+page.svelte` fetches the next batch (limit 50, offset = current length)
and deduplicates by `source:path:line_number` before appending — the search API can return
overlapping results across page boundaries. A new search resets `results` and scrolls to
top. No virtual DOM recycling — plain `{#each}` with dedup is adequate for these batch sizes.

**Context lines**: `SearchResult` fetches context lazily via `IntersectionObserver` — only
when the card scrolls into view — to avoid a burst of N requests on initial load. A
placeholder bar is shown until loaded. Falls back silently to the `snippet` field if the
request fails or returns empty lines. The `ContextResponse` returns `{start, match_index,
lines: string[], kind}` where `start` is the first line number and `match_index` locates
the matched line within the window (null if the match falls in a sparse gap).

**URL / history**: `buildUrl` encodes `q`, `mode`, `source[]`, `path`, and `panelMode`
into query params. `restoreFromParams` reconstructs `AppState` from `URLSearchParams`.
`history.pushState` / `replaceState` are called directly in `+page.svelte`.

---

## Snippet Retrieval

The `snippet` field in search results is **not stored in SQLite**. It is read live from
ZIP archives at query time:

1. FTS5 trigram index matches the query → returns `rowid`s (no text stored, `content=''`)
2. Join to `lines` table → gets `(chunk_archive, chunk_name, line_offset_in_chunk)`
3. Read chunk text from ZIP → index into lines by offset → that string is the snippet

A per-request `HashMap` cache avoids re-reading the same chunk for multiple results.

**Implication**: For files with very long lines (e.g., PDFs with no line breaks), the
snippet can be very large because there is no truncation in the pipeline. The full line
content is returned verbatim in the JSON response.

---

## Key Files

| File | Purpose |
|------|---------|
| `crates/common/src/api.rs` | All HTTP request/response types |
| `crates/common/src/config.rs` | Client + server config structs |
| `crates/extractors/text/src/lib.rs` | Text + Markdown frontmatter extraction |
| `crates/extractors/pdf/src/lib.rs` | PDF extraction (with catch_unwind) |
| `crates/extractors/media/src/lib.rs` | Image EXIF, audio tags, video metadata |
| `crates/extractors/html/src/lib.rs` | HTML text extraction + metadata |
| `crates/extractors/office/src/lib.rs` | DOCX / XLSX / PPTX extraction |
| `crates/extractors/epub/src/lib.rs` | EPUB spine + metadata extraction |
| `crates/extractors/pe/src/lib.rs` | PE (Windows executable) metadata |
| `crates/extractors/dispatch/src/lib.rs` | Unified bytes-based dispatch + `mime_to_kind` |
| `crates/extractors/archive/src/lib.rs` | Archive format iteration + orchestration |
| `crates/client/src/extract.rs` | Top-level dispatcher: archive vs. dispatch_from_path |
| `crates/client/src/scan.rs` | Filesystem walk, batch building, submission |
| `crates/server/src/worker.rs` | Inbox worker pool: router loop + N workers sharing a channel; `process_request` |
| `crates/server/src/archive.rs` | `SharedArchiveState` (atomic counter + rewrite locks); `ArchiveManager` per worker; `chunk_lines()` |
| `crates/server/src/db.rs` | All SQLite operations |
| `crates/server/src/routes/` | HTTP route handlers (see Server Routes above) |
| `crates/server/src/schema_v2.sql` | DB schema |
| `web/src/lib/api.ts` | TypeScript API client |
| `web/src/lib/appState.ts` | URL serialisation + AppState type |
| `web/src/routes/+page.svelte` | Main page — coordinator, owns all state |
| `web/src/lib/SearchView.svelte` | Search topbar + result list |
| `web/src/lib/FileView.svelte` | File topbar + sidebar + viewer panel |

---

## Key Invariants

- **`line_number = 0`** rows are metadata. Every such row carries a bracketed prefix
  tag (`[PATH]`, `[EXIF:…]`, `[TAG:…]`, `[PE:…]`, `[IMAGE:…]`, `[FILE:mime]`) that
  identifies its type. The `[PATH] <relative-path>` row is always present, ensuring
  every file is findable by name even if content extraction yields nothing. Consumers
  that need only the path row filter `content LIKE '[PATH] %'`.
- **FTS5 index is contentless** (`content=''`); content lives only in ZIPs. FTS5 is
  populated manually by the worker at insert time. The `lines` table stores only
  `(chunk_archive, chunk_name, line_offset_in_chunk)` — no content column in SQLite.
- **`archive_path` on `IndexLine`** is deprecated (schema v3) — composite paths in
  `files.path` replaced it. For backward compatibility, API endpoints still accept an
  `archive_path` query param.
- **The `files` table is per-source** — one SQLite DB per source name, stored at
  `data_dir/sources/{source}.db`. ZIP archives are shared across sources.
- **PDF extraction** wraps `pdf-extract` in `std::panic::catch_unwind` because the
  library panics on malformed PDFs rather than returning errors.
- **Locked / inaccessible files are skipped gracefully.** On Windows, some files
  (e.g. the live WSL2 `ext4.vhdx` held open by Hyper-V) cause `File::open` to block
  indefinitely rather than returning an error. Three defences are layered in
  `dispatch_from_path` and `process_file` to prevent hangs:
  1. **Known binary extension — no I/O at all.** `find_extract_text::is_binary_ext_path`
     recognises extensions like `.vhdx`, `.vmdk`, `.vdi`, `.ova`, `.iso` and returns
     early in `dispatch_from_path` **before** calling `File::open`. The same check in
     `process_file` skips the `hash_file` call (which also opens the file).
  2. **Sniff-before-read for unknown extensions.** For files not claimed by a specialist
     extractor and not on the known-binary list, `dispatch_from_path` reads only 512
     bytes first. It reads the full file only if those bytes look like text. Binary
     content is rejected after 512 bytes — not after reading gigabytes.
  3. **I/O errors → skip with warning.** Any `File::open` or `read` error in
     `dispatch_from_path` returns `Ok(vec![])` and logs a warning so the scan
     continues. The file is indexed by name only and will be retried on the next scan.

---

## Plan 015 Status: Extractor Architecture Refactor

Phase 1 is **complete**:

| Goal | Status |
|------|--------|
| Extractor crates created (`text`, `pdf`, `media`, `archive`) | ✅ Done |
| Each extractor is both a library and a CLI binary | ✅ Done |
| `find-scan` links all extractor libraries statically | ✅ Done |
| Archive extractor orchestrates PDF, media, text, nested archives | ✅ Done |
| bz2/xz archive format support | ✅ Done |
| `max_depth` passed through from config to archive extractor | ✅ Done |
| Old extractors removed from `find-common` | ✅ Done |
| `find-common` has zero extractor dependencies (lean server binary) | ✅ Done |

Phase 2 (incremental client `find-scan-watch`) and Phase 3 (subprocess spawning in the
archive extractor) are **not yet implemented**. See `docs/plans/015-extractor-architecture-refactor.md`.
