# Configuration

[← Manual home](README.md)

---

## Server config (`server.toml`)

Default locations (checked in order):

1. `$FIND_ANYTHING_SERVER_CONFIG` (environment variable)
2. `$XDG_CONFIG_HOME/find-anything/server.toml`
3. `/etc/find-anything/server.toml` (when running as root)
4. `~/.config/find-anything/server.toml`

Override with `--config <PATH>`.

```toml
[server]
bind     = "0.0.0.0:8765"              # Address and port to listen on
data_dir = "/var/lib/find-anything"    # Where the index and content archives are stored
token    = "change-me"                 # Bearer token required by all API calls

[search]
default_limit       = 50    # Default result count per request
max_limit           = 500   # Hard cap on results per request
fts_candidate_limit = 2000  # FTS5 rows evaluated before re-ranking
context_window      = 1     # Lines of context shown either side of each match
```

**`bind`** — Use `127.0.0.1:8765` to accept only local connections, or `0.0.0.0:8765` to accept connections from other machines on the network. The server has no TLS — put it behind a reverse proxy (nginx, Caddy) if you need HTTPS.

**`token`** — A shared secret presented as an HTTP `Authorization: Bearer <token>` header. All clients (web UI, CLI, `find-scan`, `find-watch`) must use the same token. Generate a strong value with `openssl rand -hex 32`.

**`fts_candidate_limit`** — Higher values improve recall and ranking quality but increase CPU per query. Raise this if searches feel like they're missing relevant results.

**`context_window`** — Each search result includes `N` lines before and after the matched line, for a total context of `2N + 1` lines. The web UI allows the user to expand context interactively regardless of this setting.

---

## Client config (`client.toml`)

Default location: `~/.config/find-anything/client.toml`

Override with `--config <PATH>` or `FIND_ANYTHING_CONFIG=<PATH>`.

All client tools (`find-scan`, `find-watch`, `find-anything`, `find-admin`) read from this same file.

```toml
[server]
url   = "http://192.168.1.10:8765"   # find-server base URL
token = "change-me"                  # Must match the server token

[[sources]]
name  = "home"
paths = ["/home/alice/documents", "/home/alice/projects"]

[scan]
exclude          = ["**/.git/**", "**/node_modules/**", "**/target/**"]
max_content_size_mb = 10
follow_symlinks     = false
include_hidden      = false

[scan.archives]
enabled               = true
max_depth             = 10
max_7z_solid_block_mb = 256

[watch]
debounce_ms   = 500
extractor_dir = ""   # auto-detected if empty

[log]
ignore = [
    "pdf_extract: unknown glyph name",
]
```

---

## Sources

A **source** is a named collection of filesystem paths that are indexed as a unit. You can have multiple sources on one machine (e.g. `code`, `documents`) and multiple machines each with their own sources — all indexed into the same server.

```toml
[[sources]]
name  = "code"
paths = ["/home/alice/code", "/home/alice/projects"]

[[sources]]
name  = "documents"
paths = ["/home/alice/Documents"]
```

- `name` — must be unique across all clients. Results in the web UI are grouped and filtered by source name.
- `paths` — one or more absolute directory paths to index. All paths are indexed under the same source name.

**Single path shorthand:** `path` (singular) is also accepted as an alias for `paths`.

---

## Scan settings

```toml
[scan]
exclude             = ["**/.git/**", "**/node_modules/**"]
max_content_size_mb = 10
follow_symlinks     = false
include_hidden      = false
noindex_file        = ".noindex"
index_file          = ".index"
```

| Setting | Default | Description |
|---|---|---|
| `exclude` | `[]` | Glob patterns (relative to source root) of paths to skip |
| `max_content_size_mb` | `10` | Skip files larger than this size. Does not apply to archives — archive members are filtered individually after extraction. |
| `follow_symlinks` | `false` | Follow symbolic links during the filesystem walk |
| `include_hidden` | `false` | Include dot-files and dot-directories |
| `noindex_file` | `.noindex` | Filename that marks a directory as excluded (see below) |
| `index_file` | `.index` | Filename for per-directory scan overrides (see below) |

**Exclude patterns** use glob syntax relative to each source root. Examples:

```toml
exclude = [
    "**/.git/**",          # Git internals everywhere
    "**/node_modules/**",  # JavaScript dependencies
    "**/target/**",        # Rust build output
    "**/__pycache__/**",   # Python bytecode
    "private/**",          # Everything under a top-level 'private' folder
]
```

---

## Archive settings

```toml
[scan.archives]
enabled               = true
max_depth             = 10
max_temp_file_mb      = 500
max_7z_solid_block_mb = 256
```

