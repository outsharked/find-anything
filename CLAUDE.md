# Claude Code Instructions for find-anything

This file contains project-specific instructions for Claude Code when working on this codebase.

## Keeping CLAUDE.md up to date

After completing any change that affects the project's architecture, key files, or non-obvious conventions described in this file, update the relevant section of CLAUDE.md so future Claude Code sessions start with accurate information. This includes:

- Changes to the schema (bump the version note in the Schema section)
- Changes to the write/read path, content storage model, or worker design
- New or renamed config fields that affect the architecture description
- Renaming key files, structs, or invariants described here

---

## Planning and Documentation

### Feature Planning

For each substantive new feature:
1. Create a numbered and named plan file in `docs/plans/`
2. Use the naming format: `NNN-feature-name.md` (e.g., `001-pdf-extraction.md`)
3. Include in the plan:
   - Overview of the feature
   - Design decisions and trade-offs
   - Implementation approach
   - Files that will be modified or created
   - Testing strategy
   - Any breaking changes or migration steps

Example plan structure:
```markdown
# Feature Name

## Overview
Brief description of what this feature does and why it's needed.

## Design Decisions
Key architectural choices and their rationale.

## Implementation
Step-by-step approach to implementing the feature.

## Files Changed
- `path/to/file.rs` - what changes
- `path/to/other.rs` - what changes

## Testing
How to test and validate the feature.

## Breaking Changes
Any breaking changes and migration guide if applicable.
```

### Existing Plans

Current plan files are stored in `docs/plans/`:
- `PLAN.md` - Original architecture and implementation plan (now historical)

---

## Architecture

> **Full architecture reference**: `docs/ARCHITECTURE.md` — crate structure, dependency
> graph, write/read paths, content storage, extraction memory model, server routes,
> web UI structure, and key invariants. The sections below are a condensed summary.

### High-level overview

find-anything is a two-process system:

- **`find-scan` (client)** — walks the filesystem, extracts content, and sends
  batches to the server over HTTP
- **`find-server`** — receives batches, stores them, and serves search queries

A shared **`find-common`** crate contains API types, config structs, and all
content extractors (text, PDF, image EXIF, audio metadata, archive).

The **web UI** is a SvelteKit app in `web/` that talks to the server via a
proxy that injects the bearer token.

### Write path (indexing)

The worker runs two sequential phases per inbox batch:

```
find-scan → POST /api/v1/bulk (gzip JSON) → inbox/{id}.gz on disk
                                              ↓
                                   Phase 1 (inline, blocking)
                                              ↓
                              upsert files table + insert FTS5 rows
                              write normalised .gz → inbox/to-archive/
                                              ↓
                                   Phase 2 (archive worker)
                                              ↓
                              read to-archive/.gz → put blob in blobs.db
                              keyed by file_hash (blake3 of raw file bytes)
```

Key invariants:
- **All DB writes go through the inbox worker** — no route handler writes to
  SQLite directly. This eliminates write contention entirely.
- The bulk route handler only writes a `.gz` file to `data_dir/inbox/` and
  returns `202 Accepted` immediately.
- The worker processes inbox files sequentially (one at a time), so there is
  never concurrent write access to a source database.
- Within a `BulkRequest`, the worker processes **deletes first, then upserts**,
  so renames (path in both lists) are handled correctly.
- **Phase 1** (request.rs) handles all SQLite work synchronously. At the end it
  writes a normalised `.gz` to `inbox/to-archive/` and notifies the archive worker.
  When re-indexing a modified file, Phase 1 reads the old blob from the content store
  (via `file_hash`) and issues the FTS5 `'delete'` command for each old line before
  inserting new content — keeping the contentless FTS5 index clean. Empty lines are
  skipped in the delete pass (issuing `'delete'` with `""` corrupts FTS5 state).
- **Phase 2** (archive_batch.rs) reads from `to-archive/` and calls
  `content_store.put(file_hash, blob)`. It is idempotent: if a hash already exists
  in `blobs.db` the put is a no-op, so duplicate files only ever store one copy.
  Line content is `trim_end()`-stripped before being stored in the blob.

### Content storage (blobs.db)

All file content is stored in **`data_dir/blobs.db`** — a single SQLite database
managed by `SqliteContentStore` (`crates/content-store/`). There are no ZIP archives.

- Content is **content-addressable**: keyed by `file_hash` (streaming blake3 of raw
  file bytes). Two files with identical bytes share one stored blob.
- Each blob is split into chunks of configurable size (default 1 KB). Each chunk
  records `(key, chunk_num, start_line, end_line, data_bytes)`. Chunk data is lines
  joined by `\n` with **no trailing newline**; `get_lines` uses `str::lines()` to
  reconstruct them, which naturally handles the empty-blob sentinel and preserves
  interior blank lines.
