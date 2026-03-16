# 065 — Client Integration Tests

## Overview

Add integration tests for the client-side tools (`find-scan` and `find-watch`) that exercise
the full round-trip: real files on disk → extraction → bulk submission → server indexing →
search query verification.

The server already has a well-established integration test suite (`crates/server/tests/`) with a
`TestServer` helper that spawns a real in-process server on a random port. Client integration
tests will follow the same pattern but add a real filesystem and drive the client library
functions directly (not via subprocess), so the extraction, batching, and submission logic is
fully covered.

---

## What is Currently Tested

| Area | Coverage |
|---|---|
| `batch.rs` | Unit tests — `build_index_files`, `build_member_index_files` |
| `subprocess.rs` | Unit + async tests — `resolve_extractor`, `substitute_args`, external extractor fixture runs |
| `walk.rs` | Unit tests — glob matching, exclude patterns, dir override merging |
| `watch.rs` | Unit tests — config resolution logic |
| Server round-trip | Covered in `crates/server/tests/` but via hand-crafted `BulkRequest`, not via the client |

**The gap:** Nothing tests the client actually walking a directory, extracting real files, batching
them, submitting to a live server, and verifying the results are searchable.

---

## Design Decisions

### Library-based, not subprocess-based

Call the client library functions directly (e.g. `scan::run_scan()`) rather than spawning
`find-scan` as a child process. This is:
- Faster — no separate binary compilation step at test time
- More reliable — no binary path resolution
- More precise — can inspect internal state, inject config structs directly
- Still covers the real extraction + subprocess extractor + batching + submission path

The only thing not covered is CLI argument parsing, which is trivial and not worth the
complexity of subprocess tests.

### In-process server

Reuse the same `TestServer::spawn()` pattern from `crates/server/tests/helpers/`. Add
`find-server` as a dev-dependency of `find-client` so the helpers can be duplicated (they
are ~50 lines) in `crates/client/tests/helpers.rs`.

### Real files on disk

Create a `tempfile::TempDir` for each test, write actual files to it, and point the scan
source at it. This exercises the full extraction pipeline including subprocess extractors.

### Extractor binaries

The tests run against a debug build. The client resolves extractors relative to the current
executable's directory (falling back to `PATH`). Tests must ensure the extractor binaries are
findable. Options:
1. Set the `extractor_dir` scan option to `target/debug/` (the Cargo build output directory,
   resolved via `env!("CARGO_MANIFEST_DIR")`)
2. Or rely on them being on `PATH` in CI (already true: `mise run test` builds the workspace)

Option 1 is preferred for hermeticity.

---

## Test Location

```
crates/client/tests/
├── helpers.rs          # TestServer, write_file, make_scan_config helpers
├── scan.rs             # find-scan integration tests
└── watch.rs            # find-watch integration tests
fixtures/               # already exists — external extractor scripts + test.nd1
```

Each file becomes an integration test target in Rust's standard `tests/` convention (no
`[[test]]` entry needed in Cargo.toml).

---

## New Dependencies

Add to `crates/client/Cargo.toml` `[dev-dependencies]`:
```toml
find-server = { path = "../server" }
reqwest = { workspace = true, features = ["json"] }
tokio = { workspace = true, features = ["rt-multi-thread", "macros"] }
```

(`tempfile` is already in `[dependencies]`.)

---

## Test Harness (`helpers.rs`)

```rust
pub struct TestEnv {
    pub server: TestServer,   // in-process server, random port
    pub source_dir: TempDir,  // root of files to index
    pub extractor_dir: String, // path to target/debug/
}

impl TestEnv {
    pub async fn new() -> Self { ... }

    /// Write a file relative to source_dir and return its absolute path.
    pub fn write_file(&self, rel: &str, content: &str) -> PathBuf { ... }

    /// Delete a file relative to source_dir.
    pub fn remove_file(&self, rel: &str) { ... }

    /// Build a minimal ScanConfig pointing at source_dir with extractor_dir set.
    pub fn scan_config(&self) -> (ScanConfig, SourceConfig) { ... }

    /// Build an ApiClient connected to self.server.
    pub fn api_client(&self) -> ApiClient { ... }

    /// Run find-scan over source_dir and wait for the server to finish processing.
    pub async fn run_scan(&self) { ... }

    /// Run find-scan with options (e.g. force, upgrade).
    pub async fn run_scan_opts(&self, opts: ScanOpts) { ... }

    /// Search via the server API and return results.
    pub async fn search(&self, query: &str) -> Vec<SearchResult> { ... }

    /// List all indexed files for the test source.
    pub async fn list_files(&self) -> Vec<FileRecord> { ... }
}
```

`TestServer` is copied from `crates/server/tests/helpers/` with minimal changes (same
`spawn()`, `wait_for_idle()`, `url()` methods).

---

## find-scan Test Cases (`scan.rs`)

### Basic indexing

**S1 — Text file is indexed and searchable**
- Write `hello.txt` containing "unique_phrase_xyzzy"
- Run scan
- Assert search for "unique_phrase_xyzzy" returns 1 result with correct path

**S2 — Multiple files, all indexed**
- Write `a.txt`, `b.md`, `c.rs`
- Run scan
- Assert `list_files()` returns all 3; search finds content from each

