# Changelog

All notable changes to find-anything are documented here.

Format: [Keep a Changelog](https://keepachangelog.com/en/1.0.0/)
Versioning: [Semantic Versioning](https://semver.org/spec/v2.0.0.html)

---

## [Unreleased]

### Added

- **Inbox circuit breaker** — the inbox worker now tracks consecutive request processing timeouts; after `inbox_timeout_circuit_breaker` (default 5) consecutive timeouts it automatically pauses the inbox and optionally sends an alert email via SMTP (`[alerts]` config block with `smtp_host`, `smtp_port`, `smtp_encryption`, `smtp_username`, `smtp_password`, `smtp_from`, `admin_email`). The counter resets on any successful request or manual `/api/v1/admin/inbox/resume`.
- **SMTP alert emails** — new `[alerts]` config section supports sending a notification email when the inbox circuit breaker trips; uses `lettre` with STARTTLS (default), TLS, or plaintext; no fallback to sendmail.


### Fixed

- **Server crash (OOM) on large archive batches** — `process_request_phase1` serialised the entire normalised `BulkRequest` to a `Vec<u8>` before writing it to the `.gz` inbox file, holding the full content in memory twice simultaneously. For a large 7z batch this could double peak memory (e.g. 800 MB of extracted content → ~1.6 GB peak). Fixed by streaming directly into the `GzEncoder` via `serde_json::to_writer`.
- **Batch formatter could hang indefinitely** — `apply_batch_formatter` called `std::process::Command::status()` with no timeout. A hung or slow prettier/biome run (e.g. a large YAML file) would block the inbox worker forever. Fixed with a 60-second timeout matching the per-file formatter pattern.
- **Search result line numbers off-by-one near chunk boundaries** — `chunk_blob` used `current.is_empty()` to decide whether to prepend a `\n` separator. When an empty line fell exactly at a chunk boundary, `push_str("")` left `current` empty even though the line's position had been recorded as `chunk_start`. The following non-empty line then also skipped its separator, causing all subsequent blob positions to be shifted -1 relative to their FTS `line_number`s, producing wrong context lines and incorrect line number display in search results.
- **Potential write-lock contention on startup** — idempotent `CREATE INDEX IF NOT EXISTS` statements were run inside the per-request `open()` path; under concurrent startup they could contend on SQLite's WAL mutex. Moved to `check_all_sources()` which runs once at startup under no concurrency. `idx_duplicates_file_id` also added to the v4 schema so fresh installs get it without migration.
- **Context lines carried server-internal line numbers** — `ContextResponse.lines` was `Vec<String>` so the client had to reconstruct line numbers as `start + index`, which is incorrect for sparse files (e.g. PDFs with gaps). Each line now carries its own `line_number` in `Vec<ContextLine>`; `find-query` and the web UI both updated to use `line.line_number` directly
- **`content_line_start` compat shim removed** — the `content_line_start` field in `/api/v1/settings` and the `contentLineStart` Svelte store were added to support old servers that used `line_number = 1` for the first content line; the current scheme (`LINE_CONTENT_START = 2`) is now assumed unconditionally
- **Directory renames not watched after rename** — when `find-watch` detected a directory rename pair, the new directory path was removed from the batch (correct) but `register_dir` was never called for it, so the new location had no inotify watch. Changes inside the renamed directory were silently missed until the next full rescan.
- **No loading cursor while expanding tree directories** — expanding a directory node in the file tree showed no feedback while the server request was in flight; the row, arrow, and name now show `cursor: wait` during the load
- **Inbox worker could deadlock SQLite under lock contention** — `store.get_lines()` (a read from `blobs.db`) was called inside an open write transaction on the source DB, unnecessarily widening the write lock window. Moved the content-store read to before the transaction opens so the two databases are never locked simultaneously.
- **Timed-out `spawn_blocking` tasks held SQLite write locks indefinitely** — dropping a `JoinHandle` from a `tokio::task::spawn_blocking` task does not cancel the underlying OS thread; a timed-out Phase 1 worker continued holding its connection (and any write lock) until it finished or was killed. Fixed by passing a `rusqlite::InterruptHandle` back to the async timeout via a oneshot channel; on timeout `handle.interrupt()` is called, causing `SQLITE_INTERRUPT` in the blocked thread so it unblocks immediately.
- **Default inbox request timeout reduced from 1800 s to 120 s** — Phase 1 does only SQLite writes (no extraction), so 1800 s allowed a single stuck request to block the inbox for 30 minutes before the circuit breaker could trip; 120 s is generous given the 30 s SQLite busy timeout.
- **Document mode shows all matching lines** — `doc:` search now returns one result per matching line (any keyword) for each qualifying file, rather than a single representative. Lines are capped at 20 per file; a `+` badge on the hit counter indicates truncation. Hits are sorted by line number and navigable via chevrons.
- **`cross_filesystems` config option** — `[scan] cross_filesystems = false` (default) prevents the walker from descending into directories on a different device than the walk root, avoiding accidental traversal of mounted backup volumes, borg archives, network shares, and bind mounts. Set to `true` to restore the previous behaviour of crossing filesystem boundaries.
- **No-auth server support** — the web UI now attempts to connect without a token first; `initialLoad()` shows the token dialog only on an `AuthError`, so servers with no authentication configured load immediately without prompting.
- **Windows service starts immediately after install** — `install_service` now calls `service.start()` after creating the service so no reboot or manual `sc start` is needed; output message updated accordingly
- **Windows installer task checkboxes** — "Start file watcher service" and "Run full scan now" are now proper Inno Setup `[Tasks]` checkboxes rather than a fixed `[Run]` entry, so they run in the same elevated context as the rest of the install
- **Configurable formatter timeouts** — `batch_formatter_timeout_secs` (default 60) and `per_file_formatter_timeout_secs` (default 10) added to `[normalization]` in `server.toml`; previously hardcoded constants with `#[cfg(test)]` overrides


---

## [0.7.2] - 2026-04-01

### Added

- **`source:` prefix in search help** — `source:` is now listed under "Scope prefixes" in the `?` help panel
- **Native iWork extraction** — `.pages`, `.numbers`, and `.key` files are extracted natively without requiring Java or Apache Tika; the archive extractor reads `.iwa` (Snappy-compressed protobuf) files and extracts text directly; all iWork logic is organised in `crates/extractors/archive/src/iwork.rs`
- **Old-format iWork support** — pre-2013 iWork files using XML (`index.apxl` / `index.xml`) instead of `.iwa` protobuf are detected and their text extracted via XML tag stripping
- **iWork preview in file viewer** — the embedded `preview.jpg` inside iWork documents is shown in the file viewer with a "View Preview" / "View Extracted" toolbar toggle
- **Image rotate buttons** — left/right 90° rotate buttons added to the image viewer toolbar; rotation resets on new image load

### Fixed

- **iWork content truncation** — text from `.iwa` files is split on embedded newlines before indexing, preventing multi-line strings from being stored as a single line and truncating subsequent content
- **Document kind not normalized** — `FileKind::Document` is now included in `is_text_like()`, so iWork and other document-kind files have `max_line_length` normalization applied server-side

### Changed

- **Removed `find-extract-iwork` / Tika dependency** — the `find-extract-iwork` binary (Java/Tika wrapper), `server_only` extractor routing, and all associated configuration have been removed; iWork extraction is now fully native

---

## [0.7.1] - 2026-03-24

### Added

- **`GET /api/v1/tree/expand` endpoint** — returns all ancestor directory listings needed to reveal a given path in a single response, replacing N parallel round-trips with one request per navigation
- **ffprobe video metadata extraction** — opt-in via `ffprobe_path` in `[scan]` config; when configured, ffprobe is used exclusively for video files (replacing nom-exif) and emits `[VIDEO:format]`, `[VIDEO:codec]`, `[VIDEO:resolution]`, `[VIDEO:fps]`, `[VIDEO:audio_codec]`, `[VIDEO:audio_channels]`, and `[VIDEO:duration]`; `SCANNER_VERSION` bumped to 7 to trigger re-extraction
- **`code` file kind** — source code, config, and markup files (`.rs`, `.py`, `.js`, `.ts`, `.json`, `.yaml`, `.html`, `.css`, `.sql`, etc.) now use kind `"code"` instead of `"text"`; plain text kind (`"text"`) is now reserved for human-readable documents (`.md`, `.txt`, `.log`, `.csv`, `.rst`); `FileKind::Code` added; `SCANNER_VERSION` bumped to 8
- **File type filter grouped and refined** — kind checkboxes are now grouped into Documents / Media / Other with category labels; Code and Text are separate entries; DICOM merged under Image (UI sends `['image', 'dicom']` to server via `expandKindsForServer`); eBook and Office are separate entries
- **Video codec warning in player** — shows an amber banner when the browser can't decode the video track (`videoWidth === 0` after `loadedmetadata`) advising the user to open in VLC; container name is shown in the message
- **Image adjustment panel** — new toolbar button in the image viewer opens a floating panel with Invert, Flip H, Flip V toggles and Brightness/Contrast sliders; applied as pure CSS filters with no library dependency

### Fixed

- **Content store not updated on re-extraction** — `archive_batch` previously skipped storing a blob if the `file_hash` key already existed, so re-indexing an unchanged file (e.g. after a `SCANNER_VERSION` bump) never updated the stored content; `put_overwrite` now deletes the old blob and writes the new one unconditionally
- **Office template files not extracted** — `.dotm`, `.dotx`, `.docm`, `.xltx`, `.xltm`, `.pptm`, `.potx`, `.potm` were misidentified as ZIP files; they are now handled by the Office extractor
- **epub files misclassified as `document`** — `detect_kind_from_ext("epub")` now returns `"epub"` instead of `"document"`; `SCANNER_VERSION` bumped to 8 to trigger re-extraction
- **Code viewer triangle causes layout shift** — clicking a line now keeps the `▶` arrow column at constant width using `visibility: hidden` instead of toggling empty text
- **NLP date pill X button did not dismiss** — clicking X now strips the detected date phrase from the query text (`nlpResult.query`) instead of setting a suppression flag; the NLP result is therefore not re-triggered on the next search
- **URL not updated when NLP-cleared query is empty** — the early-return path in `doSearch` now calls `replaceSearchState()` so the URL reflects the cleared query

### Fixed

- **find-watch: new directory contents not indexed** — when a directory is created while the watcher runs, any files already inside it were never indexed because no inotify watch existed yet; the directory-create handler now walks the new directory and submits any existing files immediately after registering the watch (W7 regression test added)
- **Tree sidebar not showing new directories via SSE** — `TreeRow` live-update check only refreshed on immediate-parent events; changed to refresh on any descendant path so new subdirectories appear without a page reload
- **Stray `▶` arrow on metadata matches with empty snippet** — the match-line arrow is now suppressed when the FTS5 snippet is empty (e.g. short DICOM tag values)
- **find-watch debug log flooded with Access events** — `Access(Open)` events from file reads (e.g. cargo) are no longer logged at debug level since they are always silently dropped

### Changed

- **find-watch: stale inotify watch errors logged at debug** — when a watched directory is deleted the kernel auto-removes the watch descriptor; `notify` may surface this as `PathNotFound` / `WatchNotFound` / `Io(NotFound)`; these are now logged at `debug` instead of `warn` since no action is needed
- **Tree sidebar expansion optimised to one request** — `prefetchTreePath` now calls `/api/v1/tree/expand` instead of N parallel `/api/v1/tree` calls; concurrent `TreeRow` auto-expansions share a single in-flight promise via `treeCache`; `DirectoryTree` root fetch is skipped when the expand response already populated the cache; prefetch is now also fired on search-result navigation (not just command-palette)

---

## [0.7.0] - 2026-03-24

### Added

- **DICOM support** — metadata extraction (`find-extract-dicom`) indexes DICOM tags (PatientName, Modality, StudyDescription, dimensions, etc.); inline PNG preview via `find-preview-dicom` subprocess (served through the unified `GET /api/v1/view` endpoint); `FileKind::Dicom` variant added; extensionless DICOM files detected via magic bytes (DICM at offset 128); JPEG2000 decoding available as optional `jpeg2000` Cargo feature
- **Unified `GET /api/v1/view` image endpoint** — replaces the separate `/api/v1/raw?convert=png` (where the client guessed the format from the file extension) and `/api/v1/dicom-preview`; the server looks up the file's `kind` in the source DB and decides: serve native bytes for browser-compatible formats (detected by magic bytes), convert to PNG via the `image` crate for other image kinds, or run `find-preview-dicom` for DICOM; the client uses a single `viewUrl` for all inline image and DICOM display
- **DICOM metadata uses `[DICOM:Tag] value` format** — consistent with EXIF `[EXIF:Make] Apple` so `parseMetaTags` can parse and display each field in the metadata drawer; previously bare values like `study: 20260323` were not parsed; `SCANNER_VERSION` bumped to 6 to trigger re-extraction
- **`[fa:duplicate]` prefix for duplicate path entries in metadata** — `FileViewer` no longer guesses by absence of `[`; only `[fa:duplicate] `-prefixed entries are treated as duplicate paths; untagged metadata (DICOM) goes to `metaLines` instead of `duplicatePaths`
- **`source:` search prefix** — restricts results to a specific source and optional path prefix; format `source:<source-name>[/path/to/dir]`; server applies `WHERE (path = ? OR path LIKE ?/%)` in both FTS5 and document-search modes
- **`source:` typeahead with auto-advance** — typing `source:` opens a keyboard-navigable dropdown showing available sources; selecting one immediately fetches and recursively auto-advances through single-option directory levels until multiple choices are found; the entire path is resolved silently before any UI update (`resolveAutoPath` is a pure async function with no intermediate state mutations); desktop-only (hidden on mobile)
- **`← results` button hidden on deeplink** — button now only renders when `results.length > 0`; navigating directly to a file URL no longer shows a back button that leads nowhere
- **Media metadata scrollable container on mobile** — EXIF/audio/video metadata no longer appears in a separate inner-scroll box below the media; `MetaDrawer` mobile styles removed `overflow-y: auto` / `max-height: 40vh` so content flows naturally; `FileViewer` outer container switches to `overflow-y: auto` on mobile; `AudioViewer` and `VideoViewer` stack vertically with a divider instead of side-by-side
- **`?` help button missing on desktop** — removed erroneous global `.help-wrap-outer { display: none; }` rule that was hiding the button at all viewport widths; it is now visible on desktop and hidden only on mobile (where the logo tap opens the mobile help panel instead)
- **`find-watch` file content not stored in blob store** — `handle_update` never computed the blake3 hash, so `file_hash` was always `None` in the bulk request; Phase 2 had no key to store the blob, leaving the file viewer empty even though the file was searchable via FTS5; fix: compute hash via shared `batch::hash_file` and attach it to the `IndexFile`; `hash_file` consolidated from duplicate copies in `scan.rs` and `watch.rs` into `batch.rs`
- **`find-watch` hang during startup on older kernels** — separated directory walk from inotify watch registration; walkdir was generating inotify OPEN events for every directory it opened, flooding the notify background thread's read loop and deadlocking the `watch()` channel on kernel 3.10; all directories are now collected first (before any watches exist), then registered in a second pass
- **`find-watch` only watched 1 directory when no `include` patterns configured** — `include_dir_prefixes(&[])` returns `Some({})` (an empty terminal set that prunes everything); guard added so an empty include list yields `None` (watch everything), matching the behaviour of `find-scan`
- **`.index` include patterns now prune directory traversal in `find-watch`** — `walk_source_tree` (shared by `find-scan` and `find-watch`) now reads `.index` files during the DFS and skips sibling subtrees excluded by their `include` field, giving both binaries identical directory-pruning behaviour
- **Svelte CSS warnings for extracted SVG icon components** — fixed unused selector warnings by replacing `.foo svg` with `.foo :global(svg)` in `SearchBox.svelte`, `AdvancedSearch.svelte`, and `+page.svelte`
- **`find-scan` not found in upload integration tests** — `resolve_find_scan` now also checks the parent of the current exe's directory; test binaries live in `target/debug/deps/` but `find-scan` is built one level up in `target/debug/`, so the previous code fell back to PATH (which works locally if `find-scan` is installed, but never in CI)
- **Watch integration tests W1–W6** — all watch tests now reliably pass: `batch_window_secs` set to 50 ms in test config (default 5 s caused timeouts); `start_watcher` waits 500 ms for inotify registration before tests touch the filesystem; `start_watcher_with_config` extracted so W5 (external extractor) shares the same startup wait; W6 added as regression test for the blob-store bug (create empty file, add content, verify both search and file-viewer content are correct); `get_file_lines` helper added to `TestEnv`
- **SVG icon components** — all inline SVGs extracted to `web/src/lib/icons/` as reusable Svelte components (21 icons: Back, Check, Clear, Copy, Download, DupChevron, Email, Filter, FitViewport, Folder, MetaClose, MetaOpen, ShareAndroid, ShareApple, ShareWindows, Spinner, ChevronLeft, ChevronRight, ChevronDown, WrapOn, WrapOff); all consumers updated to import from the shared icon directory
- **`TopBar.svelte`** — shared topbar component used by both `SearchView` and `FileView`; eliminates the previous duplication where FileView's topbar lacked sticky positioning, search-help, and the logo help-toggle; `isSearchActive` and `nlpHighlightSpan` computed inside TopBar since it owns `isTyping`
- **`AppLogo.svelte`** — magnifying-glass icon + "find-anything" text; logo text hidden on narrow widths (≤768 px), leaving just the icon; tapping the logo on mobile opens the search help panel
- **`MobilePanel.svelte`** — reusable full-screen mobile panel with a back-arrow header button; uses a portal action (`document.body.appendChild`) so it renders above all ancestor overflow/display constraints; used for search help and filter panels
- **`SearchHelpContent.svelte`** — extracted search-help markup shared between the desktop popup and the mobile panel
- **Word-wrap overflow detection** — wrap toggle button is now shown only when content actually overflows (or word-wrap is already active), using `ResizeObserver` + `afterUpdate`; wrap toggle uses SVG icons (WrapOn/WrapOff) instead of text labels
- **Share button in file viewer toolbar** — OS-aware share icon (iOS tray-arrow, Windows box-arrow, Android three-circle graph) opens a dialog with expiry selector (1 day / 1 week / 1 month / Never) and a "Create link" button; once the link is generated it appears with a copy icon and an Email button; on iOS/Android/browsers that support `navigator.share` the native share sheet is invoked instead; the PathBar inline share-link button is removed in favour of this dialog
- **Configurable link expiry** — `POST /api/v1/links` accepts an optional `expires_in_secs` field; `0` means never expires (`expires_at = i64::MAX`); omitting the field falls back to the server-configured default TTL
- **Download and Open in Explorer toolbar icons** — Download and Download Archive are now icon buttons (SVG tray-with-arrow) replacing the text labels; Download Archive shows the icon + "archive" text; Open in Explorer becomes a folder icon moved to immediately after Wrap; Download is now visible on mobile (Download Archive and Open in Explorer remain hidden)
- **`public_url` server option** — new optional `[server] public_url = "https://..."` setting; when configured, share links use this origin instead of `window.location.origin`, so links work correctly when the server is accessed through a reverse proxy on a different hostname; exposed via `GET /api/v1/settings` and consumed by the web UI
- **Mobile support (plan 089)** — responsive layout for narrow screens and mobile browsers: logo collapses to "fa", search input takes the full first row, Advanced filters moves below the search row with a sliders icon, tree sidebar and resize handle are hidden, "Open in Explorer" button and the related Preferences section are hidden, download buttons are hidden; PathBar splits into two rows (back/source on row 1, path on row 2); FileViewer toolbar metadata left-aligns on mobile; AdvancedSearch opens as a full-screen scrollable modal on mobile; Settings page uses an accordion layout instead of a fixed left nav; search results show filename on row 1 and kind/size/date on row 2; image EXIF metadata stacks below the preview
- **AdvancedSearch panel scrolls on desktop** — panel is now capped at `calc(100vh - 80px)` with an internal scroll area (`panel-body`) so filters are reachable even on short viewports; Apply/Clear footer stays pinned at the bottom; scrollbar styled to match the dark theme
- **Word Wrap button hidden for media** — the Wrap toolbar button is no longer shown when viewing images, video, audio, or a PDF in its original renderer
- **SVG inline viewer** — `.svg`/`.svgz` files now render inline by default with a "View Source / View SVG" toggle; no EXIF drawer (SVG has no metadata); source view shows the XML content as before
- **Index size / Content size in Stats** — stats page and `find-admin stats` output now show "Index size" (source DBs) and "Content size" (blobs.db) as prominent cards; removed the confusing "1 content file(s)" label
- **MetaDrawer wraps long values** — hex strings and other long values in the EXIF drawer now wrap rather than causing horizontal scroll
- **Plan 088 — find-upload delegates to find-scan** — uploaded files are now extracted by spawning `find-scan` against a temporary source directory, giving `find-upload` full extraction parity with the regular scan pipeline (archive members, PDF text, image EXIF, correct timeouts); the server no longer has its own inline extractor for uploads; new `ServerScanConfig` (`[scan]` block in `server.toml`) controls `subprocess_timeout_secs` and `max_content_size_mb`; `UploadScanHints` struct carries `include`/`exclude`/`exclude_extra`/`max_content_size_mb` from client to server; `find-upload` binary added to deployment scripts
- **`--force` accepts human-readable timestamps** — `find-scan --force` now accepts ISO date (`YYYY-MM-DD`), local datetime (`YYYY-MM-DDTHH:MM:SS` / `YYYY-MM-DD HH:MM:SS`), or Unix epoch in addition to plain epoch integers; confirmation messages display the time in human-readable local format
- **Integration tests for indexing error flow** — `crates/server/tests/indexing_errors.rs` covers recording failures, clearing errors on successful re-index, failure-survives-completion-upsert invariant, error count increment, and delete cleanup
- **"Open in Explorer" — `findanything://` protocol handler** — new `find-handler` crate implements a binary that receives `findanything://open?path=...` URLs and opens the file location in the OS file manager; Windows uses `explorer.exe /select,"path"` for local drive-letter paths with UNC/virtual-path fallback to parent folder; console flash eliminated via `windows_subsystem = "windows"` and `CREATE_NO_WINDOW`; button shows `cursor: progress` during launch; settings panel adds per-source root path inputs and "Handler installed" checkbox that gates the button; install via PowerShell one-liner (HKCU, no admin); Linux registered via `.desktop` / `xdg-mime`; Inno Setup installer includes `find-handler.exe` and registers `findanything://` in HKCR; release CI publishes `find-handler.exe` and `install-handler.ps1` as standalone assets
- **RTF viewer** — `.rtf` files now have a "Formatted / Plain" toggle in the toolbar; when formatted mode is on, the raw bytes are fetched and rendered client-side via `rtf.js` (dynamically imported — only downloaded when first used); rendered DOM elements are serialised to HTML and displayed through the existing `MarkdownViewer` prose styles; handles Windows code-page encoding correctly; falls back to "RTF rendering failed" on error
- **Test coverage improvement (plans 084–085)** — extensive unit and integration tests across the codebase, raising line coverage from ~65% toward ~78%:
  - `server/tests/raw.rs` — 12 integration tests for `GET /api/v1/raw` (auth, path traversal, content-type, download disposition, range requests, ZIP member serving); `TestServer::spawn_with_extra_config` helper added; 5 `parse_byte_range` unit tests inline
  - `server/tests/search_filters.rs` — kind filter, date filter, pagination, duplicate path, case sensitivity, auth, bad-param 400 responses, multi-source, source-filter tests (20 tests total)
  - `server/tests/search_modes.rs` — file-regex, document, doc-exact, doc-regex modes added (5 new tests); 7 `regex_to_fts_terms` unit tests inline in `search.rs`
  - `server/tests/admin.rs` — `version_gt` unit tests inline; `update_check` auth test added
  - `client/src/watch.rs` — extracted `run_event_loop<F>` from `run_watch` (generic closure keeps future `Send`); 11 new unit tests for `accumulate`, `find_source`, `is_excluded`
  - `extractors/archive/src/lib.rs` — 26 unit tests covering `accepts`, `is_archive_ext`, `detect_kind_from_name`, `has_hidden_component`, mtime helpers, streaming extraction, hidden-member filtering, corrupt ZIP handling
  - `extractors/pdf/src/lib.rs` — 12 tests covering `accepts`, `extract_from_bytes` (empty, garbage, truncated, `/Encrypt` guard), `wrap_at_words`, `max_content_kb` truncation
  - `extractors/pe/src/lib.rs` — 4 new file-based tests for `extract()` (garbage, missing file, minimal PE32); `tempfile` dev-dependency added
  - `extractors/text/src/lib.rs` — 16 new tests for `accepts`, `accepts_bytes`, `is_text_ext`, `is_binary_ext_path`, `extract_from_bytes`, `lines_from_str`
  - `extractors/epub/src/lib.rs` — 5 new tests: `find_opf_path` error path, empty OPF metadata, full `extract()` round-trip with in-memory EPUB, `extract_from_bytes` round-trip, empty-bytes error
  - `extractors/media/src/lib.rs` — image/video ext detection, `accepts()`, `extract_from_bytes` dispatch, PNG/JPEG/BMP header parsing tests
  - `extractors/office/src/lib.rs` — DOCX/PPTX/XLSX extraction tests with in-memory ZIPs; metadata (`docProps/core.xml`), paragraph parsing, slide text, cell values, max-content truncation
  - `content-store/tests/contract.rs` — `MultiContentStore` added to the contract test matrix; 5 specific behaviour tests (put-to-all, get-from-first-hit, stats summing, compact-all-backends, empty-store edge case)
  - `server/tests/context.rs` — new file: 8 tests for `GET /api/v1/context` and `POST /api/v1/context/batch` (surrounding lines, window clamping, auth, missing source/path, batch multi-file)
  - `server/tests/recent.rs` — new file: 7 tests for `GET /api/v1/recent` (ordering by mtime, limit/offset, source filter, auth, empty result)
  - `server/tests/upload.rs` — 5 new tests: `PATCH` and `HEAD` auth guards, complete single-chunk upload becomes searchable, multi-chunk upload becomes searchable
  - `server/tests/stats_cache.rs` — 8 new tests: cached stats refresh, source file/line/size counts, `by_ext`/`by_kind` breakdowns, inbox pending count, SSE stream headers and auth
  - `server/src/compaction.rs` — inline unit tests for `load_cached_stats`, `save_stats_to_slot`, and `compact_archives` dry-run
  - `server/src/db/search.rs` and `db/stats.rs` — inline unit tests for pure helper functions
- **Search syntax help button** — a `?` circle button appears to the right of the tree-toggle in the search bar (both search and file-detail views); clicking opens a scrollable popup listing scope prefixes (`file:`, `doc:`), match prefixes (`exact:`, `regex:`), `type:` filters, natural-language date syntax, and quote matching; extracted into a reusable `SearchHelp.svelte` component
- **Configurable tab width** — `server.toml` gains a `tab_width` setting (default: 4) returned via `GET /api/v1/settings`; the file viewer applies it as `tab-size` CSS on the code table; users can override it per-browser in Preferences (1/2/4/8, with Reset to server default); previously browsers used their default of 8
- **ZIP content store removed** — `ZipContentStore` and all `zip_store/` code deleted; `BackendType::Zip` removed from config; `SqliteContentStore` is now the only backend; `server/src/archive.rs` deleted; `zip` dependency dropped from `find-content-store`; UI and CLI text updated to remove ZIP references ("awaiting archive" → "awaiting write", etc.)
- **Inline audio player** — audio files now open in a split-pane view (player on the left, metadata tags on the right), matching the image/video viewer layout; `AudioViewer.svelte` added
- **Per-tag metadata display** — image, audio, and video viewers render each `[TYPE:key] value` tag on its own row instead of one joined string; `metaTags.ts` parser added with 8 unit tests
- **`imageMeta.ts` rewritten using `parseMetaTags`** — the old `^`-anchored regex patterns only matched when the dimension tag appeared first in the joined string; the new implementation uses `parseMetaTags` + a `Map` lookup so tags are found regardless of order
- **Archive member sizes stored at index time** — `MemberBatch` gains a `size: Option<u64>` field; ZIP, TAR, and 7z handlers populate it from the archive entry's declared uncompressed size; the subprocess (RAR/external) path uses `bytes.len()`; `build_member_index_files` forwards it to `IndexFile.size`; three new extractor tests and two server integration tests verify end-to-end storage and retrieval
- **Schema v3: content-addressable archive storage (plan 076)** — replaces the `lines` table (one row per line) and `canonical_file_id` alias system with three new tables: `content_blocks` (hash → integer id), `content_archives` (ZIP name → integer id), and `content_chunks` (one row per chunk per block); FTS5 rowid is now encoded as `file_id × 1,000,000 + line_number`, eliminating the `lines` join table entirely; deduplication is tracked via an explicit `duplicates(content_hash, file_id)` junction table instead of the `ON DELETE SET NULL` alias pointer that caused phantom-canonical bugs; ZIP chunk names change from `{file_id}.{chunk_number}` to `{block_id}.{chunk_number}`, so files sharing identical content share ZIP chunks; ~25× fewer DB rows for large sources; **full re-index required** (delete `data_dir/sources/`, restart, run `find-scan --force`)
- **`files_pending_content` stat** — per-source count of files whose content has not yet been written to a ZIP archive (DB-level archive backlog, independent of the `.gz` queue depth); shown in `find-admin status` as `(N pending content)` when non-zero; updated automatically when the archive queue drains
- **`duplicate_paths` in file viewer** — `GET /api/v1/file` now returns a `duplicate_paths` field populated from the `duplicates` table; the file viewer displays these links so navigating to a file via a duplicate link shows all other copies
- **`GET /api/v1/stats/stream`** — SSE endpoint that pushes `StatsStreamEvent` snapshots whenever the in-memory stats cache changes; rate-limited to `server.stats_stream_rate_hz` events per second (default: 5); `find-admin status --watch` now subscribes to this stream and redraws on each event instead of polling on a fixed interval; `StatsStreamEvent` extended to include worker status, inbox pending/failed/archive-queue counts, DB size, archive size, orphaned bytes, and paused flag so the watch view now mirrors `find-admin status` output
- **`--version` git hash** — `find-common` now has a `build.rs` that captures the git commit hash, tag, and dirty flag at build time and exposes them as package-level constants (`GIT_HASH`, `GIT_TAG`, `GIT_DIRTY`); the `tool_version!` macro now reads these constants instead of expanding `option_env!` at the calling crate's compile time, ensuring the version string is always accurate regardless of incremental build caching; dirty builds now show `{hash}+` instead of `(dev)`
- **Source stats cache** — `GET /api/v1/stats` now reads from an in-memory `SourceStatsCache` instead of running three expensive full-table-scan queries on every request (previously ~10 s on large databases); the cache is rebuilt from DB at startup (30 s delay) and daily alongside compaction; the worker applies a cheap incremental delta after each batch to keep `total_files`, `total_size`, and `by_kind` accurate without full scans; `by_ext` and `fts_row_count` are slightly stale between daily rebuilds (acceptable trade-off); `find-admin status --refresh` (new `--refresh` flag) forces a synchronous full rebuild before displaying results
- **`[log] compact = true`** — new config option for `find-server` and `find-watch`; omits the timestamp and module-path target from each log line, eliminating redundancy when running under systemd/journald which already captures these from process metadata; config is now parsed before logging is initialised so the format takes effect from the first log line
- **Ctrl+P palette: tree pre-fetch** — selecting a file from the Ctrl+P palette now pre-fetches all intermediate directory levels in parallel before the tree renders, so the sidebar expands directly to the target in one pass instead of one serial network round-trip per directory level; `treeCache.ts` provides a shared cache that also benefits subsequent manual tree navigation
- **Distinct raw-file-access error logging** — when `GET /api/v1/raw` cannot resolve a file path, the server now checks whether the source root directory itself is accessible and logs two distinct messages: "source root not accessible — mount may have failed" vs "file not found — may have been deleted from the client"
- **Indexer INFO log cleanup** — the request ID (`req_YYYYMMDD_…`) is no longer emitted in INFO-level indexer summary lines; it remains available at DEBUG level on both the start and done events for correlation when needed; INFO lines now use the shorter `[indexer:source]` tag
- **Ctrl+P palette: server-side search** — `GET /api/v1/files` now accepts optional `?q=&limit=` params; when `q` is present the server returns up to `limit` (default 50) substring-matched paths (case-insensitive LIKE) ordered by path, or the most-recently-indexed files when `q` is empty; the palette no longer downloads the full file list on open, making it practical for sources with 100 k+ files
- **Ctrl+P palette: scope badge** — the input row shows a pill badge indicating which sources are being searched: the source name when scoped to one source, per-source badges when scoped to a subset, or a dimmed "all" badge when all sources (>1) are searched
- **Ctrl+P palette: stale-result preservation** — the results list is not blanked during a refresh; new results replace the old ones in-place once the fetch completes, eliminating the flash to an empty view on each keystroke
- **Testing requirements in CLAUDE.md** — added a table specifying when to write web UI unit tests (Vitest), server integration tests (`TestServer`), and CLI end-to-end tests; added guidance on replacing deleted client-side unit tests with server-side equivalents when logic moves to the server
- **`GET /api/v1/files` integration tests** (`crates/server/tests/files_search.rs`) — 6 tests covering: full list returned when `q` absent, empty `q` returns recent files, substring match, case-insensitive match, no match returns empty, `limit` param respected
- **Client integration test suite (`crates/client/tests/`)** — 11 `find-scan` integration tests (S1–S11) and 5 `find-watch` tests (W1–W5, `#[ignore]` by default) that drive the full round-trip: real files on disk → extraction → bulk submission → server indexing → search verification; covers basic indexing, mtime-based skip, force/upgrade modes, delete detection, exclude patterns, ZIP archive members, and external extractors in both stdout and tempdir modes; a new `[lib]` target exposes client modules so tests can call `run_scan` / `run_watch` directly without spawning subprocesses
- **Automatic daily compaction** — the server now runs a wasted-space scan 30 s after startup and then daily at `compaction.start_time` (default `02:00` local time); compaction rewrites ZIP archives to remove orphaned chunks (content no longer referenced by any `lines` row); compaction only runs when orphaned bytes ≥ `compaction.threshold_pct` percent of total archive bytes (default 10 %); scan elapsed time and orphaned/total counts are logged at INFO
- **`[compaction]` config section** — new `server.toml` section with `threshold_pct` (f64, default `10.0`) and `start_time` (HH:MM string, default `"02:00"`); shown in `examples/server.toml` with commented-out defaults
- **`find-scan --force [EPOCH]`** — forces re-index of all files regardless of mtime or scanner version; useful after changing normalizer/formatter config; naturally resumable: the epoch (Unix seconds) is printed at startup, and passing it on a subsequent run skips files whose `indexed_at >= epoch` (already processed); `GET /api/v1/files` now includes `indexed_at` in each `FileRecord` to support this
- ~~**RAR archive extraction**~~ — removed; `unrar_sys` fails to cross-compile for ARM (`ptrdiff_t` undeclared with arm-linux-gnueabihf-g++ v5), blocking the ARM NAS build; `.rar` files are now treated as opaque binaries (indexed by filename only, not traversed); RAR will be re-added via an external `unrar` binary under plan 062
- **Upload endpoint integration tests** — `crates/server/tests/upload.rs` with 9 tests covering the three-endpoint upload protocol: init (POST → 201 + id), single-chunk, two-chunk resume, progress query (HEAD), gap detection (409), missing Content-Range (400), unknown id (404), and auth (401)
- **Plan: pluggable extractors and file type config** (`docs/plans/062`) — design for moving hardcoded extension→kind and extension→extractor dispatch into config; supports external tools (e.g. `lhasa`, `cabextract`) via `[scan.extractors]` with a `"builtin"` sentinel for existing formats; user config is a sparse overlay on built-in defaults; external archive extractors produce the same `outer::member` composite paths as built-ins
- **Plan: metrics and observability** (`docs/plans/063`) — design for instrumenting find-anything with the `metrics` crate facade (zero overhead when no backend configured); two backends: Prometheus `/metrics` pull endpoint and push to a remote URL; `find-scan` reports aggregate scan stats to `find-server` via `POST /api/v1/metrics/scan`; fully opt-in via a new `[metrics]` config section
- **Archive extractor integration tests** — `crates/extractors/archive/tests/extract.rs` with 17 tests covering all supported inner formats (tar, tgz, tar.bz2, tar.xz, zip, 7z, rar), deeply nested paths, 200-char filenames, unicode filenames, depth limiting, and exclude-pattern filtering; `fixtures.zip` (outer ZIP for streaming tests) and `fixtures.tgz` (node `tar` test suite fixture with PAX headers, hardlinks, and extreme paths) added as test fixtures; fixture generation script at `docs/misc/make_fixtures.py`
- **Binary size tracking** — `docs/binary-sizes.md` records unstripped release binary sizes per version to catch regressions on space-constrained systems
- **Keyboard navigation in file tree** — Arrow Up/Down moves the cursor through all tree items (sources, directories, files) without opening files; Enter opens the focused item; the cursor is highlighted via a Svelte store (`keyboardCursorPath`) so keyboard and mouse selection are visually distinct; focus is restored to the tree after a file opens so navigation can continue immediately with arrow keys
- **Reactive UI via SSE** — the web UI now connects to `GET /api/v1/recent/stream` on load and reacts to index changes in real time: expanded tree directories silently re-fetch when a file beneath them is added, removed, or renamed; the file detail view auto-reloads on modify, shows a "DELETED" banner on delete, and offers a "Renamed to …" navigation link on rename; search results show a dismissible "Index updated — refresh results" banner on add/modify, and deleted-file result cards are greyed out with strikethrough; uses `fetch()` streaming (not `EventSource`) to support bearer-token auth, with exponential back-off reconnection
- **Filename match highlighting** — path-only search results (files matched by name rather than content) now highlight the matched query terms in the file path using the same `<mark>` style as line-content matches; applies only to filename matches (`line_number = 0`, non-metadata snippets)

### Fixed

- **`find-preview-dicom` pipe-buffer deadlock (422 Unprocessable Entity)** — binary writes ~145 KB PNG to stdout but the OS pipe buffer is only 64 KB; child blocked writing while the server polled for exit, never reading; stdout is now drained in a background thread concurrently with the wait loop so the child can flush and exit
- **DICOM image viewer not shown after scanner upgrade** — `showOriginal` was only set on initial file load (`isInitial=true`); a live-update reload after `find-scan --upgrade` changed the kind to `dicom` left the viewer hidden; for media kinds with no toggle (image, audio, dicom) `showOriginal` is now always updated
- **Backspace/delete no longer triggers new search or typeahead** — `SearchBox` cancels the debounce on deletion and `TopBar` freezes the typeahead `activeToken`; searches and typeahead only fire when typing forward or pressing Enter
- **Image viewer controls overlaid on image** — zoom +/−/fit buttons moved inside the image container as a transparent hover overlay (top-left, semi-transparent dark background with blur); the separate toolbar row is removed so the image gets the full height; `pointerdown` and `dblclick` handlers guard against the toolbar to prevent drag hijack and accidental fit-reset on double-click; minimum zoom scale is now dynamic (`Math.min(0.01, fitScale * 0.5)`) so zoom-out no longer snaps up to a hard-coded minimum that exceeds fit scale
- **Duplicate paths moved to modal** — the inline expandable dup-bar row is replaced by a `"N duplicates"` badge button in the toolbar metadata area (before the kind badge); clicking opens a fixed-height scrollable modal (640 px wide) with bulleted path links; clicking the backdrop or ✕ closes it
- **`source:` with path but no free text now runs a search** — when `source:name/segment` is present alone, the last path segment is used as a `file-exact` FTS query scoped to the source and path prefix; requires at least one path segment after the source name; fallback direct-files-table scan added for cases where FTS terms are empty (e.g. `regex:.*`) but `path_prefix` is set, with three new integration tests
- **`source:` prefix minimum query length now ignores modifiers** — `hasSearchableContent` strips all prefix tokens before checking for 3+ characters of free text; `source:nas-data/path` or `doc:type:pdf` alone no longer trigger a search until a real query term is also present
- **Copy icon clipping and alignment** — redesigned as two overlapping rectangles with `fill` for the overlap area; `.copy-btn` uses `align-self: center` so it aligns with text in the PathBar row
- **SVG background colour in light mode** — corrected CSS variable from non-existent `--bg-primary` to `--bg` in `DirectImageViewer`
- **Metadata always right-aligned** — removed mobile breakpoint override that left-aligned file kind/size/date; metadata is now right-aligned at all widths
- **FileViewer toolbar order** — view-mode buttons (View Source / View SVG) appear first, then Wrap toggle, then icon buttons (folder, download, share)
- **`CreateLinkRequest` struct literals in integration tests** — added missing `expires_in_secs: None` to all test struct initialisers after the field was added to the type
- **`file:pdf` / `file:jpg` search returning too few results** — two bugs caused filename-scoped searches to miss most matches: (1) archive members (e.g. `archive.zip::report.pdf`) were scored against the outer archive path (`archive.zip`) which fails fuzzy matching for queries like "pdf"; now scored against the composite path; (2) SQLite applies `LIMIT` before the `rowid % 1_000_000 = 0` filename filter, so only a handful of filename rows survived a small candidate limit; filename-only queries now use `fts_limit` (2000) as the SQL `LIMIT` and truncate to `scoring_limit` after the rowid filter
- **Search result count shows "N+" when results are capped** — the result count now displays a "+" suffix (e.g. "2000+") when the server's candidate cap is reached, giving users an immediate signal that more results exist beyond the current page; the suffix disappears on the final page when the exact total is known
- **Upload chunk 413 Payload Too Large** — `upload_routes` now uses `DefaultBodyLimit::disable()` so large file chunks (previously capped at Axum's 2 MB default) are accepted without error
- **Indexing errors cleared by completion upsert** — `find-scan` no longer sends a completion upsert after a subprocess extraction failure (stdout/TempDir paths), preventing the server from treating the error as resolved
- **Images with mismatched extensions (e.g. JPEG saved as `.tif`)** — `convert=png` now uses magic-byte sniffing (`sniff_browser_format`) before attempting decode+re-encode; if the file's true content is already a browser-native format (JPEG, PNG, GIF, WebP, BMP), it is served directly with the correct `Content-Type` and no conversion overhead; this fixes the 422 error for files like `Monhegan Lighthouse Dory 2005.tif` which is actually a JPEG
- **Tree directory collapse** — manually collapsing a directory (or archive node) that contains the active file no longer immediately re-expands; the auto-expand reactive now gates on `activePath` changing to a new value rather than re-running whenever `expanded` changes, so user-initiated collapses are respected
- **Palette-indexed TIFF display** — TIFF files with `PhotometricInterpretation=RGBPalette` (8-bit indexed colour, common in older scanned documents) now convert correctly via `GET /api/v1/raw?convert=png`; previously returned 422 because the `image` crate's TIFF decoder does not implement this mode. Fixed via a new `image_util` module that reads the `ColorMap` tag directly, patches the PMI field in-memory to `BlackIsZero`, decodes the indices via the `image` crate, then expands them through the palette to produce a full RGB image.
- **PathBar copy buttons** — clicking "Copy share link" no longer also triggers the "Copied" state on the "Copy path" button; `fallbackCopy` now takes a callback instead of hardcoding `showCopied()`
- **PathBar tooltip positioning** — replaced native `title` attribute (which positioned the tooltip far from the cursor in flex layouts) with a CSS `::after` pseudo-element tooltip that appears above the button, right-aligned to its edge
- **Browser back/forward navigation** — back and forward now correctly update the file viewer and left-nav tree without requiring a page reload; root cause was `afterNavigate` receiving stale `$page.state` from entries created via native `history.replaceState`, preventing Svelte updates; replaced with a native `popstate` listener that always reconstructs state from URL params; the in-app ← button now calls `history.back()` instead of pushing a duplicate history entry; running a new search while viewing files rewinds the file-view history stack so that back goes to the new search, not old documents; `navDepth` tracks the number of file-view entries above the search baseline and is stored in each history entry
- **`regex:doc:` cross-line matching** — `DocRegex` mode now tests the regex against the full joined document text (with `dot_matches_new_line`) instead of per-line; previously patterns like `.*UART.*updates.*` matched nothing because `.` could not span newlines; the FTS pre-filter now uses `document_candidates` (per-token file intersection) so a file qualifies if each literal term appears *somewhere* in it, not necessarily on the same line
- **SQLite compaction metrics** — `compact()` now returns accurate `chunks_removed`, `units_deleted` (distinct orphaned blob keys), and `bytes_freed` (logical data bytes removed) for both dry-run and real compaction; dry-run previously returned `chunks_removed: 0`; `VACUUM` is issued after real deletions to reclaim disk pages
- **Audio player hang / no duration** — `GET /api/v1/raw` now returns `Content-Length` and `Accept-Ranges: bytes` for direct file downloads, and handles `Range:` requests with a proper 206 Partial Content response; browsers require this to display audio/video duration and enable seeking
- **Audio/video inline viewer for non-ZIP archive members** — `canViewInline` now requires `canServeArchiveMember` (true only when every archive in the composite path is a ZIP); RAR/TAR/7z members no longer show a broken player
- **Symphonia probe-EOF logged at WARN with member context** — probe failures for short/corrupt audio members were silently swallowed (ERROR emitted by symphonia, ignored); `extract_audio` now accepts a `label` parameter and emits `warn!("audio probe failed for '...'")` with the member path; the raw symphonia ERROR is suppressed via the `[log] ignore` list in both `defaults_client.toml` and `defaults_server.toml`
- **nom_exif parse failures logged at WARN with composite path** — video metadata parse errors from `nom_exif` were silently dropped (nom_exif logged at ERROR internally); `extract_video_nom_exif` now captures the error and emits `warn!("nom_exif parse failed for '...'")` with the full composite member path (e.g. `archive.zip::clip.mp4`); `nom_exif=off` added to tracing filters in archive, media, and scan binaries to suppress the library's own ERROR logs
- **`DUPLICATE:` empty path in file viewer** — `GET /api/v1/file` returned an empty string in `metadata` for files with no metadata content (the client deliberately stores an empty `LINE_METADATA` placeholder to keep line positions stable); the server now filters empty strings from the metadata array; the FileViewer UI also guards against empty or self-referencing duplicate path entries
- **PathBar tooltip z-index** — copy/share button tooltips were clipped behind the sticky search bar header; `z-index: 100` added to the `::after` pseudo-element so tooltips render above all other content
- **Optional gzip compression for `SqliteContentStore`** — `BackendInstanceConfig` gains a `compress: Option<bool>` field; when `true`, each chunk is gzip-compressed before storage and decompressed transparently on read (magic-byte detection, so uncompressed and compressed databases are both readable by the same code); `blobs.db` schema changes `data TEXT` → `data BLOB`; two new server.toml backends (`sqlite_4k_gz`, `sqlite_12k_gz`) for A/B benchmarking; contract test suite expanded from 24 to 36 tests (12 per backend: zip, sqlite, sqlite_compressed)
- **Backend-neutral stats and compaction output** — `find-admin status` no longer hard-codes "ZIP files"; the label is now "Content: N file(s)" and accurately reflects the active backend (47 ZIP archives or 1 SQLite database); `SqliteContentStore::storage_stats()` now returns `(1, bytes)` instead of `(row_count, bytes)` so the file count is never a meaningless chunk row number; `ContentStore::archive_stats()` renamed to `storage_stats()`; `CompactResult`/`CompactResponse` fields renamed `archives_*` → `units_*`; compaction messages use "storage unit(s)" throughout; web UI global metrics updated to match
- **`find-test` benchmarking binary (plan 083)** — new `find-test` binary in `crates/server` opens backends directly from `server.toml` (no HTTP) and runs write + concurrent read phases against each configured backend; `bench-storage` subcommand accepts `--concurrency` (repeatable), `--blobs`, `--blob-size-kb`, `--reads`, `--seed`, and `--json`; results printed as a text table or JSON; `mise run bench-storage` task added for convenience
- **SQLite elastic read connection pool** — `SqliteContentStore` now uses a `ReadPool` backed by `Mutex<PoolState>` + `Condvar` instead of a single `Mutex<Connection>`; idle connections are reused (up to 16 kept warm), new connections are opened on demand, and callers block only when the configurable hard cap (`max_read_connections`, default 100) is reached; eliminates the p99 tail-latency blow-out observed at concurrency ≥ 4 when reads serialised on one connection
- **`max_read_connections` config option** — `BackendInstanceConfig` gains an optional `max_read_connections: u32` field (SQLite only); defaults to 100; set in `server.toml` per named instance to tune the connection cap for the workload
- **Pluggable content store backends (plan 082)** — new `SqliteContentStore` stores all chunks as plain-text rows in a single `blobs.db` SQLite database, offering O(log n) PK-indexed reads vs. loading a full 10 MB ZIP archive per lookup; new `MultiContentStore` fans out writes to multiple backends simultaneously and reads from the first hit, enabling side-by-side population of all backends via a single scan; new `[storage]` section in `server.toml` selects backends as a named-instance list (`backends = [{ name = "sqlite_4k", type = "sqlite", chunk_size_kb = 4 }]`), defaulting to a single ZIP backend for backward compatibility; single-backend deployments continue to use `data_dir` directly, multi-backend deployments use `data_dir/stores/{name}/` per instance; shared `ContentStore` contract test suite in `crates/content-store/tests/contract.rs` runs 12 tests against both ZIP and SQLite implementations
- **Content store abstraction (plan 081)** — ZIP-based content storage is now isolated behind a `ContentStore` trait in a new `crates/content-store/` crate (`find-content-store`); the crate owns its own SQLite database (`data_dir/content.db`) with tables `blobs`, `archives`, `chunks`, eliminating the `content_blocks`/`content_archives`/`content_chunks` tables from every per-source DB; ZIP member names change from `{block_id}.{chunk_num}` to `{key_prefix_16}.{chunk_num}`; `AppState` gains a `content_store: Arc<dyn ContentStore>` field; `archive_batch.rs` is simplified to `content_store.put(key, blob)` calls; read path (`get_file_lines`, `get_context`, search regex) uses `content_store.get_lines()`; compaction and stats delegate to the store; `SCHEMA_VERSION` bumped 12→13 with new `schema_v4.sql` (full re-index required); `delete_source` defers orphaned blob cleanup to the next compaction pass instead of eagerly rewriting ZIPs; `ZipContentStore` retains a persistent `Mutex<ArchiveManager>` across `put` calls so small blobs pack together into shared archives instead of each allocating a new ZIP
- **Reserved line number scheme (plan 079)** — line 0 = `[PATH]` (always present), line 1 = concatenated metadata (EXIF tags, audio tags, document title/author, MIME type, all in one searchable line), line 2+ = file content; eliminates the shadowing bug where multiple metadata entries at line 0 made EXIF/audio fields unsearchable in context retrieval; `content_line_start: 2` added to `GET /api/v1/settings` so clients can compute display line numbers; `SCHEMA_VERSION` bumped to 12 (full re-index required); `SCANNER_VERSION` bumped to 2
- **Search regression fix (plan 080 follow-up)** — fuzzy content search and line-family regex search were broken after the ZIP-read removal: `Fuzzy` mode now accepts FTS-validated matches (scoring by file path as a proxy when content is unavailable), and `Regex` mode restores content reads for the post-filter; all 11 scan integration tests now pass
- **Search performance: batch content lookups + missing index** — `fts_candidates` and `document_candidates` no longer do N×2 per-row SQL queries; content for all candidates is resolved in 2 batch queries regardless of candidate count; new `idx_content_chunks_block_start` index on `content_chunks(block_id, start_line)` applied at DB open for existing databases
- **Search skips ZIP reads entirely for all non-regex modes** — `fts_candidates` and `document_candidates` return `content: ""` without touching ZIP archives; ZIP reads now only happen for regex mode (which must apply the post-filter to content) and explicit context/file requests; eliminates the dominant source of disk I/O on the search path
- **Request logger now records response status and elapsed time** — log format changes from a single `API request` line to `→ API` on start and `← API {N}ms status={S}` on finish, both at DEBUG; the INFO escalation for destructive admin ops is removed in favour of consistent DEBUG timing
- **Admin route integration tests (plan 078 phase 1)** — 10 new tests covering inbox status, clear-pending, clear-all, retry, pause-stops-processing, compact with real orphaned chunks, compact dry-run, compact deletes fully-orphaned archive, and delete-source chunk cleanup; test helpers gain `data_dir_path()`, `make_text_bulk_hashed()`, and `write_fake_gz()`
- **`mise run coverage` task** — generates an HTML test coverage report via `cargo-llvm-cov` to `target/llvm-cov/html/index.html`; `cargo-llvm-cov` added to `[tools]` so `mise install` handles it automatically (requires `rustup component add llvm-tools`)
- **Server memory reduction (plan 077)** — three memory bugs fixed in the indexing worker: (1) `archive_batch.rs` now processes one gz at a time (two sub-phases: ZIP I/O then batched SQLite writes), eliminating the `upsert_map` clone that held all batch content simultaneously; (2) `request.rs` streams gz files directly from disk via `serde_json::from_reader` + `GzDecoder<BufReader<File>>` instead of buffering the whole file in a `Vec<u8>` then a `String`; (3) `request.rs` consumes `files` by value so normalization no longer clones content strings
- **Batch formatter mode (`mode = "batch"`)** — new `FormatterMode::Batch` in `FormatterConfig`; all matching files in a batch are written to a temp directory and the formatter is called once on the directory (use `{dir}` in `args`); reduces 150 biome spawns to 1 for a typical JS/TS batch; existing `stdin` mode unchanged and still default
- **Compaction deletes fully-orphaned archives** — when every entry in an archive is orphaned the file is now deleted outright instead of being rewritten to an empty ZIP; pre-existing empty archives (left over from previous passes) are also cleaned up; `CompactResponse` gains `archives_deleted`
- **Request logger middleware** — every API request is logged with method, path, and remote address; destructive admin operations (POST/DELETE/PATCH on `/api/v1/admin/*`) log at INFO, all others at DEBUG; remote address prefers `X-Forwarded-For` header
- **`fmt_bytes` utility** — `find_common::mem::fmt_bytes` formats byte counts as human-readable strings (e.g. `"2.4 GB"`); used in compaction and log messages throughout
- **Subprocess log lines include source file path** — `relay_subprocess_logs` now takes a `file` parameter and includes it as a structured field in every relayed log line, so extractor errors can be traced back to the file being processed
- **Dead `fts_count` scan removed from search hot path** — the search handler was calling `db::fts_count()` (a full 2000-row FTS5 scan) before every query, then storing the result in `_source_total` where it was discarded; the `total` in the JSON response was already computed from `unique.len()`; removing this call eliminates a dominant bottleneck on slow hardware (responsible for ~15 s first-result latency on an ARM7 NAS with a large index)
- **`delete_source` cache eviction uses stale local** — `guard.sources.retain(|s| s.name != source_name)` used a local variable copy instead of `query.source`, meaning the stats cache was never pruned after deleting a source; now uses `query.source` directly
- **Compaction log messages showed raw byte counts** — compaction scan and compact route now use `fmt_bytes` for human-readable output
- **Nested archive extraction for external extractors** — when a tempdir-based external extractor (e.g. unrar) extracts a member that is itself a recognized archive (ZIP, 7z, TAR, etc.), `run_external_tempdir` now calls `extract_streaming` on it and prefixes `archive_path` values with `member_rel::`, mirroring `handle_nested_archive` in the native path; composite paths like `outer.rar::inner.zip::hello.txt` are now produced correctly
- **Consistent external dispatch for archive members** — `ExtractorConfig` gains an `external_dispatch` map (populated from `[scan.extractors]` by `extractor_config_from_scan`); `extract_member_bytes` in the archive extractor checks this map first, so any extension registered as an external extractor is handled identically whether found at top level or nested inside a ZIP, 7z, or any other archive format
- **`ExternalDispatchMode` / `ExternalMemberDispatch` types** — new types in `find_extract_types` carry external-extractor config into the archive extraction pipeline without creating a dependency on `find_common`
- **FTS rowid SQL constants** — `constants.rs` now exports `SQL_FTS_FILE_ID`, `SQL_FTS_LINE_NUMBER`, and `SQL_FTS_FILENAME_ONLY`; all five occurrences of the hardcoded `1000000` literal in FTS queries are replaced with these named constants; the module now documents the full encoding rationale, stability guarantee, and overflow bounds
- **`fetch_duplicates_for_file_ids` returns wrong column** — the query selected `d1.file_id, f2.path` but `r.get(0)` inferred `String`, hitting the INTEGER `file_id` column first; caused `InvalidColumnType(0, "file_id", Integer)` errors logged as WARN on every search; fixed by removing the unneeded `d1.file_id` from the SELECT so `f2.path` (TEXT) is column 0
- **Search source errors logged at WARN instead of ERROR** — a query failure during search is a code bug, not expected degraded behaviour; severity raised to `error!`
- **Archive batch duplicate block writes** — when two files in the same batch share a `content_hash`, the second would trigger a "removed stale chunk" rewrite of the ZIP; fixed by tracking `seen_block_ids` within each batch to skip redundant archive work
- **`content_chunks` UNIQUE constraint on concurrent batches** — the pre-scan "already archived?" check raced with the commit in a concurrent batch; replaced `INSERT OR IGNORE` (which silently masks bugs) with a proper re-check inside the write transaction under the source lock, so a plain `INSERT` is only reached when the row is guaranteed absent
- **`archive_queue` not updating in `find-admin status --watch`** — the archive loop never fired `stats_watch` after processing batches; now fires after each batch and triggers a full stats cache rebuild when the queue drains, so `files_pending_content` and `archive_queue` update live
- **Stats panel shows "no sources indexed yet" after fresh install** — `apply_delta` silently dropped incremental updates for sources not yet in the cache (e.g. on first scan after deleting `sources/`); now inserts a new `CachedSourceStats` entry on first delta so the web UI shows live file counts immediately without waiting for a full rebuild
- **File viewer not showing duplicates** — `FileViewer.svelte` built `duplicatePaths` from `metadata` line-0 entries (the old alias mechanism); in schema v3 those paths are never written to metadata; fixed by adding `duplicate_paths` to `FileResponse` and reading it in the file viewer
- **Phantom canonical causes archive members to return zero content** — when `find-scan --force` re-indexes an archive whose member was the dedup canonical for another archive's member, SQLite's `ON DELETE SET NULL` promoted the alias to `canonical_file_id = NULL` with no content lines; the dedup query then selected this "phantom canonical" as the target for the newly re-indexed member, leaving both with zero content lines; fixed by adding `EXISTS (SELECT 1 FROM lines WHERE file_id = id LIMIT 1)` to the dedup query so contentless phantoms are never selected; regression test added documenting the full scenario
- **7z solid archive members skipped when block unpack size is zero** — `sevenz_rust2::Block::get_unpack_size()` returns 0 for solid archives where the block-level total is absent from the header (individual file sizes are still present in `SubStreamsInfo`); the previous code treated this as "unknown size — skip block", causing all members of affected solid archives to be indexed by filename only; fixed by falling back to summing `archive.files[fi].size()` for files in the block when `get_unpack_size()` returns 0; integration test and fixture added
- **`update-server.sh` missing `find-extract-dispatch`** — the server update script was not deploying the `find-extract-dispatch` binary, meaning the dispatching extractor was never updated on the server after a release
- **CSS/SCSS/Sass/Less/Stylus files not indexed as text** — `.css`, `.scss`, `.sass`, `.less`, `.styl` extensions were absent from `is_text_ext()`; these files were treated as unknown kind and their content was not extracted
- **Ctrl+P palette: infinite loop on open** — replaced `$: if (open)` reactive block (which read `inputEl` inside a `tick().then()` callback, causing `bind:this` to re-trigger it every flush) with a `beforeUpdate` + previous-value guard that fires exactly once on the `false→true` transition
- **Ctrl+P palette: typed characters not appearing** — removed the `$: if (typeof query === 'string') scheduleSearch(query)` reactive statement that fired before `bind:value` could update `query`; search is now triggered via `on:input` reading `e.target.value` directly, matching the pattern used by the main search box
- Inline safe extractors in find-scan: text, HTML, media, and office files are now
  extracted in-process rather than via subprocess, eliminating IPC overhead for the
  most common file types. find-watch inlines text only (memory footprint concern).
- **Security model plan (plan 070)** — design document for optional user authentication and source-level RBAC: two credential types (blake3-hashed API keys for write-only indexing; argon2id-hashed user sessions for read access), role-based source visibility configured in `server.toml`, optional `[auth] required` flag, overlapping-source partitioning pattern and its limitations documented, CLI credential file, first-run admin bootstrap flow
- **Testing gaps plan (plan 071) — phase 5 complete** — remaining coverage gaps: `crates/server/src/normalize.rs` — 4 external formatter tests (`#[cfg(unix)]`) covering success path (`/bin/cat`), non-zero exit code skipped, empty output skipped, nonexistent formatter skipped; `crates/server/src/compaction.rs` — 3 `scan_wasted_space` tests covering all-chunks-referenced (zero orphans), unreferenced chunks counted as orphaned, empty content dir returns zero; `web/src/lib/lineSelection.test.ts` (new) — 22 vitest tests for `parseHash`, `formatHash`, `selectionSet`, `firstLine`, `toggleLine` covering single lines, ranges, mixed selections, round-trips, 10 000-line cap, null return, and toggle add/remove; TUS upload covered by phase 2 (8 tests in `crates/server/tests/upload.rs`); PDF panic-path covered by phase 4 (`malformed_pdf_does_not_panic` test)
- **Testing gaps plan (plan 071) — phase 4 complete** — TypeScript and component coverage: `web/src/lib/pagination.ts` — extracted `mergePage(existing, incoming, currentOffset)` from `triggerLoad` in `+page.svelte`; `web/src/lib/pagination.test.ts` — 8 vitest tests covering no-dupes, all-dupes, partial-dupes (offset advances by `incoming.length` not `fresh.length`), empty inputs, `archive_path` as discriminating key, and null `archive_path` dedup; `+page.svelte` updated to use `mergePage` for both first-page and load-more paths; `crates/server/src/fuzzy.rs` — 7 unit tests for `FuzzyScorer` covering exact match, non-match, prefix-scores-higher-than-partial, empty query, case-insensitive/sensitive matching, and consistency across calls; `crates/extractors/pdf/src/lib.rs` — panic-path test feeding malformed Type1 font PDF to `extract_from_bytes` and asserting `Ok(_)` (not a panic)
- **Testing gaps plan (plan 071) — phase 3 complete** — worker pipeline tests: `worker/request.rs` — 5 unit tests for `process_request_phase1` covering upsert (DB record created + archive gz written), delete (DB record removed, archive phase correctly skipped for delete-only batches), rename (old path removed and new path inserted in SQLite), empty request (no archive gz written), and delete-before-upsert ordering invariant; `worker/archive_batch.rs` — 3 unit tests for `run_archive_batch` covering chunk writing with line-ref update in SQLite + ZIP archive creation, gz cleanup when no matching DB entry exists, and idempotency (already-archived lines are skipped on second run)
- **Testing gaps plan (plan 071) — phases 1–2 complete** — analysis of all test coverage gaps across Rust, TypeScript, and CI; phase 1: vitest suite added to CI, 17 new Rust unit tests; phase 2: 21 new route-handler integration tests across 3 new test files: `search_modes.rs` (exact, regex, file-fuzzy, file-exact, default-fuzzy modes), `links.rs` (create/resolve round-trip, uniqueness, 404 on unknown code, auth), `admin.rs` (`delete_source`, `compact`, inbox pause/resume); `TestServer` helper upgraded to `into_make_service_with_connect_info` so `ConnectInfo<SocketAddr>` is available in route handlers; `make_text_bulk` fixed to use `[PATH] ` prefix for line_number=0 entries (required for `file-*` mode filtering); pnpm version pinned via `jdx/mise-action` in CI (reads from `.mise.toml`) instead of duplicating version in `package.json`
- **Direct link sharing (plan 068)** — share button (chain icon) in the path bar generates a short capability-based URL (`/v/xxxxxx`) that gives unauthenticated read access to that file; links expire after a configurable TTL (default 30 days, set via `[links] ttl = "30d"` in server.toml); the direct view page renders images (custom pan/zoom viewer with toolbar, double-click reset), PDFs (browser native iframe), videos (HTML5 player), and extracted text; a minimal header shows the filename, file date, Download and Open-in-app buttons; expired links return 410 and show "This link has expired"; `GET /api/v1/raw` and `GET /api/v1/file` accept `?link_code=` as an alternative credential; `GET /api/v1/raw` also accepts `?download=1` to force `attachment` Content-Disposition; link state stored in `data_dir/links.db`; expired links swept hourly by a background task; `GET /api/v1/links/:code` rate-limited to 60 req/min per IP
- **Search prefix shortcuts (plan 066)** — query prefixes parsed client-side override scope and match type without touching the Advanced panel: `file:` (filename search), `doc:` / `document:` (document search), `exact:` (exact match), `regex:` (regex match), `type:<kind>` (kind filter); prefixes can be compounded (`file:exact:report.pdf`); active prefixes appear as dismissible chips below the search box; dismissing a chip with a trailing value (e.g. `file:extra`) retains the value (`extra`) in the query
- **Scope and match-type toggles in Advanced Search** — the mode `<select>` is replaced by two toggle-button groups: Scope (Single-line / Filename / Document) and Match (Fuzzy / Exact / Regex); values survive panel open/close and serialise to the URL `mode` param via `toServerMode` / `fromServerMode`
- **New server search modes** — `file-fuzzy`, `file-exact`, `file-regex` (filename-only search), `doc-exact`, `doc-regex` (document-level exact/regex); added `group_by_file` helper in `routes/search.rs` for document-family modes
- **`[PATH]` prefix convention for `line_number=0` path entries (plan 067)** — all path lines in the index are now stored as `[PATH] relative/path` instead of bare strings; PE version-info keys reformatted as `[PE:Key]`; enables unambiguous SQL/Rust filtering of path lines vs. EXIF/audio/MIME/PE metadata at `line_number=0`; existing indexes must be rebuilt with `find-scan --force`
- **`searchPrefixes.ts` unit tests** — 39 vitest tests covering all prefix combinations, compound tokens, kind filters, conflict resolution, safety fallback, case insensitivity, quoted strings, and chip-removal behaviour (`value` field on `PrefixToken`)
- **Embedded video player (plan 069)** — video files (mp4, webm, mkv, avi, mov, wmv, m4v, flv) now open in an HTML5 `<video controls>` player in the detail view; the player is shown by default (same behaviour as images); a "View Extracted" button toggles to the metadata/text view; formats the browser cannot decode natively show the browser's built-in unsupported-format message
- **File content pagination (plan 056)** — `/api/v1/file` accepts optional `offset` and `limit` query params to return a page of content lines; `total_lines` now reflects the true line count regardless of page size; `line_offsets` is always populated for non-first pages; metadata rows (`line_number=0`) are always returned in full; `GET /api/v1/settings` includes `file_view_page_size` (default 2000, configurable via `[server] file_view_page_size` in server.toml); `FileViewer.svelte` enters paged mode when the file exceeds the threshold — the initial page is anchored to the selected line, forward pages load on scroll, backward pages load via a "Load earlier lines" button with scroll-position preservation; 12 integration tests cover pagination correctness, `total_lines` accuracy, metadata presence, `line_offsets` population, out-of-bounds offsets, and backward-compat (no params returns full file)
- **`file:` search returning EXIF/audio/PE metadata rows** — `file-*` modes now post-filter candidates in Rust (`c.content.starts_with("[PATH] ")`) after `fts_candidates` reads ZIP content, excluding non-path `line_number=0` rows; the SQL-level `AND l.line_number = 0` filter remains (content is not a column in `lines`)
- **`[PATH]` prefix shown in file viewer header** — `/api/v1/file` now strips the `[PATH] ` prefix before returning the `metadata` array so `FileViewer.svelte`'s `s === compositePath` check correctly suppresses the path line
- **`[PATH]` prefix shown in search result snippets** — `make_result` in `routes/search.rs` strips the prefix from the snippet before returning
- **Source badge ("projects") not vertically centred in PathBar** — `align-items: flex-start` changed to `align-items: center`; compensating `padding-top: 2px` on `.path-plain` removed
- **Dismissing a prefix chip also removed trailing search term** — `file:extra components` → clicking ✕ on the "filename" chip now produces `extra components`; `PrefixToken` gains a `value` field (non-prefix remainder) and `removePrefixToken` replaces rather than deletes the raw token
- **Ctrl+P file picker shows terminal filename for nested archive members** — `splitDisplayPath` now uses `lastIndexOf('::')` and `lastIndexOf('/')` to find the deepest path separator, so e.g. `fixtures.tgz::c.tar::200ccc.txt` displays as `200ccc.txt` on the left and `fixtures.tgz::c.tar` on the right; previously `c.tar::200ccc.txt` was shown unsplit on the left when the inner path contained nested `::` segments
- **`scanner_version` never persisted to database** — `pipeline.rs`'s file upsert omitted `scanner_version` from both the `INSERT` column list and the `ON CONFLICT DO UPDATE SET` clause, so every indexed file retained `scanner_version = 0` regardless of what the client submitted; `find-scan --upgrade` consequently re-indexed all files on every run since `0 < SCANNER_VERSION` always; the column is now included in the upsert so upgrade runs correctly converge
- **External pluggable extractors (`[scan.extractors]`)** — users can now wire in system tools for file formats the built-in extractors don't support; configure an extension with `mode = "stdout"` (capture tool stdout as file content) or `mode = "tempdir"` (extract to temp dir and walk members, stored as composite `outer::member` paths identical to built-in archives); the `{file}`, `{name}`, and `{dir}` placeholders are substituted in args; a `"builtin"` sentinel preserves existing behaviour for any extension; example: `rar = { mode = "tempdir", bin = "unrar", args = ["e", "-y", "{file}", "{dir}"] }` adds RAR support on platforms where the built-in `unrar_sys` crate won't compile
- **`find-watch` ignoring `[scan.extractors]` config** — external extractor routing (`resolve_extractor`) was only wired into `find-scan`; `find-watch` called `extract_via_subprocess` directly and always used the built-in extractor; both call sites in `watch.rs` now dispatch through `resolve_extractor` so e.g. a `rar = { mode = "tempdir", ... }` config is honoured on file-change events
- **`find-watch` tempdir-mode archives indexed as `"unknown"`** — when an external tempdir extractor returned members, the outer file was submitted with `kind = detect_kind_from_ext(ext)` which returned `"unknown"` for unrecognised extensions (e.g. `.rar`), preventing the file from being expanded as an archive in the UI; the outer file is now submitted with `kind = "archive"` whenever the extractor returned members with `archive_path` set
- **Tray icon pinning lost after reinstall** — `find-tray` now registers the notification-area icon using `NIF_GUID` with a stable GUID (`{8A3F5D2C-…}`) instead of the default `uID`-based registration; Windows uses the GUID to persistently track the icon's pinned/hidden preference, so reinstalling no longer loses the tray-pin or creates duplicate entries in "Other system tray icons"; the popup window also registers for the `TaskbarCreated` broadcast (with UIPI allow) and re-registers the GUID icon if Explorer restarts
- **Archive member exclude filter broken for nested archives** — the client-side filter in `scan.rs` was taking the _last_ `::` segment of a member's composite path for glob matching; for a member like `node_modules/npm.tgz::package/index.js` the last segment is `package/index.js`, losing the `node_modules/` prefix and allowing excluded directories to slip through; fixed to check all `::` segments — if any segment matches an exclude pattern the member is skipped
- **Exclude patterns not applied inside archives** — `scan.exclude` glob patterns (e.g. `**/node_modules/**`) are now applied within archive extraction: the `find-extract-archive` subprocess receives the patterns as a JSON-encoded 5th argument and filters members before emitting them; additionally, `ExtractorConfig` gains an `exclude_patterns` field used by the in-process extraction path (server-side extraction); `build_globset` is moved to `find-extract-types` so both the filesystem walker and the archive extractor share the same implementation
- **`find-watch` crash on inaccessible directories** — replaced single `RecursiveMode::Recursive` watch (which aborts the entire tree on the first permission error) with a `WalkDir`-based setup that calls `watcher.watch(dir, NonRecursive)` per directory, skipping inaccessible ones with a warning
- **`find-watch` respects `[scan]` config during watch setup** — `watch_tree` now mirrors `find-scan`'s walk behaviour: honours `include_hidden` (skips dot-files/dot-directories when `false`), `follow_symlinks`, `.noindex` markers, and `exclude` glob patterns; previously these settings were ignored during watch registration, causing e.g. `.cargo` and `.oh-my-zsh` to be needlessly traversed
- **`find-watch` applies terminal pruning** — `include_dir_prefixes` (previously only used in `find-scan`) is now shared via `path_util` and applied during watch setup; only directories on the path to an `include` pattern terminal are registered, avoiding registering watches for irrelevant subtrees
- **`find-scan` logs inaccessible paths at WARN** — permission-denied walk errors during the one-time scan now log at `warn` (was `debug`) so users can see why paths are being skipped; excluded paths that surface an OS error before `filter_entry` runs still log at `debug`
- **`find-watch` registers new directories dynamically** — `Create` events on directories trigger `watch_tree` for the new subtree so files added inside a freshly created directory are detected
- **Scan progress counters** — `new`, `modified`, and `upgraded` counts in progress log lines now only reflect files that were actually indexed; previously they were pre-incremented before `process_file` ran, so files later excluded by a filter or missing extractor were incorrectly counted as "new" in intermediate logs
- **`excluded` shown in progress logs** — excluded file count is now included in the periodic progress line (`N unchanged, M excluded`) so it's visible during a scan, not just in the final summary
- **`foreign_keys = ON` per connection** — `PRAGMA foreign_keys` is now re-enabled on every SQLite connection open; previously it was only set once at schema creation time and had no effect on subsequent connections
- **Stale path entry after rename** — `get_file_lines` and `get_metadata_context` now fix the `line_number=0` path entry inline when it doesn't match `files.path` (caused by a rename without re-indexing); guards against accumulated `line_number=0` duplicates from historical data missing the FK cascade
- **"Refresh results" banner not dismissing** — clicking "refresh results" re-triggered the `$liveEvent` reactive block (Svelte tracks `deletedPaths` as a dependency when `doSearch` resets it with `new Set()`), immediately re-setting `resultsStale = true`; fixed by tracking the last handled event by object reference and skipping re-processing of already-handled events
- **FTS5 syntax error on queries containing `.`** — `build_fts_query` now splits on any non-alphanumeric/non-underscore character (not just whitespace) so e.g. `plan.index` yields tokens `plan` and `index` instead of the bare `plan.index` token that caused `fts5: syntax error near "."`
- **Filename highlight missing tokens split by `.`** — `highlightPath` in `SearchResult.svelte` now mirrors the backend tokenisation (splitting on `\W+`) so both parts of e.g. `img.jpg` are highlighted in filename-match results
- **Pointer cursor on `:line` badge** — the non-interactive line-number badge in search result headers now shows a default cursor instead of inheriting the pointer cursor from the clickable header

### Changed

- **highlight.js language packs lazy-loaded per language** — 27 language packs are now dynamic imports instead of statically bundled; each pack is fetched on first use for that file type; `highlightFile` and `highlightLine` are now async; search result context lines render plain text immediately then update with highlighting once the pack loads
- **RTF.js build warning suppressed** — Vite `onwarn` handler silences the chunk-too-large warning for the rtf.js chunk, which is intentionally large and lazy-loaded only when an RTF file is opened
- **Vite dev server binds to `0.0.0.0`** — added `host: true` to `vite.config.ts` so the dev server is reachable from Windows Chrome when running inside WSL2 (previously bound to `127.0.0.1` only)
- **`max_line_length` removed from client `ScanConfig`** — line wrapping is a server normalization concern (`NormalizationSettings`); clients no longer configure or forward this value; `find-scan` subprocesses receive `0` (disabled) for line length
- **Duplicate paths — universal accordion bar** — duplicate file paths are now shown in a dedicated bar below the toolbar for all file types (images, audio, video, text, PDFs); single duplicate is shown inline; 2+ use a collapsible accordion with an SVG triangle chevron; removed per-viewer duplicate display from `ImageViewer`, `AudioViewer`, and `VideoViewer`
- **Plan 086 — remove `inline_threshold_bytes` and `file_content` table** — small-file inline storage is gone; all content now lives exclusively in the `SqliteContentStore`; `inline_threshold_bytes` removed from `ScanConfig`/`server.toml`; `file_content` table dropped (schema v14 migration); `content_hash` column renamed to `file_hash` everywhere; `is_binary_ext_path` hash-skip guard narrowed to `is_open_blocking_ext_path` (vmdk/vhd/iso/img only); SCANNER_VERSION bumped to 3; MIN_CLIENT_VERSION bumped to 0.6.2
- **FTS5 re-index cleanup** — on re-indexing a modified file, the worker now issues the FTS5 `'delete'` command for every old line before inserting new content; the old content is sourced from the `SqliteContentStore` using the previous `file_hash`; empty lines are skipped in the delete pass (issuing `'delete'` with `""` corrupts FTS5 state for that rowid); `process_file_phase1` receives an `Option<&dyn ContentStore>` for this purpose
- **Content-store `chunk_blob` redesign** — chunks are now stored as lines joined by `\n` with no trailing newline (previously each line was stored with a trailing `\n`); `get_lines` uses `str::lines()` for reconstruction, which correctly handles the trailing-newline artifact and preserves interior empty lines; `chunk_blob("")` naturally returns an empty vec; empty lines are no longer filtered from `get_lines` results (UX handles display)
- **Trailing whitespace trimming** — line content is now `trim_end()`-stripped before being written to the FTS5 index and before being stored in the content store blob; this keeps the indexes clean without affecting search correctness
- **Image viewer unified** — the in-app image detail view now uses `DirectImageViewer` (zoom/pan, scroll-wheel zoom, drag, double-click reset) instead of the static `ImageViewer`; `ImageViewer.svelte` deleted; the "View Split/Extracted" toggle removed (zoom replaces the need for full-width mode); EXIF metadata drawer sits alongside the viewer as before
- **Download Archive button for zip members** — when viewing a file inside a zip archive where individual member download is supported, a "Download Archive" button now appears alongside "Download", allowing the outer `.zip` to be saved directly
- **Duplicate list bullet points** — each entry in the expanded duplicates accordion now has a `•` bullet prefix for visual clarity
- **MetaDrawer toggle wider** — the collapse/expand chevron strip doubled from 20 px to 40 px for easier clicking
- **`MetaDrawer` component** — the metadata side-panel in image, audio, and video viewers is now a shared `MetaDrawer.svelte` component; it is collapsible (chevron toggle), starts collapsed for images (metadata secondary to the image) and open for audio (metadata is the primary content); eliminates duplicated layout CSS across three viewers
- **Tooltip consistency** — the `data-tooltip` + CSS `::after` pattern (introduced for PathBar) is now applied to all toolbar buttons (tree-toggle, search-help `?`, word-wrap, view-original); native `title` attributes removed; tooltips appear centred below the button at a consistent offset
- **`SearchResult.aliases` renamed to `duplicate_paths`** — field renamed to match the new `duplicates`-table-based implementation; no backwards compatibility shim (full re-index required anyway)
- **`find-common` build.rs removed** — git hash constants (`GIT_HASH`, `GIT_TAG`, `GIT_DIRTY`) are now injected via environment variables set by the mise build tasks rather than a `build.rs` script; raw `cargo build` invocations fall back to `"unknown"`; eliminates a build-time `git` subprocess that caused unnecessary rebuilds
- **`find-scan --force` prints resume hint on Ctrl+C** — interrupting a force re-index now prints `Interrupted. To resume, run: find-scan --force {epoch}` before exiting with code 130
- **`find-admin status --watch` scrollback pollution** — replaced `\x1b[2J\x1b[H` (erase-entire-display + cursor-home, which pushes previous content into terminal scrollback in Windows Terminal and iTerm2) with `\x1b[H\x1b[J` (cursor-home then erase-from-cursor) so successive watch frames overwrite in-place without accumulating in scrollback
- **SQLite WAL mode** — source databases now open with `PRAGMA journal_mode = WAL; PRAGMA synchronous = NORMAL;`; previously the default rollback-journal mode held an exclusive write lock for the full duration of every index transaction, blocking concurrent search queries; WAL mode allows reads and writes to proceed concurrently; `synchronous = NORMAL` is safe with WAL and significantly faster than the default `FULL` mode
- **Formatter stderr noise** — when an external formatter (e.g. biome) exits with a non-zero code, its stderr output is now logged at `debug` level rather than included in the `warn` line; the warn still fires so failures are visible, but multi-line tool output (e.g. biome's "Code formatting aborted due to parsing errors") no longer clutters production logs
- **Video metadata crash on large files** — replaced `audio_video_metadata` (which calls `read_to_end` on the entire file, panicking with `capacity overflow` on multi-GB MKVs) with `nom-exif` which uses seek-based I/O and never loads the full file; `nom-exif` covers MP4, MOV, MKV, WebM natively; AVI, WMV, FLV, MPEG, OGV fall back to magic-byte detection that emits at minimum a `[VIDEO:format]` line so the file is findable by container type
- **Symphonia audio metadata (replaces id3/metaflac/mp4ameta)** — the three separate audio tag libraries are replaced by a single `symphonia` dependency; in addition to tags (title, artist, album, year, genre, comment, composer, track, disc), the extractor now emits technical stream metadata as `[AUDIO:codec]`, `[AUDIO:sample_rate]`, `[AUDIO:channels]`, `[AUDIO:bit_depth]`, and `[AUDIO:duration]`; WAV, OGG Vorbis, and Opus files previously returned empty and are now fully extracted; 8 unit tests added covering WAV (generated), MP3 (ID3v2 fixture), FLAC (Vorbis comments fixture), corrupt-file graceful failure, extension dispatch, and `is_audio_ext` coverage
- **Function context structs (plan 073 stage 5)** — two functions carrying `#[allow(clippy::too_many_arguments)]` are refactored: `push_non_archive_files` in `client/src/scan.rs` gains an `ExtractedFile` struct bundling its 8 parameters; `process_request_async` in `worker/request.rs` gains `RequestContext` (4 path fields, per-request) and `IndexerHandles` (5 handle fields, constructed once per worker task); removes both `#[allow(clippy::too_many_arguments)]` annotations
- **`ParamBinder` for dynamic SQL queries (plan 073 stage 4)** — replaces manually hand-numbered `?1`/`?2`/`?3` placeholders in `db/search.rs` with a `ParamBinder` helper that auto-numbers params via sequential `push()` calls; applied to all three dynamic query sites (`fts_count`, `fts_candidates`, `document_candidates`); inserting or removing a filter clause no longer requires renumbering downstream params; removes the now-dead `kind_in_clause` helper
- **Named structs for anonymous tuples (plan 073 stage 3)** — five positional tuple types replaced with named structs: `WatchSource` (replaces 5-element tuple in `watch.rs`), `TreeRow` (intermediate DB row in `db/tree.rs`), `DocumentGroup` (representative + members in `db/search.rs`), `ScoredResult` (result + file_id in `routes/search.rs`), `AliasRow` (file_id + path in `db/mod.rs`); no behaviour change — purely internal, all public API surfaces unchanged
- **`SearchMode`, `RecentAction`, `WorkerQueueSlot` enums (plan 073 stage 2)** — `SearchMode` replaces `mode: String` in `SearchParams`; the search handler now uses an exhaustive `match` instead of `mode.as_str()` string comparisons; `RecentAction` replaces `action: String` on `RecentFile`; `WorkerQueueSlot` replaces `queue: String` on `InboxShowResponse`; all three enums preserve their existing wire formats via `serde` attributes; `SearchMode` uses `#[serde(other)]` on `Fuzzy` to safely handle unknown mode strings from future clients
- **`FileKind` enum (plan 073 stage 1)** — replaced all `kind: String` fields across the API, DB, and client with a typed `FileKind` enum (`Text`, `Pdf`, `Archive`, `Image`, `Audio`, `Video`, `Document`, `Executable`, `Epub`, `Unknown`); wire format is unchanged (`serde(rename_all = "lowercase")`); `#[serde(other)]` on `Unknown` ensures unrecognised strings from older clients deserialise cleanly; `SourceStats.by_kind` key changes from `String` to `FileKind`; `DateFilter.kinds` becomes `Vec<FileKind>`; `is_text_like()` helper replaces scattered `kind == "text" || kind == "pdf"` checks
- **Ctrl+P panel fixed at 800 px** — panel width changed from `fit-content` (min 640 px) to `min(800 px, 90vw)`; items fill the panel width (`width: 100%`) so the directory column truncates cleanly instead of expanding the panel; filename column no longer capped at 50% so long filenames are never clipped
- **`index_file_bytes` extracted to `batch.rs`** — the inline content-byte counting used by the scan loop's byte-budget flush check is now a named `pub fn index_file_bytes(file: &IndexFile) -> usize` in `batch.rs`, tested with unit tests covering normal, empty, and oversized files; `scan.rs` uses the shared helper
- **`+page.svelte` view state machine documented and centralised** — added a comment block explaining the two-state discriminated union (`fileView === null` ↔ search, non-null ↔ file panel) and the two orthogonal overlays (`showTree`, `showPalette`); all transitions to the file panel now go through a single `openFileView(fv)` helper to prevent the push-history step being omitted
- **Extractor boilerplate centralised** — `find-extract-types` gains a `run` module with `run_extractor` and `init_tracing` helpers; all six extractor `main.rs` files (text, PDF, epub, office, media, html) reduced to ~10 lines each; `serde_json` and `tracing-subscriber` deps moved from the individual extractor crates to `find-extract-types`
- **`db/mod.rs` unit tests** — 17 `#[cfg(test)]` tests added covering `delete_files_phase1` (basic delete, archive-member cascade, canonical promotion, no-op on missing), `rename_files` (path update, member rename, FTS update, skip-existing-target), FTS5 round-trip (insert→find, delete orphans entries via JOIN filter), `list_files` returning `indexed_at`, `log_activity`/`recent_activity` round-trip, and `update_last_scan`/`get_last_scan` round-trip
- **`worker/request.rs` extracted** — single-request processing logic (`process_request_phase1`, `process_request_async`, `is_db_locked`, `handle_failure`) moved from `worker/mod.rs` into a new `worker/request.rs`; `mod.rs` reduced from 769 → 378 lines, now containing only the router loop and worker spawn logic
- **`FileViewer.svelte` further split** — `PdfViewer.svelte` (iframe + loading spinner, owns `pdfLoaded` state) and `FileStatusBanner.svelte` (deleted/renamed/modified banners and indexing-error banner) extracted; `FileViewer.svelte` reduced from 665 → 533 lines
- **Shared walk module for `find-scan` and `find-watch`** — `build_globset` and the full directory-traversal logic (hidden-file pruning, `.noindex` detection, exclude-glob matching, terminal pruning) are extracted into a new `crates/client/src/walk.rs` module (`walk_source_tree`, `build_globset`); both `find-scan` and `find-watch` now delegate to the same code path, guaranteeing identical filtering behaviour across scan and watch operations
- **HTTP integration tests for `find-server`** — new `crates/server/tests/` suite (23 tests across `smoke`, `index_and_search`, `delete`, `multi_source`, `errors`) spins up a real Axum server on an ephemeral port per test and exercises the full request→worker→response cycle; server init logic extracted from `main.rs` into `lib.rs` (`create_app_state`, `build_router`) to enable in-process test server construction; `mise run test` added to run all unit and integration tests
- **`max_file_size_mb` renamed to `max_content_size_mb`** — all documentation, example configs, and installer templates updated; the old key is still accepted as an alias for backward compatibility
- **`mise dev` creates `web/build/` if missing** — `mkdir -p web/build` runs before `cargo-watch` starts so the `#[derive(RustEmbed)]` folder check doesn't abort the build when the web UI hasn't been built yet
- **`mise dev` enables debug logging** — server started with `RUST_LOG=debug` in the dev task
- **Normalization formatter paths use shims** — local `config/server.toml` updated to use `~/.local/bin/biome` and the pnpm global `prettier` shim instead of version-pinned mise install paths
- **Config warnings printed to stderr** — `parse_client_config` and `parse_server_config` now return `(Config, Vec<String>)` instead of routing unknown-key warnings through the tracing logger; client tools print them with `Warning: <message>` on stderr; the server logs them via `tracing::warn!`
- **`pending_chunk_removes` removed** — the hot-path mechanism that queued ZIP chunk refs for immediate removal during file deletes/re-indexes is removed; orphaned chunks are now reclaimed lazily by the daily compaction pass; simplifies `delete_files_phase1`, `archive_batch`, and `WorkerHandles`; the `pending_chunk_removes` table is dropped on startup via `DROP TABLE IF EXISTS`
- **Stale `pending_chunk_removes` INSERT calls removed from pipeline** — `process_file_phase1` and `process_file_phase1_fallback` still had `INSERT INTO pending_chunk_removes` statements left over from before the compaction refactor; these caused a "no such table" crash on databases that had never had the table (e.g. first run after the DROP was added); removed in both the inner-archive delete path and the per-file re-index path
- **Markdown normalization** — markdown files are no longer exempt from text normalization; they now pass through external formatters (e.g. prettier) and word-wrap at `max_line_length` like any other text file
- **Long-word hard-split** — words longer than `max_line_length` are now split at the character boundary instead of being kept whole and overflowing the limit
- **`GET /api/v1/file` response format** — `FileResponse.lines` is now `Vec<String>` (content only) instead of `Vec<ContextLine>`; line-number-0 entries are separated into a new `metadata: Vec<String>` field; `line_offsets: Vec<usize>` is included only when line numbers are not a contiguous 1-based sequence (omitted for the common case); a new `content_unavailable: bool` field signals that the file was indexed in phase 1 but the archive worker has not yet written content to ZIP
- **"Content not yet available" in file viewer** — when `content_unavailable` is set, the file viewer shows a specific message with an inline Reload link instead of rendering empty lines
- **"Content has changed" banner instead of auto-reload** — when a live SSE update fires for the file currently being viewed, the viewer now shows an amber "Content has changed — Reload" banner instead of immediately reloading; the user controls when the refresh happens
---

## [0.6.2] - 2026-03-11

### Added

- **Text normalization** — the server now normalizes all text content before writing it to ZIP archives; minified JSON/TOML files are pretty-printed using built-in formatters; any file type can be routed through an optional external formatter binary (e.g. biome, prettier, rustfmt) configured in `server.toml`; lines exceeding `normalization.max_line_length` (default 120) are word-wrapped; markdown files are exempt (line structure is semantically meaningful); normalization runs in phase 1 and the normalized content is written to a new `.gz` in `to-archive/` so the archive phase reads pre-formatted content without re-invoking any formatter
- **`max_markdown_render_kb` setting** — added to `ServerAppSettings` (default 512); exposed via `GET /api/v1/settings`; the file viewer skips HTML rendering and shows plain text when a markdown file exceeds this threshold, preventing browser stalls from very large files
- **Text file "Download" button** — the `FileViewer` toolbar now shows a `Download` link for text files (replaces the non-applicable `View Original` toggle); PDF and image files keep their existing `View Original` / `View Extracted` / `View Split` toggles unchanged
- **HTTP request tracing** — `tower_http::TraceLayer` added to the axum router; each request logs method + URI on arrival and status + elapsed on completion at DEBUG level; enable with `RUST_LOG=tower_http=debug`
- **Worker debug timing logs** — a `timed!` macro wraps all expensive steps in both the indexing phase (`read+decode gz`, `open db`, `acquire source lock`, `delete/rename paths`, `normalize <file>`, `index N files`, `cleanup writes`, `write normalized gz`) and the archive phase (`parse gz files`, `take pending chunk removes`, `remove N chunks from ZIPs`, `append chunks`, `update line refs`); each step logs elapsed ms at DEBUG level
- **`serde_json` `preserve_order` feature** — JSON objects are now pretty-printed in their original key order rather than alphabetically sorted; applies to the built-in JSON normalizer and all other `serde_json` serialization in the server

### Changed

- **`WorkerConfig` changed from `Copy` to `Clone`** — required to hold `NormalizationSettings` (which contains a `Vec`); all call sites updated to use explicit `.clone()`

- **Activity log for `find-admin recent`** — each source DB now has an `activity_log` table recording add, modify, delete, and rename events for outer files; `GET /api/v1/recent` reads from it by default (falling back to `sort=mtime` for file-table ordering); `RecentFile` response gains `action` (`"added"` / `"modified"` / `"deleted"` / `"renamed"`) and `new_path` (for renames); `find-admin recent` output shows `+`/`~`/`-`/`→` action prefixes and old→new paths for renames; deleted and renamed files remain visible up to `server.activity_log_max_entries` events (default 10 000, pruned oldest-first)
- **`IndexFile.is_new` field** — `find-scan` sets `is_new = true` when the server has no prior entry for a file (server_entry is None); `find-watch` adds `AccumulatedKind::Create` so OS create events are distinguished from modifies across the debounce window (`Create→Modify=Create`, `Create→Delete=Delete`, `Delete→Create=Create`); the server uses this field directly instead of a pre-batch DB lookup to log "added" vs "modified"

### Changed

- **`normalise_path_sep`/`normalise_root` deduplicated** — extracted from `scan.rs` and `watch.rs` into a shared `path_util` module; both binaries now use a single definition; unit tests added for UNC paths, composite `::` paths, bare drive letters, and mixed separators
- **`worker.rs` split into focused modules** — `worker.rs` (1,238 lines, 5 concerns) converted to a `worker/` directory; per-file SQLite writes extracted to `worker/pipeline.rs` (279 lines); archive-phase batch processing extracted to `worker/archive_batch.rs` (333 lines); `worker/mod.rs` retains only the inbox polling loop and request coordinator (657 lines)
- **FileViewer sub-components** — `FileViewer.svelte` (883 lines) split into `ImageViewer.svelte`, `MarkdownViewer.svelte`, and `CodeViewer.svelte`; each sub-component owns its styles and local state; `FileViewer.svelte` reduced to ~490 lines acting as a dispatcher

- **`WorkerHandles` struct** — the six runtime handles passed to `start_inbox_worker` (`status`, `archive_state`, `inbox_paused`, `deleted_bytes_since_scan`, `delete_notify`, `recent_tx`) are now bundled into a `WorkerHandles` struct; reduces the function from 8 parameters to 3 and satisfies the clippy `too_many_arguments` lint
- **`WorkerConfig` struct** — the five scalar config values (`log_batch_detail_limit`, `request_timeout`, `inline_threshold_bytes`, `archive_batch_size`, `activity_log_max_entries`) are now bundled into a `WorkerConfig` struct; `start_inbox_worker` drops from 11 parameters to 7; adding new worker settings now only requires changing the struct definition and its construction site in `main.rs`
- **Per-request archive byte-cache in `ArchiveManager`** — `read_chunk` now caches the raw bytes of each ZIP opened during an `ArchiveManager` instance's lifetime; since `new_for_reading` creates a fresh manager per blocking task (search, context, file), a single request that reads multiple chunks from the same archive pays only one `File::open` call instead of one per chunk; uses `RefCell` for interior mutability so all existing `&ArchiveManager` call sites are unchanged
- **Archive rewrite temp-file cleanup** — if `rewrite_archive` fails mid-write (e.g. disk full), the partial `.zip.tmp` file is now removed before the error propagates; previously the orphaned temp file was left on disk indefinitely
- **`FileViewState` bundles file-viewer state** — `fileSource`, `currentFile`, `fileSelection`, `panelMode`, `currentDirPrefix` (5 separate variables) replaced by a single `FileViewState | null` in `+page.svelte` and `FileView.svelte`; `fileView === null` is now the authoritative "show search results" condition (replaces `view === 'results'`); event handlers set all fields atomically; impossible states (e.g. `view === 'file'` with `currentFile === null`) are now unrepresentable; `AppState` and URL serialization are unchanged
- **`collapse` extracted from watch accumulator** — the event-collapse transition table (`Create+Modify→Create`, `Create+Delete→Delete`, `Delete+Create→Create`, `Update+Delete→Delete`, `Delete+Update→Update`, same→last-wins) is extracted to a `fn collapse(existing, new) -> AccumulatedKind` pure function; 10 unit tests cover every transition including multi-step sequences; `accumulate` now delegates to `collapse`
- **`needs_reindex` extracted from scan loop** — the file re-index decision (`None → new`, `mtime_newer → modified`, `scanner_version_old → upgraded`, `same → skip`) is extracted to a `pub(crate) fn needs_reindex(server_entry, local_mtime, upgrade) -> (bool, bool)` pure function; 8 unit tests cover new files, mtime newer/equal/older, upgrade flag on and off, current vs outdated scanner version, and composite-path filtering invariant
- **`find_common::path` module** — composite archive-member path operations (`is_composite`, `composite_outer`, `composite_member`, `split_composite`, `make_composite`, `composite_like_prefix`) are now centralised in `find-common`; all ad-hoc `contains("::")`, `split_once("::")`, and `format!("{}::%", …)` call sites across the server, client, and worker now use these helpers; eliminates the risk of divergent `::` handling between scan and watch paths

- **Server worker log format** — indexer logs now use `[indexer:source:req_stem]` prefix with a single completion line per request (`indexed N files, M deletes, K renames, ...`); archive logs use `[archive:source]` per batch; intermediate "start", "Processing N deletes", "Processed N renames", and "Phase 1 complete" lines removed (start demoted to DEBUG); "Queued bulk request" in the bulk route demoted to DEBUG; archive batch suppresses the log entirely when nothing was archived or removed (eliminates "processed 0 files" noise from delete-only batches)

- **Debug builds strip symbols** — `[profile.dev] debug = false` in the workspace `Cargo.toml`; eliminates ~90 GB of DWARF data from `target/debug`; re-enable with `debug = true` when a debugger is needed

- **`find-extract-types` micro-crate** — moves `IndexLine`, `SCANNER_VERSION`, `detect_kind_from_ext`, `ExtractorConfig`, and `mem::available_bytes` into a new minimal crate that depends only on `serde`; all nine extractor crates now depend on `find-extract-types` instead of `find-common`; `find-common` re-exports everything at the same public paths for zero churn in server/client code; breaks the rebuild cascade so touching `api.rs` or `config.rs` no longer triggers a full 14-crate recompile of all extractors (~32 s → ~4 s incremental)

### Added

- **`find-admin recent --follow` / `-f`** — new SSE follow mode stays connected to `GET /api/v1/recent/stream` and prints new activity entries as they arrive (like `tail -f`); the server sends the last `limit` historical entries as an initial burst before streaming live events; the client cancels cleanly on Ctrl+C
- **`GET /api/v1/recent/stream` SSE endpoint** — new server-sent events endpoint streams `RecentFile` JSON frames to any connected client; the inbox worker publishes each add/modify/delete/rename event to a `broadcast::Sender<RecentFile>` (capacity 256) after a successful `log_activity` write; SSE keep-alive pings every 30 s; multiple simultaneous subscribers supported
- **`[cli]` config section with `poll_interval_secs`** — new client config section controls the refresh rate for polling-based CLI modes; `find-admin status --watch` now reads this value (default 2.0 s) instead of a hardcoded 2 s constant; both `install.sh` and the Windows InnoSetup installer template include the commented-out `[cli]` block

- **Two-phase inbox processing (plan 053)** — inbox worker is now split into a single-threaded SQLite-only phase 1 (indexing) and a separate archive phase 2 (ZIP writes); phase 1 writes to SQLite only and moves the `.gz` to `inbox/to-archive/`; the archive thread batches up to `archive_batch_size` requests (default 200), coalesces last-writer-wins per path, rewrites ZIPs for pending chunk removes, appends new chunks, and updates line refs in a single transaction per source; eliminates WAL-mode SQLite deadlocks on WSL/network mounts caused by concurrent write connections; `pending_chunk_removes` table (schema v10) persists chunk refs between phases; per-source `source_lock` mutex in `SharedArchiveState` serialises SQLite writes between the two threads (held only during transactions, not during ZIP I/O)
- **Inbox files no longer moved to `processing/`** — files stay in `inbox/` until the worker finishes (success moves to `to-archive/`, failure moves to `failed/`); the router tracks in-flight paths with a `HashSet` and uses a non-blocking `try_send` to avoid pre-claiming the entire queue; at most one file is buffered ahead of the worker (channel capacity 1); crash recovery is automatic since files in `inbox/` are re-processed on restart; old `processing/` directories are migrated on first startup
- **Archive queue shown in `find-admin status`** — the Inbox line now shows `N awaiting archive` alongside pending and failed counts; `find-admin inbox` also displays the archive queue count

### Fixed

- **Scheduled `find-scan` in `find-watch`** — `find-watch` now runs `find-scan` on a configurable interval (`scan_interval_hours` in `[watch]`, default `24.0`, set to `0.0` to disable); the binary is resolved relative to the current executable so it works correctly under systemd and Windows services; overlap is prevented by tracking the child handle and skipping a tick if the previous scan is still running; missed ticks (e.g. after system sleep) are skipped rather than burst-fired; pass `--scan-now` / `-S` to also trigger one scan immediately at startup
- **`find-admin status --watch`** — new `--watch` / `-w` flag keeps the command running, redrawing stats in-place every 2 seconds using ANSI cursor-up escape codes; exits cleanly on Ctrl+C; the watch loop is sequential — it never starts a new request until the previous one has returned, so at most one stats query is open at a time
- **Worker batch start/done logging** — the worker now logs a `start` line when it picks up an inbox file (showing the inbox filename, source, file count, and delete count) and names the inbox file again in the existing `done` completion line; slow-batch warnings also include the inbox filename
- **Archive totals tracked incrementally** — `SharedArchiveState` now maintains `total_archives` and `archive_size_bytes` as atomics, seeded from the startup directory scan and updated in-place on every archive create, append, and rewrite; `GET /api/v1/stats` reads these atomics directly (zero I/O for archive stats) and uses a single `spawn_blocking` for DB-only queries with a 1 s busy-timeout; the 30 s background refresh task is removed entirely
- **File type filter in advanced search** — the Advanced search panel now has a "File type" section with checkboxes for PDF, Office/eBook, Code & Text, Image, Audio, Video, Archive, and Binary; selected kinds are sent as repeated `?kind=` query params and applied server-side as an `AND f.kind IN (...)` filter in all three search paths (FTS5 count, FTS5 candidates, document-mode candidate intersection); the filter badge count includes active kind selections
- **Windows tray popup improvements** — popup is wider (660×480), shows "Recent activity" title, has 6 px padding between the border and controls, displays full file path per row (`[source]  full/path/to/file`), uses Segoe UI 10 pt ClearType font, always shows a vertical scrollbar, and has a native drop shadow
- **Windows tray interim service status** — stopping or starting the watcher service immediately shows "Watcher: Stopping…" / "Watcher: Starting…" and disables the toggle button until the next status poll confirms the new state
- **Windows tray recent files increased to 50** — the poller now requests the 50 most recently indexed files (was 20)

### Fixed

- **`find-watch` misses files downloaded via browser** — when a browser creates a temporary file and renames it to the final name within the same debounce window, the accumulator collapses `Create→Delete` for the temp path so the rename detector sends a `PathRename` the server can't resolve (old path was never indexed); the watcher now tracks which paths were first seen as `Create` in the current window and, when those appear as the "old" side of a rename pair, upgrades the new path to `Create` so the main loop indexes it directly instead of sending an unresolvable rename
- **Stale search response overwrites date-filtered results** — concurrent `doSearch` calls (e.g. rapid typing into the search box) could leave an in-flight request from an earlier query without a date filter; if that response arrived after the current (filtered) one, it would overwrite `results` and `totalResults` while the NLP date chip still reflected the newer query; fixed by capturing `searchId` before each `await` and discarding the response if a newer search has already started
- **File path truncated in search results** — the file path in search result cards now takes all available flex space (`flex: 1`) rather than being capped at 60% of the header width, and shows a hover tooltip with the full path; paths are still ellipsed when the full text doesn't fit
- **Searches were case-sensitive by default** — fuzzy mode used nucleo's `CaseMatching::Smart` which treats an all-uppercase query as case-sensitive; switched to `CaseMatching::Ignore` so all searches are case-insensitive unless the "Case sensitive" option is explicitly enabled; document mode similarly lowercased tokens before comparing; exact mode FTS5 pre-filter was already case-insensitive but the post-filter now only applies when the option is on
- **Case-sensitive fuzzy mode matched wrong results** — with `CaseMatching::Respect`, nucleo's subsequence algorithm finds scattered lowercase letters across word boundaries (e.g. "monhegan" matched "Monhegan" via 'm' in a prior word + 'onhegan' from the capital-M word); added a per-term literal substring pre-filter so every whitespace-separated query token must appear verbatim in the candidate content before nucleo scoring is attempted
- **Case-sensitive search option** — the Advanced search panel now has a "Case sensitive" checkbox; when enabled, `?case_sensitive=1` is sent to the server; all four search modes (fuzzy, exact, regex, document) respect the flag; the filter badge count increments when active
- **Search box clear button** — a circle-×  button appears in the search box when there is text and the spinner is not active; clicking it clears the query and refocuses the input
- **Multi-hit line navigation redesign** — when a file has multiple hits, the header now shows a compact bordered pill with SVG chevron buttons (‹ / ›) flanking the current line number; buttons are always rendered but hidden via `visibility: hidden` when at the first or last hit so the layout never shifts; clicking the line number no longer navigates to the file; the overall app tone is unchanged
- **Ctrl+P command palette VS Code-style path display** — each row now shows the filename prominently on the left with the directory path dimmed and smaller to the right (matching VS Code's file picker); archive members display the member filename with the outer archive path as the directory context
- **File viewer path bar wraps on long paths** — the breadcrumb path in the file detail header now wraps to multiple lines instead of clipping; very long segments (e.g. GUIDs in mail archive paths) break mid-word rather than overflowing
- **Dark mode secondary text contrast** — `--text-dim` lightened from `#656d76` to `#8b949e`; `--text-muted` and `--badge-text` lightened from `#8b949e` to `#a0aab4`; the search-box clear button uses `--text-muted` so its semi-transparent circle background is visible against dark backgrounds

- **Inbox router dispatches immediately when worker finishes** — the router loop now wakes on the worker's done signal (via `tokio::select!`) instead of waiting up to 1 s for the next poll tick; consecutive single-file watch events now process in milliseconds rather than ~1 s each
- **Watch event buffering replaces sliding debounce** — `find-watch` now accumulates filesystem events for a fixed `batch_window_secs` (default `5.0`) from the first event rather than resetting the timer on every event; the batch is flushed immediately if it reaches `scan.batch_size` files; `debounce_ms` removed and replaced by `batch_window_secs` in `[watch]`
- **Windows file modifications not detected** — `notify` maps `FILE_ACTION_MODIFIED` (ReadDirectoryChangesW) to `ModifyKind::Any`, not `ModifyKind::Data`; the accumulator now also matches `ModifyKind::Any` so file edits are picked up on Windows
- **Windows service not starting after reinstall (root cause)** — the tray app holds an open SCM handle to the service for status polling; when the installer called `DeleteService`, the SCM marked the service "pending deletion" but could not remove it until the tray released its handle; subsequent `CreateService` failed with `ERROR_SERVICE_MARKED_FOR_DELETE`; fixed by killing `find-tray.exe` with `taskkill` before the uninstall/install sequence (tray is relaunched at the end), and hardening `install_service` to poll until `open_service` returns an error before calling `create_service`
- **`find-admin status --watch` stale characters** — `\x1b[H]` (home) + `\x1b[0J]` (clear to end) left trailing characters on lines that got shorter between redraws; now always uses `\x1b[2J\x1b[H]` (full clear) on every redraw
- **`find-admin status` shows inbox paused state** — the Inbox line now appends a yellow `PAUSED` label when inbox processing has been paused; `GET /api/v1/stats` response includes `inbox_paused: bool`
- **InnoSetup checkbox text clipped** — `UseExistingCheck` lacked an explicit height; added `Height := 24` to prevent the "Keep existing configuration" label from being cut off

- **Windows service ignores `exclude_extra`** — the Windows service code path used `toml::from_str::<ClientConfig>` directly, bypassing `parse_client_config` which merges `exclude_extra` globs into `exclude`; switched to `parse_client_config` so exclusion rules apply correctly when running as a service
- **Windows service not restarted after reinstall** — InnoSetup `[UninstallRun]` only fires on explicit uninstall, not on upgrade/reinstall; the installer now explicitly calls `find-watch.exe uninstall` before `find-watch.exe install` in `ssPostInstall`, ensuring a clean service restart on every upgrade
- **InnoSetup "Existing Configuration Found" page text clipped** — label heights and vertical positions adjusted so the descriptive text and file path are fully visible without truncation
- **`find-admin status --watch` leaves stale lines on resize** — the old approach moved the cursor up by the logical line count, which under-counted when lines wrapped at the terminal width; now uses clear-to-end-of-screen (`\x1b[0J`) after each redraw and a full screen clear (`\x1b[2J\x1b[H`) on the first draw, so the display is always correct regardless of line count changes

- **Scroll position restored on back navigation** — clicking "← results" from a file detail view now scrolls the results list back to the position it was at before the file was opened
- **Scan progress log omits zero-count fields** — the periodic progress line now always shows `N unchanged` but suppresses `new`, `modified`, and `upgraded` when they are zero, reducing noise during incremental scans where most files are unchanged
- **Sidebar scrolls independently from search results** — the page layout is now `height: 100vh; overflow: hidden` with the main content column (`overflow-y: auto`) as the scroll container instead of the window; the sidebar tree keeps its position while the user scrolls through results; the load-more scroll listener is moved from `window` to the main-content element (no behavioural change since `isNearBottom` already used viewport-relative `getBoundingClientRect`)
- **Windows tray right-click menu clipped at screen bottom** — the context menu is now shown via `TrackPopupMenuEx` with `TPM_BOTTOMALIGN | TPM_RIGHTALIGN` so it always pops up above the cursor; muda's default `TPM_TOPALIGN` caused the bottom items to be clipped off-screen when the taskbar is at the bottom of the display

### Added

- **`--version` shows commit hash** — all CLI binaries (`find-server`, `find-scan`, `find-admin`, `find-watch`, `find-upload`, `find`) now display a git commit hash suffix for non-release builds (e.g. `0.6.1 (abc1234)`); clean release-tag builds show only the version; dirty working trees show `(dev)`; implemented via a `tool_version!` macro in `find_common` using `option_env!` so each binary embeds its own build-time constants without a `build.rs` (which would fail in the cross-compilation Docker environment due to glibc version mismatches)
- **Event-driven compaction scanner** — the background orphaned-chunk scanner no longer runs on a fixed interval; instead it runs once at startup (30 s delay) and then 60 s after the last delete batch completes, deferring on each subsequent delete so rapid back-to-back deletes coalesce into a single scan; `remove_chunks` now returns bytes freed, which are accumulated in `AppState.deleted_bytes_since_scan` and added to the last scan result to give a live wasted-space estimate between scans without re-scanning; `compact_scan_interval_mins` config key removed

### Changed

- **Dependency deduplication** — `bzip2` unified to 0.6.1 (eliminated the 0.4/0.5/0.6 split); `zip` upgraded from 2.x to 8.x across all first-party crates (no code changes required); `reqwest` unified to 0.13 across the workspace (bumped Windows tray from 0.12, updated `rustls-tls` feature to `rustls`)

### Fixed

- **Search input loses focus while typing** — switching from `pushState` to `replaceState` during debounced searches prevents SvelteKit navigation from stealing focus mid-keystroke; back-button history no longer accumulates an entry per search term

### Added

- **Parallel inbox worker pool** — the inbox is now processed by N concurrent workers (default 3, configurable via `inbox_workers` in `server.toml`); a single slow or stuck request no longer blocks the queue; each worker owns its own in-progress ZIP archive allocated from a shared atomic counter so no two workers ever write to the same archive simultaneously; archive rewrites (re-indexing / deletion) are serialised per-archive via a lock registry in `SharedArchiveState`; `admin/delete_source` uses the same shared state for globally coordinated rewrite locks
- **Stale-mtime guard** — the upsert path now skips writing if the incoming file `mtime` is older than the value already stored; defends against two workers processing requests for the same file out of order
- **Configurable request timeout** — `inbox_request_timeout_secs` (default 1800 s / 30 min) replaces the previous hardcoded 600 s; with parallel workers a stuck request no longer starves the queue, so the generous default is safe
- **Inbox worker request timeout** — each inbox request is now wrapped in a 600 s timeout; if a blocking thread hangs, the worker logs an error, moves the file to `failed/`, and continues the queue so a single slow request can no longer block all other work
- **Batch summary logging** — after processing each inbox batch the worker logs a one-line INFO summary (file count, delete count, content lines, KB content, KB compressed, elapsed seconds); batches taking ≥ 120 s additionally emit a structured WARN for alerting
- **Plan 049: parallel inbox workers** — design document for global worker pool, per-worker ZIP archives, per-archive rewrite locking, and configurable worker count
- **Per-source application-level write lock** — each source now has a `std::sync::Mutex` in `SharedArchiveState`; workers acquire it per delete-chunk or per file-upsert and release immediately, replacing unreliable SQLite `busy_timeout` on network mounts (POSIX advisory locks); a large 80 k-delete batch holds the lock for only 100 paths at a time so other workers on the same source slip in between chunks
- **Batched archive rewrite on delete** — all orphaned chunk refs across an entire delete request are accumulated before calling `remove_chunks`; since `remove_chunks` groups by archive internally, each affected ZIP is now rewritten at most once per request regardless of how many delete chunks referenced it
- **Separate crash-recovery from worker startup** — `recover_stranded_requests()` is now a standalone function called unconditionally at startup; `inbox/processing/` files are always moved back to `inbox/` before the worker pool starts, including when inbox processing is paused
- **Inbox pause / resume** — `find-admin inbox-pause` stops the router from dispatching new work and immediately returns any in-flight jobs from `inbox/processing/` back to `inbox/`; `find-admin inbox-resume` resumes normal processing; `find-admin inbox` shows a PAUSED banner when paused; `GET /api/v1/admin/inbox` response includes a `paused` field
- **`find-admin compact`** — new command rewrites ZIP content archives to remove orphaned chunks (entries no longer referenced by any `lines` row); uses `--dry-run` to report wasted space without modifying files; acquires per-archive rewrite locks so it is safe to run while the server is processing inbox items
- **Background orphaned-chunk scanner** — a background task runs every `compact_scan_interval_mins` minutes (default 60, configurable) to compute total and orphaned compressed bytes across all archives using ZIP Central Directory reads (no content decompression); results are cached in `data_dir/server.db` and survive restarts; `find-admin status` shows `Wasted: X MB (Y%)` with the age of the last scan
- **Worker index in logs** — inbox worker log lines now include `[worker N]` so per-worker activity is distinguishable in `journalctl` output
- **`inbox_delete_batch_size` config** — controls how many file deletions are processed per SQLite transaction (default 100); `inbox_delete_batch_size = 100` in `defaults_server.toml`
- **`db::delete_files` returns chunk refs** — the function now returns `Vec<ChunkRef>` instead of performing ZIP cleanup inline; callers do the ZIP rewrite after releasing the source lock, so the lock is held only for the fast DB transaction

- **`include` in `.index` files** — per-directory `.index` files now support an `include` field containing glob patterns; only files matching at least one pattern are indexed within that subtree, allowing precise whitelisting without `.noindex` (e.g. `include = ["myfolder/**"]` in `backups/.index` indexes only `backups/myfolder/`); replacement semantics — innermost `.index` with `include` wins; patterns are relative to the directory containing the `.index` file
- **`find-admin recent`** — new subcommand lists the N most recently indexed or recently modified files across all sources; supports `--limit` and `--mtime` flags; `--json` for machine-readable output
- **`find-admin check` reports min client version** — the server's `min_client_version` is now included in the version line of `find-admin check` output

### Fixed

- **Missing extractor binary treated as deployment error** — when a subprocess extractor binary is not found (`ENOENT`), `find-scan` and `find-watch` now log a single `ERROR` (suppressing per-file repetitions) and skip the file entirely so it is retried once the binary is correctly deployed; previously the file would be indexed filename-only with no way to recover without a forced rescan
- **`/api/v1/tree` crashes on NULL size** — `list_dir` now reads `size` as `Option<i64>`; archive members with unknown size (NULL in the DB) no longer cause an "Invalid column type Null" error
- **`update-nas.sh` missing `find-extract-dispatch`** — the NAS deploy script now includes `find-extract-dispatch` alongside all other extractor binaries

### Changed

- **`GET /api/v1/recent` validates limit** — requests exceeding `MAX_RECENT_LIMIT` (1000) now return HTTP 400 Bad Request instead of silently capping the result

---

## [0.6.1] - 2026-03-07

### Added

- **Tray recent-files popup** — left-clicking the tray icon opens a Win32 popup window listing the 20 most recently indexed files; the list refreshes continuously while the popup is open (demand-driven polling); each row shows `[source]  filename   (parent dir)`; popup auto-dismisses on focus loss or Escape
- **`GET /api/v1/recent`** — new server endpoint returns the N most recently indexed outer files (no archive members) across all sources, sorted by `indexed_at` descending with `mtime` as a fallback for rows predating the feature
- **`[tray]` config section** — `poll_interval_ms` controls how often the popup refreshes while open (default 1000 ms)
- **Windows installer robustness** — service install is now idempotent (stops and deletes an existing service before recreating); `find-tray.exe` is force-killed before file copy; `CloseApplications=yes` and `RestartApplications=yes` added for graceful shutdown on upgrade
- **Windows installer UX** — directory-selection page removed; `path` is auto-populated from `%SYSTEMDRIVE%\` and `include` from `%USERPROFILE%` relative path; source name defaults to `%COMPUTERNAME%`; find-watch service registration and tray launch now happen automatically (no finish-page checkboxes); only the optional full-scan checkbox remains
- **Windows installer icon** — setup application now shows the magnifying-glass icon (`icon_active.ico`) instead of the plain blue square

### Fixed

- **Tray popup race condition** — left-clicking the tray icon to dismiss the popup no longer immediately reopens it; close request from `WM_ACTIVATE`/`WA_INACTIVE` is captured before processing tray click events
- **Tray right-click menu** — removed `with_menu()` from `TrayIconBuilder` (which caused the menu to appear on both clicks); context menu is now shown manually via `show_context_menu_for_hwnd` on right-click only
- **Recent files `indexed_at` always populated** — the worker now updates `indexed_at` on every re-index (not only on first insert), so the popup shows recently re-indexed files; the `recent_files` DB query falls back to `mtime` for rows where `indexed_at IS NULL` (databases predating the feature)
- **Windows service description** — includes link to GitHub repository

- **`--version` flag** — all binaries (`find-scan`, `find-watch`, `find-anything`, `find-admin`, `find-upload`, `find-server`) now support `--version` to print the build version
- **Server minimum client version** — `GET /api/v1/settings` now returns `min_client_version`; all client binaries check this on startup and refuse to run with a clear error if they are too old; update `MIN_CLIENT_VERSION` in `crates/common/src/api.rs` whenever a breaking API change is made
- **Project commit workflow** — `.claude/commands/commit.md` codifies the pre-commit checklist (clippy, `MIN_CLIENT_VERSION` check, CHANGELOG update)
- **Fix include-glob directory pruning** — sibling directories were incorrectly traversed when using include patterns; `Users/Administrators/**` would also descend into `Users/Administrator` because `Users/` was treated as an ancestor prefix; the fix (a) separates ancestor dirs from terminal dirs so only children of the correct terminal are allowed, and (b) uses `rfind('/')` before the first wildcard so patterns with `?`, `{…}`, or `[…]` in directory names never cut a component in half; negation patterns (`!`) now cause the pruning to fall back to traversing everything rather than silently skipping files; simplified to a single terminal set with a three-way filter check (exact, ancestor, descendant); unit tests added for extraction and allow/deny logic
- **Suppress access-denied walk warnings on Windows** — "access denied" errors during directory traversal (e.g. `C:\Users\Administrator`) are now logged at `debug` level instead of `warn`; errors on paths that match an exclude glob are also suppressed since the exclusion should have prevented the descent
- **Self-update** — About panel now checks for new releases via the server (`GET /api/v1/admin/update/check`) and can apply them in one click (`POST /api/v1/admin/update/apply`); the server downloads the matching binary from GitHub, replaces itself atomically, and exits cleanly so systemd restarts onto the new version; only available when running under systemd (`INVOCATION_ID` set); version check results are cached for one hour

---

## [0.6.0] - 2026-03-06

### Added

- **`find-scan` directory argument** — `find-scan <dir>` rescans all files under a source subdirectory (full rescan, scoped deletions to that subtree only, no `scan_timestamp` update); previously only individual files were accepted
- **Search result metadata** — mtime, file size, and kind are now shown right-aligned in the search result title bar; the duplicates bubble moves to immediately after the file path
- **PDF original view from tree** — PDFs opened from the tree, directory listing, or command palette now default to the rendered (original) view; search-result opens continue to default to extracted text so match context is visible immediately
- **Grouped search results** — multiple hits in the same file are now shown as a single result card with clickable line-number badges (`:123`, `:456`); clicking a badge updates the context snippet without opening the file; the active badge is highlighted
- **Build kind in About screen** — the About panel now shows `(release)`, `(dev)`, or a short commit hash alongside the version; determined at build time from `GIT_TAG` and `GIT_DIRTY` env vars (no GitHub API call required); CI release workflow injects these automatically
- **`log_batch_detail_limit` server config** — `[server] log_batch_detail_limit = 5` (default); for batches up to this size the worker logs each file path individually; for larger batches it logs only the count, preventing log floods on big scans
- **`exclude_extra` in example config** — `examples/client.toml` now includes a commented `exclude_extra = []` field so users can discover the additive-patterns option without reading the docs
- **PathBar clipboard fallback** — copy-path button now uses `document.execCommand` as a fallback when `navigator.clipboard` is unavailable (e.g. non-HTTPS contexts); "Copied" label replaces the icon briefly to confirm success
- **Light/dark/system theme** — Preferences panel now has an Appearance section with three options: Dark, Light, and "Inherit from browser"; choice is persisted in user profile; an inline script in `app.html` sets `data-theme` before first paint to prevent flash; `prefers-color-scheme` media query is tracked live for the system option; syntax highlighting (hljs) also switches to a GitHub Light palette in light mode
- **Sidebar source header polish** — source names no longer show a chevron triangle; font size increased to 14 px (slightly larger than the 13 px tree rows); active source is bold (`font-weight: 700`) with a subtle `--bg-hover` background tint (lighter in dark mode, darker in light mode)
- **PathBar copy icon fixes** — icon no longer clips at the bottom (`overflow-y: visible` on the path container); added 6 px left margin for breathing room between path and icon; vertical alignment corrected (`align-items: center` instead of `baseline`)

- **NLP date search** — natural language date phrases embedded in search queries are parsed and converted to date range filters automatically; supports `last month`, `last year`, `last week`, `last weekend`, `yesterday`, `last Monday`, `in the last N days`, `since`, `before`, `after`, named months, and explicit ranges; the detected phrase is highlighted green in the search box and shown as a dismissible chip below the bar; calendar vs rolling semantics are distinguished by the presence of an "in the"/"within the" prefix
- **Result count date context** — the result count line now includes the active date range: `390 results between 2/1/2026 and 2/28/2026`, `200 results after 9/1/2025`, etc.
- **Manual** — nine-page reference manual under `docs/manual/` covering installation, configuration, indexing, search, web UI, supported file types, administration, services, and troubleshooting; README now links to it
- **Admin panel** — new "Admin" section in Settings shows pending/failed inbox item counts and a "Retry Failed" button that moves failed batches back to the inbox for reprocessing

### Changed

- **`base_url` removed** — the `base_url` source config option and all related UI (PathBar external link, Preferences "Base URL overrides" panel, `sourceBaseUrls` profile field) have been removed; the feature was unused and the server URL is now the canonical access point for all files

### Fixed

- **Archive member sizes** — `size` is now `null` for archive members rather than `0`; search results and file viewer no longer show "0 bytes" when the size of a member cannot be determined (schema v6→v7 migration makes the `size` column nullable)
- **`find-scan --upgrade`** — replaces `--full`; re-indexes only files whose stored `scanner_version` is older than the current client version, making post-release re-indexing fast and naturally resumable (interrupted runs skip files already upgraded); schema v7→v8 adds `scanner_version INTEGER DEFAULT 0` to the `files` table
- **`find-admin show` timestamp** — `scan_ts` is now printed as a human-readable RFC2822 local time instead of a raw Unix epoch number
- **Sticky search bar** — `overflow: hidden` on `.main-content` was suppressing `position: sticky` on the topbar in the results view; moved to the file-view-only selector so the search bar now remains fixed at the top while scrolling through results
- **Search result filename never hidden** — file path in result cards is now `flex-shrink: 0` (max 60% width) and the line-ref badge list clips rather than wraps, so a long list of line-number badges can no longer push the filename off-screen

---

## [0.5.6] - 2026-03-05

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
- **`mise run build-x64` task** — builds web UI then compiles all binaries for x86_64 release
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
- **CLI reference** — new `docs/CLI.md` with comprehensive documentation for all binaries: `find-server`, `find-scan`, `find-watch`, `find-anything`, `find-admin` (all subcommands), full config references, and extractor binary table
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
