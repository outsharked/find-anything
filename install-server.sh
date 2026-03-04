#!/bin/sh
# find-anything server installer
# Installs find-server and configures it as a systemd service.
#
# Usage: curl -fsSL https://raw.githubusercontent.com/jamietre/find-anything/master/install-server.sh | sh
#
# For client installation use install.sh instead:
#   curl -fsSL https://raw.githubusercontent.com/jamietre/find-anything/master/install.sh | sh
#
# SKIP_CONFIG=1 skips all interactive prompts

set -e

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

echo ""
echo "Installing find-anything server ${VERSION} (${PLATFORM})..."

# ── Download and extract ───────────────────────────────────────────────────────

TARBALL="find-anything-${VERSION}-${PLATFORM}.tar.gz"
URL="https://github.com/${REPO}/releases/download/${VERSION}/${TARBALL}"
TMPDIR="$(mktemp -d)"
trap 'rm -rf "$TMPDIR"' EXIT

echo "Downloading ${URL}..."
curl -fsSL "$URL" -o "${TMPDIR}/${TARBALL}"

echo "Extracting..."
tar -xzf "${TMPDIR}/${TARBALL}" -C "${TMPDIR}"
EXTRACTED_DIR="${TMPDIR}/find-anything-${VERSION}-${PLATFORM}"

# ── Determine service mode ─────────────────────────────────────────────────────

if [ "${SKIP_CONFIG:-0}" = "1" ]; then
  SERVICE_MODE="user"
else
  echo ""
  echo "Service installation mode:"
  echo "  system — installs as a system service (requires root/sudo)"
  echo "           runs as a dedicated 'find-anything' user"
  echo "           config: /etc/find-anything/server.toml"
  echo "           data:   /var/lib/find-anything"
  echo "  user   — installs as a systemd user service (no root required)"
  echo "           config: ~/.config/find-anything/server.toml"
  echo "           data:   ~/.local/share/find-anything"
  echo ""
  printf "Install mode [system/user] (default: user): "
  read -r SERVICE_MODE </dev/tty
  SERVICE_MODE="${SERVICE_MODE:-user}"
fi

case "$SERVICE_MODE" in
  system)
    if [ "$(id -u)" -ne 0 ]; then
      echo "System service installation requires root. Re-run with sudo." >&2
      exit 1
    fi
    DEFAULT_INSTALL_DIR="/usr/local/bin"
    CONFIG_DIR="/etc/find-anything"
    DATA_DIR="/var/lib/find-anything"
    SERVICE_USER="find-anything"
    ;;
  user)
    DEFAULT_INSTALL_DIR="$HOME/.local/bin"
    if [ -n "$XDG_CONFIG_HOME" ]; then
      CONFIG_DIR="$XDG_CONFIG_HOME/find-anything"
    else
      CONFIG_DIR="$HOME/.config/find-anything"
    fi
    if [ -n "$XDG_DATA_HOME" ]; then
      DATA_DIR="$XDG_DATA_HOME/find-anything"
    else
      DATA_DIR="$HOME/.local/share/find-anything"
    fi
    SERVICE_USER=""
    ;;
  *)
    echo "Unknown mode '$SERVICE_MODE'. Choose 'system' or 'user'." >&2
    exit 1
    ;;
esac

if [ "${SKIP_CONFIG:-0}" = "1" ]; then
  INSTALL_DIR="${INSTALL_DIR:-$DEFAULT_INSTALL_DIR}"
else
  printf "Install directory [%s]: " "$DEFAULT_INSTALL_DIR"
  read -r INSTALL_DIR_INPUT </dev/tty
  INSTALL_DIR="${INSTALL_DIR_INPUT:-$DEFAULT_INSTALL_DIR}"
fi

CONFIG_FILE="$CONFIG_DIR/server.toml"

# ── Install binaries ──────────────────────────────────────────────────────────

mkdir -p "$INSTALL_DIR"

BINARIES="find-server find-anything find-scan find-watch \
  find-extract-text find-extract-pdf find-extract-media find-extract-archive \
  find-extract-html find-extract-office find-extract-epub"

for bin in $BINARIES; do
  if [ -f "${EXTRACTED_DIR}/${bin}" ]; then
    install -m 755 "${EXTRACTED_DIR}/${bin}" "${INSTALL_DIR}/${bin}"
  fi
done

echo "Installed binaries to ${INSTALL_DIR}/"

# ── PATH check ────────────────────────────────────────────────────────────────

case ":$PATH:" in
  *":${INSTALL_DIR}:"*) ;;
  *)
    echo ""
    echo "NOTE: ${INSTALL_DIR} is not in your PATH."
    echo "Add this to your shell profile:"
    echo "  export PATH=\"${INSTALL_DIR}:\$PATH\""
    ;;
