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

The server applies normalization to text and PDF content before writing it to the index. This turns minified files into readable, line-per-concept content and ensures no line exceeds a configured length. Normalization does not modify markdown files.

```toml
[normalization]
max_line_length = 120   # wrap lines longer than this (0 = disabled)

# External formatters are tried in order; first match that exits 0 wins.
# [[normalization.formatters]]
# path       = "/usr/local/bin/biome"
# extensions = ["js", "ts", "jsx", "tsx", "css", "graphql", "json", "jsonc"]
# args       = ["format", "--stdin-file-path", "{name}", "-"]
#
# [[normalization.formatters]]
# path       = "/usr/local/bin/prettier"
# extensions = ["html", "vue", "svelte", "scss", "less", "yaml", "yml", "md"]
# args       = ["--stdin-filepath", "{name}"]
```

### How normalization works

For each indexed text file the server attempts the following steps in order, stopping at the first success:

1. **Built-in pretty-printer** — JSON and TOML files are pretty-printed using the built-in `serde_json` / `toml` crates. Parse failures fall through to step 2.
2. **External formatter** — the `formatters` list is walked in order. The first entry whose `extensions` list matches the file is invoked with the file content on stdin. If it exits 0 and writes non-empty output, that output is used. Failures (non-zero exit, timeout after 5 s, empty output) try the next entry.
3. **Word-wrap** — any line longer than `max_line_length` characters is split at the last word boundary before the limit. This step always runs after steps 1–2.

**Markdown files are excluded** — line structure is semantically meaningful in markdown (a wrapped paragraph would break rendering). Markdown is never modified regardless of line length.

**Existing content is not retroactively normalized.** Normalization applies only when a file is indexed for the first time or re-indexed. To normalize already-indexed files, run `find-scan --reindex`.

### `max_line_length`

| Value | Effect |
|---|---|
| `120` (default) | Lines longer than 120 characters are word-wrapped |
| `0` | Normalization disabled entirely — content is stored as-is |

### External formatter configuration

Each `[[normalization.formatters]]` entry has three fields:

| Field | Description |
|---|---|
| `path` | Absolute path to the formatter binary |
| `extensions` | List of file extensions (without `.`, lowercase) this formatter handles |
| `args` | Command-line arguments. Use `{name}` as a placeholder for the filename (needed by tools that detect file type from the name) |

The formatter receives the file content on **stdin** and must write the formatted result to **stdout**.

### Well-known formatters

| Tool | Extensions | Install |
|---|---|---|
| [Biome](https://biomejs.dev/) | `js ts jsx tsx css graphql json jsonc` | [biomejs.dev/installation](https://biomejs.dev/installation/) |
| [Prettier](https://prettier.io/) | `html vue svelte angular scss less yaml yml md` (and 50+ via plugins) | [prettier.io/docs/en/install](https://prettier.io/docs/en/install.html) |
| [Ruff](https://docs.astral.sh/ruff/) | `py pyi` | [docs.astral.sh/ruff/installation](https://docs.astral.sh/ruff/installation/) |
| [gofmt](https://pkg.go.dev/cmd/gofmt) | `go` | Bundled with the [Go toolchain](https://go.dev/doc/install) |
| [rustfmt](https://rust-lang.github.io/rustfmt/) | `rs` | Bundled with `rustup` — `rustup component add rustfmt` |
| [CSharpier](https://csharpier.com/) | `cs` | [csharpier.com/docs/Installation](https://csharpier.com/docs/Installation) |
| [Taplo](https://taplo.tamasfe.dev/) | `toml` | [taplo.tamasfe.dev/#installation](https://taplo.tamasfe.dev/#installation) (overrides built-in TOML formatter) |

**Recommended setup** — configure Biome first (fast Rust binary, covers most common code types with no runtime dependency) and Prettier second (slower Node.js, but covers HTML, Vue, Svelte, and anything Biome doesn't handle). Extensions that appear in both lists are served by Biome because it comes first:

```toml
[[normalization.formatters]]
path       = "/usr/local/bin/biome"
extensions = ["js", "ts", "jsx", "tsx", "css", "graphql"]
args       = ["format", "--stdin-file-path", "{name}", "-"]

[[normalization.formatters]]
path       = "/usr/local/bin/prettier"
extensions = ["html", "vue", "svelte", "scss", "less", "yaml", "yml"]
args       = ["--stdin-filepath", "{name}"]
```

> **Note:** Tools must be explicitly configured with an absolute `path`. There is no auto-detection from `$PATH` — this avoids unexpected behaviour across environments where the same tool may be installed in different locations (or not at all).

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
