#!/bin/bash
# Basic error handling for old bash compatibility
set +e  # disable for now
set +u  # disable for now

# Create/refresh convenient symlinks in the repo root to release binaries.
# Usage:
#   ./scripts/link-binaries.sh           # assumes you already built
#   ./scripts/link-binaries.sh --build   # build release first, then link

ROOT_DIR="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT_DIR"

if [ "${1:-}" = "--build" ]; then
  echo "[link] building release…"
  cargo build --release
fi

BIN_DIR="$ROOT_DIR/target/release"

link_bin() {
  local name="$1"
  if [ -x "$BIN_DIR/$name" ]; then
    ln -sfn "$BIN_DIR/$name" "$ROOT_DIR/$name"
    echo "[link] $name -> target/release/$name"
  else
    echo "[link] warn: $BIN_DIR/$name not found (build first?)" >&2
  fi
}

link_bin daizo-mcp
link_bin daizo-cli

echo "[link] done. You can now run ./daizo-mcp and ./daizo-cli"

