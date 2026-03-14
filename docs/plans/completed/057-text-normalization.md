# Text Normalization Subsystem

## Overview

Text content in the index can contain arbitrarily long lines — a minified JS
file is one line, a large JSON blob may be one line, a log file with embedded
base64 data may have 500 KB lines. This breaks line-count-based pagination
(plan 056), wastes bandwidth rendering content that is illegible anyway, and
can stall or crash the browser.

This plan introduces a server-side normalization step applied to all text
content **before it is written to ZIP archives**. It ensures every line stored
in the index is bounded in length. For formats that have a canonical pretty-
print (JSON, TOML, etc.) it also reformats the content so that minified files
become readable.

As a consequence of this plan, plan 056 can rely on line count as a safe
pagination unit and the "view original" toggle can be simplified.

---

## Design Decisions

### Server-side, not client-side

Normalization is applied in the **inbox worker**, as the final transformation
before `chunk_lines()` writes content to ZIP archives. This means:

- All content — from `find-scan` batches, from server-side archive extraction,
  from any future ingestion path — passes through the same normalizer.
- Normalization can be improved or reconfigured without re-deploying clients.
- Old clients still produce correctly-normalized output on re-index.

The cost is that unnormalized content traverses the network from client to
server. This is acceptable: the bulk upload is already gzip-compressed, and
the content size limit (100 MB per file) already guards against truly enormous
uploads.

### No re-index of existing content

Normalization applies to new or re-indexed files only. Existing content in ZIP
archives is not retroactively transformed. Users who want normalized content
for old files can run `find-scan --upgrade`. A note in the changelog will
explain this.

### Fallback chain

For every file the normalizer attempts strategies from most-specific to most-
general, stopping at the first success:

```
1. Built-in pretty-printer (JSON, TOML)         → always available
2. External formatter subprocess (optional)     → if configured + exit 0
3. Word-wrap at max_line_length                 → always available
```

Any step that fails (parse error, tool not found, tool exits non-zero, timeout)
is silently skipped; the next step is tried. The result is always a valid
`Vec<IndexLine>` — normalization never prevents a file from being indexed.

### Line numbers

Normalization changes line numbers relative to the original file. This is
accepted. The system's goal is content discovery, not cursor-accurate
navigation. The word-wrap step uses a consistent algorithm so the same input
always produces the same line numbering.

---

## Formatter Options

### Built-in formatters (always on, no config required)

| Format | Crate | Behaviour |
|--------|-------|-----------|
| JSON / JSONC | `serde_json` | Parse → `to_string_pretty`. Already a transitive dep. |
| TOML | `toml` | Parse → `to_string_pretty`. Already used for config. |

These are hardcoded to their extensions, require no configuration, and handle
parse failures gracefully (fall through to word wrap). They are always tried
before the external formatter list.

### External formatter registry (configurable)

Any number of external formatters can be configured as an ordered list. The
normalizer walks the list and uses the first entry whose extension list matches
the file and whose process exits successfully. This covers any tool that can
read from stdin and write formatted output to stdout.

**Well-known tools and their supported extensions:**

