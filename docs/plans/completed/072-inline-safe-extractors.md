# Inline Safe Extractors Implementation Plan

> **For agentic workers:** REQUIRED: Use superpowers:subagent-driven-development (if subagents available) or superpowers:executing-plans to implement this plan. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Eliminate subprocess overhead for safe extractors (text, HTML, media, office) in find-scan by calling their library functions in-process; find-watch inlines text only.

**Architecture:** Replace the two-step `ExtractorChoice + extractor_binary_for()` system with a single `resolve_extractor()` that returns a unified `ExtractorRoute` enum. The caller passes an `inline_set` slice to control which extractors are inlined vs. subprocess. `process_file()` in scan.rs and the equivalent in watch.rs match on the route enum.

**Tech Stack:** Rust, tokio, anyhow — `find_extract_html::extract`, `find_extract_media::extract`, `find_extract_office::extract`, `find_extract_dispatch::dispatch_from_path` (all already in-tree as library crates).

**Spec:** `docs/superpowers/specs/2026-03-16-inline-safe-extractors-design.md`

---

## File Map

| File | Change |
|------|--------|
| `crates/client/src/subprocess.rs` | Add `ExtractorRoute`, `InlineKind`; add `extract_inline()`; extend `resolve_extractor()`; delete `extractor_binary_for()`; update `extract_via_subprocess` and `start_archive_subprocess` signatures |
| `crates/client/src/scan.rs` | Update `process_file()` to match on `ExtractorRoute`; pass `inline_set` |
| `crates/client/src/watch.rs` | Update `handle_update()` to match on `ExtractorRoute`; pass text-only `inline_set` |
| `crates/client/Cargo.toml` | Add `find-extract-html` and `find-extract-office` deps |

---

## Chunk 1: New Types, Resolver, and Inline Dispatch

### Task 1: Add `ExtractorRoute` and `InlineKind` types

**Files:**
- Modify: `crates/client/src/subprocess.rs` (around line 44, replacing `ExtractorChoice`)

- [ ] **Step 1: Replace `ExtractorChoice` with `ExtractorRoute` and add `InlineKind`**

  In `subprocess.rs`, replace the existing `ExtractorChoice` enum (lines 46–51) with:

  ```rust
  /// Which extractor to use for a given file.
  #[derive(Debug)]
  #[allow(dead_code)] // intentional: find-client compiles multiple binaries; not all use every export
  pub enum ExtractorRoute {
      /// Call the extractor library directly in-process.
      Inline(InlineKind),
      /// Spawn the archive subprocess (streaming MPSC path).
      Archive,
      /// Spawn a non-archive extractor subprocess; contains the resolved binary path.
      Subprocess(String),
      /// Use a user-configured external tool.
      External(ExternalExtractorConfig),
  }

  /// Identifies which in-process extractor library to call.
  #[derive(PartialEq, Debug)]
  pub enum InlineKind {
      /// Text/code files — routed through find_extract_dispatch::dispatch_from_path.
      Text,
      Html,
      Media,
      Office,
  }
  ```

  Keep the existing `SubprocessOutcome` and `ExternalOutcome` enums unchanged.

- [ ] **Step 2: Verify it compiles (with expected errors on old call sites)**

  ```bash
  cargo check -p find-client 2>&1 | head -40
  ```

  Expected: errors about `ExtractorChoice` not found in `scan.rs` and `watch.rs` — that is fine and expected. No errors inside `subprocess.rs` itself yet.

---

### Task 2: Rewrite `resolve_extractor` and delete `extractor_binary_for`

**Files:**
- Modify: `crates/client/src/subprocess.rs` (lines 53–69 and 784–826)