- Reads use a PK-indexed range query: `get_lines(key, lo, hi)` returns only the
  chunk(s) that overlap the requested line range — no full-blob load.
- WAL mode + a read-connection pool (`SqliteContentStore`) allow unlimited concurrent
  readers while a single write mutex serialises puts.
- Compaction (`/api/v1/admin/compact`) deletes blobs whose key no longer appears in
  any source DB's `files.file_hash` column, then VACUUMs.

There is **no** separate `lines` table. The FTS5 rowid encodes both the `file_id`
and `line_number` arithmetically:

```
rowid = file_id × 1_000_000 + line_number
```

This lets the search query decode file and line position from the FTS result without
a JOIN to an auxiliary table.

### Read path (search)

```
GET /api/v1/search → FTS5 query → decode (file_id, line_number) from rowid
                   → JOIN files → fetch content via content_store.get_lines(file_hash, lo, hi)
                   → return matched lines + snippets
```

Context retrieval (`/api/v1/context`, `/api/v1/file`) uses the same
`content_store.get_lines` path. A per-request cache avoids re-fetching the same
chunk for files with many matched lines.

### Directory tree

`GET /api/v1/tree?source=X&prefix=foo/bar/` uses a **range-scan** on the
`files` table:

```sql
WHERE path >= 'foo/bar/' AND path < 'foo/bar0'
```

(`prefix_bump` increments the last byte of the prefix string to get the upper
bound.) Results are grouped server-side into virtual directory nodes and file
nodes. Only immediate children of the prefix are returned; the UI lazy-loads
subdirectories on expand.

### Key invariants and non-obvious details

- **`line_number = 0`** is always the file's own relative path, indexed so
  every file is findable by name even if content extraction yields nothing.
- **Archive members as first-class files (plan 012):**
  - Inner archive members use **composite paths** with `::` as a separator:
    - `taxes/w2.zip::wages.pdf` (member of a ZIP)
    - `data.tar.gz::report.txt::inner.zip::file.txt` (nested archives)
  - Each member has its own `file_id` in the `files` table
  - The `::` separator is reserved and cannot be used in regular file paths
  - Archive members get their `kind` detected from their filename (not inherited from outer archive)
  - Deletion: `DELETE FROM files WHERE path = 'x' OR path LIKE 'x::%'` removes all members
  - Re-indexing: When an outer archive changes, the server deletes all `path LIKE 'archive::%'` members first
  - Client filters `::` paths from deletion detection (only outer files are tracked client-side)
  - Tree browsing: `GET /api/v1/tree?prefix=archive.zip::` lists archive members
  - Ctrl+P: Archive members appear as `zip → member` and are fully searchable
  - UI: Archive files (`kind="archive"`) expand in the tree like directories
- **`archive_path`** on `IndexLine` is deprecated (schema v3) — composite paths in `files.path` replaced it.
  For backward compatibility, external API endpoints still accept an `archive_path` query param.
- **PDF extraction** uses a fork of `pdf-extract` at
  `https://github.com/jamietre/pdf-extract`, pinned by git rev in
  `crates/extractors/pdf/Cargo.toml`. The local working copy lives at
  `/home/jamiet/code/pdf-extract/`. When investigating PDF extraction bugs or
  panics, look in the fork — particularly `pdf-extract/src/lib.rs`.

  **Workflow for any change to the fork:**
  1. Edit `/home/jamiet/code/pdf-extract/src/lib.rs` (or other fork files)
  2. `cd /home/jamiet/code/pdf-extract && git add -p && git commit`
  3. `git push` — pushes to `github.com:jamietre/pdf-extract`
  4. Copy the new commit hash (first 7 chars)
  5. Update `rev = "XXXXXXX"` in `crates/extractors/pdf/Cargo.toml`
  6. `cargo update -p pdf-extract` to refresh `Cargo.lock`
  7. `cargo build -p find-extract-pdf` to verify it compiles

  **Never** leave fork changes uncommitted/unpushed — the build pins a specific
  rev, so local edits have no effect until committed, pushed, and the rev updated.

  The fork avoids calling `type1_encoding_parser::get_encoding_map` (which
  panics on malformed Type1 font data) by calling `type1_encoding_parser::parse()`
  directly and handling errors gracefully.
- The `files` table is per-source (one SQLite DB per source name, stored at
  `data_dir/sources/{source}.db`). Archives are shared across sources.
- The **FTS5 index is contentless** (`content=''`); content lives only in ZIPs.
  FTS5 is populated manually by the worker at insert time.
