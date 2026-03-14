# Plan 054: `find-extract-types` micro-crate

## Overview

Touching any file in `find-common` (including `api.rs` when adding a field)
currently triggers a full rebuild of all 14 first-party crates — because every
extractor transitively depends on `find-common`. This takes ~32 s even with
warm deps.

The fix is to extract the tiny subset of types that the extractors actually need
into a new zero-dependency micro-crate (`find-extract-types`). Extractors depend
on that instead of `find-common`, breaking the cascade.

**Before:** `api.rs` change → `find-common` → 8 extractors + dispatch + client + server (14 crates, ~32 s)
**After:**  `api.rs` change → `find-common` → client + server (~3 crates, ~5 s)

## What the extractors actually use from `find-common`

Every extractor (`text`, `pdf`, `media`, `archive`, `html`, `office`, `epub`,
`pe`, `dispatch`) imports exactly:

```rust
use find_common::api::IndexLine;
use find_common::config::ExtractorConfig;
```

`find-extract-archive` also imports:
```rust
use find_common::mem::available_bytes;
```

Nothing else. No HTTP types, no config parsing, no logging.

## New crate: `crates/extract-types/`

### Contents

**`src/lib.rs`** — re-exports the three modules:
```rust
pub mod index_line;
pub mod extractor_config;
pub mod mem;
```

**`src/index_line.rs`** — moved verbatim from `find-common/src/api.rs`:
```rust
use serde::{Deserialize, Serialize};

/// Version of the scanner/extraction logic.
pub const SCANNER_VERSION: u32 = 1;

/// A single extracted line sent from client → server.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IndexLine {
    pub archive_path: Option<String>,
    pub line_number: usize,
    pub content: String,
}

/// Classify a file by its extension alone — no extractor lib deps.
pub fn detect_kind_from_ext(ext: &str) -> &'static str { ... }
```

**`src/extractor_config.rs`** — moved verbatim from `find-common/src/config.rs`:
```rust
#[derive(Debug, Clone, Copy)]
pub struct ExtractorConfig {
    pub max_content_kb: usize,
    pub max_depth: usize,
    pub max_line_length: usize,
    pub max_temp_file_mb: usize,
    pub include_hidden: bool,
    pub max_7z_solid_block_mb: usize,
}

impl Default for ExtractorConfig { ... }
// Note: from_scan() and from_extraction() are NOT moved here — they depend
// on ScanConfig and ExtractionSettings which live in find-common. Those
// constructor methods stay in find-common/src/config.rs as inherent impls
// that build ExtractorConfig from common config types.
```

**`src/mem.rs`** — moved verbatim from `find-common/src/mem.rs`.

### `Cargo.toml`
```toml
[package]
name = "find-extract-types"
version = "0.6.1"
edition = "2021"

[dependencies]
serde = { workspace = true }
```

Only `serde` — no tokio, no anyhow, no rusqlite, nothing heavy.

## Changes to `find-common`

### `Cargo.toml`
Add dependency:
```toml
find-extract-types = { path = "../extract-types" }
```

### `src/api.rs`
- Remove `detect_kind_from_ext`, its tests, `SCANNER_VERSION`, and `IndexLine`
- Add at top: `pub use find_extract_types::index_line::{detect_kind_from_ext, IndexLine, SCANNER_VERSION};`
- Everything else in api.rs is untouched

### `src/config.rs`
- Remove the `ExtractorConfig` struct and its `Default` impl
- Keep `from_scan()` and `from_extraction()` as standalone functions (or move to `impl ExtractorConfig` via re-export trick — see below)
- Add at top: `pub use find_extract_types::extractor_config::ExtractorConfig;`
- The `from_scan` and `from_extraction` constructors reference `ScanConfig` /
  `ExtractionSettings` which live in `find-common`, so they stay in
  `find-common/src/config.rs` as an `impl ExtractorConfig` block — this works
  because Rust allows impl blocks for types from other crates in the same
  workspace when the orphan rules are satisfied (they are here, since
  `find-common` owns neither type nor trait — actually this won't work due to
  orphan rules).

  **Alternative (simpler):** keep `from_scan` and `from_extraction` as free
  functions in `find-common/src/config.rs`:
  ```rust
  pub fn extractor_config_from_scan(scan: &ScanConfig) -> ExtractorConfig { ... }
  pub fn extractor_config_from_extraction(e: &ExtractionSettings) -> ExtractorConfig { ... }
  ```
  Call sites in `find-client` and `find-server` are updated accordingly.
  There are only a handful of call sites.

