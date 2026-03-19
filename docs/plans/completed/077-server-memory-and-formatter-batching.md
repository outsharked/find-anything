# 077 — Server Memory Reduction + Batch Formatter Optimization

## Overview

The server crashed on 2026-03-18 after consuming ~4 GB of RAM indexing a large
archive (`msys64-j.treworgy.zip`, ~7000 source files). Two root causes:

1. **Multiple full copies of file content held in memory simultaneously** —
   in both the Phase 1 indexing loop and the Phase 2 archive batch.
2. **One external formatter process spawned per file** — for a batch of 200
   JS/TS files that is 200 biome/prettier invocations, each with process startup
   overhead and pipe buffers.

This plan fixes the memory bugs first, then optimises formatter invocations by
introducing a `tempdir` batch mode that calls the formatter once per batch
rather than once per file.

---

## Part 1 — Memory Fixes

### Bug 1: `archive_batch.rs` loads all requests then clones them

`process_archive_batch` currently parses **all** `archive_batch_size` (default
200) gz files into `parsed_requests`, then iterates over them a second time to
build `upsert_map` — which is a full clone of every `IndexFile`. Peak memory
for one archive batch pass = 2 × (all content in 200 requests).

**Fix:** Split the batch into two sub-phases so ZIP I/O is one-at-a-time (low
memory) while SQLite writes remain batched per source (same efficiency as
before).

```
BEFORE: load all 200 → build upsert_map (2× memory) → archive → delete all

AFTER — sub-phase A (per gz, sequential):
  for each gz:
    stream-parse → DB reads (no lock) → ZIP I/O → collect ArchivedFile
    metadata (block_id + chunk refs, no content strings) → drop BulkRequest

AFTER — sub-phase B (per source, once):
  for each source:
    open 1 DB connection → acquire source lock once →
    1 transaction for ALL content_chunks rows across all gz files for that source
    → delete gz files
```

This preserves the original single-transaction SQLite efficiency (one commit
per source per batch) while keeping only one `BulkRequest` in memory at a time.
The `upsert_map` clone is eliminated; correctness is maintained by a
**stale-content check**: if `file.content_hash` differs from the DB's current
value, the file is skipped (a newer gz holds the right content).

Additionally, `files` is consumed by value inside the ZIP phase, so
`line_data` is moved (not cloned) from `IndexFile.lines` — no duplicate
content strings within a single gz either.

### Bug 2: `request.rs` buffers the whole gz file unnecessarily

```rust
// current
let compressed = std::fs::read(request_path)?;      // Vec<u8>
let mut decoder = GzDecoder::new(&compressed[..]);
let mut json = String::new();
decoder.read_to_string(&mut json)?;                  // String (larger)
let request = serde_json::from_str(&json)?;          // BulkRequest (similar)
(compressed, request)                                // both returned!
```

`compressed` is only used afterward for `compressed_bytes = compressed.len()`.
Meanwhile `json` was correctly dropped inside the block — but `compressed`
escapes and lives for the rest of the ~400-line function.

**Fix:** Stream directly from disk. Use `serde_json::from_reader` on a
`GzDecoder<BufReader<File>>` — no intermediate `Vec<u8>` or `String` needed.
Get the compressed file size from `fs::metadata` before opening.

```rust
let compressed_bytes = std::fs::metadata(request_path)?.len() as usize;
let decoder = GzDecoder::new(BufReader::new(File::open(request_path)?));
let request: BulkRequest = serde_json::from_reader(decoder)?;
```

Apply the same fix to `parse_gz_request` in `archive_batch.rs`.

### Bug 3: `request.rs` holds `request.files` and `normalized_files` simultaneously

```rust
// current
for file in &request.files {                            // borrow → keeps alive
    let normalized_lines = normalize::normalize_lines(
        file.lines.clone(),                             // clone lines
        ...
    );
    normalized_files.push(file.clone());                // clone whole file
}
// request.files still live here, alongside normalized_files
```