- **Archive depth limit:** Nested archives are extracted recursively up to
  `scan.archives.max_depth` (default: 10) to prevent zip bomb attacks. When
  exceeded, only the filename is indexed with a warning logged.

### Key files

| File | Purpose |
|------|---------|
| `crates/common/src/api.rs` | All HTTP request/response types |
| `crates/common/src/extract/` | Content extractors (text, pdf, image, audio, archive) |
| `crates/server/src/worker.rs` | Inbox polling loop + `BulkRequest` processing |
| `crates/server/src/archive.rs` | ZIP archive management + `chunk_lines()` |
| `crates/server/src/db.rs` | All SQLite operations |
| `crates/server/src/routes.rs` | HTTP route handlers (reads + bulk write) |
| `crates/server/src/schema_v2.sql` | DB schema |
| `crates/client/src/scan.rs` | Filesystem walk, extraction, batch submission |
| `crates/client/src/api.rs` | HTTP client (one method per endpoint) |
| `web/src/lib/api.ts` | TypeScript API client |
| `web/src/routes/+page.svelte` | Main page — view state machine |

---

## Tooling

**Always check `mise tasks` before doing things manually** — there are mise tasks for most common operations:

| Task | Purpose |
|------|---------|
| `mise run release` | Bump version, update CHANGELOG, commit, tag, and publish a GitHub release |
| `mise run clippy` | Run clippy lints (matches CI — fails on warnings) |
| `mise run check` | Type-check all Rust crates and the web UI |
| `mise run build-release` | Build web UI then compile find-server release binary |
| `mise run dev` | Start Rust API + Vite dev server with live reload |

- **Package manager:** `pnpm` (not npm). Use `pnpm` for all web commands in `web/`.
  - Type-check: `pnpm run check`
  - Dev server: `pnpm run dev`
  - Build: `pnpm run build`

---

## Project Conventions

### Rust style

