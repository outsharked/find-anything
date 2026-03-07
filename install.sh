#!/bin/sh
# find-anything client installer
# Installs find-scan, find-watch, find-anything, and the extractor binaries.
# Configures the client to talk to a running find-anything server.
#
# Usage: curl -fsSL https://raw.githubusercontent.com/jamietre/find-anything/master/install.sh | sh
#
# For server installation use install-server.sh instead:
#   curl -fsSL https://raw.githubusercontent.com/jamietre/find-anything/master/install-server.sh | sh
#
# SKIP_CONFIG=1 skips all interactive prompts (binaries only, no config written)

set -e

# Print the appropriate `systemctl restart find-watch` command for this machine.
print_restart_cmd() {
  set +e
  if systemctl --user status find-watch >/dev/null 2>&1; then
    echo "  systemctl --user restart find-watch"
  elif systemctl status find-watch >/dev/null 2>&1; then
    echo "  sudo systemctl restart find-watch"
  else
    echo "  systemctl --user restart find-watch   # or: sudo systemctl restart find-watch"
  fi
  set -e
}

REPO="jamietre/find-anything"

# ── Detect platform ────────────────────────────────────────────────────────────

OS="$(uname -s)"
ARCH="$(uname -m)"

case "$OS" in
  Linux)  OS_NAME="linux" ;;
  Darwin) OS_NAME="macos" ;;
  *)
    echo "Unsupported OS: $OS"
    exit 1
    ;;
esac

case "$ARCH" in
  x86_64)          ARCH_NAME="x86_64" ;;
  aarch64 | arm64) ARCH_NAME="aarch64" ;;
  armv7l)          ARCH_NAME="armv7" ;;
  *)
    echo "Unsupported architecture: $ARCH"
    exit 1
    ;;
esac

PLATFORM="${OS_NAME}-${ARCH_NAME}"

# ── Resolve version ────────────────────────────────────────────────────────────

LATEST_VERSION="$(curl -fsSL "https://api.github.com/repos/${REPO}/releases/latest" \
  | grep '"tag_name"' \
  | sed 's/.*"tag_name": *"\([^"]*\)".*/\1/')"
if [ -z "$LATEST_VERSION" ]; then
  LATEST_VERSION="(unknown)"
fi

if [ -n "$VERSION" ]; then
  echo "Latest version: ${LATEST_VERSION}"
  echo "Using VERSION override: ${VERSION}"
else
  VERSION="$LATEST_VERSION"
  echo "Latest version: ${VERSION}"
fi

if [ -z "$VERSION" ] || [ "$VERSION" = "(unknown)" ]; then
  echo "Could not determine latest version. Set VERSION explicitly and retry." >&2
  exit 1
fi

INSTALL_DIR="${INSTALL_DIR:-$HOME/.local/bin}"

# ── Install directory prompt ───────────────────────────────────────────────────

if [ "${SKIP_CONFIG:-0}" != "1" ]; then
  printf "Install directory [%s]: " "$INSTALL_DIR"
  read -r INSTALL_DIR_INPUT </dev/tty
  INSTALL_DIR="${INSTALL_DIR_INPUT:-$INSTALL_DIR}"
fi

echo ""
echo "Installing find-anything ${VERSION} (${PLATFORM}) to ${INSTALL_DIR}..."

# ── Download and extract ───────────────────────────────────────────────────────

TARBALL="find-anything-${VERSION}-${PLATFORM}.tar.gz"
URL="https://github.com/${REPO}/releases/download/${VERSION}/${TARBALL}"
TMPDIR="$(mktemp -d)"
trap 'rm -rf "$TMPDIR"' EXIT

echo "Downloading ${URL}..."
curl -fsSL "$URL" -o "${TMPDIR}/${TARBALL}"

echo "Extracting..."
tar -xzf "${TMPDIR}/${TARBALL}" -C "${TMPDIR}"

# ── Install binaries ───────────────────────────────────────────────────────────

mkdir -p "$INSTALL_DIR"
EXTRACTED_DIR="${TMPDIR}/find-anything-${VERSION}-${PLATFORM}"

BINARIES="find-anything find-scan find-watch find-server find-admin \
  find-extract-text find-extract-pdf find-extract-media find-extract-archive \
  find-extract-html find-extract-office find-extract-epub"

