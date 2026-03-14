# Full Running-Server Integration Test

## Overview

A heavyweight end-to-end test suite that spins up a real `find-server` process
(not an in-process test server), runs `find-scan` against real files, and
verifies the full stack: filesystem walk → subprocess extraction → inbox worker
→ SQLite + ZIP storage → search API → UI-facing responses.

Unlike the existing in-process integration tests in `crates/server/tests/`
(which use `create_app_state` + `build_router` and stub out content via
`post_bulk`), this suite exercises the actual binaries as deployed. It catches
issues that in-process tests cannot: binary-not-found errors, IPC serialisation
mismatches, subprocess OOM, config parsing, and cross-binary version checks.

**Not run in CI** — this suite is local-only at first. It depends on built
release (or debug) binaries and a writable scratch directory, and takes longer
than the unit/integration test budget allows in CI. A future step might add a
nightly CI job for it.

## Design Decisions

**Real processes over in-process:** `find-server` is launched as a child process
via `std::process::Command` (or `tokio::process::Command`). `find-scan` is also
invoked as a subprocess. This matches the actual deployment model exactly.

**Fixture directory:** A `tests/e2e/fixtures/` directory contains a curated set
of real files: plain text, a PDF, a ZIP with nested content, a RAR, an image
with EXIF, an Office document. These are small but representative.

**`#[ignore]` by default:** Every test is annotated `#[ignore]` so
`cargo test --workspace` (CI) never runs them. Run locally with:
```
cargo test -p find-server --test e2e -- --ignored
```
or via a new mise task `mise run test-e2e`.

**Port allocation:** The server is started on port 0 (OS-assigned) with the port
written to stdout on startup (or read from the stats endpoint after a brief
poll). Each test gets its own server instance and temp `data_dir`.

**Scenario model:** Each test function is a full scenario: start server → run
scan → wait for idle (poll `/api/v1/stats`) → assert search/context/tree results
→ optionally mutate (delete/rename) → re-scan → assert updated results →
shut down server.

## Implementation

### 1. Test binary location

Tests resolve binaries via a helper that checks (in order):
1. `FIND_BIN_DIR` env var (set by `mise run test-e2e`)
2. `target/release/` relative to workspace root
3. `target/debug/` relative to workspace root

Panics if a required binary is not found, with a clear message pointing to
`cargo build` or `mise run build`.

### 2. Server lifecycle helper

```rust
struct E2eServer {
    process: Child,
    base_url: String,
    data_dir: TempDir,
    client: reqwest::Client,
}

impl E2eServer {
    async fn spawn(extra_config: &str) -> Self { ... }
    async fn wait_for_idle(&self) { ... }   // polls /api/v1/stats
    async fn run_scan(&self, source: &str, path: &Path) { ... }
}

impl Drop for E2eServer {
    fn drop(&mut self) { self.process.kill().ok(); }
}
```

The server config is written to a temp file with:
- `data_dir` = `TempDir` path
- `token` = `"e2e-test-token"`
- `bind` = `"127.0.0.1:0"`  (requires server to print actual port on startup,
  or we poll a known port by pre-allocating with `TcpListener::bind("0")`)

### 3. Test file layout

```
crates/server/tests/e2e/
  fixtures/
    text/
      notes.txt           — plain text, known content
      code.rs             — Rust source file
    archives/
      sample.zip          — ZIP with a .txt and a nested .tar
      sample.rar          — RAR with a .txt member
    documents/
      sample.pdf          — small single-page PDF (synthetic or real)
    media/
      photo.jpg           — JPEG with EXIF GPS/date data
  mod.rs                  — E2eServer + helpers
  scenarios/
    text_search.rs
    archive_members.rs
    delete_and_rescan.rs
    rename.rs
    upload_endpoint.rs    — full upload → extraction → searchable
    multi_source.rs
```

### 4. Scenarios

#### `text_search`
- Index `fixtures/text/`
- Search for a word that appears only in `notes.txt` → assert 1 result with
  correct `source`, `path`, `line_number`, snippet
- Search for a word in `code.rs` → assert found
- `GET /api/v1/file` → verify content matches the real file on disk

#### `archive_members`
- Index a directory containing `sample.zip`
- Assert composite-path members appear in search: `sample.zip::inner.txt`
- Assert nested archive members: `sample.zip::nested.tar::file.txt`
- Assert RAR members are searchable: `sample.rar::doc.txt`
- Tree browse: `GET /api/v1/tree?prefix=sample.zip::` lists members

#### `delete_and_rescan`
- Index a file, verify searchable
- Delete the file from disk, re-run `find-scan`
- Verify the file no longer appears in search results

#### `rename`
- Index a file, verify searchable under old path
- Rename the file on disk, re-run `find-scan`
- Verify old path gone, new path searchable

#### `upload_endpoint`
- POST `/api/v1/upload` → PATCH full content in one chunk → wait_for_idle
- Verify the uploaded file is searchable (proves extractor subprocess ran)
- This is the test that specifically requires `find-extract-text` on PATH

#### `multi_source`
- Two sources indexed with overlapping filename `readme.txt`
- Search scoped to source A does not return source B's result
- Unscoped search returns both
- `GET /api/v1/sources` lists both

### 5. Mise task

```toml
[tasks.test-e2e]
description = "Run end-to-end integration tests (local only, not CI)"
run = """
  cargo build -p find-server -p find-scan -p find-extract-text \
              -p find-extract-archive -p find-extract-pdf
  FIND_BIN_DIR=target/debug \
  cargo test -p find-server --test e2e -- --ignored --test-threads=1
"""
```

`--test-threads=1` avoids port collisions and keeps output readable.

## Files Changed

- `crates/server/tests/e2e/mod.rs` — **new** — `E2eServer`, binary resolver, helpers
- `crates/server/tests/e2e/fixtures/` — **new** — small representative test files
- `crates/server/tests/e2e/scenarios/text_search.rs` — **new**
- `crates/server/tests/e2e/scenarios/archive_members.rs` — **new**
- `crates/server/tests/e2e/scenarios/delete_and_rescan.rs` — **new**
- `crates/server/tests/e2e/scenarios/rename.rs` — **new**
- `crates/server/tests/e2e/scenarios/upload_endpoint.rs` — **new**
- `crates/server/tests/e2e/scenarios/multi_source.rs` — **new**
- `.mise.toml` — add `test-e2e` task

## Not in CI (at first)

The suite is gated behind `#[ignore]` so it is invisible to `cargo test
--workspace`. The `.github/workflows/ci.yml` file is **not** changed. Once the
suite is stable and fast enough, a nightly workflow can be added that sets
`FIND_BIN_DIR` and runs `-- --ignored`.

## Breaking Changes

None — additive only.
