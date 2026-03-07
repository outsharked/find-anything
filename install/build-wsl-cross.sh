#!/usr/bin/env bash
# WSL-specific: cross-compile Windows binaries via Docker/cross (mingw-w64)
# and package them with Inno Setup (must be installed on the Windows host).
#
# ── Prerequisites ─────────────────────────────────────────────────────────────
#
# 1. Docker Desktop (running, WSL integration enabled for your distro)
#    Used by `cross` to run the mingw-w64 build container.
#
# 2. cross  — install once with:
#      cargo install cross --git https://github.com/cross-rs/cross
#
# 3. Inno Setup 6  — install on Windows:
#      winget install JRSoftware.InnoSetup
#    or download from https://jrsoftware.org/isinfo.php
#    The installer looks for ISCC.exe under /mnt/c/Program Files (x86)/Inno Setup 6/
#    You can also set ISCC=/path/to/ISCC.exe to override.
#
# 4. windres (for find-tray VERSIONINFO resource embedding)
#    windres is provided by the mingw-w64 toolchain INSIDE the cross Docker
#    image (ghcr.io/cross-rs/x86_64-pc-windows-gnu:edge) — no host
#    installation is required when building via `cross`.
#
#    If you ever build WITHOUT cross (i.e. plain cargo targeting
#    x86_64-pc-windows-gnu on a native Linux host), install the host-side
#    mingw toolchain which includes x86_64-w64-mingw32-windres:
#      sudo apt install gcc-mingw-w64-x86-64    # Debian/Ubuntu/WSL
#      sudo dnf install mingw64-gcc             # Fedora
#
#    On native Windows (x86_64-pc-windows-msvc), winres uses rc.exe from the
#    Windows SDK automatically — no separate windres install needed.
#
# ── Native Windows builds (preferred for CI/release) ─────────────────────────
#
#   From a PowerShell terminal:
#     cargo build --release --workspace --target x86_64-pc-windows-msvc
#     iscc /DAppVersion=vX.Y.Z /DBinDir=target\x86_64-pc-windows-msvc\release packaging\windows\find-anything.iss
#
# See docs/windows/README.md for full installation instructions.
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
# ISCC.exe is a Windows process; it needs Windows-style paths.
# wslpath -w converts /home/... → \\wsl.localhost\Distro\home\...
BIN_DIR_WIN="$(wslpath -w "${BIN_DIR}")"
cd packaging/windows
"$ISCC" "/DAppVersion=v${VERSION}" "/DBinDir=${BIN_DIR_WIN}" find-anything.iss
echo "Installer: packaging/windows/Output/find-anything-setup-v${VERSION}-windows-x86_64.exe"