At the peak of the loop: `request.files` + `file.lines.clone()` +
`normalized_files` (growing) ≈ 3× content in memory.

**Fix:** Destructure `BulkRequest` into owned fields and consume `files` by
value. Pass `file.lines` (moved) into `normalize_lines` (change its signature
to take `Vec<IndexLine>` by value, which it already does — the clone is
unnecessary). Build `normalized_files` in place as the original is consumed.

```rust
// after
let BulkRequest { source, files, delete_paths, rename_paths,
                  scan_timestamp, indexing_failures } = request;
let mut normalized_files = Vec::with_capacity(files.len());
for file in files {                                     // consume, no borrow
    let file = if file.kind.is_text_like() {
        let lines = normalize::normalize_lines(file.lines, &file.path, &cfg);
        IndexFile { lines, ..file }                     // no clone
    } else {
        file
    };
    // process file, then push
    normalized_files.push(file);
}
// request.files is gone; only normalized_files exists
```

---

## Part 2 — Batch Formatter Optimization

### Problem

`try_external_formatters` in `normalize.rs` is called once per file. For a
200-file batch containing 150 JS/TS files configured to use biome, that is 150
process spawns. Each spawn pays:
- ~10–50 ms process startup (biome is fast; Node-based prettier is 200–500 ms)
- stdin/stdout pipe buffer allocation
- OS scheduler context switches

At 10 ms/spawn × 150 files = 1.5 seconds wasted on process management alone.
At 300 ms/spawn (prettier) × 150 files = 45 seconds.

### Solution: TempDir batch mode

Add a `mode = "tempdir"` option to `FormatterConfig`. In this mode the
normalizer:

1. Collects **all** files in the batch whose extension matches this formatter.
2. Writes their content to a single temp directory (one file per source file).
3. Runs the formatter **once** on the temp directory.
4. Reads back the formatted content and maps it to the original files.
5. Falls back to word-wrap per-file if the formatter fails.

This reduces N process spawns to 1 per formatter per batch.

#### Config

Add a `mode` field to `FormatterConfig` (default `"stdin"` for backward compat):

```rust
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum FormatterMode {
    #[default]
    Stdin,    // existing: one process per file, content via stdin/stdout
    TempDir,  // new: one process per batch, files written to a temp dir
}

pub struct FormatterConfig {
    pub path: String,
    pub extensions: Vec<String>,
    pub args: Vec<String>,        // {name} for stdin, {dir} for tempdir
    #[serde(default)]
    pub mode: FormatterMode,
}
```

Example server config using biome in tempdir mode:

```toml
[[normalization.formatters]]
path = "/usr/local/bin/biome"
extensions = ["js", "ts", "jsx", "tsx", "css", "graphql"]
args = ["format", "--write", "{dir}"]
mode = "tempdir"

[[normalization.formatters]]
path = "/usr/local/bin/prettier"
extensions = ["html", "vue", "svelte", "scss", "less", "yaml", "yml"]
args = ["--write", "{dir}/**"]
mode = "tempdir"
```

Existing `stdin`-mode configs are unchanged and continue to work per-file.

#### Temp directory layout

Files are written as `{index:05}.{ext}` (e.g. `00042.js`) to avoid name
collisions. A `Vec<(usize, usize)>` maps `temp_index → files[] index` so
results can be mapped back.

#### Batch normalization API

Add a new batch-level entry point alongside the existing per-file one:

```rust
/// Normalize all text-like files in a batch, calling each tempdir-mode
/// formatter once per batch (rather than once per file).
///
/// `files` is a slice of `(name, lines)` pairs.  Returns a parallel Vec of
/// normalized lines.  Word-wrap is always applied as the final step.
pub fn normalize_batch(
    files: &mut [(String, Vec<IndexLine>)],
    cfg: &NormalizationSettings,
)
```

