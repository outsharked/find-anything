# 092 — Line Offset Bug Research: FTS Index vs Blob Position Divergence

## Summary

Search results show matched content at the wrong line (one position off) in the file
viewer. When searching for a known term, the ▶ highlight points to the adjacent line
rather than the line actually containing the match.

---

## Symptoms

- Searching for "zzyzx" in `music3:/Users/jamie/code/auth-bump-out/pnpm-lock.yaml`
  returned line 7194 (display) in search results.
- The blob (content store) had the matching sha1 hash at blob position 7193.
- FTS rowid encoded line 7194 (server line) for the same content.
- Discrepancy: FTS is 1 ahead of blob position → `get_lines(7194)` returned the
  content of the line *after* the match.

---

## Root Cause

**File:** `crates/content-store/src/sqlite_store/mod.rs` — `chunk_blob()`

The function splits a blob string into fixed-size chunks and records each chunk's
`start_line` / `end_line` as 0-based blob positions. These positions must align
exactly with the FTS `line_number` values, because `get_lines(lo, hi)` is called
with FTS-derived line numbers.

### The bug

Before the fix, the separator logic was:

```rust
// OLD — buggy
if !current.is_empty() {
    current.push('\n');
}
current.push_str(line);
```

**Scenario that triggers it:** an empty line (`line = ""`) falls as the *first* line
of a new chunk (immediately after a flush):

1. After flushing chunk N: `current = ""`
2. `line = ""` → `push_str("")` → `current` remains `""`
3. Next non-empty line: `!current.is_empty()` is **false** → `'\n'` separator is
   **skipped**
4. `current = "non_empty_content"` — the empty line was never stored in the chunk data

The chunk's `start_line` is set to the empty line's position, but the chunk *data*
only contains the non-empty content starting at the next position. When
`get_lines(start_line)` decodes this chunk with `text.lines().enumerate()`, position
0 returns the non-empty content instead of the empty string.

**Effect:** All blob positions after the first affected boundary are **shifted by −1**
relative to the FTS line numbers that encoded the correct `line_number` at insert
time. A search returning FTS line N resolves to blob position N, which now contains
the content originally assigned line number N+1.

### Why it's hard to detect

- Only fires when an empty line is the *first* line processed in a new chunk.
- With 4 KB chunks and typical file content, this happens rarely — roughly once
  per few hundred chunk boundaries in a dense YAML file.
- Words like `resolution:`, `engines:`, `dev:` appear hundreds of times in
  pnpm-lock.yaml, so naive cross-checking produces many false positives. Only
  unique tokens (sha1 hashes) reliably detect the divergence.

---

## Investigation Evidence

### Confirmation of the bug mechanism

Chunk 22 in the current blob (pnpm-lock.yaml, hash `b095fa93…`):

```
start_line=2787, end_line=2943
raw text starts: '\n  /array-flatten/1.1.1:...'
```

The leading `\n` proves the empty line at position 2787 **is** stored — this blob was
written with the fixed code. An unfixed blob at the same boundary would start with
`'  /array-flatten/1.1.1:'` (no leading `\n`), placing that content at position 2787
instead of position 2788.

### Current state (after fix deployed)

```
Blob position 7195: '    resolution: { integrity: sha1-EFqwNXznKyAqiouUkzZyZXteKpo= }'
FTS rowid 331007195 → line 7195: contains "zzyzx" ✓
Diff = 0 (blob and FTS agree)
```

At time of initial user report, the blob had the match at position **7193** while
FTS had it at **7194** (server line) — a 1-line divergence caused by exactly one
empty-line-at-boundary event earlier in the file.

### False-positive investigation

Running FTS queries for common words (`resolution:`, `engines:`) near boundaries
produced many apparent mismatches — these were all false positives because each word
appears hundreds of times. Checking *unique* sha1 hashes confirmed all boundaries
are clean after the fix:

```
Boundary 391/392, blob[392]: sha1='sha1-WsgizpfuxGdBq3C' → FTS line 392 MATCH ✓
Boundary 735/736, blob[736]: sha1='sha1-Yr+Ysto80h1iYVT' → FTS line 736 MATCH ✓
Boundary 981/982, blob[980]: sha1='sha1-2uOEYT3o93wZaoh' → FTS line 980 MATCH ✓
Boundary 1223/1224, blob[1224]: sha1='sha1-a9QOV/596UqpBIU' → FTS line 1224 MATCH ✓
```

---

## The Fix

Replace `!current.is_empty()` with an explicit `first_in_chunk` boolean:

```rust
// NEW — correct
let mut first_in_chunk = true;
...
if !first_in_chunk {
    current.push('\n');
}
first_in_chunk = false;
current.push_str(line);
// Reset first_in_chunk = true after each flush
```

This correctly tracks whether we're at the first line of a new chunk regardless of
whether that line is empty.

**Regression test added:** `empty_line_at_chunk_boundary_preserved` in
`crates/content-store/src/sqlite_store/mod.rs`.

---

## Why Old Blobs Were Not Self-Correcting

`archive_batch.rs` calls `content_store.put_overwrite()` (not the idempotent `put`)
on every archive pass. So blobs *are* rewritten on each re-index. However:

- `find-scan` only re-submits files whose mtime has changed (or `--force` is used).
- Files whose raw bytes haven't changed since the buggy server ran will retain the
  buggy blob until the next forced re-scan.
- Phase 1 (FTS) and Phase 2 (blob) are both driven by the same `IndexLine` data, so
  after a re-index with the fix, both are corrected together.

---

## Display Layer Note

`displayLine(n)` in `SearchResult.svelte` subtracts 1 for content lines:

```typescript
function displayLine(n: number): number {
    return n >= 2 ? n - 1 : n;
}
```

Server line 7195 → display line 7194. This is intentional (line 0 = path, line 1 =
metadata; first *content* line displays as "1"). The display offset is not part of
the bug.

---

## Remaining Risk

Files indexed before the fix was deployed and not re-indexed since may still have
divergent blobs. A forced full re-scan (`find-scan --force`) of affected sources
would trigger `put_overwrite` for all files, correcting all blobs.

The fix is currently uncommitted (working copy only). It needs to be committed,
the server rebuilt, and a forced re-scan run for any source that was indexed
while the bug was active.

---

## Files Involved

| File | Role |
|------|------|
| `crates/content-store/src/sqlite_store/mod.rs` | `chunk_blob()` — the bug and fix |
| `crates/server/src/worker/archive_batch.rs` | Phase 2: calls `put_overwrite`, builds blob from IndexLines |
| `crates/server/src/worker/pipeline.rs` | Phase 1: inserts FTS rows using `line_number` from IndexLines |
| `crates/server/src/db/mod.rs` | `read_chunk_for_file()` — calls `get_lines(line_number, line_number)` |
| `crates/server/src/routes/context.rs` | Context endpoint, computes `match_index` |
| `web/src/lib/SearchResult.svelte` | `displayLine()` — subtracts 1 for display |