**S3 — Unchanged file is not re-indexed**
- Write `stable.txt`, run scan
- Record the server `indexed_at` for `stable.txt`
- Run scan again
- Assert `indexed_at` has not changed (mtime unchanged → skip)

**S4 — Modified file is re-indexed**
- Write `changing.txt` with "version one", run scan
- Overwrite with "version two" (bump mtime via `filetime` or `std::fs::File` + sleep)
- Run scan again
- Assert search finds "version two" and not "version one"

**S5 — Deleted file is removed from index**
- Write `doomed.txt`, run scan, assert indexed
- Delete the file
- Run scan again
- Assert `list_files()` no longer contains `doomed.txt`; search returns nothing

**S6 — Exclude patterns respected**
- Write `included.txt` and `.git/config` (excluded by default)
- Run scan
- Assert only `included.txt` is indexed

### Archives

**S7 — ZIP members are indexed as composite paths**
- Write `test.zip` containing `readme.txt` with "archive_content_xyz"
- Run scan
- Assert search finds "archive_content_xyz" with path `test.zip::readme.txt`

**S8 — External extractor (tempdir mode)**
- Write `test.nd1` using the fixture format
- Configure `[scan.extractors]` with `nd1 = { mode = "tempdir", bin = "<fixtures>/find-extract-nd1", args = [...] }`
- Run scan
- Assert all 5 fixture members are indexed as composite paths

**S9 — External extractor (stdout mode)**
- Write `test.nd1`
- Configure with `mode = "stdout"` extractor
- Run scan
- Assert content lines are indexed under the outer path (no composite paths)

### Scan modes

**S10 — `--force` re-indexes all files**
- Write `force.txt`, run scan, record `indexed_at`
- Run scan with `force = true` (ScanOpts)
- Assert `indexed_at` has advanced

**S11 — `--upgrade` re-indexes outdated scanner versions**
- Manually submit a file with `scanner_version = 0` via `post_bulk`
- Run scan with `upgrade = true`
- Assert the file now has `scanner_version = SCANNER_VERSION`

---

## find-watch Test Cases (`watch.rs`)

Watch tests are inherently timing-sensitive. Use a short `debounce_ms` (e.g. 50ms) and call
`server.wait_for_idle()` after each filesystem mutation. Run `watch::run_watch()` in a
background task and cancel it via a `CancellationToken` at the end of each test.

**W1 — New file is indexed**
- Start watcher on empty source_dir
- Write `new.txt` with "watch_created_xyz"
- Wait for idle
- Assert search finds "watch_created_xyz"

**W2 — Modified file is re-indexed**
- Write `mutable.txt` with "version_a", run a scan first to establish baseline
- Start watcher
- Overwrite with "version_b"
- Wait for idle
- Assert search finds "version_b", not "version_a"

**W3 — Deleted file is removed**
- Write `ephemeral.txt`, scan to index it
- Start watcher
- Delete the file
- Wait for idle
- Assert search returns nothing for the file's content

**W4 — Renamed file updates index**
- Write `old_name.txt`, scan to index it
- Start watcher
- Rename to `new_name.txt`
- Wait for idle
- Assert `old_name.txt` no longer in `list_files()`; `new_name.txt` is present

**W5 — External extractor honoured by watch**
- Configure `nd1` extractor (same as S8)
- Start watcher
- Write `test.nd1`
- Wait for idle
- Assert members indexed as composite paths

---

## Files Changed

- `crates/client/Cargo.toml` — add dev-dependencies (`find-server`, `reqwest`, `tokio`)
- `crates/client/tests/helpers.rs` — new: `TestEnv`, `TestServer` (copied from server helpers)
- `crates/client/tests/scan.rs` — new: S1–S11
- `crates/client/tests/watch.rs` — new: W1–W5
- `crates/client/src/scan.rs` — may need minor visibility changes (`pub(crate)` → `pub`) to
  expose `run_scan` and `ScanOpts` to integration tests

---

## Testing Strategy

Run with:
```
cargo test -p find-client --test scan
cargo test -p find-client --test watch
```

Or all at once:
```
cargo test -p find-client
```

The server integration tests already run in CI via `cargo test --workspace` — they work
because the server is in-process and ports are random. The find-scan tests are the same
pattern and will run in CI without any special treatment.

**find-scan tests** run in CI automatically. No `#[ignore]` needed.

**find-watch tests** are the exception — they depend on OS-level filesystem event
notification (inotify on Linux) and are timing-sensitive on slow CI runners. Mark all watch
tests `#[ignore]` initially and run them locally:

```bash
# Run all client integration tests (scan only — watch tests ignored by default):
cargo test -p find-client

# Run watch tests locally:
cargo test -p find-client --test watch -- --include-ignored
```

Promote individual watch tests to CI (remove `#[ignore]`) once they prove stable under load.

---

## Breaking Changes

None. All new files.

---

## Out of Scope

- `find-anything` query CLI — already covered by the server integration tests which verify
  search results directly via the API. CLI argument parsing is trivial.
- `find-admin`, `find-upload` — separate tools with their own planned test coverage.
- Windows-specific paths (`find-tray`, `find-service`) — not testable on Linux CI.
- Performance / load testing.
