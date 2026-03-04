# Claude Code Instructions for find-anything

This file contains project-specific instructions for Claude Code when working on this codebase.

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

```
find-scan → POST /api/v1/bulk (gzip JSON) → inbox/{id}.gz on disk
                                              ↓
                                    background worker (polls every 1s)
                                              ↓
                              for each file: remove old chunks from ZIPs
                                              ↓
                              chunk content → append to content_NNNNN.zip
                                              ↓
                              upsert files table + insert lines table + FTS5
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

### Content storage (ZIP archives)

File content is stored in rotating ZIP archives under `data_dir/sources/content/NNNN/`,
organized into subfolders by thousands. SQLite holds only metadata and FTS index.

- Folder structure: `content/0000/`, `content/0001/`, ... (4-digit zero-padded: archive_num / 1000)
- Each folder contains up to 1000 archives
- Archives named: `content_00001.zip`, `content_00002.zip`, etc. (5-digit zero-padded)
- Example: `sources/content/0000/content_00123.zip`, `sources/content/0001/content_01234.zip`
- Maximum capacity: 9,999 subfolders × 1000 archives = 9,999,000 archives (~99.99 TB)
- Target size is 10 MB per archive (measured by on-disk compressed size).
- Each file's content is split into ~1 KB chunks.
- Chunk names: `{relative_path}.chunk{N}.txt`
- The `lines` table stores `(chunk_archive, chunk_name, line_offset_in_chunk)`
  — no content inline in SQLite.
- On re-indexing a file, old chunks are **removed before new ones are written**
  to prevent duplicate entry names in the ZIP.
- On deletion, the SQLite transaction stays open across the ZIP rewrite; only
  committed if the ZIP rewrite succeeds (rollback on failure = atomicity).

### Read path (search)

```
GET /api/v1/search → FTS5 query → candidate rows (chunk_archive, chunk_name,
                                   line_offset_in_chunk)
                   → read chunk from ZIP (cached per request)
                   → return matched lines + snippets
```

Context retrieval (`/api/v1/context`, `/api/v1/file`) reads chunks from ZIP
the same way, with a per-request HashMap cache to avoid re-reading the same
chunk file twice.

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

### Commits

**Do not automatically commit changes.** Always wait for explicit user instruction before running `git commit`. Complete the implementation and verify it works first; the user will ask to commit when ready.

**Before pushing or committing Rust changes, run clippy:**

```sh
mise run clippy
```

This matches the CI check (`cargo clippy --workspace -- -D warnings`). Fix all warnings before committing — clippy failures will fail the CI build.

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