- [ ] **Step 1: Write the new `resolve_extractor` tests first**

  Add to the `#[cfg(test)]` block at the bottom of `subprocess.rs`:

  ```rust
  #[test]
  fn route_html_with_html_in_inline_set_returns_inline() {
      use find_common::config::ScanConfig;
      let scan = ScanConfig::default();
      let path = std::path::Path::new("page.html");
      let route = super::resolve_extractor(path, &scan, &None, &[super::InlineKind::Html]);
      assert!(matches!(route, super::ExtractorRoute::Inline(super::InlineKind::Html)));
  }

  #[test]
  fn route_html_without_html_in_inline_set_returns_subprocess() {
      use find_common::config::ScanConfig;
      let scan = ScanConfig::default();
      let path = std::path::Path::new("page.html");
      let route = super::resolve_extractor(path, &scan, &None, &[]);
      assert!(matches!(route, super::ExtractorRoute::Subprocess(_)));
  }

  #[test]
  fn route_pdf_always_subprocess() {
      use find_common::config::ScanConfig;
      let scan = ScanConfig::default();
      let path = std::path::Path::new("doc.pdf");
      // Even if someone passes all InlineKinds, PDF must remain subprocess.
      let all = &[super::InlineKind::Text, super::InlineKind::Html,
                  super::InlineKind::Media, super::InlineKind::Office];
      let route = super::resolve_extractor(path, &scan, &None, all);
      assert!(matches!(route, super::ExtractorRoute::Subprocess(_)));
  }

  #[test]
  fn route_zip_always_archive() {
      use find_common::config::ScanConfig;
      let scan = ScanConfig::default();
      let path = std::path::Path::new("archive.zip");
      let route = super::resolve_extractor(path, &scan, &None, &[]);
      assert!(matches!(route, super::ExtractorRoute::Archive));
  }

  #[test]
  fn route_external_entry_still_returned() {
      use find_common::config::{ExternalExtractorConfig, ExternalExtractorMode, ExtractorEntry, ScanConfig};
      let mut scan = ScanConfig::default();
      scan.extractors.insert(
          "nd1".to_string(),
          ExtractorEntry::External(ExternalExtractorConfig {
              mode: ExternalExtractorMode::TempDir,
              bin: "/usr/bin/extract-nd1".to_string(),
              args: vec!["{file}".to_string(), "{dir}".to_string()],
          }),
      );
      let path = std::path::Path::new("file.nd1");
      let route = super::resolve_extractor(path, &scan, &None, &[]);
      assert!(matches!(route, super::ExtractorRoute::External(_)));
  }

  #[test]
  fn route_media_inline_set_respected() {
      use find_common::config::ScanConfig;
      let scan = ScanConfig::default();
      let path = std::path::Path::new("photo.jpg");
      let route_inline = super::resolve_extractor(path, &scan, &None, &[super::InlineKind::Media]);
      let route_sub    = super::resolve_extractor(path, &scan, &None, &[]);
      assert!(matches!(route_inline, super::ExtractorRoute::Inline(super::InlineKind::Media)));
      assert!(matches!(route_sub, super::ExtractorRoute::Subprocess(_)));
  }

  #[test]
  fn route_unknown_extension_is_dispatch_subprocess() {
      use find_common::config::ScanConfig;
      let scan = ScanConfig::default();
      let path = std::path::Path::new("file.xyz"); // unknown extension, not used elsewhere
      let route = super::resolve_extractor(path, &scan, &None, &[]);
      match &route {
          super::ExtractorRoute::Subprocess(bin) => {
              assert!(bin.contains("find-extract-dispatch"), "unexpected binary: {bin}");
          }
          _ => panic!("expected Subprocess, got different variant"),
      }
  }
  ```