for bin in $BINARIES; do
  if [ -f "${EXTRACTED_DIR}/${bin}" ]; then
    install -m 755 "${EXTRACTED_DIR}/${bin}" "${INSTALL_DIR}/${bin}"
  fi
done

echo ""
echo "Installed to: ${INSTALL_DIR}"
echo "  find-server          — search server"
echo "  find-scan            — initial indexer"
echo "  find-watch           — incremental file watcher"
echo "  find-anything        — command-line search client"
echo "  find-admin           — admin utilities: config, stats, sources, check, inbox"
echo "  find-extract-*       — extractor binaries (used by find-watch)"
echo ""

# ── PATH check ────────────────────────────────────────────────────────────────

case ":$PATH:" in
  *":${INSTALL_DIR}:"*) ;;
  *)
    echo "NOTE: ${INSTALL_DIR} is not in your PATH."
    echo "Add this to your shell profile:"
    echo ""
    echo "  export PATH=\"\$HOME/.local/bin:\$PATH\""
    echo ""
    ;;
esac

# ── Configuration ─────────────────────────────────────────────────────────────

if [ "${SKIP_CONFIG:-0}" = "1" ]; then
  echo "Skipping configuration (SKIP_CONFIG=1)."
  exit 0
fi

# Determine config directory
if [ -n "$XDG_CONFIG_HOME" ]; then
  CONFIG_DIR="$XDG_CONFIG_HOME/find-anything"
else
  CONFIG_DIR="$HOME/.config/find-anything"
fi
CONFIG_FILE="$CONFIG_DIR/client.toml"

if [ -f "$CONFIG_FILE" ]; then
  echo "Configuration already exists at $CONFIG_FILE"
  printf "Re-configure? [y/N] "
  read -r RECONFIGURE </dev/tty
  case "$RECONFIGURE" in
    y|Y) ;;
    *)
      echo "Skipping configuration. Existing config preserved."
      echo ""
      echo "Restart the watcher to pick up the new binary:"
      echo ""
      print_restart_cmd
      echo ""
      exit 0
      ;;
  esac
fi

echo "Client configuration"
echo "  find-anything server URL and token (from your server's server.toml)."
echo ""

while true; do
  printf "Server URL [http://localhost:8765]: "
  read -r SERVER_URL </dev/tty
  SERVER_URL="${SERVER_URL:-http://localhost:8765}"

  # Test connectivity (no auth needed — just checking the server is up)
  printf "Checking server connectivity... "
  if curl -fsS --max-time 5 "${SERVER_URL}/" >/dev/null 2>&1; then
    echo "OK"
    break
  else
    echo "no response"
    echo ""
    echo "WARNING: Could not reach ${SERVER_URL}"
    echo "  Make sure the server is running and the URL is correct."
    echo ""
    printf "Re-enter URL, or press enter to continue anyway? [re-enter/continue]: "
    read -r CONN_CHOICE </dev/tty
    case "${CONN_CHOICE:-re-enter}" in
      c|co|con|cont|conti|contin|continu|continue) break ;;
      *) echo "" ;;
    esac
  fi
done

printf "Bearer token (from server.toml): "
read -r TOKEN </dev/tty

if [ -z "$TOKEN" ]; then
  echo "Token cannot be empty." >&2
  exit 1
fi

DEFAULT_SOURCE_NAME="$(hostname | cut -d. -f1)"
printf "Source name (identifies this machine in search results) [%s]: " "$DEFAULT_SOURCE_NAME"
read -r SOURCE_NAME </dev/tty
SOURCE_NAME="${SOURCE_NAME:-$DEFAULT_SOURCE_NAME}"

printf "Directory to index [%s]: " "$HOME"
read -r DIR_INPUT </dev/tty
if [ -z "$DIR_INPUT" ]; then
  DIR_INPUT="$HOME"
fi

# ── Write client.toml ─────────────────────────────────────────────────────────

mkdir -p "$CONFIG_DIR"

