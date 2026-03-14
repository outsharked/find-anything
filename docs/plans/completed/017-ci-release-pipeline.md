# Plan 017: GitHub CI & Release Pipeline

## Overview

Establishes automated CI and a release pipeline so end users can install
find-anything without building from source.

## What's Added

| File | Purpose |
|------|---------|
| `.github/workflows/ci.yml` | Test + clippy on every push/PR; web type-check |
| `.github/workflows/release.yml` | Binary build matrix + GitHub Release on `v*` tags |
| `install.sh` | `curl \| sh` installer — detects platform, downloads release tarball |
| `Dockerfile` | Multi-stage build for `find-server` (rust:slim → debian:bookworm-slim) |
| `docker-compose.yml` | Server with data volume; binds to `127.0.0.1:8080` |
| `examples/server.toml` | Annotated server config template for Docker users |

## CI Workflow

Runs on every push and pull request. Two parallel jobs:

- **`test`** — `cargo test --workspace` + `cargo clippy -- -D warnings`
- **`web`** — `pnpm install && pnpm run check` in `web/`

## Release Workflow

Triggered by pushing a `v*.*.*` tag. Build matrix:

| Platform | Runner | Rust target |
|----------|--------|-------------|
| Linux x86_64 | `ubuntu-24.04` | `x86_64-unknown-linux-gnu` |
| Linux arm64 | `ubuntu-24.04-arm` | `aarch64-unknown-linux-gnu` |
| macOS arm64 | `macos-latest` | `aarch64-apple-darwin` |
| macOS x86_64 | `macos-latest` | `x86_64-apple-darwin` |

Each job packages all 8 binaries into `find-anything-{version}-{platform}.tar.gz`.
A final `publish` job collects all 4 tarballs and creates the GitHub Release.

**Binaries included in each tarball:**
`find`, `find-scan`, `find-watch`, `find-server`,
`find-extract-text`, `find-extract-pdf`, `find-extract-media`, `find-extract-archive`

## Install Script

```sh
curl -fsSL https://raw.githubusercontent.com/jamietre/find-anything/main/install.sh | sh
```

Detects OS and arch, fetches the latest release from the GitHub API, extracts
binaries to `~/.local/bin` (override with `INSTALL_DIR=...`). Warns if the
destination isn't in PATH.

## Docker

```sh
# Copy and edit the config
cp examples/server.toml server.toml
# Edit token, data_dir = /data, bind = 0.0.0.0:8080

docker compose up -d

# Point find-scan at it from the host
find-scan --config client.toml
```

The web UI is not included in the Docker image — run it locally with
`node web/build/` or `pnpm dev` in `web/`.

## Not in This Plan

- Windows binaries (medium-term roadmap)
- Web UI Docker image
- Docker Hub / GHCR image publishing (can be added to release.yml later)
- Homebrew formula