- [ ] **Step 2: Run tests — expect compile failure because `resolve_extractor` still has old signature**

  ```bash
  cargo test -p find-client resolve_extractor 2>&1 | head -30
  ```

  Expected: compile errors (old signature doesn't match new call sites in tests).

- [ ] **Step 3: Implement the new `resolve_extractor` and private `resolve_binary` helper**

  Replace the existing `resolve_extractor` function (lines 53–69) and `extractor_binary_for` function (lines 784–826) with:

  ```rust
  /// Resolve the binary path for a named extractor binary.
  /// Search order: configured extractor_dir → same dir as current exe → PATH.
  fn resolve_binary(name: &str, extractor_dir: &Option<String>) -> String {
      if let Some(dir) = extractor_dir {
          return format!("{}/{}", dir, name);
      }
      if let Ok(exe) = std::env::current_exe() {
          if let Some(dir) = exe.parent() {
              let candidate = dir.join(name);
              if candidate.exists() {
                  return candidate.to_string_lossy().to_string();
              }
          }
      }
      name.to_string()
  }

  /// Resolve the extractor route for a given file path.
  ///
  /// Resolution order:
  /// 1. User-configured `scan.extractors` entry → `External` (unless overridden to builtin)
  /// 2. Archive extensions → `Archive` (always subprocess regardless of inline_set)
  /// 3. PDF → `Subprocess("find-extract-pdf")` (always subprocess)
  /// 4. Extension matches an inline-eligible type and kind is in inline_set → `Inline(kind)`
  /// 5. Extension matches an inline-eligible type but kind not in inline_set → `Subprocess(binary)`
  /// 6. Everything else → `Subprocess("find-extract-dispatch")`
  #[allow(dead_code)] // used by find-scan; other binaries share this module
  pub fn resolve_extractor(
      path: &Path,
      scan: &ScanConfig,
      extractor_dir: &Option<String>,
      inline_set: &[InlineKind],
  ) -> ExtractorRoute {
      let ext = path
          .extension()
          .and_then(|e| e.to_str())
          .unwrap_or("")
          .to_lowercase();

      // 1. User-configured extractor override.
      if let Some(entry) = scan.extractors.get(&ext) {
          match entry {
              ExtractorEntry::Builtin(_) => {} // fall through to built-in routing
              ExtractorEntry::External(cfg) => return ExtractorRoute::External(cfg.clone()),
          }
      }

      // 2. Archive — always subprocess (streaming MPSC path is bespoke).
      if find_extract_archive::is_archive_ext(&ext) {
          return ExtractorRoute::Archive;
      }

      // 3. PDF — always subprocess (fork can panic on malformed data).
      if ext == "pdf" {
          return ExtractorRoute::Subprocess(resolve_binary("find-extract-pdf", extractor_dir));
      }

      // 4 & 5. Inline-eligible types — honour inline_set.
      let inline_kind: Option<InlineKind> = match ext.as_str() {
          "html" | "htm" | "xhtml" => Some(InlineKind::Html),
          "jpg" | "jpeg" | "png" | "gif" | "bmp" | "ico" | "webp" | "heic"
          | "tiff" | "tif" | "raw" | "cr2" | "nef" | "arw"
          | "mp3" | "flac" | "ogg" | "m4a" | "aac" | "wav" | "wma" | "opus"
          | "mp4" | "mkv" | "avi" | "mov" | "wmv" | "webm" | "m4v" | "flv" => Some(InlineKind::Media),
          "docx" | "xlsx" | "xls" | "xlsm" | "pptx" => Some(InlineKind::Office),
          _ => None,
      };

      if let Some(kind) = inline_kind {
          let binary = match &kind {
              InlineKind::Html   => "find-extract-html",
              InlineKind::Media  => "find-extract-media",
              InlineKind::Office => "find-extract-office",
              InlineKind::Text   => "find-extract-dispatch",
          };
          if inline_set.contains(&kind) {
              return ExtractorRoute::Inline(kind);
          } else {
              return ExtractorRoute::Subprocess(resolve_binary(binary, extractor_dir));
          }
      }

      // 5b. Specialist subprocess types (not inline-eligible) — route to their dedicated binary.
      // epub must come before the dispatch fallthrough to preserve its dedicated extractor.
      if ext == "epub" {
          return ExtractorRoute::Subprocess(resolve_binary("find-extract-epub", extractor_dir));
      }

      // 6. Text/code and everything else — dispatch (inline if Text is in inline_set).
      if inline_set.contains(&InlineKind::Text) {
          ExtractorRoute::Inline(InlineKind::Text)
      } else {
          ExtractorRoute::Subprocess(resolve_binary("find-extract-dispatch", extractor_dir))
      }
  }
  ```

  Also update `extract_via_subprocess` to take the already-resolved binary string instead of calling `extractor_binary_for` internally. Change its signature from:

  ```rust
  pub async fn extract_via_subprocess(
      abs_path: &Path,
      scan: &ScanConfig,
      extractor_dir: &Option<String>,
  ) -> SubprocessOutcome
  ```

  to:

  ```rust
  pub async fn extract_via_subprocess(
      abs_path: &Path,
      scan: &ScanConfig,
      binary: &str,
  ) -> SubprocessOutcome
  ```

  **Exactly one line changes inside `extract_via_subprocess`:** remove line 288 (`let binary = extractor_binary_for(abs_path, extractor_dir);`). Everything else in the function body stays unchanged — the `is_archive` and `is_pdf` variables, their declarations, and their use in `cmd.arg(...)` are all kept as-is. The `binary` variable that was computed by `extractor_binary_for` is now the `binary: &str` parameter instead.

  **Dead-code note:** After the routing refactor, `extract_via_subprocess` will never be called with an archive path (archives route to `ExtractorRoute::Archive`), so the `is_archive` branch inside the function becomes dead code. Leave it in place for now — it is harmless and will be naturally removed in a future cleanup pass if desired. Do not remove it here as it is not part of this change's scope.

  Similarly update `start_archive_subprocess` to take a `binary: &str` parameter instead of `extractor_dir: &Option<String>`. Its callers will pass `resolve_binary("find-extract-archive", extractor_dir)`.

  **Note:** `start_archive_subprocess` is a `pub fn` called from `scan.rs`. Its signature change must be reflected there in Task 4.

- [ ] **Step 4: Run the new resolver tests**

  ```bash
  cargo test -p find-client route_ 2>&1
  ```

  Expected: all new `route_*` tests pass; old `resolve_extractor_*` tests may fail (they use the old `ExtractorChoice` enum) — delete the three old tests (`resolve_extractor_builtin_sentinel_falls_through`, `resolve_extractor_unknown_extension_is_builtin`, `resolve_extractor_external_entry_returned`) as they are superseded by the new ones.

- [ ] **Step 5: Run all find-client tests (ignoring scan.rs / watch.rs compile errors for now)**

  ```bash
  cargo test -p find-client --lib 2>&1 | head -50
  ```

  Expected: only compile errors in `scan.rs` and `watch.rs` call sites. No failures in subprocess.rs itself.

---

### Task 3: Add `extract_inline` and new Cargo deps

**Files:**
- Modify: `crates/client/Cargo.toml`
- Modify: `crates/client/src/subprocess.rs`

- [ ] **Step 1: Add new Cargo dependencies**

  In `crates/client/Cargo.toml`, after the existing `find-extract-media` line, add:

  ```toml
  find-extract-html      = { path = "../extractors/html" }
  find-extract-office    = { path = "../extractors/office" }
  ```

- [ ] **Step 2: Add import at top of `subprocess.rs`**

  In the `use find_extract_archive::MemberBatch;` section, add:

  ```rust
  use find_extract_dispatch::dispatch_from_path;
  ```

  (The existing `use find_extract_dispatch::dispatch_from_bytes;` stays as-is.)

- [ ] **Step 3: Write the test for `extract_inline`**

  Add to the `#[cfg(test)]` block in `subprocess.rs`:

  ```rust
  #[test]
  fn extract_inline_text_returns_lines() {
      use find_common::config::{ExtractorConfig, ScanConfig};
      let cfg = find_common::config::extractor_config_from_scan(&ScanConfig::default());
      // Use the Cargo manifest directory to find a known text file.
      let manifest = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
      let path = manifest.join("src/subprocess.rs"); // large file, always non-empty
      let lines = super::extract_inline(super::InlineKind::Text, &path, &cfg);
      assert!(!lines.is_empty(), "expected text lines from subprocess.rs");
  }

  #[test]
  fn extract_inline_html_returns_lines() {
      use find_common::config::ScanConfig;
      let cfg = find_common::config::extractor_config_from_scan(&ScanConfig::default());
      let manifest = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
      let fixture = manifest.join("tests/fixtures");
      // Look for any .html fixture; skip test if none found.
      let html_file = std::fs::read_dir(&fixture).ok()
          .and_then(|mut d| d.find(|e| {
              e.as_ref().ok()
                  .and_then(|e| e.path().extension().map(|x| x == "html"))
                  .unwrap_or(false)
          }))
          .and_then(|e| e.ok())
          .map(|e| e.path());
      if let Some(path) = html_file {
          let lines = super::extract_inline(super::InlineKind::Html, &path, &cfg);
          assert!(!lines.is_empty(), "expected html lines");
      }
      // No HTML fixture → pass silently (not a regression).
  }
  ```

- [ ] **Step 4: Run test to verify it fails (function doesn't exist yet)**

  ```bash
  cargo test -p find-client extract_inline 2>&1 | head -20
  ```

  Expected: compile error — `extract_inline` not found.

- [ ] **Step 5: Implement `extract_inline`**

  Add to `subprocess.rs` (after `resolve_extractor`):

  ```rust
  /// Call an extractor library in-process without spawning a subprocess.
  ///
  /// On error, logs a warning and returns an empty vec (same semantics as a
  /// subprocess `Failed` outcome: the file will be indexed by filename only).
  ///
  /// `extract_inline` is synchronous. When called from an async context it
  /// will block the Tokio executor thread; this is an accepted trade-off for
  /// this change — `spawn_blocking` wrapping is out of scope.
  #[allow(dead_code)] // used by find-scan; other binaries share this module
  pub fn extract_inline(kind: InlineKind, path: &Path, cfg: &ExtractorConfig) -> Vec<IndexLine> {
      let result = match kind {
          InlineKind::Text => dispatch_from_path(path, cfg),
          InlineKind::Html => find_extract_html::extract(path, cfg),
          InlineKind::Media => find_extract_media::extract(path, cfg),
          InlineKind::Office => find_extract_office::extract(path, cfg),
      };
      match result {
          Ok(lines) => lines,
          Err(e) => {
              warn!("inline extraction failed for {}: {e:#}", path.display());
              vec![]
          }
      }
  }
  ```

  **Do NOT add a new `use find_common::config::ExtractorConfig;` import** — it is already present on line 17 of `subprocess.rs` in the existing `use find_common::config::{..., ExtractorConfig, ...};` block. Adding it again will produce a duplicate import error.

- [ ] **Step 6: Run `extract_inline` tests**

  ```bash
  cargo test -p find-client extract_inline 2>&1
  ```

  Expected: `extract_inline_text_returns_lines` passes. HTML test passes or is silently skipped.

  > **⚠️ Do NOT commit yet.** `scan.rs` and `watch.rs` still reference `ExtractorChoice::Builtin` which no longer exists. The workspace won't compile (and `mise run clippy` would fail) until both files are updated in Tasks 4 and 5. The commit happens at the end of Task 5.

---

## Chunk 2: Wire Up scan.rs and watch.rs

### Task 4: Update `scan.rs` dispatch

**Files:**
- Modify: `crates/client/src/scan.rs`

The current `process_file()` at line 479 dispatches on `subprocess::ExtractorChoice`. It needs updating to dispatch on `subprocess::ExtractorRoute`.

Key changes:
- Call site: `subprocess::resolve_extractor(abs_path, &eff_scan)` → `subprocess::resolve_extractor(abs_path, &eff_scan, &eff_scan.extractor_dir, SCAN_INLINE_SET)`
- Add a constant at the top of `scan.rs` (or near `process_file`):
  ```rust
  const SCAN_INLINE_SET: &[subprocess::InlineKind] = &[
      subprocess::InlineKind::Text,
      subprocess::InlineKind::Html,
      subprocess::InlineKind::Media,
      subprocess::InlineKind::Office,
  ];
  ```
- The archive subprocess call: `subprocess::start_archive_subprocess(abs_path.to_path_buf(), &eff_scan, &eff_scan.extractor_dir)` → pass the resolved archive binary:
  ```rust
  use find_common::config::ScanConfig; // already imported
  let archive_bin = subprocess::resolve_binary_for_archive(&eff_scan.extractor_dir);
  subprocess::start_archive_subprocess(abs_path.to_path_buf(), &eff_scan, &archive_bin)
  ```
  (Add a `pub fn resolve_binary_for_archive(extractor_dir: &Option<String>) -> String` in subprocess.rs that calls `resolve_binary("find-extract-archive", extractor_dir)` — this avoids exposing the private `resolve_binary`.)

- [ ] **Step 1: Add `resolve_binary_for_archive` to `subprocess.rs`**

  ```rust
  /// Resolve the path to the find-extract-archive binary.
  #[allow(dead_code)]
  pub fn resolve_binary_for_archive(extractor_dir: &Option<String>) -> String {
      resolve_binary("find-extract-archive", extractor_dir)
  }
  ```

- [ ] **Step 2: Update the match in `process_file()`**

  Replace the block starting at `match subprocess::resolve_extractor(abs_path, &eff_scan) {` through the closing `}` (lines 479–745) with:

  ```rust
  match subprocess::resolve_extractor(abs_path, &eff_scan, &eff_scan.extractor_dir, SCAN_INLINE_SET) {
      subprocess::ExtractorRoute::External(ref ext_cfg) => {
          // ... existing External arm unchanged ...
      }
      subprocess::ExtractorRoute::Archive => {
          // ... existing archive arm unchanged, but update the start_archive_subprocess call:
          // OLD: subprocess::start_archive_subprocess(abs_path.to_path_buf(), &eff_scan, &eff_scan.extractor_dir)
          // NEW: subprocess::start_archive_subprocess(abs_path.to_path_buf(), &eff_scan, &subprocess::resolve_binary_for_archive(&eff_scan.extractor_dir))
      }
      subprocess::ExtractorRoute::Subprocess(ref binary) => {
          // Existing non-archive subprocess arm (was `ExtractorChoice::Builtin` + not archive),
          // but update the extract_via_subprocess call:
          // OLD: subprocess::extract_via_subprocess(abs_path, &eff_scan, &eff_scan.extractor_dir).await
          // NEW: subprocess::extract_via_subprocess(abs_path, &eff_scan, binary).await
          let t0 = std::time::Instant::now();
          if ctx.quiet { lazy_header::set_pending(&abs_path.to_string_lossy()); }
          let outcome = subprocess::extract_via_subprocess(abs_path, &eff_scan, binary).await;
          if ctx.quiet { lazy_header::clear_pending(); }

          let lines = match outcome {
              subprocess::SubprocessOutcome::Ok(lines) => lines,
              subprocess::SubprocessOutcome::BinaryMissing => {
                  warn!("skipping {rel_path}: extractor binary not found (file will be retried once the binary is installed)");
                  return Ok(false);
              }
              subprocess::SubprocessOutcome::Failed => {
                  if eff_scan.server_fallback {
                      if let Err(e) = upload::upload_file(ctx.api, abs_path, rel_path, mtime, ctx.source_name).await {
                          warn!("server fallback upload failed for {rel_path}: {e:#}");
                      } else {
                          return Ok(true);
                      }
                  }
                  vec![]
              }
          };

          let extract_ms = t0.elapsed().as_millis() as u64;
          push_non_archive_files(ctx, rel_path, abs_path, mtime, size, kind, lines, extract_ms, is_new).await?;
      }
      subprocess::ExtractorRoute::Inline(inline_kind) => {
          // `inline_kind` is the InlineKind enum variant (bound here to avoid shadowing
          // the outer `kind: String` computed from detect_kind on line 473).
          let t0 = std::time::Instant::now();
          if ctx.quiet { lazy_header::set_pending(&abs_path.to_string_lossy()); }
          let ext_config = extractor_config_from_scan(&eff_scan);
          let lines = subprocess::extract_inline(inline_kind, abs_path, &ext_config);
          if ctx.quiet { lazy_header::clear_pending(); }

          let extract_ms = t0.elapsed().as_millis() as u64;
          // `kind` here is the outer String variable from line 473, not the InlineKind.
          push_non_archive_files(ctx, rel_path, abs_path, mtime, size, kind, lines, extract_ms, is_new).await?;
      }
  }
  ```

- [ ] **Step 3: Compile check**

  ```bash
  cargo check -p find-client 2>&1 | head -40
  ```

  Expected: only `watch.rs` errors remain. `scan.rs` should be clean.

- [ ] **Step 4: Run the full find-client test suite**

  ```bash
  cargo test -p find-client 2>&1 | tail -20
  ```

  Expected: all existing tests pass.

---

### Task 5: Update `watch.rs` dispatch

**Files:**
- Modify: `crates/client/src/watch.rs`

The `handle_update` function at lines 481–506 does the same dispatch pattern. Update it to use `ExtractorRoute` with text-only inlining.

- [ ] **Step 1: Add the watch inline set constant near `handle_update`**

  Add near the top of the function or as a module-level constant:

  ```rust
  const WATCH_INLINE_SET: &[subprocess::InlineKind] = &[subprocess::InlineKind::Text];
  ```

- [ ] **Step 2: Update the match in `handle_update`**

  Replace the block at lines 481–506:

  ```rust
  let lines = match subprocess::resolve_extractor(abs_path, eff_scan, extractor_dir, WATCH_INLINE_SET) {
      subprocess::ExtractorRoute::External(ref ext_cfg) => match ext_cfg.mode {
          ExternalExtractorMode::Stdout => {
              match subprocess::run_external_stdout(abs_path, ext_cfg, eff_scan).await {
                  subprocess::ExternalOutcome::Ok(lines) => lines,
                  subprocess::ExternalOutcome::BinaryMissing => return Ok(()),
                  subprocess::ExternalOutcome::Failed(_) => vec![],
              }
          }
          ExternalExtractorMode::TempDir => {
              let ext_config = extractor_config_from_scan(eff_scan);
              match subprocess::run_external_tempdir(abs_path, ext_cfg, eff_scan, &ext_config).await {
                  subprocess::ExternalOutcome::Ok(lines) => lines,
                  subprocess::ExternalOutcome::BinaryMissing => return Ok(()),
                  subprocess::ExternalOutcome::Failed(_) => vec![],
              }
          }
      },
      subprocess::ExtractorRoute::Archive => {
          // watch.rs does not support archive streaming — treat as subprocess dispatch.
          match subprocess::extract_via_subprocess(
              abs_path, eff_scan,
              &subprocess::resolve_binary_for_archive(extractor_dir),
          ).await {
              subprocess::SubprocessOutcome::Ok(lines) => lines,
              subprocess::SubprocessOutcome::BinaryMissing => return Ok(()),
              subprocess::SubprocessOutcome::Failed => vec![],
          }
      }
      subprocess::ExtractorRoute::Subprocess(ref binary) => {
          match subprocess::extract_via_subprocess(abs_path, eff_scan, binary).await {
              subprocess::SubprocessOutcome::Ok(lines) => lines,
              subprocess::SubprocessOutcome::BinaryMissing => return Ok(()),
              subprocess::SubprocessOutcome::Failed => vec![],
          }
      }
      subprocess::ExtractorRoute::Inline(kind) => {
          let ext_config = extractor_config_from_scan(eff_scan);
          subprocess::extract_inline(kind, abs_path, &ext_config)
      }
  };
  ```

  **Note on Archive arm in watch.rs:** find-watch processes real-time file events and does not use the streaming MPSC archive path. Archives detected in watch.rs were previously routed through `extract_via_subprocess` (which handled them the same way as other subprocesses — no streaming). The `Archive` arm above preserves this by using `extract_via_subprocess` with the archive binary, so behaviour is unchanged.

- [ ] **Step 3: Full compile + clippy**

  ```bash
  cargo check -p find-client 2>&1
  mise run clippy 2>&1 | tail -20
  ```

  Expected: no errors or warnings.

- [ ] **Step 4: Run full test suite**

  ```bash
  cargo test -p find-client 2>&1 | tail -20
  ```

  Expected: all tests pass.

- [ ] **Step 5: Commit all Chunk 1 + Chunk 2 changes together**

  The workspace now compiles. Commit everything at once:

  ```bash
  git add crates/client/src/subprocess.rs crates/client/src/scan.rs crates/client/src/watch.rs crates/client/Cargo.toml
  git commit -m "feat: inline safe extractors in find-scan (text, HTML, media, office)"
  ```

---

### Task 6: Final verification and CHANGELOG

- [ ] **Step 1: Run clippy (matches CI)**

  ```bash
  mise run clippy 2>&1
  ```

  Expected: clean (`0 warnings`). Fix any warnings before proceeding.

- [ ] **Step 2: Run all workspace tests**

  ```bash
  cargo test --workspace 2>&1 | tail -30
  ```

  Expected: all pass.

- [ ] **Step 3: Binary size sanity check**

  ```bash
  cargo build --release -p find-client 2>/dev/null
  ls -lh target/release/find-scan target/release/find-watch
  ```

  Informational only — note the sizes. find-scan will grow slightly (html + office libs now linked); find-watch growth will be minimal (text path was already linked via dispatch).

- [ ] **Step 4: Update CHANGELOG**

  Add to the `[Unreleased]` section of `CHANGELOG.md`:

  ```markdown
  - Inline safe extractors in find-scan: text, HTML, media, and office files are now
    extracted in-process rather than via subprocess, eliminating IPC overhead for the
    most common file types. find-watch inlines text only (memory footprint concern).
  ```

- [ ] **Step 5: Final commit**

  ```bash
  git add CHANGELOG.md
  git commit -m "chore: update CHANGELOG for inline extractors"
  ```