See [`docs/rust-style.md`](docs/rust-style.md) for binding patterns and idioms
specific to this codebase. When a situation is not covered there, refer to the
[Rust API Guidelines](https://rust-lang.github.io/api-guidelines/) and the
[Clippy lint catalogue](https://rust-lang.github.io/rust-clippy/master/).

---

### Rust: Configuration objects over threaded parameters

When a function needs to pass configuration to downstream callers, prefer a
config struct over threading individual parameters:

- **Threshold:** As soon as you would thread **more than one parameter** through
  a call chain, introduce a config struct instead.
- **Pattern:** Define the struct in `find-common` (so all crates can share it),
  derive `Copy`, and pass `&ConfigStruct` by reference.
- **Example:** `ExtractorConfig` in `crates/common/src/config.rs` bundles
  `max_size_kb`, `max_depth`, and `max_line_length` — used by `find-extract-pdf`,
  `find-extract-archive`, and `find-client`.
- **Constructor:** Provide a `from_scan(scan: &ScanConfig) -> Self` method (or
  equivalent) so call sites build the struct once from the top-level config and
  pass it down, rather than unpacking fields at every level.

This keeps function signatures stable when new settings are added: only the
struct definition and its construction site change, not every function in the
call chain.

---

### Default client.toml template — keep Linux and Windows in sync

The default `client.toml` written during installation exists in two places:

| File | Location of template |
|------|----------------------|
| Linux / macOS | `install.sh` — heredoc starting around `cat > "$CONFIG_FILE" <<EOF` |
| Windows installer | `packaging/windows/find-anything.iss` — `BuildToml()` function in `[Code]` |

Both must produce **identical** commented-out option blocks. When adding or
removing a config option in one, update the other at the same time.

---

### Testing requirements

Apply the following testing requirements whenever making changes:

| Change type | Required tests |
|---|---|
| Web UI logic (TypeScript/Svelte) | Client-side unit tests in `web/src/lib/*.test.ts` using Vitest |
| New or changed HTTP endpoints | Integration tests in `crates/server/tests/` using `TestServer` |
| New or changed CLI behaviour (`find-scan`, `find-watch`, `find-admin`) | End-to-end tests that invoke the binary or use the client API |

**Web UI unit tests** — place alongside the module under test (e.g. `commandPaletteLogic.test.ts` next to `commandPaletteLogic.ts`). Run with `pnpm run test` inside `web/`.

**Server integration tests** — use `TestServer::spawn()` from `crates/server/tests/helpers/`. Create a new `crates/server/tests/<feature>.rs` file for each new endpoint or significant change. Existing test files are good reference examples. Run with `cargo test --test <name>`.

**CLI end-to-end tests** — invoke the compiled binary against a running `TestServer`. Use the existing pattern in `crates/server/tests/` as a guide. Run with `cargo test`.

When deleting client-side logic that was previously unit-tested, replace those tests with equivalent server-side integration tests if the behaviour moved to the server.

---

### Commits

**Do not automatically commit changes.** Always wait for explicit user instruction before running `git commit`. Complete the implementation and verify it works first; the user will ask to commit when ready.

**Pre-commit checklist** (enforced by `.claude/commands/commit.md`):

1. **Clippy** — run `mise run clippy` and fix all warnings before committing Rust changes. This matches the CI check (`cargo clippy --workspace -- -D warnings`).
2. **`MIN_CLIENT_VERSION`** — if any API changes are breaking (removed endpoints, changed required request/response fields, incompatible behaviour), update `MIN_CLIENT_VERSION` in `crates/common/src/api.rs` to the current package version before committing.
3. **CHANGELOG** — add a summary of changes to the `[Unreleased]` section of `CHANGELOG.md`.

---

### `MIN_CLIENT_VERSION` — API compatibility enforcement

`MIN_CLIENT_VERSION` is defined in `crates/common/src/api.rs` and included in every `GET /api/v1/settings` response. All client binaries (`find-scan`, `find-watch`, `find-anything`, `find-admin`, `find-upload`) check this on startup and refuse to run if their own version is older.

**When to update it:** Any time a change to the HTTP API would cause an older client to misbehave — e.g. a required request field is added, a response field is removed, an endpoint is renamed or deleted, or semantics change in an incompatible way.

**How to update it:** Set the string to the current package version (same value as in `Cargo.toml`):

```rust
// crates/common/src/api.rs
pub const MIN_CLIENT_VERSION: &str = "0.7.0"; // ← bump to current version
```

Backwards-compatible additions (new optional fields, new endpoints) do **not** require a bump.

---

### Search result keys and load-more dedup (prevent duplicate-key regressions)

The keyed `{#each}` in `ResultList.svelte` uses:
```
`${source}:${path}:${archive_path ?? ''}:${line_number}`
```

**All four fields are required.** `archive_path` distinguishes members of the same archive (e.g. `outer.zip::a.txt` vs `outer.zip::b.txt` both map `path = outer.zip`). If any new discriminating field is added to `SearchResult`, add it to this key too.

**Client-side dedup is mandatory in `triggerLoad` and must not be removed.** The server deduplicates within a single request, but cross-request duplicates occur in the load-more path. Each page request expands `scoring_limit = offset + limit + 200`, so the server processes more FTS5 candidates per page. This re-ranks the candidate set — an item at position 45 on page 0 can shift to position 69 on page 1. The same `(source, path, archive_path, line_number)` tuple will then appear in both pages. Duplicate keys in the keyed `{#each}` throw a runtime error and prevent DOM updates, which keeps the load-more sentinel in place and causes an infinite request loop. The fix for a duplicate-key regression is always to restore the dedup filter in `triggerLoad`, not to remove it.

**`loadOffset` must advance by `resp.results.length`, not `fresh.length`.** If dedup removes some items from a page, `results.length` grows by less than what the server returned. Using `results.length` as the server offset would re-request the same range, stalling pagination. `loadOffset` tracks the server cursor independently of how many client-visible items were added.

---

### Versioning

This project follows semantic versioning (MAJOR.MINOR.PATCH):

**Patch version increment (0.0.X):**
- Increment the patch version each time a feature is completed and merged
- Examples: bug fixes, small enhancements, new extractors, UI improvements
- Update version in all `Cargo.toml` files (workspace members)

**Minor version increment (0.X.0):**
- Suggest a minor version bump for substantial changes that add significant value
- Examples:
  - Major new capabilities (real-time watching, OCR)
  - Multiple related features that together form a cohesive release
  - Breaking API changes (though we try to avoid these)
  - Significant architectural improvements

**Major version increment (X.0.0):**
- Reserved for v1.0 (production-ready) and major breaking changes after that

**Process:**
1. When completing a feature, update the patch version
2. If changes are substantial, suggest a minor version bump in the commit message
3. Update `ROADMAP.md` to mark features as completed in the appropriate version section
4. Add a summary of changes to the `[Unreleased]` section of `CHANGELOG.md` as work is done
5. When cutting a release, move the `[Unreleased]` entries to a new versioned section (e.g. `## [0.2.5] - YYYY-MM-DD`)
6. When creating the git tag, include an annotated message with bullet points summarising the high-level features and major bug fixes (e.g. `git tag -a v0.5.2 -m $'v0.5.2\n\n- PDF viewer improvements\n- Symmetric duplicate links\n- Archive indexing resumability'`)
7. Create a GitHub release using `gh release create <tag> --title "<tag>" --notes "..."` with the same high-level bullet points as release notes