# Escape URL, token, source name, and path for TOML
SERVER_URL_ESC="$(printf '%s' "$SERVER_URL" | sed 's/\\/\\\\/g; s/"/\\"/g')"
TOKEN_ESC="$(printf '%s' "$TOKEN" | sed 's/\\/\\\\/g; s/"/\\"/g')"
SOURCE_NAME_ESC="$(printf '%s' "$SOURCE_NAME" | sed 's/\\/\\\\/g; s/"/\\"/g')"
DIR_ESC="$(printf '%s' "$DIR_INPUT" | sed 's/\\/\\\\/g; s/"/\\"/g')"

cat > "$CONFIG_FILE" <<EOF
[server]
url   = "$SERVER_URL_ESC"
token = "$TOKEN_ESC"

[[sources]]
name = "$SOURCE_NAME_ESC"
path = "$DIR_ESC"
# include  = []   # Glob patterns to limit indexing (e.g. ["docs/**", "src/**"])

[scan]
# max_file_size_mb = 10   # Skip files larger than this (MB)
# max_line_length  = 120    # Wrap long lines at this column (0 = disable)
# follow_symlinks  = false
# include_hidden   = false  # Index dot-files and dot-directories
# Extra glob patterns to skip, added to the built-in defaults.
# Use exclude = [...] instead to replace the defaults entirely.
# exclude_extra = []

[scan.archives]
# enabled   = true
# max_depth = 10   # Max nesting depth for archives-within-archives

[watch]
# debounce_ms   = 500   # Wait this long (ms) after last change before re-indexing
# extractor_dir = ""    # Path to find-extract-* binaries (default: auto-detect)

[tray]
# poll_interval_ms = 1000   # Refresh interval while popup is open (ms)
EOF

echo ""
echo "Configuration written to: $CONFIG_FILE"
echo "  Edit this file to add more sources, change exclude patterns, etc."

# ── Install systemd user service ──────────────────────────────────────────────

echo ""
echo "Setting up find-watch service..."

if command -v systemctl >/dev/null 2>&1 && systemctl --user status >/dev/null 2>&1; then
  # systemd user session is active — install as a user service
  SYSTEMD_USER_DIR="$HOME/.config/systemd/user"
  mkdir -p "$SYSTEMD_USER_DIR"

  cat > "$SYSTEMD_USER_DIR/find-watch.service" <<EOF
[Unit]
Description=find-anything file watcher
After=network.target

[Service]
Type=simple
ExecStart=${INSTALL_DIR}/find-watch --config ${CONFIG_FILE}
Restart=on-failure
RestartSec=5s
Environment=RUST_LOG=find_watch=info
Environment=PATH=${INSTALL_DIR}:/usr/local/bin:/usr/bin:/bin

[Install]
WantedBy=default.target
EOF

  cat > "$SYSTEMD_USER_DIR/find-scan.service" <<EOF
[Unit]
Description=find-anything initial index scan
After=network.target

[Service]
Type=oneshot
ExecStart=${INSTALL_DIR}/find-scan --config ${CONFIG_FILE}
Environment=RUST_LOG=find_scan=info
Environment=PATH=${INSTALL_DIR}:/usr/local/bin:/usr/bin:/bin
EOF

  systemctl --user daemon-reload
  systemctl --user enable find-watch
  systemctl --user start find-watch

  WATCH_SERVICE_TYPE="user"
  SCAN_SERVICE_TYPE="user"
  echo ""
  echo "find-watch systemd user service installed and started."
  echo "  Status:  systemctl --user status find-watch"
  echo "  Logs:    journalctl --user -u find-watch -f"
  echo "  Stop:    systemctl --user stop find-watch"

elif command -v systemctl >/dev/null 2>&1 && [ -d "/run/systemd/system" ]; then
  # systemd is present but no user session (headless server, Synology DSM, etc.).
  # Try to install as a system service directly.  Fall back to staging with
  # instructions if we can't write to /etc/systemd/system.

  CURRENT_USER=$(id -un)

  # Determine sudo prefix: empty if root, "sudo" if passwordless sudo available.
  if [ "$(id -u)" = "0" ]; then
    SYSD_SUDO=""
    CAN_INSTALL_SYSTEM=1
  elif command -v sudo >/dev/null 2>&1 && sudo -n true 2>/dev/null; then
    SYSD_SUDO="sudo"
    CAN_INSTALL_SYSTEM=1
  else
    CAN_INSTALL_SYSTEM=0
  fi

  WATCH_UNIT_CONTENT="[Unit]
Description=find-anything file watcher
After=network.target