esac

# ── Configuration ─────────────────────────────────────────────────────────────

if [ "${SKIP_CONFIG:-0}" = "1" ]; then
  echo "Skipping configuration (SKIP_CONFIG=1)."
  exit 0
fi

if [ -f "$CONFIG_FILE" ]; then
  echo ""
  echo "Configuration already exists at $CONFIG_FILE"
  printf "Re-configure? [y/N] "
  read -r RECONFIGURE </dev/tty
  case "$RECONFIGURE" in
    y|Y) ;;
    *)
      echo "Skipping configuration. Existing config preserved."
      exit 0
      ;;
  esac
fi

echo ""
echo "Server configuration"
echo ""

printf "Bind address [127.0.0.1:8765]: "
read -r BIND_ADDR </dev/tty
BIND_ADDR="${BIND_ADDR:-127.0.0.1:8765}"

printf "Data directory [%s]: " "$DATA_DIR"
read -r DATA_DIR_INPUT </dev/tty
if [ -n "$DATA_DIR_INPUT" ]; then
  DATA_DIR="$DATA_DIR_INPUT"
fi

# Generate a secure token if not already present
EXISTING_TOKEN=""
if [ -f "$CONFIG_FILE" ]; then
  EXISTING_TOKEN="$(grep 'token' "$CONFIG_FILE" | sed 's/.*token *= *"\([^"]*\)".*/\1/')"
fi

if [ -n "$EXISTING_TOKEN" ] && [ "$EXISTING_TOKEN" != "CHANGE_ME" ]; then
  printf "Bearer token [keep existing]: "
  read -r TOKEN_INPUT </dev/tty
  TOKEN="${TOKEN_INPUT:-$EXISTING_TOKEN}"
else
  # Auto-generate a token
  if command -v openssl >/dev/null 2>&1; then
    GENERATED="$(openssl rand -base64 32)"
  else
    GENERATED="$(head -c 32 /dev/urandom | base64 | tr -d '\n=')"
  fi
  printf "Bearer token [generated: press enter to use]: "
  printf "\n  %s\n" "$GENERATED"
  printf "Token (leave blank to use generated): "
  read -r TOKEN_INPUT </dev/tty
  TOKEN="${TOKEN_INPUT:-$GENERATED}"
fi

if [ -z "$TOKEN" ]; then
  echo "Token cannot be empty." >&2
  exit 1
fi

# ── Write server.toml ─────────────────────────────────────────────────────────

mkdir -p "$CONFIG_DIR"
mkdir -p "$DATA_DIR"

BIND_ESC="$(printf '%s' "$BIND_ADDR" | sed 's/\\/\\\\/g; s/"/\\"/g')"
DATA_DIR_ESC="$(printf '%s' "$DATA_DIR" | sed 's/\\/\\\\/g; s/"/\\"/g')"
TOKEN_ESC="$(printf '%s' "$TOKEN" | sed 's/\\/\\\\/g; s/"/\\"/g')"

cat > "$CONFIG_FILE" <<EOF
[server]
bind     = "$BIND_ESC"
data_dir = "$DATA_DIR_ESC"
token    = "$TOKEN_ESC"

# ── Per-source filesystem paths ───────────────────────────────────────────────
# When set, the server can serve files directly for inline viewing and download.
# The source name must match the name used in the client's [[sources]] config.
#
# [sources.home]
# path = "/home/myuser"
#
# [sources.work]
# path = "/mnt/work"

# [search]
# default_limit       = 50      # Default number of results per search
# max_limit           = 500     # Hard cap on results per search
# fts_candidate_limit = 2000    # FTS5 candidates evaluated per query
# context_window      = 1       # Lines of context shown around each match
EOF

# Restrict config permissions (contains the token)
chmod 600 "$CONFIG_FILE"

echo ""
echo "Configuration written to: $CONFIG_FILE"
echo "  Edit this file to change bind address, limits, etc."

# ── System service: create dedicated user ─────────────────────────────────────

if [ "$SERVICE_MODE" = "system" ]; then
  if ! id "$SERVICE_USER" >/dev/null 2>&1; then
    echo ""
    echo "Creating system user '$SERVICE_USER'..."
    useradd --system --no-create-home --shell /usr/sbin/nologin \
      --comment "find-anything server" "$SERVICE_USER"
  fi
  chown -R "$SERVICE_USER:$SERVICE_USER" "$DATA_DIR"
  chown "$SERVICE_USER:$SERVICE_USER" "$CONFIG_FILE"
fi

# ── Install systemd service ───────────────────────────────────────────────────

echo ""
echo "Setting up find-server service..."

if command -v systemctl >/dev/null 2>&1; then

  if [ "$SERVICE_MODE" = "system" ]; then
    SERVICE_FILE="/etc/systemd/system/find-server.service"
    cat > "$SERVICE_FILE" <<EOF
[Unit]
Description=find-anything search server
After=network.target

[Service]
Type=simple
User=${SERVICE_USER}
Group=${SERVICE_USER}
ExecStart=${INSTALL_DIR}/find-server --config ${CONFIG_FILE}
Restart=on-failure
RestartSec=5s
Environment=RUST_LOG=find_server=info
ReadWritePaths=${DATA_DIR}

[Install]
WantedBy=multi-user.target
EOF
    systemctl daemon-reload
    systemctl enable find-server
    systemctl start find-server
    echo ""
    echo "find-server system service installed and started."
    echo "  Status:  systemctl status find-server"
    echo "  Logs:    journalctl -u find-server -f"
    echo "  Stop:    systemctl stop find-server"

  else
    # User service
    SYSTEMD_USER_DIR="$HOME/.config/systemd/user"
    SERVICE_FILE="$SYSTEMD_USER_DIR/find-server.service"
    mkdir -p "$SYSTEMD_USER_DIR"
    cat > "$SERVICE_FILE" <<EOF
[Unit]
Description=find-anything search server
After=network.target

[Service]
Type=simple
ExecStart=${INSTALL_DIR}/find-server --config ${CONFIG_FILE}
Restart=on-failure
RestartSec=5s
Environment=RUST_LOG=find_server=info

[Install]
WantedBy=default.target
EOF
    systemctl --user daemon-reload
    systemctl --user enable find-server
    systemctl --user start find-server
    echo ""
    echo "find-server systemd user service installed and started."
    echo "  Status:  systemctl --user status find-server"
    echo "  Logs:    journalctl --user -u find-server -f"
    echo "  Stop:    systemctl --user stop find-server"
  fi

elif [ "$OS_NAME" = "macos" ]; then
  PLIST_DIR="$HOME/Library/LaunchAgents"
  PLIST_FILE="$PLIST_DIR/com.jamietre.find-server.plist"
  mkdir -p "$PLIST_DIR"
  cat > "$PLIST_FILE" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key>
  <string>com.jamietre.find-server</string>
  <key>ProgramArguments</key>
  <array>
    <string>${INSTALL_DIR}/find-server</string>
    <string>--config</string>
    <string>${CONFIG_FILE}</string>
  </array>
  <key>RunAtLoad</key>
  <true/>
  <key>KeepAlive</key>
  <true/>
  <key>StandardOutPath</key>
  <string>${HOME}/Library/Logs/find-server.log</string>
  <key>StandardErrorPath</key>
  <string>${HOME}/Library/Logs/find-server.log</string>
</dict>
</plist>
EOF
  launchctl load "$PLIST_FILE"
  echo ""
  echo "find-server launchd agent installed and started."
  echo "  Logs:  tail -f ~/Library/Logs/find-server.log"
  echo "  Stop:  launchctl unload $PLIST_FILE"

else
  echo ""
  echo "Autostart not configured (systemd not detected)."
  echo "Run find-server manually:"
  echo ""
  echo "  ${INSTALL_DIR}/find-server ${CONFIG_FILE}"
fi

# ── Summary ───────────────────────────────────────────────────────────────────

echo ""
echo "Server installation complete!"
echo ""
echo "  Listening:  $BIND_ADDR"
echo "  Data:       $DATA_DIR"
echo ""
echo "  Config:     $CONFIG_FILE"
echo "    ^ Edit this file to change settings, then restart the service."
echo ""
echo "  Token:      $TOKEN"
echo "    ^ Use this token when running install.sh on client machines."
echo ""
if [ "$SERVICE_MODE" = "system" ]; then
  echo "Service commands:"
  echo "  systemctl status find-server"
  echo "  systemctl stop find-server"
  echo "  systemctl start find-server"
  echo "  systemctl restart find-server"
  echo "  journalctl -u find-server -f"
elif [ "$OS_NAME" = "macos" ]; then
  echo "Service commands:"
  echo "  launchctl list com.jamietre.find-server"
  echo "  launchctl stop com.jamietre.find-server"
  echo "  launchctl start com.jamietre.find-server"
  echo "  tail -f ~/Library/Logs/find-server.log"
else
  echo "Service commands:"
  echo "  systemctl --user status find-server"
  echo "  systemctl --user stop find-server"
  echo "  systemctl --user start find-server"
  echo "  systemctl --user restart find-server"
  echo "  journalctl --user -u find-server -f"
fi
echo ""
echo "To index this machine, also run the client installer:"
echo "  curl -fsSL https://raw.githubusercontent.com/jamietre/find-anything/master/install.sh | sh"
