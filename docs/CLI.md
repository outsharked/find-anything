# Command-Line Reference

find-anything is a two-process system. `find-server` stores the index and serves
search queries; the client tools (`find-scan`, `find-watch`, `find-anything`,
`find-admin`) run on the machines whose files you want to index.

---

## find-server

Runs the HTTP server that receives indexed content, stores it, and serves
search queries and the web UI.

```
find-server [OPTIONS]
```

| Option            | Description                                     |
| ----------------- | ----------------------------------------------- |
| `--config <PATH>` | Path to server config file (see defaults below) |

**Config path defaults** (in priority order):

1. `FIND_ANYTHING_SERVER_CONFIG` environment variable
2. `$XDG_CONFIG_HOME/find-anything/server.toml`
3. `/etc/find-anything/server.toml` — when running as root (typical for system services)
4. `~/.config/find-anything/server.toml` — otherwise

**Other environment variables**

| Variable   | Description                                                                       |
| ---------- | --------------------------------------------------------------------------------- |
| `RUST_LOG` | Log filter (default: `warn,find_server=info`). Set to `debug` for verbose output. |

**Example**

```sh
# Explicit path
find-server --config /etc/find-anything/server.toml

# Let the binary pick the right default
find-server
```

**Server config reference** (`server.toml`)

```toml
[server]
bind     = "0.0.0.0:8765"              # Address and port to listen on
data_dir = "/var/lib/find-anything"    # Where SQLite DBs and content ZIPs are stored
token    = "change-me"                 # Bearer token required by all API calls

[search]
default_limit       = 50    # Default result count per search request
max_limit           = 500   # Hard cap on results per request
fts_candidate_limit = 2000  # FTS5 candidates evaluated before ranking
context_window      = 1     # Lines shown before/after each match (total = 2×N+1)
```

---

## find-scan

Walks the configured source paths, extracts text content, and submits batches
to the server. Run periodically (e.g. via cron or systemd timer) to keep the
index up to date.

```
find-scan [OPTIONS] [FILE]
```

| Argument / Option | Description                                                                                                                                                                                            |
| ----------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| `[FILE]`          | Scan a single file instead of all sources. The file must be under a configured source path. Mtime checking is skipped — the file is always re-indexed.                                                 |
| `[DIRECTORY]`     | Scan all the members of the directory recursively. The directory must be under a configured source path. Mtime checking is skipped — all files are always re-indexed.                                  |
| `--config <PATH>` | Client config file (default: `~/.config/find-anything/client.toml`)                                                                                                                                    |
| `--upgrade`       | Force a full re-index of every file that was scanned with an older tool version                                                                                                                        |
| `--quiet`         | Suppress per-file processing logs; only warnings, errors, and the final summary are printed                                                                                                            |
| `--dry-run`       | Walk the filesystem and compare with server state without extracting or submitting anything; prints how many files would be added, modified, unchanged, and deleted. Cannot be combined with `[FILE]`. |

Deleted files are removed from the index

**Examples**

```sh
# Incremental scan
find-scan

# Preview what would change without touching the index
find-scan --dry-run

# Full reindex of any files indexed with older tool version
find-scan --upgrade

# Re-index a single file immediately (e.g. after manually editing it)
find-scan /home/user/documents/notes.md
```

---

## find-watch

Watches the configured source paths with inotify (Linux) or FSEvents (macOS)
and re-indexes files as they change. Intended to run as a long-lived daemon
alongside `find-server`.

```
find-watch [OPTIONS]
```

| Option            | Description                                                         |
| ----------------- | ------------------------------------------------------------------- |
| `--config <PATH>` | Client config file (default: `~/.config/find-anything/client.toml`) |

Changes are debounced (default 500 ms) before being sent to the server. Because
watch mode uses the same extractor pipeline as `find-scan`, all the same
`scan.*` config settings apply.

**Example**

```sh
find-watch --config ~/.config/find-anything/client.toml
```

---

## find-anything

Command-line search client. Queries the server and prints results to stdout,
suitable for use in scripts or terminal workflows.

```
find-anything [OPTIONS] <PATTERN>
```

| Argument / Option   | Description                                                         |
| ------------------- | ------------------------------------------------------------------- |
| `<PATTERN>`         | Search pattern (fuzzy or exact depending on `--mode`)               |
| `--mode <MODE>`     | `fuzzy` (default) or `exact`                                        |
| `--source <NAME>`   | Restrict search to this source (repeatable for multiple)            |
| `--limit <N>`       | Maximum results to return (default: 50)                             |
| `--offset <N>`      | Skip first N results for pagination (default: 0)                    |
| `-C, --context <N>` | Lines of context around each match, like `grep -C` (default: 0)     |
| `--no-color`        | Suppress ANSI colour output                                         |
| `--config <PATH>`   | Client config file (default: `~/.config/find-anything/client.toml`) |

