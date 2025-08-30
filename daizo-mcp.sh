#!/bin/bash
set -euo pipefail
# Resolve repo root as the directory containing this script
DIR="$(cd "$(dirname "$0")" && pwd)"
BIN="$DIR/target/release/daizo-mcp"
if [ ! -x "$BIN" ]; then
  echo "Binary not found at $BIN. Build first: cargo build --release" >&2
  exit 1
fi
exec "$BIN"
