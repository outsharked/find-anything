# Integration Tests for the Public HTTP API

## Overview

The find-anything server has comprehensive unit tests per module but no HTTP-level integration tests. This plan adds an integration test suite that covers the full request→worker→response cycle using a real Axum server on an ephemeral port.

## Design Decisions

**Real TCP over `axum::test::TestClient`:** Tests exercise actual HTTP routing, content-encoding, auth headers, and the async two-phase worker pipeline — not just route handler logic.

**Lib/bin split:** Cargo integration tests (`tests/*.rs`) can only import from a `[lib]` target. We extract server initialization logic from `main.rs` into `lib.rs` and expose two public functions: `create_app_state` and `build_router`. This keeps `main.rs` minimal (arg parsing + bind + serve).

**Auth in tests:** Use a fixed `token = "test-token"` in test config. The `reqwest` client is built with a default `Authorization: Bearer test-token` header. Auth error tests use a fresh server with a different token and send no header.

**Worker idle detection:** Poll `GET /api/v1/stats` every 50 ms until `inbox_pending == 0 && archive_queue == 0`. Covers both phases of the two-phase worker (SQLite writes then ZIP I/O). Timeout 10 s.

## Implementation

### 1. Add `[lib]` stanza to `crates/server/Cargo.toml`

```toml
[lib]
name = "find_server"
path = "src/lib.rs"
```

The `[[bin]]` entry stays pointing at `src/main.rs`. No new dependencies needed — `reqwest`, `tokio`, `serde_json`, `flate2`, `tempfile` are all already present.

### 2. Create `crates/server/src/lib.rs`

Move from `main.rs`:
- All `mod` declarations (`archive`, `compaction`, `db`, `fuzzy`, `normalize`, `routes`, `upload`, `worker`) — kept `pub(crate)`
- `WebAssets`, `serve_static`, `serve_index_html`
- `CachedUpdateCheck`, `AppState`

Expose two public functions:

```rust
pub async fn create_app_state(config: ServerAppConfig, data_dir: PathBuf) -> anyhow::Result<Arc<AppState>>
pub fn build_router(state: Arc<AppState>) -> Router
```

`create_app_state` contains: dir creation, schema check, `SharedArchiveState::new`, `load_cached_stats`, broadcast channel, `Arc<AppState>` construction, `recover_stranded_requests`, spawn inbox worker + upload cleanup + compaction scanner.

`build_router` contains: the upload + main router construction + `TraceLayer`.

### 3. Trim `crates/server/src/main.rs`

```rust
use find_server::{create_app_state, build_router};
// tracing init, arg parsing, config loading ...
let state = create_app_state(config, data_dir).await?;
let app = build_router(state);
let listener = TcpListener::bind(&bind).await?;
axum::serve(listener, app).await?;
```

### 4. Create test helpers (`crates/server/tests/helpers.rs`)

```rust
pub struct TestServer {
    pub base_url: String,
    pub client: reqwest::Client,   // has default Authorization header
    _data_dir: tempfile::TempDir,
}

impl TestServer {
    pub async fn spawn() -> Self { ... }
    pub async fn wait_for_idle(&self) { ... }  // polls /api/v1/stats
    pub async fn post_bulk(&self, req: &BulkRequest) { ... }  // gzip encodes + POSTs
}

pub fn make_text_bulk(source: &str, path: &str, content: &str) -> BulkRequest { ... }
// line_number=0 is the filename line; content lines start at 1
```

### 5. Test files

#### `tests/smoke.rs`
- `GET /api/v1/settings` → 200, valid `AppSettingsResponse`
- `GET /api/v1/stats` (empty) → 200, `sources: []`
- `GET /api/v1/sources` (empty) → 200, empty array
- `GET /api/v1/search` (no `q` param) → 400/422

#### `tests/index_and_search.rs`
- `post_bulk` → `wait_for_idle` → `GET /api/v1/search?q=<word>` finds the file
- Search without source param searches all sources
- `GET /api/v1/context` returns surrounding lines
- `GET /api/v1/file` returns full content with `file_kind == "text"`
- `GET /api/v1/stats` shows `total_files == 1`
- `GET /api/v1/tree` shows directory and file entries
- `GET /api/v1/recent` shows indexed file
- `GET /api/v1/sources` shows source name

#### `tests/delete.rs`
- Index → verify found → bulk delete → verify gone
- Delete non-existent path is safe

#### `tests/multi_source.rs`
- Two sources are isolated from each other in search
- Both appear in `GET /api/v1/sources`

#### `tests/errors.rs`
- Unknown source name → 200 empty results (not 404)
- Context for file in non-existent source → 500
- Bulk POST without `Content-Encoding: gzip` → 415
- Bulk POST with wrong token → 401

## Files Changed

- `crates/server/Cargo.toml` — add `[lib]` stanza
- `crates/server/src/main.rs` — slim down to arg parsing + call lib functions
- `crates/server/src/lib.rs` — **new** — `AppState`, `create_app_state`, `build_router`
- `crates/server/tests/helpers.rs` — **new** — `TestServer`, `make_text_bulk`
- `crates/server/tests/smoke.rs` — **new**
- `crates/server/tests/index_and_search.rs` — **new**
- `crates/server/tests/delete.rs` — **new**
- `crates/server/tests/multi_source.rs` — **new**
- `crates/server/tests/errors.rs` — **new**

## Running the Tests

### Mise task

Add to `.mise.toml`:

```toml
[tasks.test]
description = "Run all tests (unit + integration)"
run = "cargo test --workspace"
```

Running just the server integration tests:
```bash
cargo test -p find-server             # all server tests
cargo test -p find-server smoke       # smoke subset only
```

### GitHub Actions (`.github/workflows/ci.yml`)

**No changes required.** The existing `Test` job runs `cargo test --workspace`, which automatically picks up Cargo integration test binaries in `crates/server/tests/`. The `mkdir -p web/build` stub step already present satisfies the `rust-embed` compile-time requirement for `WebAssets`.

Optional improvement — split the existing `Test` step into two for clearer CI output:

```yaml
- name: Unit tests
  run: cargo test --workspace --lib

- name: Integration tests
  run: cargo test -p find-server --test '*'
```

## Verification

```bash
cargo build -p find-server           # verify lib/bin split compiles
cargo test -p find-server smoke      # fast smoke tests first
cargo test -p find-server            # full suite
mise run clippy                      # no warnings
```

Each test function calls `TestServer::spawn()` which gets its own `TempDir` and random port — safe to run with `--test-threads=N`.

## Breaking Changes

None. The refactor is internal to `find-server`; no public API or config format changes.
