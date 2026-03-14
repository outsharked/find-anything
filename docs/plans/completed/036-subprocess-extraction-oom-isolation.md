# 036 — Subprocess Extraction in find-scan (OOM Isolation)

## Overview

`lopdf` (PDF) and `sevenz_rust2` (7z) call `handle_alloc_error` on OOM, which **aborts**
the process — not a panic, so `catch_unwind` cannot intercept it. The only reliable fix is
process isolation: if an extractor subprocess OOMs, only that subprocess dies; `find-scan`
continues to the next file.

`find-watch` already solves this by spawning the `find-extract-*` binaries. This plan ports
`find-scan` to the same model, removing all memory guards in the process.

## What already exists (reuse, don't reinvent)

- All `find-extract-*` binaries exist (v0.1.8). They accept CLI args, write
  `Vec<IndexLine>` JSON to stdout, tracing logs to stderr.
- `watch.rs` has three functions to move: `extract_via_subprocess`,
  `relay_subprocess_logs`, `extractor_binary_for`.
- `relay_subprocess_logs` **already captures stderr** (`tokio::process::Command::output()`
  collects both stdout and stderr) and re-emits it through the parent's tracing subscriber.
  Subprocess logs appear in find-scan output at the correct level — nothing is lost.
- `batch.rs::build_member_index_files` already handles `Vec<MemberBatch>` from the
  streaming path; we reuse this logic unchanged.
- `MemberBatch` in `find_extract_archive` already carries `lines`, `content_hash`,
  `skip_reason` — exactly the richer format needed. We just need to make it
  `Serialize/Deserialize` and switch the binary to output `Vec<MemberBatch>`.

## Design Decisions

- **Archive binary output changes from `Vec<IndexLine>` to `Vec<MemberBatch>`** — this
  preserves content hashes and skip reasons that the flat format loses.
- **No backward compatibility shim** — both find-scan and find-watch are updated together.
  find-watch calls `extract_archive_via_subprocess` for archives and flattens inline.
- **Two clean functions** — `extract_via_subprocess` for non-archive files (returns
  `Vec<IndexLine>`), `extract_archive_via_subprocess` for archives (returns
  `Vec<MemberBatch>`). No combined function with archive detection.
- **`available_bytes()` stays** — still used by the archive extractor's 7z solid block guard.
  That guard prevents a subprocess from even attempting a block it will certainly OOM on,
  saving a crash log entry and subprocess spawn overhead.
- **`max_pdf_size_mb` removed** — with process isolation, OOM only kills the subprocess;
  the static size limit and dynamic memory guards in the PDF extractor are no longer needed.

## Implementation

### 1. Extend `MemberBatch` to be serializable (`find_extract_archive`)

Add `#[derive(Serialize, Deserialize)]` to `MemberBatch` in
`crates/extractors/archive/src/lib.rs`.

Change `crates/extractors/archive/src/main.rs` to call `extract_streaming` (collecting
batches into a `Vec<MemberBatch>`) and output that as JSON, instead of calling the
non-streaming `extract()` and outputting `Vec<IndexLine>`:

```rust
let mut batches: Vec<MemberBatch> = Vec::new();
find_extract_archive::extract_streaming(&path, &cfg, &mut |batch| {
    batches.push(batch);
})?;
println!("{}", serde_json::to_string_pretty(&batches)?);
```

### 2. New `crates/client/src/subprocess.rs`

Move from `watch.rs` (lines ~372–503):
- `pub fn relay_subprocess_logs(stderr, binary, file)` — unchanged
- `pub fn extractor_binary_for(path, extractor_dir: &Option<String>) -> String` — unchanged
- `pub async fn extract_via_subprocess(path, scan: &ScanConfig, extractor_dir: &Option<String>) -> Vec<IndexLine>` — for non-archive files only (PDF, text, image, audio); panics/errors return empty vec
- `pub async fn extract_archive_via_subprocess(path, scan: &ScanConfig, extractor_dir: &Option<String>) -> Vec<MemberBatch>` — archive files only; parse `Vec<MemberBatch>` from binary stdout

Update `watch.rs` to import from `subprocess`, remove the moved functions, and update
the archive call site to call `extract_archive_via_subprocess` and flatten inline:
```rust
let batches = subprocess::extract_archive_via_subprocess(path, scan, extractor_dir).await;
let lines: Vec<IndexLine> = batches.into_iter().flat_map(|b| b.lines).collect();
```

