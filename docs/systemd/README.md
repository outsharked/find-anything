# Systemd Unit Files for find-anything

This directory contains systemd unit files for running `find-server` and
`find-watch` as background services.

## Directory Layout

```
systemd/
  user/            # User-mode units (personal workstation)
    find-server.service
    find-watch.service
  system/          # System-mode units (multi-user server)
    find-server.service
    find-watch@.service   # Template unit (instantiated per-user)
```

---

## User-Mode Setup (Personal Workstation)

User-mode services run under your own account, without root. This is the
recommended setup for a personal development machine.

### 1. Install binaries

```sh
cargo build --release
sudo cp target/release/find-server  /usr/local/bin/
sudo cp target/release/find-watch   /usr/local/bin/
sudo cp target/release/find-extract-text    /usr/local/bin/
sudo cp target/release/find-extract-pdf     /usr/local/bin/
sudo cp target/release/find-extract-media   /usr/local/bin/
sudo cp target/release/find-extract-archive /usr/local/bin/
```

### 2. Install config files

```sh
mkdir -p ~/.config/find-anything

# Create ~/.config/find-anything/server.toml
cat > ~/.config/find-anything/server.toml <<EOF
[server]
bind     = "127.0.0.1:8080"
data_dir = "/home/$USER/.local/share/find-anything"
token    = "change-me"
EOF

# Create ~/.config/find-anything/client.toml
cat > ~/.config/find-anything/client.toml <<EOF
[server]
url   = "http://localhost:8080"
token = "change-me"

[[sources]]
name  = "home"
path = "/home/$USER"
include = ["documents/**", "projects/**"]

[watch]
debounce_ms = 500
EOF
```

### 3. Install unit files

```sh
mkdir -p ~/.config/systemd/user
cp docs/systemd/user/find-server.service ~/.config/systemd/user/
cp docs/systemd/user/find-watch.service  ~/.config/systemd/user/
systemctl --user daemon-reload
```

### 4. Enable and start

```sh
systemctl --user enable --now find-server.service
systemctl --user enable --now find-watch.service
```

### 5. Initial index

Run `find-scan` once to populate the initial index before the watcher takes
over incremental updates:

```sh
find-scan --config ~/.config/find-anything/client.toml
```

### Useful commands

```sh
# Check status
systemctl --user status find-server
systemctl --user status find-watch

# Tail logs
journalctl --user -u find-server -f
journalctl --user -u find-watch -f

# Debug logging
RUST_LOG=find_watch=debug journalctl --user -u find-watch -f

# Restart after upgrade
systemctl --user restart find-server find-watch
```

---

## System-Mode Setup (Multi-User Server)

System-mode services run as a dedicated `find-anything` user (for the server)
or as named users (for per-user watchers via the template unit).

### 1. Create service account

```sh
sudo useradd --system --no-create-home --shell /sbin/nologin find-anything
sudo mkdir -p /var/lib/find-anything
sudo chown find-anything:find-anything /var/lib/find-anything
```

### 2. Install binaries and config

```sh
# Binaries (same as user-mode step 1 above)

sudo mkdir -p /etc/find-anything
sudo cp /path/to/server.toml /etc/find-anything/server.toml
sudo cp /path/to/client.toml /etc/find-anything/client.toml
sudo chmod 640 /etc/find-anything/server.toml /etc/find-anything/client.toml
sudo chown root:find-anything /etc/find-anything/server.toml
```

### 3. Install unit files

```sh
sudo cp docs/systemd/system/find-server.service    /etc/systemd/system/
sudo cp docs/systemd/system/find-watch@.service    /etc/systemd/system/
sudo systemctl daemon-reload
```

### 4. Enable and start

```sh
# Start the server
sudo systemctl enable --now find-server.service

# Start the watcher for a specific user (e.g. "alice")
sudo systemctl enable --now find-watch@alice.service
```

### 5. Initial index (per user)

```sh
sudo -u alice find-scan --config /etc/find-anything/client.toml
```

### Useful commands

```sh
# Logs
journalctl -u find-server -f
journalctl -u find-watch@alice -f
```

---

## Config File Reference

### `server.toml`

```toml
[server]
bind     = "127.0.0.1:8080"   # listen address
data_dir = "/var/lib/find-anything"
token    = "your-secret-token"
```

### `client.toml`

```toml
[server]
url   = "http://localhost:8080"
token = "your-secret-token"

[[sources]]
name  = "home"
paths = ["/home/alice/documents", "/home/alice/projects"]

[scan]
max_file_size_mb = 10
exclude = ["**/.git/**", "**/node_modules/**", "**/target/**"]

[scan.archives]
max_depth = 10

[watch]
debounce_ms   = 500
extractor_dir = "/usr/local/bin"   # optional — auto-detected if omitted
```

---

## How `find-scan` Fits In

`find-watch` does **not** do an initial scan on startup. The expected workflow
is:

1. Run `find-scan` once to build the initial index.
2. Start `find-watch` to keep the index current as files change.

To re-index everything with latest version of client (e.g. after an upgrade):

```sh
find-scan --config ~/.config/find-anything/client.toml --upgrade
```

---

## Debug Logging

Set `RUST_LOG` to increase verbosity:

```sh
# In unit file [Service] section:
Environment=RUST_LOG=find_watch=debug

# Or for systemd-analyze verify (no runtime):
RUST_LOG=find_watch=debug find-watch --config client.toml
```

---

## Validating Unit Files

```sh
systemd-analyze verify docs/systemd/user/find-watch.service
systemd-analyze verify docs/systemd/system/find-watch@.service
```
