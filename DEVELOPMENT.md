# Development Guide

## Prerequisites

| Tool                         | Purpose               | Install                                                           |
| ---------------------------- | --------------------- | ----------------------------------------------------------------- |
| Rust (stable)                | Build all Rust crates | `curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs \| sh` |
| [mise](https://mise.jdx.dev) | Tool version manager + task runner | `curl https://mise.run \| sh`              |

After installing mise, run `mise install` in the repo root — this installs the correct versions of Node.js and pnpm (pinned in `.mise.toml`) and configures the environment.

---

## Mise Tasks

All common workflows are available as `mise run <task>`:

| Task                     | Description                                                    |
| ------------------------ | -------------------------------------------------------------- |
| `mise run dev`           | Start Rust API server + Vite dev server (live reload for both) |
| `mise run server`        | Start Rust API server only (with cargo-watch auto-rebuild)     |
| `mise run scan`          | Incremental scan of configured sources                         |
| `mise run scan-full`     | Full (non-incremental) re-scan                                 |
| `mise run check`         | Type-check Rust workspace + web UI                             |
| `mise run clippy`        | Run clippy lints (matches CI — fails on warnings)              |
| `mise run build-release` | Build web UI, then compile `find-server` with embedded assets  |
| `mise run build-x64`     | Build web UI, then compile all binaries for x86_64             |
| `mise run build-arm`     | Build all binaries for ARM7 NAS (see below)                    |
| `mise run admin`         | Show inbox status                                              |
| `mise run clean-db`      | Delete local index data (requires re-scan)                     |

---

## Development Setup

### 1. Install web dependencies

```sh
pnpm install
```

### 2. Create config files

Copy and edit the example configs:

```sh
cp examples/server.toml config/server.toml
cp client.toml.example config/client.toml   # edit: set server URL and source paths
```

### 3. Start the dev environment

```sh
mise run dev
```

This starts:

- `cargo watch` on `crates/` — rebuilds and restarts `find-server` on Rust changes
- `vite` on `web/` — serves the UI at `http://localhost:5173` with hot reload

The Vite dev server proxies API requests to `find-server` so you can work on
the UI without rebuilding the Rust server for every frontend change.

---

## Building

### Native (x86_64)

Builds the web UI first (so `find-server` embeds the production assets), then compiles all binaries:

```sh
mise run build
```

Output: `target/release/find-{server,scan,watch,...}`

### ARM7 (for NAS deployment)

ARM7 binaries require [`cross`](https://github.com/cross-rs/cross), which uses Docker to
build against a compatible glibc version. Building natively on an x86_64 machine links
against the host's glibc (which may be newer than what the NAS supports).

**First-time setup:**

```sh
# Install cross
cargo install cross --locked

# Add the ARM7 rustup target
rustup target add armv7-unknown-linux-gnueabihf

# Docker must be running
```

**Build:**

```sh
mise run build-arm
```

Output: `target/armv7-unknown-linux-gnueabihf/release/find-{server,scan,watch,...}`

This matches what the CI release pipeline produces for the `linux-armv7` artifact.

> **Why `cross` and not plain `cargo`?**
> Plain cross-compilation (`cargo build --target armv7-...`) links against the host
> machine's glibc. If the host has glibc 2.39 but the NAS has an older version, the
> binary will fail to run with "version `GLIBC_2.39` not found". `cross` uses a Docker
> image with an older glibc that matches what the NAS runs.

---

## Linting

CI runs clippy with `-D warnings` (warnings are errors). Run the same check locally before pushing:

```sh
mise run clippy
```

---

## CI / Release

CI runs on every push (`ci.yml`):

- `cargo test --workspace` + `cargo clippy -- -D warnings`
- `pnpm run check` (TypeScript type-check)

Releases are triggered by pushing a `v*.*.*` tag (`release.yml`). The build matrix produces:

| Platform       | Target                                        |
| -------------- | --------------------------------------------- |
| Linux x86_64   | `x86_64-unknown-linux-gnu`                    |
| Linux aarch64  | `aarch64-unknown-linux-gnu`                   |
| Linux ARM7     | `armv7-unknown-linux-gnueabihf` (via `cross`) |
| macOS arm64    | `aarch64-apple-darwin`                        |
| macOS x86_64   | `x86_64-apple-darwin`                         |
| Windows x86_64 | `x86_64-pc-windows-msvc`                      |

Each platform produces a `.tar.gz` (or `.zip` on Windows) attached to the GitHub Release.

---

## Project Structure

```
crates/
  common/              # Shared API types, config, fuzzy search (no extractor deps)
  server/              # HTTP server, SQLite, ZIP archive management
  client/              # find-scan, find-watch, find-anything, find-admin binaries
  extractors/
    text/              # Plain text, source code, Markdown + frontmatter
    pdf/               # PDF text extraction (pdf-extract)
    media/             # Image EXIF, audio tags, video metadata
    archive/           # ZIP / TAR / GZ / BZ2 / XZ / 7Z + orchestration
    html/              # HTML visible text extraction
    office/            # DOCX / XLSX / PPTX extraction
    epub/              # EPUB ebook extraction
    pe/                # Windows PE binary metadata
    dispatch/          # Unified dispatcher: routes bytes to the right extractor
  windows/
    service/           # find-watch Windows service wrapper
    tray/              # find-tray system tray app
web/                   # SvelteKit web UI
config/                # Local dev config files (not committed) + deployment scripts
docs/                  # Architecture, plans, CLI reference, systemd unit files
```

See [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md) for a detailed description of the
system design, write path, read path, and key invariants.