| Tool | Extensions | Notes |
|------|-----------|-------|
| [biome](https://biomejs.dev/) | js ts jsx tsx css graphql json jsonc | Single Rust binary, ~10 ms/file, no Node required |
| [prettier](https://prettier.io/) | html vue svelte angular scss less yaml yml md and 50+ via plugins | Slower (Node.js), but broadest coverage including community plugins |
| [ruff format](https://docs.astral.sh/ruff/) | py pyi | Rust-based Python formatter |
| [gofmt](https://pkg.go.dev/cmd/gofmt) | go | Bundled with Go toolchain |
| [rustfmt](https://rust-lang.github.io/rustfmt/) | rs | Bundled with Rust toolchain |
| [csharpier](https://csharpier.com/) | cs | .NET tool |
| [taplo](https://taplo.tamasfe.dev/) | toml | Would override the built-in TOML formatter |

The typical deployment would configure biome first (fast, covers most common
code types) and prettier second (slower but covers HTML, Vue, Svelte, and
anything biome doesn't handle). Extensions that appear in both lists are served
by biome, because it comes first.

No auto-detection from PATH. Tools must be explicitly configured with a path.
This avoids surprising behaviour across different environments.

### Word-wrap fallback (always applied when no formatter succeeds)

Any line longer than `normalization.max_line_length` (default 120) is split
at the last word boundary before the limit. If no word boundary exists (e.g.
a long base64 string), split at the character boundary. This matches the
existing PDF wrap algorithm — reuse `wrap_at_words()` from
`crates/extractors/pdf/src/lib.rs`.

---

## Markdown

Markdown is excluded from normalization:

- Lines in markdown are semantically meaningful — wrapping a paragraph changes
  rendering in the `MarkdownViewer`.
- Markdown files are overwhelmingly human-written prose with natural line
  lengths; the "huge line" problem is rare in practice.
- A hard size cap on markdown rendering (see UI Changes below) handles the
  pathological case without touching the stored content.

---

## Implementation

### 1. Config (`crates/common/src/config.rs`)

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FormatterConfig {
    /// Absolute path to the formatter binary.
    pub path: String,
    /// File extensions this formatter handles (without leading dot, lowercase).
    pub extensions: Vec<String>,
    /// Command-line arguments. Use `{name}` as a placeholder for the filename
    /// (used by tools like biome/prettier to detect the file type).
    /// Example: ["format", "--stdin-filepath", "{name}", "-"]
    pub args: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NormalizationSettings {
    /// Maximum line length before word-wrap is applied (0 = disabled).
    #[serde(default = "default_norm_max_line_length")]
    pub max_line_length: usize,

    /// External formatters tried in order. First matching extension that
    /// exits successfully wins. Empty list = word-wrap only.
    #[serde(default)]
    pub formatters: Vec<FormatterConfig>,
}
fn default_norm_max_line_length() -> usize { 120 }
impl Default for NormalizationSettings { ... }
```

Add `normalization: NormalizationSettings` to `ServerAppSettings`.

Add to `server.toml` default template (both `install.sh` and
`packaging/windows/find-anything.iss`):
```toml
[normalization]
# max_line_length = 120

# Example: biome for JS/TS/CSS, prettier for everything else it supports.
# Formatters are tried in order; first match that exits 0 wins.
#
# [[normalization.formatters]]
# path = "/usr/local/bin/biome"
# extensions = ["js", "ts", "jsx", "tsx", "css", "graphql"]
# args = ["format", "--stdin-filepath", "{name}", "-"]
#
# [[normalization.formatters]]
# path = "/usr/local/bin/prettier"
# extensions = ["html", "vue", "svelte", "scss", "less", "yaml", "yml"]
# args = ["--stdin-filepath", "{name}"]
```

### 2. Normalization module (`crates/server/src/normalize.rs`)

Single public function:
```rust
pub fn normalize_lines(
    lines: Vec<IndexLine>,
    name: &str,          // filename / archive member name for extension detection
    cfg: &NormalizationSettings,
) -> Vec<IndexLine>
```

Internal logic:
1. Return unchanged if `cfg.max_line_length == 0` (disabled).
2. Determine extension from `name` (lowercase).
3. Try built-in pretty-printer (JSON/JSONC → `serde_json`, TOML → `toml`).
   On parse failure, continue to step 4.
4. Walk `cfg.formatters` in order. For each entry whose `extensions` list
   contains the file's extension:
   - Reconstruct the full text from `lines` (join with `\n`)
   - Invoke the binary with `args` (substituting `{name}`), piping text to stdin
   - If the process exits 0 and stdout is non-empty, use the output as the new text
   - If the process exits non-zero, times out (5 s), or produces empty output, skip to the next formatter
5. Apply word-wrap to any line exceeding `max_line_length`.

Line numbers are reassigned sequentially after normalization (1-based, dense,
no gaps). Steps 3–4 replace the entire content; step 5 is always applied on
top of whatever step 3/4 produced.

### 3. Worker integration (`crates/server/src/worker/pipeline.rs`)

After deserialising a `BulkRequest` upsert entry, before passing lines to
`chunk_lines()`, call:

```rust
let lines = normalize::normalize_lines(lines, &file.path, &cfg.normalization);
```

The same call is added in the dispatch-extractor path where archive members'
text content is produced server-side.

### 4. UI changes (`web/src/lib/FileViewer.svelte`)

**Remove "view original" for text files:**

`showOriginal` is currently only meaningful for images and PDFs. The toggle
button is visible for all kinds. After this change:

- `file_kind = "text"`: hide the toggle button entirely. Show a "Download"
  icon/link instead (links to the existing original-file serve endpoint).
- `file_kind = "pdf"`: unchanged (keep toggle).
- `file_kind = "image"`: unchanged (keep toggle).
- `file_kind = "markdown"`: rename the toggle to "Formatted / Plain" (rendered
  markdown vs. the extracted text). Apply a `max_markdown_render_kb` check
  (default 512 KB): if the file exceeds this threshold, skip rendering and
  show only the plain text view.

**`max_markdown_render_kb`** is added to `SettingsResponse` and read from
`ServerAppSettings` (default 512). This gives operators control over the
threshold without a UI rebuild.

---

## Files Changed

| File | Change |
|------|--------|
| `crates/common/src/config.rs` | Add `NormalizationSettings` + `max_markdown_render_kb` |
| `crates/common/src/api.rs` | Add `max_markdown_render_kb` to `SettingsResponse` |
| `crates/server/src/normalize.rs` | **New file** — normalization logic |
| `crates/server/src/worker/pipeline.rs` | Call `normalize_lines` before `chunk_lines` |
| `crates/server/src/routes/settings.rs` | Expose `max_markdown_render_kb` |
| `install.sh` | Add `[normalization]` block to default config template |
| `packaging/windows/find-anything.iss` | Same |
| `web/src/lib/FileViewer.svelte` | Remove text toggle; add Download; markdown size cap |
| `web/src/lib/api.ts` | Add `max_markdown_render_kb` to `Settings` type |

`crates/extractors/pdf/src/lib.rs` — no change (word-wrap helper reused via
a shared location, or duplicated if the crate boundary makes sharing awkward).

---

## Testing

1. **Large JSON**: index a minified JSON file (e.g. a bundled `package-lock.json`
   with no newlines). Verify stored content is pretty-printed with lines ≤ 120.
2. **Invalid JSON**: index a `.json` file containing invalid syntax. Verify
   it is indexed (word-wrapped), not dropped.
3. **biome (if configured)**: index a minified `.js` file. Verify it is
   formatted. Disable biome path; verify fallback to word wrap.
4. **Long log lines**: index a log file with 500-char lines. Verify all lines
   in the index are ≤ 120 chars.
5. **Markdown**: index a large `.md` file. Verify content is unchanged (no
   wrapping). Verify the UI caps rendered markdown at `max_markdown_render_kb`.
6. **Text "view original" removed**: open a `.txt` and `.js` file in the UI.
   Verify no toggle button, only a Download link.

---

## Effect on Plan 056

After this plan is complete:

- All text content in the index has bounded line lengths.
- Plan 056 can safely use line count as the pagination unit.
- The "view original" toggle no longer exists for text files, so pagination
  only needs to handle the extracted-text view (not a raw-file view).
- Markdown is capped at a hard size limit and shown in full if under the cap;
  no pagination needed for its rendered view.

---

## Breaking Changes

- Existing indexed text content is not normalized retroactively. Files
  re-indexed after this change will have different line numbers than before.
  This is expected and documented.
- `MIN_CLIENT_VERSION` does not need bumping — this is a server-side change
  with no API surface change.
