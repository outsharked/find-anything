# Changelog

All notable changes to find-anything are documented here.

Format: [Keep a Changelog](https://keepachangelog.com/en/1.0.0/)
Versioning: [Semantic Versioning](https://semver.org/spec/v2.0.0.html)

---

## [Unreleased]

### Added

- **`find-admin delete`** - handle corrupted zip files
- **`find-scan`** - improve logging when scanning large numbers of files
- **`find-extract-dispatch` standalone binary** — unknown file types now route through `find-extract-dispatch` (instead of `find-extract-text`) so the full dispatch pipeline (PDF → media → HTML → office → EPUB → PE → text → MIME fallback) applies even when invoked as a subprocess
- **Windows dev scripts** — `config/update-win.sh` copies cross-compiled binaries directly to the local Windows install path for quick iteration; `mise run build-win` builds Windows binaries without invoking the Inno Setup installer

### Fixed

- **Locked / inaccessible files no longer hang the scan** — three-layer defence prevents `find-scan` from blocking on Windows files held open by other processes (e.g. the live WSL2 `ext4.vhdx` held by Hyper-V): (1) known binary extensions (`.vhdx`, `.vmdk`, `.vdi`, `.ova`, `.iso`, etc.) skip `File::open` entirely in both extraction and content hashing; (2) unknown extensions are sniff-tested with 512 bytes before reading further — binary content is rejected immediately without reading the full file; (3) any remaining I/O error logs a warning and skips the file rather than failing the scan
- **Windows include filter with bare drive root** — `path = "C:"` is now normalised to `C:/` so `strip_prefix` produces clean relative paths and include filters work correctly
- **Windows include filter subdirectory traversal** — directory pruning now correctly descends into subdirectories within `**` wildcard patterns (e.g. `Users/jamie/**` now indexes all files under `Users/jamie/`, not just files in the root of `Users/jamie/`)
- **Missing batch-submit log on final flush** — the last batch (submitted after the scan loop ends) now logs "submitting batch — N files, M deletes" consistently with all other batch submissions
- **Empty files incorrectly deduplicated** — `hash_file` now returns no hash for 0-byte files; previously all empty files shared the same blake3-of-empty-bytes hash, causing them to be linked as duplicates of each other regardless of type or location

---

## [0.5.5] - 2026-03-04

### Added

- **Browser tab title** — the web UI now shows "Find Anything" as the page title in browser tabs and bookmarks
- **Raw endpoint logging** — all silent 404/400 failure paths in `GET /api/v1/raw` now emit `tracing::warn!` log lines indicating exactly why the request failed (source not configured, root path invalid, file not found, or illegal path component); aids diagnosing image/file loading issues

### Fixed

- **`install.sh` headless Linux service** — on Linux systems without an active systemd user session (e.g. SSH-only servers), the installer now automatically installs `find-watch` as a system service when running as root or with passwordless sudo, rather than silently skipping service setup
- **`install-server.sh` missing `--config` flag** — the generated systemd unit `ExecStart` line now correctly passes `--config <path>` to `find-server` (the flag was omitted in the previous generated unit)
- **`install-server.sh` server.toml example** — the generated `server.toml` now includes a commented-out `[sources.xxx]` / `path =` example showing how to configure filesystem paths for raw file serving

### Changed

- **Windows tray: stable notification-area GUID** — removed; `tray-icon` 0.21 does not expose `with_guid`; limitation documented in a code comment for a future upgrade

---

## [0.5.4] - 2026-03-04

### Added

- **`find-admin delete-source <name>`** — deletes all indexed data for a named source: removes the source SQLite database and scrubs its content chunks from the shared ZIP archives; prompts for confirmation with file count shown unless `--force` is passed; new `DELETE /api/v1/admin/source` server endpoint
- **Nested ZIP member extraction** — the raw endpoint now supports two-level nested ZIP members (`outer.zip::inner.zip::file`); the maximum supported depth is configurable via `server.download_zip_member_levels` (default 2); deeper nesting returns 403
- **Copy-path button in PathBar** — a clipboard icon next to the file path copies the full composite path (`archive.zip::member`) to the clipboard; icon swaps to a checkmark for 1.5 s after copying
- **Windows installer: source name field** — the installer now prompts for a source name (default `Home`) instead of hardcoding `"home"` in the generated `client.toml`
- **Windows installer: config review page** — a new "Review Configuration" wizard page shows the generated `client.toml` in a monospace editor before it is written; users can freely edit it
- **Windows installer: tray autostart** — `find-tray.exe` is now launched immediately at the end of installation (not just added to registry autostart); `scan-and-start.bat` also starts the tray after the initial scan

### Changed

- **Windows install directory** — changed from `%LOCALAPPDATA%\find-anything` to `%LOCALAPPDATA%\FindAnything`
- **Windows tray icon** — updated to use the same magnifier icon as the web UI favicon
- **Windows tray: no console window** — `find-tray.exe` is now built with `windows_subsystem = "windows"`; no CMD window appears on launch, and running it from a terminal detaches immediately
- **Windows tray: stable notification-area GUID** — `TrayIconBuilder::with_guid` is now set to a hard-coded app-specific GUID; Windows uses this to persist "always show" preferences across application updates (previously the icon had to be re-pinned after each reinstall)
- **Windows installer spacing** — increased padding between labels and input boxes; labels use `AutoSize = True` to avoid truncation

### Fixed

- **Archive member mtime Y2K correction** — old ZIP tools that stored 2-digit years in the DOS datetime field produced future timestamps (e.g. 2077 instead of 1977); timestamps more than 0 seconds in the future but ≤ 2099 are now corrected by subtracting 100 years; timestamps after 2099 are discarded

---

## [0.5.3] - 2026-03-04

### Added

- **Date-range search filter** — the Advanced search panel now includes From / To date pickers that filter results by file modification time; archive members carry their own mtime extracted from ZIP extended timestamps (UTC unix i32) or DOS datetime fallback, TAR header mtime, or 7z `last_modified_date`; an `mtime` index is applied to existing databases on upgrade
- **Advanced search Apply button** — filter changes are now staged locally inside the panel and committed only when Apply is clicked; the Apply button is highlighted blue when the draft differs from the currently-applied state, and dimmed otherwise; clicking Apply fires the search and closes the panel
- **Calendar picker button** — a 📅 button on each date field calls `showPicker()` for reliable activation, replacing the browser's small native calendar icon (which is hidden)

### Changed

- **Date placeholder text dimmed** — the `mm/dd/yyyy` placeholder in empty date fields is rendered at 25 % opacity so it doesn't compete visually with entered values

### Fixed

- **Empty batch HTTP call skipped** — `submit_batch` now returns early when files, deletes, and failures are all empty, avoiding a pointless round-trip on the final flush
- **Redundant stat syscall in inbox worker** — `tokio::fs::metadata(&path)` replaced with `entry.metadata().await` (reuses the already-opened `DirEntry`)
- **Windows installer no longer blocks on initial scan** — removed the inline `find-scan --full` call during install; users run it manually after setup using the command printed in the install summary

---

## [0.5.2] - 2026-03-03

### Added

- **`find-scan --dry-run`** — scan the filesystem and report counts of new/modified/unchanged/to-delete files without submitting anything to the server
- **`parseImageDimensions` utility** — extracts image dimensions from indexed metadata lines, handling all three tag families: `[EXIF:PixelXDimension]`/`[EXIF:PixelYDimension]`, `[EXIF:ImageWidth]`/`[EXIF:ImageLength]`, and `[IMAGE:dimensions] WxH`
- **PDF defaults to original view** — opening a PDF from the tree or file browser now shows the embedded PDF viewer by default; opening from search results (where extracted-text context is relevant) still defaults to the extracted text view
- **PDF loading spinner** — a spinner is shown while the PDF iframe loads, replacing the blank panel
- **Encrypted PDF detection** — PDFs flagged as password-protected at index time (`/Encrypt` token detected) show a "🔒 This PDF is password-protected" notice instead of attempting to display the PDF inline (which would show a browser error)
- **Symmetric duplicate links** — both the canonical copy and any alias now show each other as `DUPLICATE:` links; previously only the alias showed the canonical, not vice versa

### Changed

- **`run_scan` refactored to use structs** — introduced `ScanOptions` (`full`, `quiet`, `dry_run`) and `ScanSource` (`name`, `paths`, `base_url`) to replace long parameter lists, per project convention
- **Archive member download button** — ZIP members now show "Download" (direct member extraction); non-ZIP archive members and archive files themselves show "Download Archive"; regular files unchanged ("Download Original")
- **"View Extracted" image no longer stretches small images** — full-width image view now uses `max-width: 100%` and centers the image; tiny images render at natural size instead of being blown up to fill the panel
- **Tree expand arrow larger** — increased from 11 px to 14 px for easier clicking
- **Clicking a ZIP file in the tree shows FileViewer** — previously set `panelMode = 'dir'` which hid the FileViewer; now opens the FileViewer so "Download Archive" and metadata are accessible
- **`kind` badge hidden for `raw` files** — files with `file_kind = "raw"` (e.g. image-based PDFs with no extractable text) no longer show a "raw" badge in the viewer toolbar

### Fixed

- **Archive indexing resumability** — the client now sends a `mtime=0` sentinel before submitting archive members and a completion upsert with the real mtime afterwards; if indexing is interrupted mid-archive, the next scan detects the `mtime=0` and re-indexes from scratch instead of leaving a partial member set

---

## [0.5.1] - 2026-03-03

### Added

- **Image split view** — images now open in a split layout by default: image on the left, EXIF/metadata on the right; "View Extracted" button switches to a full-width scrollable image view; "View Split" returns to the split layout
- **Duplicate file links in detail view** — when a file is a dedup alias, the canonical copy's path now appears in the metadata panel as `DUPLICATE: <clickable link>`; clicking navigates directly to that copy; handles both regular files and archive members
- **Inline viewing of images and PDFs inside archives** — images and PDFs that are members of a ZIP archive can now be viewed inline; the raw endpoint extracts the member from the outer ZIP on the fly (seeking directly via the ZIP central directory, no full-archive scan); non-browser-native image formats (e.g. TIFF) are converted to PNG server-side; members larger than 64 MB are refused to prevent OOM
- **Correctly-sized image loading placeholder** — the pulsing placeholder shown while an image loads is sized to the image's actual dimensions (parsed from EXIF `ImageWidth`/`ImageLength` or `[IMAGE:dimensions]` metadata); it fills the container when the image is larger than the viewport and shrinks to the image's natural size when it is smaller, matching the same `aspect-ratio` + `max-width` + `max-height` constraint the `<img>` element uses

### Fixed

- **Metadata shown as fixed header above content** — EXIF/tag metadata and duplicate-path banners were rendered in a `flex-shrink: 0` panel above the scrollable code area, potentially squeezing the content into a tiny window; they now render inside the scroll container so they consume no fixed space
- **Archive member content not shown** — `get_file_lines`, `get_metadata_context`, and `get_line_context` queried lines via a path JOIN which returned nothing for alias files (lines are stored under the canonical `file_id`); all three now call `resolve_file_id` (`COALESCE(canonical_file_id, id)`) and query by `file_id` directly
- **Duplicate alias paths shown as clickable links in search results** — the `+N duplicates` entries in search results were plain text; they now dispatch an `open` event identical to clicking the main result, navigating to that specific copy
- **Tree sidebar scrolls to active item** — when a file is opened and the tree auto-expands to reveal it, the highlighted row now scrolls into view (centred) using `scrollIntoView` after a tick
- **Image placeholder stuck loading forever** — `loading="lazy"` combined with `display: none` caused the browser to never fetch hidden images; removed `loading="lazy"` so images load immediately on render
- **Path line appearing as metadata** — zero-lines in the DB have no guaranteed order; `zeroLines.slice(1)` was unreliable when EXIF lines were inserted before the path line; now filters by `content !== compositePath` instead of relying on position
- **No metadata for JPEG images** — `extract_image_basic` only handled PNG/GIF/WebP/BMP; added JPEG SOF-marker scanning (reads up to 64 KB) to extract dimensions, bit depth, and colour mode; added `gif` and `bmp` to `is_image_ext` so those formats also run through the image extractor

## [0.5.0] - 2026-03-01

### Added

- **Serve original files (`GET /api/v1/raw`)** — the server can now stream original files directly from the filesystem; configure a root path per source via `[sources.<name>] path = "/..."` in `server.toml`; the endpoint validates paths (rejects `..`, leading `/`, and `::` archive-member paths) and canonicalizes to prevent traversal attacks
- **Cookie-based auth (`POST /api/v1/auth/session`)** — sets an `HttpOnly; SameSite=Strict` session cookie so browser-native requests (`<img src>`, iframes, download links) authenticate without custom headers; `check_auth()` now accepts either the `Authorization: Bearer` header or the `find_session` cookie; existing API clients are unaffected
- **Original file view in FileViewer** — images and PDFs show a "View Original" / "Extracted" toggle; all other file types show a "Download Original" link; TIFF and other non-browser-renderable images are converted to PNG on the fly via the `image` crate; download links always serve the real original file
- **Extension breakdown in stats panel** — the "By Kind" breakdown now has a Kind / Extension pill toggle; Extension mode shows file counts and sizes by file extension (top 20, with a "Show all N extensions" button); data is computed by new `get_stats_by_ext` SQL function using custom `file_basename` / `file_ext` SQLite scalar functions registered at connection open time (SQLite has no built-in equivalent)

### Fixed

- **Log ignore filters not suppressing subprocess messages** — `relay_subprocess_logs` now checks patterns via `logging::is_ignored()` before emitting each relayed line; previously the per-layer `event_enabled` hook was not reliably firing for relay events, so patterns like `pdf_extract: unknown glyph name` had no effect on subprocess output despite being configured
- **Extension stats returning empty** — the SQL query used `REVERSE()` which is not a built-in SQLite function; the query silently failed and returned an empty vec; replaced with custom `file_basename` / `file_ext` Rust scalar functions registered on the connection
- **`.ix` / `.ixd` / `.ixh` binary files indexed as text** — dtSearch index files start with a text-like ASCII header followed by binary control bytes, fooling the `content_inspector` sniff; added these extensions to the `is_binary_ext` list in the text extractor
- **Zero search results for queries hitting files with aliases** — `fetch_aliases_for_canonical_ids` was calling `row.get(0)` on a query selecting `(canonical_file_id INTEGER, path TEXT)`, reading the integer column as a string; rusqlite returned a type error that was swallowed by the search handler, yielding empty results for any query that matched deduplicated files; fixed by using `row.get(1)`
- **lopdf internal object-load errors logged as ERROR** — these are recoverable parse warnings emitted at ERROR level by lopdf's own tracing instrumentation; suppressed in the `find-extract-pdf` subprocess default filter (`lopdf=off`) since they are not actionable and clutter the output

## [0.4.0] - 2026-03-01

### Added

- **Server-side extraction fallback (`find-upload`)** — when `find-scan`'s extractor subprocess fails (OOM, corrupt file, etc.), the raw file can now be uploaded to the server for server-side extraction via a new resumable chunked upload API (`POST /api/v1/upload`, `PATCH /api/v1/upload/{id}`, `HEAD /api/v1/upload/{id}`); uploads resume automatically from the last acknowledged byte after a connection error; a new `scan.server_fallback = true` config option enables this path; a new `find-upload` binary lets you manually upload a specific file for indexing
- **`max_content_size_mb` replaces `max_file_size_mb`** — the config key is renamed and the semantics change: files are no longer skipped when they exceed the limit; instead, content is truncated at the limit and the file is always indexed (at minimum by filename); the old key is still accepted with a deprecation warning; all extractors updated to truncate rather than skip — the text extractor uses `.take()`, the archive extractor truncates per-member reads, the PDF extractor stops after hitting the byte limit
- **Archive extraction streams NDJSON** — `find-extract-archive` now emits one JSON object per line (NDJSON) as each member is extracted, rather than buffering all members into a single array; `find-scan` reads batches through a bounded channel (capacity 8) and processes them one at a time; this eliminates the parent-process OOM that occurred when a large archive (e.g. a zip containing many large text files) produced hundreds of MB of JSON output that was buffered in memory before parsing
- **Streaming file hash** — content hashes for deduplication now use a 64 KB streaming read instead of `std::fs::read` (which loaded the entire file into memory); eliminates OOM crashes when hashing large files in the main `find-scan` process
- **Deterministic scan order** — `find-scan` now sorts files alphabetically by relative path before processing; previously HashMap iteration order was randomised per-process, meaning a crash would hit a different file each run; with sorted order the same file appears in the logs each time, making OOM attribution straightforward
- **Richer scan progress log** — periodic progress now breaks out `new` (absent from server DB) and `modified` (mtime changed) counts separately from `unchanged`; makes it easy to distinguish a partially-populated server DB from actual file modifications; final summary line updated to match

### Fixed

- **Archive extraction OOM in parent process** — `cmd.output().await` buffered the entire subprocess stdout before returning; for a zip with many large members this could exceed 512 MB in the parent; fixed by switching the archive extractor to NDJSON streaming and reading line-by-line in the parent via a bounded channel
- **Large-file hash OOM** — `std::fs::read` in the archive and non-archive paths allocated the full file size in the parent process before the subprocess even ran; replaced with a 64 KB streaming hasher
- **Axum upload route syntax** — upload routes used the old axum v0.6 `:id` capture syntax; axum v0.7+ requires `{id}`; caused a panic at server startup with "Path segments must not start with `:`"
- **`find-watch` crash on `SubprocessOutcome`** — `watch.rs` was passing the result of `extract_via_subprocess` directly to `build_index_files` without unwrapping the new `SubprocessOutcome` enum, causing a compile error

### Changed

- **`ExtractionSettings` server config section** — server `server.toml` gains an `[extraction]` section (`max_content_size_mb`, `max_line_length`, `max_archive_depth`) used when extracting uploaded files server-side; defaults match the client defaults

## [0.3.1] - 2026-02-28

### Added

- **Subprocess-based extraction in find-scan (OOM isolation)** — `find-scan` now spawns `find-extract-*` subprocesses for all file extraction (matching the model already used by `find-watch`); if a subprocess OOMs or crashes (e.g. `lopdf` or `sevenz_rust2` calling `handle_alloc_error`), only the subprocess dies — `find-scan` logs a warning and continues to the next file; the three shared helper functions (`extract_via_subprocess`, `relay_subprocess_logs`, `extractor_binary_for`) are moved from `watch.rs` to a new `client/src/subprocess.rs`; a new `extract_archive_via_subprocess` returns the full `Vec<MemberBatch>` to preserve content hashes and skip reasons; the archive binary now outputs `Vec<MemberBatch>` (serializable) instead of the flat `Vec<IndexLine>`, so content hashes and per-member skip reasons are no longer lost when calling via subprocess; `scan.archives.extractor_dir` added to `ScanConfig` (same semantics as the existing `watch.extractor_dir`)
- **Remove PDF memory guards** — `scan.max_pdf_size_mb` and the dynamic available-memory check in `find-extract-pdf` are removed; with subprocess isolation these guards are no longer needed (an OOM only kills the subprocess); `available_bytes()` in `find_common::mem` remains, still used by the 7z solid-block guard in the archive extractor
- **Build hash in settings API** — `GET /api/v1/settings` now returns `schema_version` (current SQLite schema version) and `git_hash` (short commit hash, injected at compile time via `GIT_HASH` env var in mise tasks); makes it easy to confirm exactly what build is running without bumping the version
- **FTS row count in stats API** — `GET /api/v1/stats` now returns `fts_row_count` per source for diagnosing FTS index health
- **Windows POSIX emulation excludes** — default scan exclusions now cover MSYS2, Git for Windows, and Cygwin installation trees (`**/msys64/**`, `**/Git/mingw64/**`, `**/cygwin64/**`, etc.) to avoid indexing gigabytes of Unix toolchain binaries on Windows
- **7z OOM protection: dynamic memory guard** — before decoding each solid block, `find-scan` now reads `/proc/meminfo` (`MemAvailable`) and skips the block if the estimated decoder allocation would exceed 75% of available memory, emitting filename-only entries with a skip reason; blocks reporting zero unpack size (solid archives where sizes aren't stored in the header) are also skipped rather than risking an unrecoverable abort; addresses crashes on memory-constrained systems (e.g. 500 MB RAM NAS) where the LZMA dictionary allocation for a 120 MB block exhausted available memory
- **7z stabilisation probe** — `crates/extractors/archive/build.rs` probes at compile time for `std::alloc::set_alloc_error_hook` becoming available on stable Rust (tracking issue rust-lang/rust#51245); if stabilised, a `compile_error!` fires directing developers to the upgrade path in `docs/plans/034-7z-oom-crash.md`
- **Plan 034: 7z OOM crash** — documents the root cause, history of attempted fixes, current approach, and future options including `set_alloc_error_hook`
- **PDF OOM protection** — two-layer guard prevents lopdf from aborting the process on memory-constrained systems: a static `scan.max_pdf_size_mb` config limit (default 32 MB) skips oversized PDFs before reading their bytes; a dynamic `/proc/meminfo` check requires ≥4× the file's size of available memory before attempting extraction; both paths fall back to filename-only indexing with a WARN; `available_bytes()` extracted to `find_common::mem` and shared with the 7z extractor
- **Periodic indexing progress log** — `find-scan` now logs `"{N} indexed, {M} unchanged so far..."` every 5 seconds while skipping unchanged files, so scans with many unchanged files are no longer silent between the walk and the final summary
- **Lazy extraction header logging** — when a third-party crate (e.g. `lopdf`, `sevenz_rust2`) emits a WARN-or-above log during file extraction, `find-scan` now prefixes it with a single `INFO Processing <path>` line so the offending file is immediately identifiable; the header is emitted at most once per file and suppressed entirely for files that produce no warnings; events from `find_`-prefixed targets are excluded (our own warn! calls already include the path); plan 035 documents the design

### Fixed

- **FTS trigram tokenizer not applied to existing databases** — `CREATE VIRTUAL TABLE IF NOT EXISTS` silently skipped recreation of `lines_fts` when the trigram tokenizer was added to the schema, leaving existing installations with the unicode61 (word) tokenizer; `migrate_v6` now drops and recreates the table with the correct tokenizer; schema version bumped to 6
- **FTS search returning no results for short queries** — `build_fts_query` was wrapping all terms in double quotes, making them FTS5 phrase queries which require at least 3 trigrams (≥5 chars); 3–4 character queries like `"test"` silently returned zero results; terms are now passed unquoted in fuzzy mode (FTS5 special characters are stripped instead)
- **Worker SQLite writes now use explicit transactions** — each file's index operations are wrapped in a single `BEGIN`/`COMMIT` transaction, reducing write amplification and improving throughput on slow storage
- **Slow indexing steps now logged** — worker steps taking >500 ms emit a `WARN` with timing, making it easier to diagnose performance issues on slow NAS storage
- **Search result placeholder rows shorter than content rows** — placeholder skeleton lines in the search results card were driven to ~17 px by a 10 px space character, while content rows are ~21.5 px (13 px code font × 1.5 line-height); placeholder now has `min-height: 20px` and `align-items: center`

### Added

- **`scan.exclude_extra` config field** — new `exclude_extra` array in `[scan]` appends patterns to the built-in defaults without replacing them; `exclude` still replaces defaults entirely for users who need full control; `exclude_extra` is merged into `exclude` at parse time so the rest of the codebase sees one unified list
- **`find-admin check` shows build hash and schema version** — the check command now prints `Server version: X.Y.Z (build XXXXXXX, schema vN)` matching the info available from `GET /api/v1/settings`
- **Stabilisation probe for `set_alloc_error_hook`** — `mise run probe-alloc-hook` checks whether `std::alloc::set_alloc_error_hook` compiles on stable Rust yet (tracking rust-lang/rust#51245); pass = hook is stable and subprocess isolation can be replaced; fail = still nightly-only; replaces the old `build.rs` probe which was incompatible with cross-compiled ARM builds due to a glibc version mismatch

### Fixed

- **`last_scan` timestamp now recorded even if scan is interrupted** — previously the scan timestamp was only sent with the final batch; an interrupted scan left `find-admin status` showing "last scan: never" even when thousands of files had been indexed; the timestamp is now captured at scan start and included in every batch
- **Subprocess log lines no longer repeat the filename and binary name** — `relay_subprocess_logs` previously attached `file=` and `binary=` fields to every relayed line; the filename is already shown by the lazy extraction header (`Processing <path>`), so both fields are removed from individual log events

### Changed

- **Scan progress log format** — periodic progress message changed from `"{N} indexed, {M} unchanged so far..."` to `"processed {N+M} files ({M} unchanged) so far..."` to make clear that "indexed" is the count of new/changed files actually sent to the server, not the total files seen; batch-submit and final summary messages updated consistently
- **Dependency updates** — `rusqlite` 0.31→0.38, `reqwest` 0.12→0.13 (feature `rustls-tls`→`rustls`, new `query` feature required), `notify` 6→8, `colored` 2→3

---

## [0.3.0] - 2026-02-27

### Added

- **C# syntax highlighting** — `.cs` files now get full syntax highlighting in the file viewer; also removes the non-functional Haskell entry from the extension map
- **Document search mode** — new "Document" option in the search mode dropdown finds files where all query terms appear anywhere in the file (not necessarily on the same line); returns one result per file; when the file viewer is opened, all lines containing query terms are highlighted simultaneously; implemented via per-token FTS5 `DISTINCT file_id` queries intersected in Rust, with the best FTS-ranked line per file as the representative result
- **Context window preference** — the number of context lines shown in search result cards (previously only configurable in `server.toml`) is now also settable per-browser in the Preferences panel; options: 0 (match only), 1, 2, 3, or 5 lines; stored in localStorage and takes priority over the server default; a Reset button reverts to the server setting
- **Favicon** — added a magnifying glass favicon (`web/static/favicon.ico`) with 16/24/32/48/256px frames matching the app's blue accent colour (`#58a6ff`) on a transparent background
- **Screenshots in README** — four annotated screenshots (search results, file viewer, command palette, index statistics) added under `docs/screenshots/`; displayed in the README as a four-column table with expandable `<details>` panels
- **Demo images with EXIF metadata in seed script** — `seed-demo-db.py` now inserts two realistic demo image records (`photos/fujifilm-golden-gate.jpg`, `photos/pixel8-london-bridge.jpg`) with full EXIF metadata (camera, lens, exposure, GPS) written into a real content ZIP chunk so they appear correctly in the file viewer

### Fixed

- **Archive member sizes no longer inflate total indexed size** — `build_member_index_files` was propagating the outer archive's file size to every extracted member; a 1,000-member 10 GB zip was contributing ~10 TB to the "indexed" figure in the stats panel; members now correctly get `size = 0` since individual uncompressed sizes are not available at index time, while the outer archive's size is still counted via its own file record
- **Stats panel no longer flashes on background refresh** — the loading spinner was shown on every 2-second poll while indexing was active, causing a full re-render and making copy/paste impossible; background refreshes now update in place with no visible flash; only the very first page load shows the spinner
- **Settings page scrollbar now appears at the window edge** — the `.content` area had `max-width: 640px` with `overflow-y: auto`; the scrollbar rendered at the right edge of the 640px box, leaving empty dark space to the right; the constraint is removed so the scroll container fills the full available width

### Added

- **Worker status footer in stats panel** — the "currently indexing" indicator is moved from the cramped inline metrics strip to a dedicated full-width status bar at the bottom of the panel, showing the pulsing dot, source name, and full file path in monospace with ellipsis; displays "Idle" when the worker is not running
- **find-scan one-shot systemd unit** — `install.sh` now writes a `find-scan.service` oneshot unit alongside `find-watch.service` on Linux (user session and system variants) and a launchd plist on macOS; the initial scan is not started automatically — instructions to start it are printed at the end of installation; uses incremental scan (not `--full`) by default so reinstalls are safe; `--full` is documented as a manual override for forced re-indexing
- **Demo data scripts** — `docs/misc/generate-demo-data.py` creates synthetic files (Markdown, Rust, Python, TOML, JSON, JPEG with EXIF, zip and tar.gz archives) in `/tmp/find-demo/projects/` and `/tmp/find-demo/notes/` for use with `config/client.toml`; `docs/misc/seed-demo-db.py` seeds source databases with 500 synthetic file records across all supported kinds with a year of scan history, for populating the stats panel without a real index
- **pdf-extract fork: replace `get_encoding_map` with safe `parse()` call** — `type1_encoding_parser::get_encoding_map` calls `parse().expect()` which panics on malformed Type1 font data; replaced with a direct call to `parse()` (returns `Result`) with the encoding map logic inlined, so parse failures are logged as warnings instead of panicking; fork updated to rev `4e8e145`

### Added

- **Extraction skip reasons surfaced as indexing errors** — when an archive member is not extracted (too large, read failure, checksum mismatch), the reason is now recorded as an `IndexingFailure` and stored in the `indexing_errors` table, so users see an explanation in the file viewer ("⚠ Indexing error: too large to index (500 MB, limit 10 MB)") rather than a silently empty file; for 7z solid blocks that exceed the memory limit, one summary error is stored on the outer archive path and shown as a fallback for any member of that archive
- **7z solid-block memory limit** (`scan.archives.max_7z_solid_block_mb`, default 256 MB) — the LZMA decoder allocates a dictionary buffer proportional to the solid block's total unpack size regardless of individual file sizes; archives with blocks exceeding this limit are now skipped safely (filenames indexed, content not extracted) instead of aborting the process with an out-of-memory error; the default of 256 MB is conservative enough for memory-constrained systems such as NAS boxes (tested at 500 MB total RAM); lower the limit further in `client.toml` if needed
- **7z extraction refactored to per-block decoding** — `sevenz_streaming` now parses the archive header separately from the data stream, iterates blocks individually via `BlockDecoder`, and skips oversized blocks before the LZMA decoder (and its dictionary allocation) is ever created; previously used `ArchiveReader::for_each_entries` which created the decoder for every block unconditionally

### Added

- **Subprocess log integration** — standalone extractor binaries (`find-extract-pdf`, `find-extract-archive`, etc.) now initialise a `tracing-subscriber` logger writing to stderr (no timestamps, no ANSI, level filter from `RUST_LOG`, default `warn`); `find-watch` captures subprocess stderr and re-emits each line through its own tracing subscriber at the matching level with `binary` and `file` context fields, so extractor warnings pass through the same `log.ignore` filters as in-process events
- **Content deduplication** — files with identical content are stored only once; subsequent files with the same blake3 hash are recorded as aliases pointing to the canonical entry; search results show a `+N duplicates` badge that expands to reveal all duplicate paths; when the canonical file is deleted, the first alias is automatically promoted (chunk references reused, no ZIP rewrite); schema bumped to v5 (`content_hash`, `canonical_file_id` columns on `files` table)
- **pdf-extract path operator bounds checks** — the seven PDF path-construction operators (`w`, `m`, `l`, `c`, `v`, `y`, `re`) now guard against malformed PDFs that provide fewer operands than required; previously caused `index out of bounds` panics; now logs a warning and skips the operator so text extraction continues

### Added

- **Default configuration in TOML files** — built-in defaults for scan, watch, archives, and log settings are now defined in `crates/common/src/defaults_client.toml` and `crates/common/src/defaults_server.toml`, embedded into the binary at compile time via `include_str!` and parsed lazily on first access; the `default_*` functions in `config.rs` are now one-line delegates; a unit test (`embedded_defaults_parse`) verifies that both files parse correctly so TOML errors are caught at `cargo test` time
- **Expanded default scan exclusions** — the built-in `exclude` list now covers Linux virtual/runtime filesystems (`proc/`, `sys/`, `dev/`, `run/`, `tmp/`, `var/tmp/`, `var/lock/`, `var/run/`), Linux binary-only directories (`bin/`, `sbin/`, `lib/`, `lib64/`, `usr/bin/`, `usr/sbin/`, `usr/libexec/`, `usr/lib/debug/`), Windows system trees (`Windows/System32/`, `Windows/SysWOW64/`, `Windows/WinSxS/`, `Windows/Installer/`, `SoftwareDistribution/`, `Windows/Temp/`, `AppData/Local/Temp/`), and macOS caches (`Library/Caches/`); config-bearing paths (`/etc/`, `/usr/lib/systemd/`, `/usr/share/`) are intentionally kept; Linux root-level patterns omit the `**/` prefix so they only match at the scan root (not inside user home directories or data shares), while Windows patterns use `**/` since Windows trees appear nested in backup archives
- **Per-directory indexing control** — place a `.noindex` file in any directory to exclude it and all descendants from indexing; place a `.index` TOML file to override scan settings for a subtree (`exclude`, `max_file_size_mb`, `include_hidden`, `follow_symlinks`, `archives.enabled`, `archives.max_depth`, `max_line_length`); `exclude` is additive (appended to parent list), all other fields replace; both marker filenames are configurable via `scan.noindex_file` / `scan.index_file` in `client.toml`; control files themselves are never indexed; overrides are applied in `find-scan` (with per-directory caching) and `find-watch` (per-event, no cache needed)
- **Worker status in stats** — `GET /api/v1/stats` now returns a `worker_status` field (`idle` or `processing` with `source` and `file`); `find-admin status` prints a `Worker:` line showing `idle` or `● processing source/file`; the web Stats panel shows a pulsing dot and filename in the metrics strip while indexing is active

### Added

- **Configurable log-ignore patterns** — new `[log]` section in `client.toml` and `server.toml` with an `ignore` field (list of regex strings); any log event whose message matches a pattern is silently dropped before reaching the output formatter; default suppresses `"unknown glyph name"` noise from pdf-extract when processing PDFs with non-standard glyph names

### Fixed

- **Stats panel auto-refresh** — the Index Statistics panel now polls `GET /api/v1/stats` every 2 s while the worker is processing and every 30 s when idle, so pending count and worker status update without a manual page reload
- **pdf-extract panic on corrupt deflate stream in Type1 font** — `get_contents()` (which decompresses the font stream via lopdf) was called before the existing `catch_unwind`, so a lopdf panic on a corrupt deflate stream escaped; moved `get_contents()` inside the closure so both decompression failures and type1-encoding-parser panics are caught in one place
- **pdf-extract panic in `current_point()` on empty path** — replaced `self.ops.last().unwrap()` + catch-all `panic!()` with a safe `match`; returns `(0., 0.)` when the path has no ops or contains an unrecognised op type
- **Log-ignore filter broken for `log`-crate events** — `tracing-log` bridges external `log::warn!` calls into tracing using a fixed callsite whose `metadata().target()` is always the literal string `"log"`, not the originating crate name; the actual crate (e.g. `pdf_extract`) is stored in a `log.target` field on the event; the `LogIgnoreFilter` now reads that field and falls back to `metadata().target()` for native tracing events, so patterns like `"pdf_extract: unknown glyph name"` now correctly suppress noise from the external pdf-extract crate; also fixed the hardcoded default pattern which referenced `find_extract_pdf` (our wrapper crate) instead of `pdf_extract` (the external crate that actually emits the warnings)
- **pdf-extract panic on malformed Type1 font encoding** — wrapped `type1_encoding_parser::get_encoding_map` in `catch_unwind`; the underlying crate calls `.expect()` on parse failure so malformed Type1 font data previously panicked; now logs a warning and continues
- **pdf-extract panic on malformed font widths array** — replaced `assert_eq!` with `warn!` when a PDF's `Widths` array length doesn't match `last_char - first_char + 1`; some PDFs have more entries than declared and previously caused a hard panic; now logs a warning and continues
- **pdf-extract double logging on panic** — panic hook now emits a single `ERROR` line combining the file path and panic info, replacing the previous three separate log lines (hook file, hook panic info, Err arm message)
- **Scan mtime diff delay** — eliminated the upfront pass that stat'd every local file to build the `to_index` list before extraction began; mtime is now checked inline per file, skipping unchanged files before any config resolution or extraction work; also removes the double-stat on files that were indexed (mtime was previously fetched in the filter and again in the loop body); progress logging updated to show `"N to delete; processing M local files..."` and a final `"scan complete — N indexed, M unchanged, K deleted"` summary
- **Filesystem walk performance** — removed an `exists()` syscall per directory during the `.noindex` check in `walk_paths`; the marker file is now detected by filename in the walk loop body (zero extra syscalls in the common case), and any collected files under a `.noindex` directory are pruned in a single pass after the walk; added 5-second progress logging (`walking filesystem… N files found so far`) and a completion log line
- **`config/update.sh` path resolution** — script now uses `$(dirname "$0")` so it works when called from any working directory; also added `set -euo pipefail` to stop on first failure

### Fixed

- **PDF ZapfDingbats panic** — forked `pdf-extract` as `jamietre/pdf-extract` and fixed four `unwrap()` panics on unknown glyph names; the critical fix is in the core-font with-encoding path, which now tries both the Adobe Glyph List and the ZapfDingbats table before skipping silently; the other three sites use `unwrap_or(0)`; also replaced the permanently-silent `dlog!` macro with `log::debug!` so debug output is available to consumers that initialise a logger
- **PDF extraction hardened against malformed documents** — ~35 additional bad-input panic sites in the forked `pdf-extract` replaced with `warn!` + safe fallback so as much text as possible is returned even from broken PDFs; areas covered: UTF-16 decode (lossy instead of panic), unknown font encoding names (fall back to PDFDocEncoding), Type1/CFF font parse failures (skip with warning), Differences array unexpected types (skip entry), missing encoding/unicode fallback in `decode_char` (return empty string), Type0/CID font missing DescendantFonts or Encoding (fall back to Identity-H), ToUnicode CMap parse failures (skip map), malformed colorspace arrays (fall back to DeviceRGB), content stream decode failure (skip page), and bad operator operands (skip operator); `show_text` no longer panics when no font is selected

### Added

- **`mise run clippy` task** — runs `cargo clippy --workspace -- -D warnings` matching the CI check; CLAUDE.md updated to require clippy passes before committing Rust changes
- **`mise run build-arm` task** — cross-compiles all binaries for ARM7 (armv7-unknown-linux-gnueabihf) using `cross`, matching the CI release build and avoiding glibc version mismatches on NAS deployments
- **`mise run build-server` task** — builds web UI then compiles all binaries for x86_64 release
- **`DEVELOPMENT.md`** — new developer guide covering prerequisites, mise tasks, native and ARM7 build instructions (`cross` usage explained), linting, CI/release matrix, and project structure
- **Expanded default excludes** — added OS/platform-specific patterns: Synology (`#recycle`, `@eaDir`, `#snapshot`), Windows (`$RECYCLE.BIN`, `System Volume Information`), macOS (`__MACOSX`, `.Spotlight-V100`, `.Trashes`, `.fseventsd`), Linux (`lost+found`), and VCS (`svn`, `.hg`)
- **Full paths in extraction error messages** — PDF and other extraction errors now log the full file path (e.g. `/data/archive.zip::Contract.pdf`) instead of just the filename, making it easier to locate the problematic file

### Fixed

- **Clippy warnings** — fixed three clippy lints that were failing CI: `single_component_path_imports` in `scan.rs`, `collapsible_if` in `routes/admin.rs`, `collapsible_else_if` in `admin_main.rs`

### Added

- **File viewer metadata panel** — `line_number=0` entries that carry file metadata (EXIF tags, ID3 tags, etc.) are now shown in a dedicated panel above the code area, without line numbers; the file's own path line is omitted entirely since it is already displayed in the path bar
- **Search result filename/metadata match display** — results matched by filename or metadata (`line_number=0`) no longer display `:0` in the result header and show the matched snippet directly without a line number column; context is not fetched for these results
- **`mise dev` full dev environment** — `mise dev` now starts both the Rust API server (via cargo-watch) and the Vite dev server together, giving live reload for both Rust and Svelte/TypeScript changes
- **File viewer code table layout** — fixed table column widths so the code column claims all available horizontal space; previously `table-layout: auto` distributed spare width across all columns, making the line-number column much wider than needed and pushing code content toward the centre
- **Unified extraction dispatch** (`find-extract-dispatch` crate) — new crate that is the single source of truth for bytes-based content extraction; both the archive extractor and `find-client` now route all non-archive content through the same full pipeline (PDF → media → HTML → office → EPUB → PE → text → MIME fallback); archive members gain HTML, Office document, EPUB, and PE extraction that was previously only applied to regular files; eliminates a class of bugs where features added to the regular-file path were not reflected in archive-member extraction
- **Indexing error reporting** — extraction failures are now tracked end-to-end: the client reports them in each bulk upload, the server stores them in a new `indexing_errors` table (schema v4), and the UI surfaces them in a new **Errors** panel in Settings; the file detail view shows an amber warning banner when a file had an extraction error; the Stats panel shows an error count badge per source
- **`find-admin` binary** — unified administrative utility replacing `find-config`; subcommands: `config`, `stats`, `sources`, `check`, `inbox`, `inbox-clear`, `inbox-retry`
- **Admin inbox endpoints** — `GET /api/v1/admin/inbox` (list pending/failed files), `DELETE /api/v1/admin/inbox?target=pending|failed|all`, `POST /api/v1/admin/inbox/retry`; all require bearer-token auth
- **Disk usage stats** — statistics dashboard now shows SQLite DB size and ZIP archive size
- **`find-server --config` flag** — `find-server` now uses `--config <PATH>` (consistent with `find-scan`, `find-watch`, and `find-anything`); the flag defaults to `$XDG_CONFIG_HOME/find-anything/server.toml`, `/etc/find-anything/server.toml` when running as root, or `~/.config/find-anything/server.toml` otherwise; overridable with `FIND_ANYTHING_SERVER_CONFIG`
- **CLI reference** — new `docs/cli.md` with comprehensive documentation for all binaries: `find-server`, `find-scan`, `find-watch`, `find-anything`, `find-admin` (all subcommands), full config references, and extractor binary table
- **Startup schema check** — `find-server` now validates the schema version of every existing source database at startup and exits with a clear error if any are incompatible, rather than failing on the first query
- **`find-admin inbox-show <name>`** — new subcommand that decodes and summarises a named inbox item (by filename, with or without `.gz`); searches the pending queue first, then failed; marks the result `[FAILED]` if found in the failed queue; accepts `--json` for raw output; implemented via a new `GET /api/v1/admin/inbox/show?name=<name>` endpoint
- **Exclude patterns applied to archive members** — `scan.exclude` globs (e.g. `**/node_modules/**`, `**/target/**`) now filter archive members in the same way they filter filesystem paths; previously, archives containing excluded directories (such as Lambda deployment ZIPs with bundled `node_modules`) would index all their members regardless of the exclude config

### Removed

- **`find-config` binary** — replaced by `find-admin config`

### Fixed

- **`find-admin`/`find-scan` config not found when running as root** — client tools now look for `/etc/find-anything/client.toml` when running as root (UID 0) before falling back to `~/.config/find-anything/client.toml`; matches the existing behaviour for `find-server` and aligns with the system-mode install layout where `client.toml` is placed in `/etc/find-anything/`
- **Empty `sources` list rejected at parse time** — `[[sources]]` is now optional in `client.toml`; a config with only `[server]` is valid; `find-scan` exits cleanly with a log message when no sources are configured, allowing a minimal server-side config to be used by admin tools without scan configuration
- **Archive OOM on solid 7z blocks** — `entry.size()` in sevenz-rust2 returns 0 for entries in solid blocks, bypassing the pre-read size guard and allowing unbounded allocation; all three archive extractors (ZIP, TAR, 7z) now use `take(size_limit + 1)` as a hard memory cap on the actual read, independent of the header-reported size; oversized members index their filename only and the stream is drained to maintain decompressor integrity
- **Archive members misidentified as `text`** — files inside archives with unknown or binary extensions (e.g. ELF executables, `.deb` packages, files with no extension) were previously labelled `text`; dispatch now always emits a `[FILE:mime]` line for binary content using `infer` with an `application/octet-stream` fallback; `detect_kind_from_ext` now returns `"unknown"` for unrecognised extensions instead of `"text"`; the scan pipeline promotes `"unknown"` to `"text"` only when content inspection confirms the bytes are text
- **`mise dev` Ctrl+C not stopping server** — hitting Ctrl+C left `find-server` running and caused `address already in use` on the next start; `cargo-watch` is now launched via `setsid` so it leads its own process group, and the trap sends `SIGTERM` to the entire group (cargo-watch + cargo run + find-server) rather than just the top-level process
- **Streaming archive extraction** — archive members are now processed one at a time via a bounded channel; lines for each member are freed after the batch is submitted, keeping memory usage proportional to one member rather than the whole archive; nested ZIP archives that fit within `max_temp_file_mb` are extracted in-memory (no disk I/O), larger ones spill to a temp file, and nested 7z archives always use a temp file (required by the 7z API); nested TAR variants are streamed directly with zero extra allocation
- **Archive scan progress** — `find-scan` now logs `extracting archive <name> (N/M)` when it begins processing each archive, so long-running extractions are visible rather than appearing stuck at `0/M files completed`
- **Archive batch progress log** — the mid-archive batch submission log now shows per-batch member count alongside the cumulative total (e.g. `102 members, 302 total`), making it clear when the 8 MB byte limit (rather than the 200-item count limit) triggered the flush
- **`include_hidden` applied to archive members** — archive members whose path contains a hidden component (a segment starting with `.`) are now filtered according to the `include_hidden` config setting, consistent with how the filesystem walker filters hidden files and directories
- **Corrupt nested archive log noise** — "Could not find EOCD" and similar errors for unreadable nested archives are now logged at DEBUG instead of WARN; the outer member filename is still indexed regardless
- **`mise inbox` / `inbox-clear` tasks** — fixed missing `--` separator causing `--config` to be parsed by `cargo run` instead of the binary; added both tasks to `.mise.toml`
- **Archive member line_number=0 duplicate** — archive members were being indexed with two `line_number=0` entries: one from the extractor (containing only the member filename) and one added by the batch builder (containing the full composite path); the extractor's version is now discarded, leaving exactly one path line per member
- **Content archive corruption recovery** — if the most recent content ZIP was left incomplete by a server crash (missing EOCD), the server previously failed every subsequent inbox request that tried to append to it; it now detects the corrupt archive on startup and skips to a new file instead
- **Multi-source search query** — searching with more than one source selected produced `Failed to deserialize query string: duplicate field 'source'`; the search route now parses repeated `?source=a&source=b` params correctly using `form_urlencoded` rather than `serde_urlencoded`
- **7z solid archive CRC failures** — files in a solid 7z block that were skipped due to the `max_file_size_mb` limit were not having their bytes drained from the decompressor stream; this left the stream at the wrong offset, causing every subsequent file in the block to read corrupt data and fail CRC verification; the reader is now always drained on size-limit skips
- **7z archive compatibility** — replaced `sevenz-rust` with `sevenz-rust2` (v0.20); adds support for LZMA, BZIP2, DEFLATE, PPMD, LZ4, ZSTD codecs inside 7z archives, fixing widespread `ChecksumVerificationFailed` errors on real-world archives; 50% faster decompression on LZMA2 archives
- **Archive log noise** — read failures for binary members (images, video, audio) inside ZIP, TAR, and 7z archives are now logged at DEBUG instead of WARN
- **Logging** — unknown config key warnings now always appear; default log filter changed to `warn,<crate>=info` so warnings from all crates (including `find-common`) are visible; `find-config` and `find-anything` now initialize a tracing subscriber so they emit warnings too
- **Schema version check** — `find-server` now detects incompatible (pre-chunk) SQLite databases on startup and prints a clear error with instructions to delete and rebuild, instead of crashing with a cryptic SQL error
- **Archive content extraction** — fixed a bug where any archive member whose file extension was not in the known-text whitelist (dotfiles, `.cmd`, `.bat`, `.vbs`, `.ahk`, `.reg`, `.code-workspace`, `.gitignore`, etc.) had its content silently skipped; content sniffing now operates on in-memory bytes rather than attempting to open a non-existent on-disk path
- **Text extension whitelist** — added Windows script formats (`.cmd`, `.bat`, `.vbs`, `.ahk`, `.au3`, `.reg`), editor/IDE project files (`.code-workspace`, `.editorconfig`), and common dotfile names (`.gitignore`, `.gitattributes`, `.gitmodules`, `.dockerignore`) as recognised text types
- **Archive resilience** — ZIP and TAR extractors now skip corrupt or unreadable entries with a warning and continue processing the rest of the archive, rather than aborting on the first error; 7z read errors are now logged with the entry name instead of silently discarded
- **Archive size limit** — archive container files (`.zip`, `.tar.gz`, `.7z`, etc.) are now exempt from the whole-file `max_file_size_mb` check; the per-member size limit inside the extractor still applies, so individual oversized members are skipped while the rest of the archive is processed
- **Archive memory safety** — ZIP, TAR, and 7z extractors now check each entry's uncompressed size header before reading into memory; oversized members are skipped without allocating, preventing OOM on archives containing very large individual files
- **Error chain logging** — extraction failures in `find-scan` now use `{:#}` formatting to print the full anyhow error chain (e.g. `opening zip: invalid Zip archive: …`) rather than just the outermost context string
- **Tree infinite-nesting bug** — expanding a subdirectory inside an archive (e.g. `archive.7z → settings/`) no longer produces an infinite cascade of empty arrow nodes; archive virtual directory entries now carry a trailing `/` in their path so the server correctly strips the prefix on the next `listDir` call

---

## [0.2.5] - 2026-02-24

### Changed

- `max_file_size_kb` renamed to `max_file_size_mb`; default changed from 1 MB to 10 MB
- `find-anything` binary renamed from `find` to avoid conflict with the coreutils `find` command

### Added

- **`find-config`** — new binary that shows the effective client configuration with all defaults filled in; also warns on unknown config keys
- **Unknown config key warnings** — all three client binaries and `find-server` now emit a `WARN` log for any unrecognised TOML keys
- **Default config path** — all client tools now default to `~/.config/find-anything/client.toml`; overridable via `FIND_ANYTHING_CONFIG` env var or `XDG_CONFIG_HOME`
- **About tab** in Settings — shows server version and a "Check for updates" button
- **Scan progress** — `find-scan` now logs `X/Y files completed` on each batch submission
- **armv7 build target** — supports Synology NAS and other 32-bit ARM Linux devices
- **Restart instructions** — install script prints the correct `systemctl restart` command after an upgrade
- **Server connectivity check** — client install script tests the server URL before proceeding

### Fixed

- `find-server` invocation now uses positional config path argument (not `--config` flag)
- Install scripts: all `read` prompts work correctly when piped via `curl | sh`
- systemd detection: check `/run/systemd/system` presence rather than `systemctl --user status`
- Synology DSM: install script falls back to system-level service unit with `sudo mv` instructions

### Removed

- `ocr` config setting (was never implemented)

---

## [0.2.4] - 2024-12-01

### Added

- **Windows Inno Setup installer** — wizard-style installer with server URL/token/directory prompts; writes `client.toml`; registers `find-watch` as a Windows service
- **`install-server.sh`** — dedicated server installer; configures systemd (system or user mode), generates a secure bearer token, writes annotated `server.toml`
- **`install.sh` improvements** — interactive prompts for URL, token, source name, and directories; generates annotated `client.toml`; sets up `find-watch` systemd user service
- **WinGet manifest** — `Outsharked.FindAnything` package with inno and zip installer entries
- **Unified settings page** — sidebar nav with Preferences, Stats, and About tabs

### Changed

- Release pipeline builds web UI and embeds it into `find-server` binary
- `install.sh` and `install-server.sh` split from a single combined script

---

## [0.2.3] - 2024-11-15

### Added

- **Infinite scroll** — preemptively loads next page when near bottom; cross-page deduplication prevents duplicate keys
- **Lazy context loading** — `IntersectionObserver` fetches context only when result card is visible
- **Command palette** — Ctrl+P opens a file-search palette across all indexed sources
- **Markdown rendering** — `.md` files rendered as HTML in the file viewer with raw/rendered toggle
- **Debounced live search** — 500ms debounce; previous results stay visible while new search is in-flight

### Changed

- Frontend refactored into `SearchView`, `FileView`, `appState` coordinator modules
- `ContextResponse` now returns `{start, match_index, lines[], kind}`
- Server routes split into `routes/` submodule (search, context, file, tree, bulk, settings)
- Page-scroll architecture replaces inner scroll container

---

## [0.2.2] - 2024-11-01

### Added

- **Windows support** — native x86_64-pc-windows-msvc builds
- **`find-watch` Windows Service** — self-installing via `windows-service` crate with `install`/`uninstall`/`service-run` subcommands
- **`find-tray` system tray** — Windows tray icon with Run Full Scan, Start/Stop Watcher, Open Config, and Quit actions
- **`install-windows.ps1`** — downloads latest release, extracts to `%LOCALAPPDATA%`, creates config, installs service

---

## [0.2.1] - 2024-10-15

### Added

- **`find-extract-html`** — strips tags, extracts `[HTML:title]`/`[HTML:description]` metadata and visible text
- **`find-extract-office`** — indexes DOCX paragraphs, XLSX/XLS/XLSM rows, PPTX slide text; title/author metadata
- **`find-extract-epub`** — full chapter text; `[EPUB:title/creator/publisher/language]` metadata
- New `"document"` file kind for docx/xlsx/xls/xlsm/pptx/epub

---

## [0.2.0] - 2024-10-01

### Added

- **GitHub Actions CI** — `cargo test`, `cargo clippy`, and web type-check on every push/PR
- **Binary release matrix** — Linux x86_64/aarch64, macOS arm64/x86_64; platform tarballs on GitHub Releases
- **Docker** — multi-stage `find-server` image; `docker-compose.yml` with data volume
- **`install.sh`** — `curl | sh` installer; auto-detects platform, fetches latest release

---

## [0.1.9] - 2024-09-15

### Added

- **`find-watch` daemon** — inotify/FSEvents/ReadDirectoryChanges watcher with configurable debounce
- **Rename handling** — both sides of a rename processed correctly after debounce window
- **Subprocess extraction** — spawns `find-extract-*` binary per file type
- **Systemd unit files** — user-mode and system-mode units with installation docs

---

## [0.1.8] - 2024-09-01

### Changed

- **Extractor architecture refactor** — each extractor is now a standalone binary (`find-extract-text`, `find-extract-pdf`, `find-extract-media`, `find-extract-archive`) and a shared library crate

---

## [0.1.7] - 2024-08-15

### Added

- **Markdown YAML frontmatter** — title, author, tags, and arbitrary fields indexed as `[FRONTMATTER:key] value`

---

## [0.1.6] - 2024-08-01

### Changed

- **Archive subfolder organization** — `sources/content/NNNN/` thousands-based structure; capacity ~99.99 TB

---

## [0.1.5] - 2024-07-15

### Added

- **Word wrap toggle** — toolbar button with localStorage persistence
- **Source selector dropdown** — replaces pill-based filter; scales to many sources

---

## [0.1.4] - 2024-07-01

### Added

- **Video metadata** — format, resolution, duration from MP4, MKV, WebM, AVI, MOV and more

---

## [0.1.3] - 2024-06-15

### Added

- **Archive members as first-class files** — composite `archive.zip::member.txt` paths; each member has its own `file_id`
- **Command palette** — Ctrl+P file search across all indexed sources
- **Improved fuzzy scoring** — exact substring matches get a large score boost

### Changed

- `FilePath` class refactor — unified path representation eliminates sync issues

---

## [0.1.2] - 2024-06-01

### Added

- **`GET /api/v1/tree`** — prefix-based directory listing using range-scan SQL
- **Directory tree sidebar** — collapsible tree with lazy loading
- **Breadcrumb navigation** — clickable path segments; clicking a directory shows directory listing
- **Atomic archive deletion** — SQLite transaction stays open across ZIP rewrite; rolls back on failure

---

## [0.1.1] - 2024-05-15

### Added

- **ZIP-backed content storage** — file content in rotating 10 MB ZIP archives; SQLite holds only metadata and FTS index
- **Async inbox processing** — client submits gzip-compressed batches; server worker polls and processes asynchronously
- **Contentless FTS5 index** — `lines` table stores chunk references; schema v2
- **Auto-migration** — detects and drops v1 schema on startup

---

## [0.1.0] - 2024-05-01

### Added

- Full-text search with FTS5 trigram indexing
- Fuzzy, exact, and regex search modes
- Multi-source support
- Archive content indexing (zip, tar, tar.gz, tar.bz2, tar.xz, 7z)
- Incremental scanning based on mtime
- File exclusion patterns (gitignore-style globs)
- PDF text extraction
- Image EXIF metadata (camera, GPS, dates)
- Audio metadata (ID3, Vorbis, MP4 tags)
- SvelteKit web UI with live search, file preview, and source filtering
