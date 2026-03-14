# Archive Compaction

## Overview

ZIP content archives can accumulate orphaned chunks — entries that are no longer
referenced by any `lines` row in any source database.  This happens when:
- A delete batch commits its DB transaction but the server dies before the ZIP
  rewrite, leaving the chunks in the archive but unreferenced.
- (Historically, before the two-phase delete was introduced, rewrites could
  partially complete.)

This plan adds:
1. A **background stats scanner** that periodically computes the total size of
   orphaned chunks across all archives and caches the result in a server-wide
   SQLite database (`server.db`).
2. **`find-admin compact`** to rewrite affected archives and reclaim space.
3. **`find-admin status`** shows cached wasted-space stats when available.

## Design Decisions

### ZIP catalog scan (no content reads)

The ZIP format stores a Central Directory at the end of the file.
`ZipArchive::new()` reads only the Central Directory into memory.  Iterating
entries via `by_index(i)` seeks to each local file header (tiny, within the
same small file) and exposes `.compressed_size()` and `.name()` — no content
is ever decompressed.  For our 10 MB archives this is fast.

### Referenced set

To identify orphaned chunks we build a `HashSet<(archive_name, chunk_name)>`
from `SELECT DISTINCT chunk_archive, chunk_name FROM lines` across every source
database, then compare each archive's entries against the set.

### Persistence: `data_dir/server.db`

A new server-wide key-value SQLite database (`meta` table, same schema as
per-source `meta` tables) stores the scan results:
- `compact_orphaned_bytes`
- `compact_total_bytes`
- `compact_scanned_at`

This survives server restarts; `find-admin status` always shows the last known
value without having to wait for the next scan cycle.

In-memory `AppState.compaction_stats` mirrors the DB value for zero-cost reads
in the stats route.

### Compaction

`compact_archives` acquires the per-archive `rewrite_lock` (same lock used
by workers) before rewriting, so compaction is safe to run while the server
is processing inbox items.

A `--dry-run` flag reports what would be freed without writing anything.

### Scan interval

`server.compact_scan_interval_mins` (default: 60).  The background task also
runs once shortly after startup so the value is populated quickly after a fresh
install.

## Implementation

### Files changed

- `crates/common/src/config.rs` — add `compact_scan_interval_mins`
- `crates/common/src/defaults_server.toml` — add default
- `crates/common/src/api.rs` — add `CompactResponse`; add optional
  `orphaned_bytes` / `orphaned_stats_age_secs` to `StatsResponse`
- `crates/server/src/compaction.rs` — new: scanner + compaction logic
- `crates/server/src/main.rs` — spawn scanner; add `AppState.compaction_stats`
- `crates/server/src/routes/admin.rs` — `POST /api/v1/admin/compact`
- `crates/server/src/routes/mod.rs` — re-export
- `crates/server/src/routes/stats.rs` — include compaction stats in response
- `crates/client/src/api.rs` — `compact()` method
- `crates/client/src/admin_main.rs` — `Compact { dry_run }` command

## Testing

- `find-admin compact --dry-run` should show non-zero wasted bytes after
  deliberately orphaning chunks (e.g. killing the server mid-delete).
- Running `find-admin compact` twice should show 0 bytes freed on the second run.
- `find-admin status` should show wasted-space line after the background
  scanner completes its first cycle.