[Service]
User=${CURRENT_USER}
ExecStart=${INSTALL_DIR}/find-watch --config ${CONFIG_FILE}
Restart=on-failure
RestartSec=5s
Environment=RUST_LOG=find_watch=info
Environment=PATH=${INSTALL_DIR}:/usr/local/bin:/usr/bin:/bin

[Install]
WantedBy=multi-user.target"

  SCAN_UNIT_CONTENT="[Unit]
Description=find-anything initial index scan
After=network.target

[Service]
User=${CURRENT_USER}
Type=oneshot
ExecStart=${INSTALL_DIR}/find-scan --config ${CONFIG_FILE}
Environment=RUST_LOG=find_scan=info
Environment=PATH=${INSTALL_DIR}:/usr/local/bin:/usr/bin:/bin"

  if [ "$CAN_INSTALL_SYSTEM" = "1" ]; then
    printf '%s\n' "$WATCH_UNIT_CONTENT" | $SYSD_SUDO tee /etc/systemd/system/find-watch.service > /dev/null
    printf '%s\n' "$SCAN_UNIT_CONTENT"  | $SYSD_SUDO tee /etc/systemd/system/find-scan.service  > /dev/null
    $SYSD_SUDO systemctl daemon-reload
    $SYSD_SUDO systemctl enable find-watch
    $SYSD_SUDO systemctl start find-watch

    WATCH_SERVICE_TYPE="system"
    SCAN_SERVICE_TYPE="system"
    echo ""
    echo "find-watch system service installed and started."
    echo "  Status:  sudo systemctl status find-watch"
    echo "  Logs:    sudo journalctl -u find-watch -f"
    echo "  Stop:    sudo systemctl stop find-watch"
  else
    # No root/sudo — write to staging and give manual instructions.
    UNIT_STAGING_WATCH="$HOME/.config/find-anything/find-watch.service"
    UNIT_STAGING_SCAN="$HOME/.config/find-anything/find-scan.service"
    printf '%s\n' "$WATCH_UNIT_CONTENT" > "$UNIT_STAGING_WATCH"
    printf '%s\n' "$SCAN_UNIT_CONTENT"  > "$UNIT_STAGING_SCAN"

    echo ""
    echo "Could not install system service (sudo not available)."
    echo "Unit files written to:"
    echo "  $UNIT_STAGING_WATCH"
    echo "  $UNIT_STAGING_SCAN"
    echo ""
    echo "To install manually:"
    echo "  sudo mv $UNIT_STAGING_WATCH /etc/systemd/system/find-watch.service"
    echo "  sudo mv $UNIT_STAGING_SCAN  /etc/systemd/system/find-scan.service"
    echo "  sudo systemctl daemon-reload"
    echo "  sudo systemctl enable find-watch"
    echo "  sudo systemctl start find-watch"

    WATCH_SERVICE_TYPE="system"
    SCAN_SERVICE_TYPE="system"
  fi

elif [ "$OS_NAME" = "macos" ]; then
  # macOS: suggest launchd
  PLIST_DIR="$HOME/Library/LaunchAgents"
  mkdir -p "$PLIST_DIR"

  cat > "$PLIST_DIR/com.jamietre.find-watch.plist" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key>
  <string>com.jamietre.find-watch</string>
  <key>ProgramArguments</key>
  <array>
    <string>${INSTALL_DIR}/find-watch</string>
    <string>--config</string>
    <string>${CONFIG_FILE}</string>
  </array>
  <key>RunAtLoad</key>
  <true/>
  <key>KeepAlive</key>
  <true/>
  <key>StandardOutPath</key>
  <string>${HOME}/Library/Logs/find-watch.log</string>
  <key>StandardErrorPath</key>
  <string>${HOME}/Library/Logs/find-watch.log</string>
</dict>
</plist>
EOF

  cat > "$PLIST_DIR/com.jamietre.find-scan.plist" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key>
  <string>com.jamietre.find-scan</string>
  <key>ProgramArguments</key>
  <array>
    <string>${INSTALL_DIR}/find-scan</string>
    <string>--config</string>
    <string>${CONFIG_FILE}</string>
  </array>
  <key>RunAtLoad</key>
  <false/>
  <key>KeepAlive</key>
  <false/>
  <key>StandardOutPath</key>
  <string>${HOME}/Library/Logs/find-scan.log</string>
  <key>StandardErrorPath</key>
  <string>${HOME}/Library/Logs/find-scan.log</string>