| Setting | Default | Description |
|---|---|---|
| `enabled` | `true` | Extract and index archive members (ZIP, TAR, 7Z, etc.) |
| `max_depth` | `10` | Maximum nesting depth for archives-within-archives (guards against zip bombs) |
| `max_temp_file_mb` | `500` | Max size of a temp file created during nested 7z/large-ZIP extraction |
| `max_7z_solid_block_mb` | `256` | 7z solid blocks larger than this are indexed by filename only — lower on memory-constrained systems |

When archives are enabled, each member is indexed as a separate searchable file using the path `archive.zip::member/path.txt`. See [File types → Archives](06-file-types.md#archives) for details.

---

## Per-directory control (`.noindex` / `.index`)

### `.noindex`

Place an empty file named `.noindex` in any directory to tell `find-scan` and `find-watch` to skip that directory and everything inside it:

```sh
touch /home/alice/projects/private/.noindex
```

The filename is configurable via `scan.noindex_file`.

### `.index`

Place a `.index` file in any directory to override scan settings for that subtree. The file is TOML and accepts a subset of `[scan]` settings:

```toml
# .index — place in a directory to override scan settings for this subtree

# Override the file size limit
max_content_size_mb = 100

# Add extra excludes (relative to this directory)
exclude = ["build/**", "*.tmp"]

# Force indexing of hidden files in this subtree
include_hidden = true
```

Settings in `.index` apply to the directory it's in and all subdirectories, unless overridden by a deeper `.index` file. Settings from `.index` are merged on top of the global `[scan]` config — they do not replace it entirely.

---

## Watch settings

```toml
[watch]
debounce_ms   = 500
extractor_dir = ""
```

| Setting | Default | Description |
|---|---|---|
| `debounce_ms` | `500` | Milliseconds to wait after the last filesystem event before processing changes. Higher values reduce noise from editors that do multiple writes per save. |
| `extractor_dir` | `""` | Directory containing the `find-extract-*` binaries. Auto-detected from the location of `find-watch` if empty. |

---

## Text normalization

The server applies normalization to text and PDF content before writing it to the index. This turns minified files into readable, line-per-concept content and ensures no line exceeds a configured length.

```toml
[normalization]
max_line_length = 120              # wrap lines longer than this (0 = disabled)
batch_formatter_timeout_secs = 60  # kill batch formatter after this many seconds; falls back to per-file
per_file_formatter_timeout_secs = 10  # kill per-file formatter after this many seconds; file is skipped

# External formatters — recommended: use mode = "batch" (one process per batch,
# much faster than the default stdin mode which spawns once per file).

[[normalization.formatters]]
path       = "/usr/local/bin/biome"
extensions = ["js", "ts", "jsx", "tsx", "css", "graphql"]
args       = ["format", "--write", "{dir}"]
mode       = "batch"

[[normalization.formatters]]
path       = "/usr/local/bin/prettier"
extensions = ["html", "vue", "svelte", "scss", "less", "yaml", "yml"]
args       = ["--write", "{dir}"]
mode       = "batch"
```

### How normalization works

For each indexed text file the server attempts the following steps in order, stopping at the first success:

1. **Built-in pretty-printer** — JSON and TOML files are pretty-printed using the built-in `serde_json` / `toml` crates. Parse failures fall through to step 2.
2. **External formatter** — the `formatters` list is walked in order. The first entry whose `extensions` list matches the file is used. How it is invoked depends on `mode` (see below).
3. **Word-wrap** — any line longer than `max_line_length` characters is split at the last word boundary before the limit. This step always runs after steps 1–2.

**Markdown files are excluded** — line structure is semantically meaningful in markdown. Markdown is never modified regardless of line length.

**Existing content is not retroactively normalized.** Normalization applies only when a file is indexed for the first time or re-indexed. To normalize already-indexed files, run `find-scan --force`.

### `max_line_length`

| Value | Effect |
|---|---|
| `120` (default) | Lines longer than 120 characters are word-wrapped |
| `0` | Normalization disabled entirely — content is stored as-is |

### `batch_formatter_timeout_secs`

How long (in seconds) the server waits for a batch-mode formatter process to finish before killing it. Default: **60**.

When the timeout is exceeded the server falls back to per-file mode: each file is re-submitted individually with its own `per_file_formatter_timeout_secs` limit. This ensures that one slow or hung formatter does not block an entire batch.

### `per_file_formatter_timeout_secs`

How long (in seconds) the server waits for each individual formatter invocation in per-file fallback mode. Default: **10**.

Files whose formatter exceeds this limit are skipped (kept with their original content). Other files in the same batch are still processed normally.

### External formatter configuration

Each `[[normalization.formatters]]` entry supports these fields:

| Field | Description |
|---|---|
| `path` | Absolute path to the formatter binary |
| `extensions` | List of file extensions (without `.`, lowercase) this formatter handles |
| `args` | Command-line arguments. Use `{dir}` (batch mode) or `{name}` (stdin mode) as a placeholder |
| `mode` | `"batch"` or `"stdin"` — how the formatter receives input. Default: `"stdin"` |

#### `mode = "batch"` (recommended)

All matching files in the request are written to a temp directory (`00001.ext`, `00002.ext`, …) and the formatter is run **once on the whole directory**. Use `{dir}` in `args` for the temp directory path. The formatter must modify files in-place.

This is much faster than stdin mode for large batches — a request with 150 JS files calls the formatter once instead of 150 times.

#### `mode = "stdin"` (default)

The formatter is invoked **once per file** with the file's content on stdin. It must write the formatted result to stdout. Exit code 0 with non-empty stdout is treated as success; non-zero exit or empty output falls through to the next formatter. Use `{name}` in `args` for the filename (needed by tools that detect file type from the extension).

Use stdin mode for tools that do not support formatting a directory (e.g. `gofmt`, `rustfmt`).

### Well-known formatters

| Tool | Extensions | Recommended mode | Install |
|---|---|---|---|
| [Biome](https://biomejs.dev/) | `js ts jsx tsx css graphql json jsonc` | batch | [biomejs.dev/installation](https://biomejs.dev/installation/) |
| [Prettier](https://prettier.io/) | `html vue svelte angular scss less yaml yml md` (and 50+ via plugins) | batch | [prettier.io/docs/en/install](https://prettier.io/docs/en/install.html) |
| [Ruff](https://docs.astral.sh/ruff/) | `py pyi` | batch | [docs.astral.sh/ruff/installation](https://docs.astral.sh/ruff/installation/) |
| [gofmt](https://pkg.go.dev/cmd/gofmt) | `go` | stdin | Bundled with the [Go toolchain](https://go.dev/doc/install) |
| [rustfmt](https://rust-lang.github.io/rustfmt/) | `rs` | stdin | Bundled with `rustup` — `rustup component add rustfmt` |
| [CSharpier](https://csharpier.com/) | `cs` | batch | [csharpier.com/docs/Installation](https://csharpier.com/docs/Installation) |
| [Taplo](https://taplo.tamasfe.dev/) | `toml` | stdin | [taplo.tamasfe.dev/#installation](https://taplo.tamasfe.dev/#installation) (overrides built-in TOML formatter) |

### Recommended setup

Configure Biome first (fast Rust binary, no runtime dependency, covers most common web code) and Prettier second (slower Node.js, but covers HTML, Vue, Svelte, and anything Biome doesn't handle):

```toml
[[normalization.formatters]]
path       = "/usr/local/bin/biome"
extensions = ["js", "ts", "jsx", "tsx", "css", "graphql"]
args       = ["format", "--write", "{dir}"]
mode       = "batch"

[[normalization.formatters]]
path       = "/usr/local/bin/prettier"
extensions = ["html", "vue", "svelte", "scss", "less", "yaml", "yml"]
args       = ["--write", "{dir}"]
mode       = "batch"
```

For Python, Go, and Rust:

```toml
[[normalization.formatters]]
path       = "/usr/local/bin/ruff"
extensions = ["py", "pyi"]
args       = ["format", "{dir}"]
mode       = "batch"

[[normalization.formatters]]
path       = "/usr/local/bin/gofmt"
extensions = ["go"]
args       = []
# mode = "stdin"  (default — gofmt reads stdin, writes stdout)

[[normalization.formatters]]
path       = "/home/user/.cargo/bin/rustfmt"
extensions = ["rs"]
args       = ["--edition", "2021"]
# mode = "stdin"  (default)
```

> **Note:** Tools must be configured with an absolute `path`. There is no auto-detection from `$PATH` — this avoids unexpected behaviour across environments where the same tool may be installed in different locations (or not at all).

---

## Log suppression

The `[log]` section lets you silence specific noisy log messages using regular expressions matched against `"target: message"`:

```toml
[log]
ignore = [
    "pdf_extract: unknown glyph name",   # very frequent in PDFs with unusual fonts
    "find_extract_media: unsupported",    # unsupported media format variants
]
```

This is particularly useful for PDF extraction warnings that fire hundreds of times per document.

---

[← Installation](01-installation.md) | [Next: Indexing →](03-indexing.md)
