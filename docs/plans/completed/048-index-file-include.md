# 048 — Per-directory `include` patterns via `.index` file

## Overview

Add an `include` field to `ScanOverride` (the struct parsed from per-directory
`.index` files). When present, only files matching the specified glob patterns
are indexed within that directory subtree. This is the complement of the
existing `exclude` field and allows users to whitelist specific subdirectories
without needing `.noindex`.

## Motivation

A user has a `backups/` directory containing thousands of files they do not
want indexed, except for one subdirectory (`backups/myfolder/`). The current
options are:

- Place `.noindex` in `backups/` — but then `myfolder/` is also excluded with
  no way to create an exception.
- Add a second source pointing at `backups/myfolder/` — verbose, requires
  client.toml changes for every exception.

With this feature the user places a `.index` file in `backups/` containing:

```toml
include = ["myfolder/**"]
```

This tells the scanner: within `backups/`, only index files matching
`myfolder/**`. No `.noindex` file is needed.

## Design Decisions

**`.index` only, not `[scan]` in client.toml.**
Adding `include` to the global `[scan]` section would conflict with
source-level `include` (different roots, no way to reconcile). Per-directory
`.index` files are naturally scoped to their subtree, so include patterns are
unambiguous and relative to the directory containing the file.

**Source-level `include` and `.index` include compose by intersection.**
A file must satisfy both to be indexed. In practice the source-level include
defaults to `["**"]` (everything), so `.index` include is the effective filter.

**`include` is replacement, not additive.**
Unlike `exclude` (which appends to the parent list), `include` replaces —
because its purpose is to define the complete allowed set for this subtree.
A child `.index` can narrow the parent's include further via intersection.

**`.noindex` and `.index` are mutually exclusive.**
If both are present in the same directory, `.noindex` wins (current behaviour,
no change). Users should use `.index` with `include` instead of `.noindex` when
they need fine-grained control.

**Patterns are relative to the directory containing the `.index` file.**
`include = ["myfolder/**"]` in `backups/.index` matches `backups/myfolder/**`
relative to the source root.

## Implementation

### 1. `crates/common/src/config.rs`

- Add `include: Option<Vec<String>>` to `ScanOverride`.
- In `apply_override`: if `ov.include` is `Some`, replace `result.include` with
  the new patterns (adjusted to be relative to the source root by prepending
  the directory's relative path prefix).

  Actually — the simpler approach: store the raw patterns on `ScanConfig` and
  apply them at the file-matching site, where the directory prefix is known.

  **Revised**: `ScanConfig` grows an `include_override: Option<Vec<String>>`
  field that, when `Some`, restricts indexing to only matching files within the
  current effective scan scope. The scan loop checks this alongside source-level
  `include`.

### 2. `crates/client/src/scan.rs`

- In `resolve_effective_scan` (the per-directory config cache), when a `.index`
  file sets `include`, record both the patterns and the directory-relative prefix
  so patterns can be matched against source-relative paths.
- In the file collection loop, after the existing source-level include check,
  additionally check the `.index` include if present.

### 3. `crates/client/src/watch.rs`

- `resolve_watch_config` already applies `.index` overrides; extend it to also
  apply the include filter when checking whether a changed file should be indexed.

## Files Changed

- `crates/common/src/config.rs` — `ScanOverride` + `ScanConfig` + `apply_override`
- `crates/client/src/scan.rs` — include check in walk loop
- `crates/client/src/watch.rs` — include check in watch event handler

## Testing

- Unit tests in `config.rs` for `ScanOverride` round-trip with `include`.
- Unit tests in `config.rs` for `apply_override` with include (replacement, not additive).
- Integration-style tests in `scan.rs` or a dedicated test module with an
  in-memory directory tree verifying that `.index` include restricts what is collected.

## Breaking Changes

None. `include` in `.index` is a new optional field; existing `.index` files
without it behave identically to today.
