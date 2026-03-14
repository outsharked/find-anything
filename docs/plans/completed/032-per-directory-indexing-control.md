# Plan 032: Per-Directory Indexing Control (.noindex / .index)

## Context

Users need a way to opt directories out of indexing without editing the central config file
(`.noindex` marker) and to tune scan behaviour per-subtree without central coordination
(`.index` override file). The `.noindex` use case covers large scratch dirs, private notes,
or build outputs that vary per-project. The `.index` use case covers directories where a
different file size limit, hidden-file policy, or archive depth makes sense.

Both marker filenames are configurable at the top level so operators can use organisation-
specific conventions (e.g. `.nofind`, `.findconfig`).

---

## Design Decisions

### `.noindex` — skip the directory and all descendants

When the scanner enters a directory and finds a file named `scan.noindex_file` (default
`.noindex`), it returns immediately without descending or indexing anything in that subtree.
The check happens **before** the hidden-file filter so that `.noindex` itself (which starts
with `.`) is always detected even when `include_hidden = false`.

### `.index` — override `[scan]` parameters for a subtree

A TOML file with the same field names as the `[scan]` section of `client.toml`, but all
fields optional. Present in any directory, it overrides scan config for that directory and
all descendants (until a deeper `.index` or `.noindex` is encountered).

**Merge semantics:**
- `exclude` is **additive** — patterns are appended to the parent config's list; you can
  never un-exclude something the parent already excluded.
- All other fields are **replacement** — the innermost `.index` value wins.
- Multiple levels compose: global → parent override → child override, applied in order.

**Example `.index` file:**
```toml
# Index hidden files in this directory
include_hidden = true

# Skip any temporary output files
exclude = ["*.tmp", "**/out/**"]

[archives]
enabled = false
```

### Fields overridable in `.index`

| Field | Notes |
|-------|-------|
| `exclude` | additive — appended to parent list |
| `max_file_size_mb` | replacement |
| `include_hidden` | replacement |
| `follow_symlinks` | replacement |
| `archives.enabled` | replacement |
| `archives.max_depth` | replacement |
| `max_line_length` | replacement |

Not overridable: `noindex_file`, `index_file` (global names), `archives.max_temp_file_mb`
(internal tuning, not useful per-dir).

### The control files are never indexed themselves

`noindex_file` and `index_file` entries are filtered out of the file map entirely —
they are control files, not content.

---

## Implementation

### Step 1 — `find-common/src/config.rs`

Added to `ScanConfig`:
```rust
#[serde(default = "default_noindex_file")]
pub noindex_file: String,   // default: ".noindex"

#[serde(default = "default_index_file")]
pub index_file: String,     // default: ".index"
```

Added new structs:
```rust
pub struct ScanOverride {
    pub exclude: Option<Vec<String>>,
    pub max_file_size_mb: Option<u64>,
    pub include_hidden: Option<bool>,
    pub follow_symlinks: Option<bool>,
    pub archives: Option<ArchiveOverride>,
    pub max_line_length: Option<usize>,
}

pub struct ArchiveOverride {
    pub enabled: Option<bool>,
    pub max_depth: Option<usize>,
}
```

Added `ScanConfig::apply_override` (exclude additive, others replace) and
`load_dir_override(dir, index_filename)` free function.

### Step 2 — `find-client/src/scan.rs`

**Modified `walk_paths`** `filter_entry`:
1. Control files (`noindex_file`, `index_file`) → return false immediately.
2. For directories: check if the directory contains `noindex_file` → return false (skips
   entire subtree, runs before the hidden-file filter).
3. Hidden-file and exclude-glob checks unchanged.

**Added `resolve_effective_scan`**: walks ancestors from root → file's directory, applying
`.index` overrides. Caches effective `ScanConfig` per directory — each directory's
`.index` is parsed at most once per scan.

**Modified extraction loop**: per-file config resolution via `resolve_effective_scan`;
per-directory `GlobSet` cache to avoid rebuilding identical globs for every file in the
same directory. Archive member exclusion uses the effective GlobSet.

### Step 3 — `find-client/src/watch.rs`

**Added `resolve_watch_config`**: walks ancestors from source root → file's parent,
checking for `.noindex` (returns skip=true if found) and applying `.index` overrides.
No caching (events are infrequent).

**Modified event processing loop**: calls `resolve_watch_config` per event; skips events
from `.noindex` subtrees; uses effective `ScanConfig` for exclusion checks and passes it
to `extract_via_subprocess`.

**Updated `find_source`**: now returns `(source_name, rel_path, source_root)` so the
source root is available for `resolve_watch_config` without a second map scan.

**Updated `handle_update` / `extract_via_subprocess`**: accept `&ScanConfig` directly
instead of `&ClientConfig` so the effective (potentially overridden) config is used.

---

## Files Changed

| File | Change |
|------|--------|
| `crates/common/src/config.rs` | Add `noindex_file`/`index_file` to `ScanConfig`; add `ScanOverride`, `ArchiveOverride`; add `ScanConfig::apply_override`; add `load_dir_override` |
| `crates/client/src/scan.rs` | Augment `filter_entry` for `.noindex` + control-file exclusion; add `resolve_effective_scan`; use per-file config in extraction loop |
| `crates/client/src/watch.rs` | Add `resolve_watch_config`; use it per event for skip-check and effective config; update `find_source`, `handle_update`, `extract_via_subprocess` signatures |

No new crates, no new binaries, no server changes, no UI changes.

---

## Verification

1. `cargo test --workspace` passes.
2. `mise run clippy` clean.
3. Manual test — create a directory structure:
   ```
   test-source/
     normal/file.txt
     skip-me/.noindex
     skip-me/secret.txt
     override-me/.index   (exclude = ["*.log"], include_hidden = true)
     override-me/.hidden-file
     override-me/data.log
     override-me/data.txt
   ```
   Run `find-scan`. Verify:
   - `skip-me/secret.txt` is **not** indexed
   - `override-me/.hidden-file` **is** indexed (include_hidden override)
   - `override-me/data.log` is **not** indexed (exclude override)
   - `override-me/data.txt` **is** indexed
   - `normal/file.txt` **is** indexed
   - `.noindex` and `.index` control files do **not** appear in results
4. Ctrl+P search: none of the skipped paths appear.
5. `find-watch`: modify `skip-me/secret.txt` — verify the event is silently dropped.
6. `find-watch`: modify `override-me/data.txt` — verify it is indexed correctly.
7. Update `scan.noindex_file = ".nofind"` in `client.toml`, rename `.noindex` to `.nofind` — verify same skip behaviour.
