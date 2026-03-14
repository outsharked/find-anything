# Plan 016: Incremental File Watcher (`find-watch`)

## Overview

Phase 2 of the extractor architecture refactor (plan 015). Turns `find-watch`
from a stub into a working daemon that monitors configured source paths and
pushes single-file updates to the server as files change — without doing a full
re-scan.

The watcher uses subprocess mode for extraction: it spawns the appropriate
`find-extract-{type}` binary for each changed file and parses its JSON output.
This keeps the watch loop isolated from heavy extractor deps.

Server: no changes. The watch client reuses `POST /api/v1/bulk` (1-file
payload), exactly like `find-scan`.

## Design Decisions

- **Subprocess extraction**: `find-watch` spawns `find-extract-{type}` binaries
  rather than linking extractor libs directly. The binary path is resolved by
  looking next to the current executable, then PATH.
- **Debounce**: A 500ms (configurable) debounce window collapses rapid
  successive events on the same path into a single action.
- **No initial scan**: Startup only logs sources. Run `find-scan` first, then
  `find-watch` to keep it current.
- **Rename handling**: notify emits From→Delete and To→Update. After debounce
  both paths are handled correctly.
- **AccumulatedKind collapse**:
  - Create/Modify(Data) → Update
  - Remove → Delete
  - Update then Delete → Delete
  - Delete then Create → Update
  - Metadata-only Modify → ignored

## Implementation

### 1. `WatchConfig` in `crates/common/src/config.rs`

Add `WatchConfig` struct and a `watch` field to `ClientConfig`:

```rust
pub struct WatchConfig {
    pub debounce_ms: u64,      // default 500
    pub extractor_dir: Option<String>,  // auto-detect if None
}
```

### 2. `detect_kind_from_ext` in `crates/common/src/api.rs`

Pure extension-based kind detection, no extractor lib deps:

```rust
pub fn detect_kind_from_ext(ext: &str) -> &'static str
```

### 3. New `crates/client/src/batch.rs`

Move `build_index_files` and `submit_batch` from `scan.rs`. Uses
`detect_kind_from_ext` instead of `extract::detect_kind` for archive member
kind detection, so no extractor lib deps in this module.

### 4. `crates/client/src/scan.rs` updates

Remove moved functions, import from `batch.rs`.

### 5. `crates/client/src/watch.rs` — Full Implementation

- `run_watch(config)` — entry point; sets up watcher on all source paths
- `build_source_map(sources)` — maps root paths to (source_name, root_str)
- `find_source(path, map)` — longest-match to find source + compute rel_path
- Debounce loop: notify → mpsc(1000) → drain until silence → flush batch
- `extract_via_subprocess(path, config)` → `Vec<IndexLine>`
- `extractor_binary_for(path, extractor_dir)` → binary name/path
- `handle_update` / `handle_delete` → `api.bulk(BulkRequest { ... })`

Exclusion globs: reuse same patterns from `ScanConfig.exclude`.

### 6. Systemd unit files under `docs/systemd/`

- `user/find-server.service` — personal workstation server unit
- `user/find-watch.service` — personal workstation watcher unit
- `system/find-server.service` — system-wide server unit
- `system/find-watch@.service` — per-user template watcher unit
- `README.md` — installation instructions

## Files Changed

| Path | Change |
|------|--------|
| `crates/common/src/config.rs` | Add `WatchConfig`, add `watch` field to `ClientConfig` |
| `crates/common/src/api.rs` | Add `detect_kind_from_ext` |
| `crates/client/src/batch.rs` | **New** — `build_index_files` + `submit_batch` |
| `crates/client/src/scan.rs` | Remove moved fns, import from `batch` |
| `crates/client/src/scan_main.rs` | Add `mod batch` |
| `crates/client/src/watch.rs` | Full implementation |
| `crates/client/src/watch_main.rs` | Add `mod api; mod batch` |
| `docs/systemd/user/find-server.service` | **New** |
| `docs/systemd/user/find-watch.service` | **New** |
| `docs/systemd/system/find-server.service` | **New** |
| `docs/systemd/system/find-watch@.service` | **New** |
| `docs/systemd/README.md` | **New** |

## Config File Example

```toml
[server]
url   = "http://localhost:8080"
token = "your-secret-token"

[[sources]]
name  = "home"
paths = ["/home/alice/documents", "/home/alice/projects"]

[scan]
max_file_size_kb = 1024
exclude = ["**/.git/**", "**/node_modules/**", "**/target/**"]

[scan.archives]
max_depth = 10

[watch]
debounce_ms   = 500
extractor_dir = "/usr/local/bin"   # optional, auto-detected if omitted
```

## Testing

1. `cargo build` — clean compile
2. Manual test:
   - Start `find-server`
   - Run `find-scan` on a test directory
   - Start `find-watch` on the same directory
   - Create/modify/delete a file → verify server receives the update
   - Create/modify/delete a PDF → verify PDF text is extracted via subprocess
   - Rename a file → verify old path removed and new path indexed
3. Systemd validation:
   - `systemd-analyze verify docs/systemd/user/find-watch.service`

## Breaking Changes

None. `WatchConfig` is serde default so existing config files without a
`[watch]` section continue to work unchanged.

## Not in This Plan (Future)

- Separate `find-watch` crate for true lean binary
- `find-watch --initial-scan` flag for delta sync on startup
- Systemd watchdog (`sd_notify`) integration
