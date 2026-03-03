#!/usr/bin/env bash
# Copy the latest Windows installer to C:\temp so it is ready to run on Windows.
set -euo pipefail

REPO_ROOT="$(dirname "$0")/.."
OUTPUT_DIR="$REPO_ROOT/packaging/windows/Output"
# WSL mounts Windows drives at /mnt/c; Git Bash uses /c — try both.
if [[ -d "/mnt/c" ]]; then
  DEST_DIR="/mnt/c/temp"
else
  DEST_DIR="/c/temp"
fi

# Find the most recently built installer.
INSTALLER=$(ls -t "$OUTPUT_DIR"/find-anything-setup-*.exe 2>/dev/null | head -1)
if [[ -z "$INSTALLER" ]]; then
    echo "No installer found in $OUTPUT_DIR — run 'mise run build-windows' first."
    exit 1
fi

mkdir -p "$DEST_DIR"
cp "$INSTALLER" "$DEST_DIR/"
echo "Copied $(basename "$INSTALLER") → $DEST_DIR/"