Add `mod subprocess;` to `crates/client/src/scan_main.rs`.

### 3. Add `extractor_dir` to `ScanConfig`

In `crates/common/src/config.rs`, add to `ScanConfig`:
```rust
#[serde(default)]
pub extractor_dir: Option<String>,
```
`#[serde(default)]` gives `None` without needing a `defaults_client.toml` entry or a
`ScanDefaults` field. `ScanConfig::apply_override` and `ScanOverride` need no change
(extractor_dir is global-only, not per-directory).

### 4. Replace extraction branches in `scan.rs`

**Non-archive path** — replace the `extract::extract(abs_path, &cfg)` call:
```rust
lazy_header::set_pending(&abs_path.to_string_lossy());
let lines = subprocess::extract_via_subprocess(abs_path, &eff_scan, &eff_scan.extractor_dir).await;
lazy_header::clear_pending();
```
The `Vec<IndexLine>` result feeds into `build_index_files` exactly as before.
The `[FILE:mime]` kind-refinement check stays unchanged — it works on the same Vec.

`extract::extract` was synchronous; the subprocess call is async. `run_scan` is already
async, so `.await` works directly (no `spawn_blocking` needed).

**Archive path** — replace the entire `spawn_blocking` block and channel:
```rust
// Submit outer archive file first (server deletes stale members on receipt)
batch.push(outer_file);
submit_batch(..., vec![], None).await?;
batch_bytes = 0;

lazy_header::set_pending(&abs_path.to_string_lossy());
let member_batches = subprocess::extract_archive_via_subprocess(
    abs_path, &eff_scan, &eff_scan.extractor_dir).await;
lazy_header::clear_pending();

// Process each MemberBatch identically to the old streaming path.
for member_batch in member_batches {
    if member_batch.lines.is_empty() {
        if let Some(reason) = member_batch.skip_reason {
            failures.push(IndexingFailure { path: rel_path.clone(), error: truncate_error(&reason, MAX_ERROR_LEN) });
        }
        continue;
    }
    // Apply effective exclude patterns, record per-member skip reason, batch & submit
    // ... (unchanged logic from current streaming path)
}
```

### 5. Remove `max_pdf_size_mb` and memory guards

- `crates/common/src/config.rs` — remove `max_pdf_size_mb` from `ScanConfig`,
  `ScanDefaults`, `ScanOverride`, `ExtractorConfig`; remove `default_max_pdf_size_mb()`
- `crates/common/src/defaults_client.toml` — remove `max_pdf_size_mb = 32`
- `crates/extractors/pdf/src/lib.rs` — remove static size guard and `available_bytes()`
  guard from both `extract()` and `extract_from_bytes()`

## Files Changed

| File | Change |
|------|--------|
| `crates/extractors/archive/src/lib.rs` | Add `#[derive(Serialize, Deserialize)]` to `MemberBatch` |
| `crates/extractors/archive/src/main.rs` | Switch to `extract_streaming` → output `Vec<MemberBatch>` |
| `crates/client/src/subprocess.rs` | **New** — functions moved from `watch.rs`; add `extract_archive_via_subprocess` returning `Vec<MemberBatch>` |
| `crates/client/src/watch.rs` | Remove 3 functions; import from `subprocess`; flatten `Vec<MemberBatch>` for archive files |
| `crates/client/src/scan.rs` | Replace both extraction branches with subprocess calls |
| `crates/client/src/scan_main.rs` | Add `mod subprocess;` |
| `crates/common/src/config.rs` | Remove `max_pdf_size_mb`; add `extractor_dir` to `ScanConfig` |
| `crates/common/src/defaults_client.toml` | Remove `max_pdf_size_mb = 32` |
| `crates/extractors/pdf/src/lib.rs` | Remove memory guards |

## Testing

1. `cargo build --workspace` — clean build
2. `mise run clippy` — no warnings
3. `cargo test --workspace` — all tests pass
4. Manual: run `find-scan` against a directory with PDFs and 7z archives; confirm they
   index correctly and content hashes / deduplication still work
5. Manual: confirm `find-scan` continues scanning after a subprocess failure (kill
   `find-extract-pdf` mid-run or point it at a corrupt PDF)

## Breaking Changes

None externally. The archive binary's stdout format changes (`Vec<IndexLine>` →
`Vec<MemberBatch>`), but the binary is internal — only invoked by find-scan/find-watch,
both of which are updated in this change.