</dict>
</plist>
EOF

  launchctl load "$PLIST_DIR/com.jamietre.find-watch.plist"
  launchctl load "$PLIST_DIR/com.jamietre.find-scan.plist"

  WATCH_SERVICE_TYPE="macos"
  SCAN_SERVICE_TYPE="macos"
  echo ""
  echo "find-watch launchd agent installed and started."
  echo "  Status:  launchctl list com.jamietre.find-watch"
  echo "  Logs:    tail -f ~/Library/Logs/find-watch.log"
  echo "  Stop:    launchctl unload $PLIST_DIR/com.jamietre.find-watch.plist"

else
  # No systemd at all
  WATCH_SERVICE_TYPE="manual"
  SCAN_SERVICE_TYPE="manual"
  echo ""
  echo "Autostart not configured (systemd not detected)."
  echo "Start find-watch manually:"
  echo ""
  echo "  ${INSTALL_DIR}/find-watch --config ${CONFIG_FILE}"
fi

# ── Summary ───────────────────────────────────────────────────────────────────

echo ""
echo "Installation complete!"
echo ""
echo "  Server:    $SERVER_URL"
echo "  Binaries:  $INSTALL_DIR"
echo ""
echo "  Config:    $CONFIG_FILE"
echo "    ^ Edit this file to add sources, change excludes, etc."
echo ""
if [ "$WATCH_SERVICE_TYPE" = "user" ]; then
  echo "Service commands:"
  echo "  systemctl --user status find-watch"
  echo "  systemctl --user restart find-watch"
  echo "  systemctl --user stop find-watch"
  echo "  journalctl --user -u find-watch -f"
elif [ "$WATCH_SERVICE_TYPE" = "system" ]; then
  echo "Service commands:"
  echo "  sudo systemctl status find-watch"
  echo "  sudo systemctl restart find-watch"
  echo "  sudo systemctl stop find-watch"
  echo "  sudo journalctl -u find-watch -f"
elif [ "$WATCH_SERVICE_TYPE" = "macos" ]; then
  echo "Service commands:"
  echo "  launchctl list com.jamietre.find-watch"
  echo "  launchctl start com.jamietre.find-watch"
  echo "  launchctl stop com.jamietre.find-watch"
  echo "  tail -f ~/Library/Logs/find-watch.log"
fi
echo ""
echo "If upgrading, restart the watcher to pick up the new binary:"
echo ""
print_restart_cmd
echo ""
echo "──────────────────────────────────────────────────────────────"
echo "First-time setup: run the initial scan"
echo "──────────────────────────────────────────────────────────────"
echo ""
echo "The initial scan indexes all configured directories. It can take"
echo "a long time on large collections — run it as a background service"
echo "so a disconnected terminal won't kill it."
echo ""
if [ "$SCAN_SERVICE_TYPE" = "user" ]; then
  echo "  systemctl --user start find-scan"
  echo ""
  echo "Follow progress:"
  echo "  journalctl --user -u find-scan -f"
elif [ "$SCAN_SERVICE_TYPE" = "system" ]; then
  echo "  sudo systemctl start find-scan"
  echo ""
  echo "Follow progress:"
  echo "  sudo journalctl -u find-scan -f"
elif [ "$SCAN_SERVICE_TYPE" = "macos" ]; then
  echo "  launchctl start com.jamietre.find-scan"
  echo ""
  echo "Follow progress:"
  echo "  tail -f ~/Library/Logs/find-scan.log"
else
  echo "  ${INSTALL_DIR}/find-scan --config ${CONFIG_FILE}"
fi
echo ""
echo "For a full re-scan (re-indexes every file from scratch):"
echo ""
if [ "$SCAN_SERVICE_TYPE" = "user" ]; then
  echo "  systemctl --user set-environment FIND_SCAN_ARGS=--full"
  echo "  systemctl --user start find-scan"
  echo "  systemctl --user unset-environment FIND_SCAN_ARGS"
  echo ""
  echo "Or directly:"
fi
echo "  ${INSTALL_DIR}/find-scan --config ${CONFIG_FILE} --full"
echo ""
