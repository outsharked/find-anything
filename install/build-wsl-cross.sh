#!/usr/bin/env bash
# WSL-specific: cross-compile Windows binaries via Docker/cross (mingw-w64)
# and package them with Inno Setup (must be installed on the Windows host).
#
# For native Windows builds (preferred, matches CI), run from a PowerShell
# terminal on Windows:
#   cargo build --release --workspace --target x86_64-pc-windows-msvc
#
# See docs/windows/README.md for manual installation instructions.
set -euo pipefail

REPO_ROOT="$(dirname "$0")/.."
cd "$REPO_ROOT"

VERSION=$(grep -m1 '^version' crates/common/Cargo.toml | sed 's/version = "//; s/"//')
TARGET="${WINDOWS_TARGET:-x86_64-pc-windows-gnu}"
# WSL mounts Windows drives at /mnt/c; Git Bash uses /c — search both.
if [[ -z "${ISCC:-}" ]]; then
  for candidate in \
    "/mnt/c/Program Files (x86)/Inno Setup 6/ISCC.exe" \
    "/mnt/c/Program Files/Inno Setup 6/ISCC.exe" \
    "/mnt/c/Program Files (x86)/Inno Setup 5/ISCC.exe" \
    "/c/Program Files (x86)/Inno Setup 6/ISCC.exe" \
    "/c/Program Files/Inno Setup 6/ISCC.exe"
  do
    if [[ -f "$candidate" ]]; then
      ISCC="$candidate"
      break
    fi
  done
  if [[ -z "${ISCC:-}" ]]; then
    echo "ERROR: ISCC.exe not found. Install Inno Setup 6 or set ISCC=/path/to/ISCC.exe"
    exit 1
  fi
fi

echo "Building find-anything v${VERSION} for Windows (${TARGET})..."
# Pre-clean the web build output; SvelteKit's adapter-static uses rmSync which
# fails on WSL2 when the directory already exists and is non-empty.
rm -rf web/build
pnpm --dir web run build
cross build --release --target "$TARGET"

BIN_DIR="$(pwd)/target/${TARGET}/release"
cd packaging/windows
"$ISCC" "/DAppVersion=v${VERSION}" "/DBinDir=${BIN_DIR}" find-anything.iss
echo "Installer: packaging/windows/Output/find-anything-setup-v${VERSION}-windows-x86_64.exe"
