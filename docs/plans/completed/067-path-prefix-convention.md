# 067 — Standardise `line_number=0` Metadata Prefixes

## Overview

All `line_number=0` entries in the `lines` table now carry a bracketed prefix tag that
identifies the type of metadata stored, making each entry unambiguously identifiable
without inspecting the surrounding context.

## Problem

The `line_number=0` bucket was originally used only for the file's relative path, but
grew to include EXIF tags, audio metadata, MIME type, and PE version info — all stored
at the same `line_number=0` with no structural distinction.

This caused a correctness bug in plan 066's `file-*` search modes: the SQL filter
`AND l.line_number = 0` was intended to match only filename rows but also matched
EXIF, MIME, and PE metadata rows, producing false positives.

## Design

Each metadata line now starts with a bracketed prefix:

| Prefix | Example content | Source |
|---|---|---|
| `[PATH]` | `[PATH] docs/report.pdf` | All files (required) |
| `[EXIF:key]` | `[EXIF:Make] Canon` | Image EXIF (pre-existing) |
| `[TAG:key]` | `[TAG:artist] Miles Davis` | Audio tags (pre-existing) |
| `[IMAGE:key]` | `[IMAGE:width] 1920` | Image dimensions (pre-existing) |
| `[FILE:mime]` | `[FILE:mime] application/pdf` | MIME detection (pre-existing) |
| `[PE:key]` | `[PE:ProductName] Microsoft Word` | PE version info (this plan) |

The `[PATH]` prefix on path lines enables a precise SQL filter for `file-*` search modes:
```sql
AND l.line_number = 0 AND l.content LIKE '[PATH] %'
```

## Files Changed

- `crates/client/src/batch.rs` — all path lines now use `format!("[PATH] {}", path)`
- `crates/client/src/scan.rs` — all four archive sentinel path lines updated
- `crates/client/src/subprocess.rs` — archive member filename marker updated
- `crates/extractors/pe/src/lib.rs` — version keys reformatted as `[PE:Key]`; bare
  filename line removed (caller already adds `[PATH]` via `build_index_files`)
- `crates/server/src/db/search.rs` — `filename_clause` tightened to include
  `AND l.content LIKE '[PATH] %'` in both `fts_count` and `fts_candidates`
- `crates/server/src/routes/search.rs` — `make_result` strips `[PATH] ` prefix from
  snippet so the UI displays clean paths without the tag

## Migration

Existing indexes use bare path strings. After upgrading, run:

```
find-scan --force
```

This causes the client to re-submit all files through the normal bulk write path,
which overwrites old path lines with the new `[PATH]` prefix. No DB blow-away needed.

## Testing

- `cargo test -p find-client` — all batch.rs unit tests updated for `[PATH]` prefix
- `mise run clippy` — no new warnings
- Manual: `file:` prefix search should now return only filename matches, not EXIF/PE rows
