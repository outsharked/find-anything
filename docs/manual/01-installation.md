# Installation

[← Manual home](README.md)

---

## Server (Linux & macOS)

Run this on the machine that will host the central index:

```sh
curl -fsSL https://raw.githubusercontent.com/jamietre/find-anything/master/install-server.sh | sh
```

The script will ask for:

- **Install directory** — default `/usr/local/bin` (system) or `~/.local/bin` (user)
- **Service mode** — `system` (root, `/etc/find-anything/`) or `user` (`~/.config/find-anything/`)
- **Bind address** — the address and port `find-server` will listen on (e.g. `0.0.0.0:8765`)
- **Data directory** — where the index and content archives are stored
- **Admin token** — auto-generated root credential, stored in `server.toml`

After installation it writes an annotated `server.toml`, installs the systemd service, enables and starts it, then prints the admin token.

To connect clients, create a one-time invite for each (valid 15 minutes, single use):

```sh
find-admin invite create --name synology1 --scope update-index
# Invite code:  7K3M-9P2Q
```

Scopes are `read` (search and browse), `update-index` (adds indexing/uploads — what scanners need) and `admin` (everything). Keep the admin token on the server; clients should use invites.

**Install a specific version:**
```sh
VERSION=v0.2.4 curl -fsSL https://raw.githubusercontent.com/jamietre/find-anything/master/install-server.sh | sh
```

**Skip all prompts (scripted/CI use):**
```sh
SKIP_CONFIG=1 curl -fsSL https://raw.githubusercontent.com/jamietre/find-anything/master/install-server.sh | sh
```

---

## Client (Linux & macOS)

Run this on each machine whose files you want to index:

```sh
curl -fsSL https://raw.githubusercontent.com/jamietre/find-anything/master/install.sh | sh
```

The script will ask for:

- **Install directory** — default `~/.local/bin`
- **Server URL** — the URL of your `find-server` (e.g. `http://192.168.1.10:8765`)
- **Invite code** — from `find-admin invite create` on the server (a raw token also works)
- **Directories to watch** — the paths you want indexed

It then writes `client.toml`, installs the `find-watch` systemd user service, enables it, and prints the `find-scan` command to run for the initial index.

**After client installation:**
```sh
# Run the initial full scan (only needed once)
find-scan

# Verify find-watch is running
systemctl --user status find-watch
```

`find-watch` handles all subsequent updates automatically. `find-scan` can be re-run at any time to catch up if `find-watch` was offline.

---

## Windows client

Download the installer from [GitHub Releases](https://github.com/jamietre/find-anything/releases/latest) and run it.

The setup wizard asks for:

- Server URL and invite code (or bearer token)
- Directories to watch

It then registers `find-watch` as a Windows service (auto-starts on login), runs the initial scan, and creates a Start Menu shortcut.

**Manual service management (PowerShell as Administrator):**
```powershell
Stop-Service find-watch
Start-Service find-watch
Get-Service find-watch
```

Config file location: `%APPDATA%\find-anything\client.toml`

---

## Docker

For running the server in a container:

```sh
git clone https://github.com/jamietre/find-anything
cd find-anything
cp examples/server.toml server.toml
# Edit server.toml: set a strong token and data_dir
docker compose up -d
```

The `docker-compose.yml` mounts `./server.toml` and the data directory. Adjust the bind mount for `data_dir` to wherever you want the index stored persistently.

To run a specific version, edit the image tag in `docker-compose.yml`:
```yaml
image: ghcr.io/jamietre/find-anything:v0.2.4
```

---

## Build from source

Requires Rust (stable toolchain) and Node.js with pnpm (for the web UI).

```sh
git clone https://github.com/jamietre/find-anything
cd find-anything

# Build server + all client binaries
cargo build --release

# Build web UI
cd web && pnpm install && pnpm build
```

Binaries are placed in `target/release/`.

**Cross-compilation** (e.g. ARM7 for a NAS):
```sh
rustup target add armv7-unknown-linux-musleabihf
cargo build --release --target armv7-unknown-linux-musleabihf
```

See [DEVELOPMENT.md](../../DEVELOPMENT.md) for the full dev setup, mise tasks, and CI pipeline notes.

---

## What gets installed

| Binary | Role |
|---|---|
| `find-server` | Central index server |
| `find-scan` | Initial filesystem scanner |
| `find-watch` | Real-time file watcher |
| `find-anything` | CLI search client |
| `find-admin` | Admin utilities |
| `find-extract-*` | Extractor sub-processes (must be co-located with `find-watch`) |

The `find-extract-*` binaries are invoked by `find-watch` as subprocesses during extraction. They must be in the same directory as `find-watch`, or on `PATH`, or their location must be set in `client.toml` via `watch.extractor_dir`.

---

[Next: Configuration →](02-configuration.md)