Internal flow:
1. For each `FormatterMode::TempDir` formatter in config order:
   - Find all files in the batch whose extension matches **this** formatter and
     have not already been handled by an earlier formatter.
   - Write their content to a fresh temp directory: `{tmpdir}/{i:05}.{ext}`.
   - Run **one** formatter process on the temp dir.
   - If exit 0: read back each temp file and replace `lines` in-place; mark
     these files as "handled" so later formatters skip them.
   - If non-zero / timeout: leave lines unchanged (word-wrap will apply).
2. For each `FormatterMode::Stdin` formatter: call existing per-file logic for
   any files not already handled.
3. Apply word-wrap to all files (existing `apply_word_wrap` per file).

**Example with two formatters:**
- biome (tempdir, extensions: js/ts/jsx/tsx/css) → 1 process, handles all JS/TS files
- prettier (tempdir, extensions: html/vue/svelte/scss) → 1 process, handles all HTML files
- Total for a batch with 150 JS + 30 HTML files: **2 processes** instead of 180.

The existing `normalize_lines(lines, name, cfg)` is kept for callers outside
the batch path (tests, any future single-file code path).

#### Integration in `request.rs`

Replace the per-file `normalize::normalize_lines` call with a single pre-pass
over all files before the indexing loop:

```rust
// Before the per-file loop, build a slice of (name, lines) for text-like files:
let mut to_normalize: Vec<(usize, String, Vec<IndexLine>)> = files.iter_mut()
    .enumerate()
    .filter(|(_, f)| f.kind.is_text_like())
    .map(|(i, f)| (i, f.path.clone(), std::mem::take(&mut f.lines)))
    .collect();

normalize::normalize_batch_indexed(&mut to_normalize, &cfg.normalization);

// Put lines back
for (i, _, lines) in to_normalize {
    files[i].lines = lines;
}
```

This requires consuming `files` before the per-file loop, which integrates
naturally with the Bug 3 fix (files are already owned by this point).

---

## Files Changed

| File | Change |
|------|--------|
| `crates/server/src/worker/request.rs` | Bugs 2 & 3: streaming gz parse; consume files by value; single normalize pre-pass |
| `crates/server/src/worker/archive_batch.rs` | Bug 1: process one gz at a time; streaming gz parse |
| `crates/common/src/config.rs` | Add `FormatterMode` enum; add `mode` field to `FormatterConfig` |
| `crates/common/src/defaults_server.toml` | Document `mode = "tempdir"` in comments |
| `crates/server/src/normalize.rs` | Add `FormatterMode::TempDir` branch; add `normalize_batch` |

---

## Testing

### Memory fixes (implemented)
- All 131 existing unit tests continue to pass.
- `archive_batch::tests::stale_content_hash_is_skipped` — verifies that a gz
  carrying an older `content_hash` is skipped and deleted without corrupting
  the block under the newer hash.
- `archive_batch::tests::multiple_gz_files_same_source_both_archived` — verifies
  that two gz files for the same source both get their content_chunks written
  (sub-phase B batches them into one transaction).
- `request::tests::normalization_applied_to_text_file` — verifies that long
  lines are wrapped through the owned-files (no-clone) code path and content
  is preserved.
- `request::tests::normalization_skipped_for_non_text_file` — verifies that
  image/binary files reach `to-archive/` unchanged.

### Batch formatter
- Unit test `normalize_batch` with a mock `tempdir`-mode formatter (a shell
  script that appends `// formatted` to every file it processes). Verify all
  files in a batch are formatted in a single call.
- Unit test fallback: if the formatter exits non-zero, verify word-wrap is
  still applied.
- Unit test that `stdin`-mode formatters still work per-file as before.

---

## Breaking Changes

None. The `mode` field defaults to `"stdin"`, so existing `server.toml` configs
continue to work without modification. The memory fixes are purely internal and
do not change any API surface.

---

## Order of Implementation

1. **Bug 1** — archive_batch.rs one-at-a-time (highest impact, self-contained)
2. **Bug 2** — streaming gz parse in both files (low risk, high value)
3. **Bug 3** — consume files by value in request.rs (prerequisite for Part 2)
4. **Part 2** — batch formatter (builds on Bug 3's owned-files structure)