**Examples**

```sh
# Fuzzy search across all sources
find-anything terraform

# Exact search in a specific source with 2 lines of context
find-anything --mode exact --source code -C 2 "fn process_file"

# Paginate through results
find-anything --limit 20 --offset 40 config
```

---

## find-admin

Administrative utility for inspecting server state and managing the inbox.
All subcommands communicate with the server via the API (bearer-token auth)
using the URL and token from the client config.

```
find-admin [OPTIONS] <COMMAND>
```

**Global options**

| Option            | Description                                                         |
| ----------------- | ------------------------------------------------------------------- |
| `--config <PATH>` | Client config file (default: `~/.config/find-anything/client.toml`) |
| `--json`          | Print raw JSON instead of human-readable output                     |

---

### find-admin check

Verify that the server is reachable and the token is accepted.

```sh
find-admin check
```

---

### find-admin config

Print the effective client configuration with all defaults filled in.
Useful for verifying what values will be used when running `find-scan` or
`find-watch`.

```sh
find-admin config
```

---

### find-admin sources

List all indexed sources known to the server.

```sh
find-admin sources
```

---

### find-admin status

Print per-source statistics: file count, total size, last scan time, and a
breakdown by file kind.

```sh
find-admin status
find-admin status --json
```

---

### find-admin inbox

Show the current inbox state: how many batch files are pending processing and
how many have failed.

```sh
find-admin inbox
```

---

### find-admin inbox-clear

Delete inbox files. By default targets only the **pending** queue.

```
find-admin inbox-clear [OPTIONS]
```

| Option     | Description                                    |
| ---------- | ---------------------------------------------- |
| `--failed` | Target the **failed** queue instead of pending |
| `--all`    | Target both pending and failed queues          |
| `--yes`    | Skip the confirmation prompt                   |

```sh
# Clear pending queue (prompts for confirmation)
find-admin inbox-clear

# Clear failed queue without prompting
find-admin inbox-clear --failed --yes
```

---

### find-admin inbox-retry

Move files from the **failed** queue back to **pending** so the server will
attempt to process them again.

```sh
find-admin inbox-retry
```

---

## Client config reference

All client tools (`find-scan`, `find-watch`, `find-anything`, `find-admin`)
read from the same config file.

Default path: `~/.config/find-anything/client.toml`
Override with: `--config <PATH>` or `FIND_ANYTHING_CONFIG=<PATH>`

```toml
[server]
url   = "http://192.168.1.10:8765"   # find-server base URL
token = "change-me"                  # Bearer token (must match server config)

# One or more sources to index. Each source is a named collection of paths.
[[sources]]
name     = "code"
paths    = ["/home/user/code", "/home/user/projects"]

[[sources]]
name  = "documents"
path = "/home/user/Documents"

[scan]
# Glob patterns (relative to each source root) to exclude from indexing.
exclude = [
    "**/.git/**",
    "**/node_modules/**",
    "**/target/**",
    "**/__pycache__/**",
]
max_content_size_mb = 10  # Skip files larger than this (does not apply to archives)
follow_symlinks  = false  # Follow symbolic links during filesystem walk
include_hidden   = false  # Include dot-files and dot-directories

[scan.archives]
enabled               = true   # Extract content from ZIP, TAR, 7z, etc.
max_depth             = 10     # Maximum nesting depth for archives-within-archives
max_temp_file_mb      = 500    # Max size of temp file for nested 7z / oversized nested ZIP
max_7z_solid_block_mb = 256    # 7z solid blocks larger than this are indexed by filename only
                                # (lower this on memory-constrained systems such as NAS boxes)

[watch]
debounce_ms   = 500       # Milliseconds to wait after last event before re-indexing
extractor_dir = ""        # Directory containing find-extract-* binaries (auto-detected if empty)
```

---

## Extractor binaries

The following binaries perform content extraction and can also be run standalone
for debugging. Each accepts a file path as its first argument and prints
extracted lines to stdout.

| Binary                 | Handles                                   |
| ---------------------- | ----------------------------------------- |
| `find-extract-text`    | Plain text, source code, scripts, config  |
| `find-extract-pdf`     | PDF documents                             |
| `find-extract-archive` | ZIP, TAR, TGZ, TBZ2, TXZ, GZ, BZ2, XZ, 7Z |
| `find-extract-epub`    | EPUB e-books                              |
| `find-extract-html`    | HTML files (strips tags)                  |
| `find-extract-office`  | DOCX, XLSX, PPTX (Office Open XML)        |
| `find-extract-media`   | Audio/video metadata (EXIF, ID3, etc.)    |
| `find-extract-pe`      | Windows PE executables (exports, imports) |

**Example**

```sh
find-extract-pdf /path/to/document.pdf
find-extract-archive /path/to/backup.tar.gz
```