### `src/mem.rs`
- Either keep a thin wrapper that re-exports from `find-extract-types::mem`,
  or just leave it in place (mem.rs has no transitive cost — it's already in
  find-common which the extractors are being decoupled from).
- Since only `find-extract-archive` uses `available_bytes`, and it will now
  depend on `find-extract-types` instead of `find-common`, move `mem.rs`
  wholesale into `find-extract-types` and re-export from `find-common` for
  any other callers.

### `src/lib.rs`
Keep `pub mod mem;` but have it re-export from `find-extract-types`:
```rust
pub use find_extract_types::mem;
```

## Changes to each extractor

For each of `text`, `pdf`, `media`, `archive`, `html`, `office`, `epub`, `pe`, `dispatch`:

### `Cargo.toml`
Replace:
```toml
find-common = { path = "../../common" }
```
With:
```toml
find-extract-types = { path = "../../extract-types" }
```

### `src/*.rs`
Replace:
```rust
use find_common::api::IndexLine;
use find_common::config::ExtractorConfig;
use find_common::mem::available_bytes;   // archive only
```
With:
```rust
use find_extract_types::index_line::{IndexLine, detect_kind_from_ext, SCANNER_VERSION};
use find_extract_types::extractor_config::ExtractorConfig;
use find_extract_types::mem::available_bytes;   // archive only
```

(Or add convenience re-exports to `find-extract-types/src/lib.rs` so it's just
`use find_extract_types::{IndexLine, ExtractorConfig}` — probably cleaner.)

## Changes to workspace `Cargo.toml`

Add to `[members]`:
```toml
"crates/extract-types",
```

## Call sites for `from_scan` / `from_extraction`

Search for callers of `ExtractorConfig::from_scan` and
`ExtractorConfig::from_extraction` — these will need updating to call the new
free functions if that approach is taken.

```bash
grep -rn "ExtractorConfig::from_" crates/
```

Expected: only in `find-client/src/scan.rs` and `find-server/src/worker.rs` or
routes. Update those call sites.

## Files changed summary

| File | Change |
|------|--------|
| `crates/extract-types/` | **New crate** (create) |
| `crates/extract-types/Cargo.toml` | New |
| `crates/extract-types/src/lib.rs` | New |
| `crates/extract-types/src/index_line.rs` | Moved from `find-common/src/api.rs` |
| `crates/extract-types/src/extractor_config.rs` | Moved from `find-common/src/config.rs` |
| `crates/extract-types/src/mem.rs` | Moved from `find-common/src/mem.rs` |
| `Cargo.toml` | Add `crates/extract-types` to `[members]` |
| `crates/common/Cargo.toml` | Add `find-extract-types` dep |
| `crates/common/src/api.rs` | Remove moved items, add `pub use` re-exports |
| `crates/common/src/config.rs` | Remove `ExtractorConfig` struct, add re-export, keep constructors as free fns |
| `crates/common/src/mem.rs` | Remove or re-export from `find-extract-types` |
| `crates/common/src/lib.rs` | Update `mod mem` if needed |
| `crates/extractors/*/Cargo.toml` | Replace `find-common` dep with `find-extract-types` |
| `crates/extractors/*/src/*.rs` | Update `use` paths |
| `crates/client/src/*.rs` | Update call sites for `from_scan` / `from_extraction` if renamed |
| `crates/server/src/*.rs` | Update call sites for `from_extraction` if renamed |

## Testing

```bash
cargo check --workspace          # everything compiles
mise run clippy                  # no warnings
touch crates/common/src/api.rs && cargo build -p find-server  # should be ~5s, not ~32s
```

## Notes

- `find-extract-types` has no build scripts, no proc macros, no heavy deps —
  it will compile in under 1 second and almost never changes.
- `find-common` re-exports everything at the same public paths (`api::IndexLine`,
  `config::ExtractorConfig`, etc.) so callers outside the extractor crates need
  no changes.
- The Windows crates (`find-tray-win`, `find-windows-service`) do not use
  `IndexLine` or `ExtractorConfig` directly — they depend on `find-common` for
  other things and are unaffected by this split.
