#!/bin/bash
# Basic error handling for old bash compatibility
set +e  # disable for now
set +u  # disable for now

# One-liner bootstrap installer for daizo-mcp
# Example (after publishing):
#   curl -fsSL https://raw.githubusercontent.com/sinryo/daizo-mcp/main/scripts/bootstrap.sh | bash -s -- --yes --write-path

usage() {
  cat <<EOF
Usage: bootstrap.sh [--prefix <DAIZO_DIR>] [--repo <git-url>] [--yes] [--write-path]

Options:
  --prefix <path>   Install base (DAIZO_DIR). Default: \$DAIZO_DIR or ~/.daizo
  --repo <url>      Git repo to clone/update. Default: https://github.com/sinryo/daizo-mcp
  --yes             Non-interactive; assume yes to prompts where safe
  --write-path      Append DAIZO_DIR/PATH exports to your shell rc (~/.zshrc or ~/.bashrc)

This will:
  - Ensure git and cargo exist (suggest rustup if missing)
  - Clone/update repo under \$DAIZO_DIR/src/daizo-mcp
  - Run scripts/install.sh to build+install to \$DAIZO_DIR/bin and rebuild indexes
EOF
}

PREFIX="${DAIZO_DIR:-}"
REPO_URL="https://github.com/sinryo/daizo-mcp"
YES=0
WRITE_PATH=0

while [ $# -gt 0 ]; do
  case "$1" in
    --prefix) PREFIX="$2"; shift 2;;
    --repo) REPO_URL="$2"; shift 2;;
    --yes) YES=1; shift;;
    --write-path) WRITE_PATH=1; shift;;
    -h|--help) usage; exit 0;;
    *) echo "Unknown arg: $1" >&2; usage; exit 1;;
  esac
done

if [ -z "${PREFIX}" ]; then PREFIX="$HOME/.daizo"; fi
export DAIZO_DIR="$PREFIX"

need() {
  if ! command -v "$1" >/dev/null 2>&1; then
    echo "[need] missing dependency: $1" >&2
    return 1
  fi
}

echo "[env] DAIZO_DIR=$DAIZO_DIR"
echo "[need] checking git/cargo..."
NEED_RUST=0
need git || { echo "Please install git (e.g., brew install git)" >&2; exit 1; }
if ! command -v cargo >/dev/null 2>&1; then
  NEED_RUST=1
  echo "[need] cargo not found"
  if [ $YES -eq 1 ]; then
    echo "[hint] Install rustup: curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh" >&2
  else
    read -r -p "Install Rust toolchain now? (y/N) " ans </dev/tty || true
    case "$ans" in
      [Yy]*)
        curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
        export PATH="$HOME/.cargo/bin:$PATH"
        NEED_RUST=0
        ;;
    esac
  fi
fi
if [ $NEED_RUST -eq 1 ]; then
  echo "[error] cargo is required. Install with rustup and re-run." >&2
  exit 1
fi

REPO_BASE="$DAIZO_DIR/src"
REPO_DIR="$REPO_BASE/daizo-mcp"
mkdir -p "$REPO_BASE"
if [ -d "$REPO_DIR/.git" ]; then
  echo "[repo] updating $REPO_DIR"
  git -C "$REPO_DIR" pull --ff-only
else
  echo "[repo] cloning $REPO_URL -> $REPO_DIR"
  git clone --depth 1 "$REPO_URL" "$REPO_DIR"
fi

echo "[install] running scripts/install.sh"
bash "$REPO_DIR/scripts/install.sh" --prefix "$DAIZO_DIR" ${WRITE_PATH:+--write-path}

# Try to auto-register with Claude Code if available
if command -v claude >/dev/null 2>&1; then
  echo "[mcp] attempting to register with Claude Code..."
  if claude mcp add daizo "$DAIZO_DIR/bin/daizo-mcp" 2>/dev/null; then
    echo "[mcp] successfully registered daizo MCP server with Claude Code"
  else
    echo "[mcp] Claude Code auto-registration failed (this is fine)"
  fi
else
  echo "[mcp] claude CLI not found - skipping Claude Code registration"
fi

# Try to auto-register with Codex if config exists
CODEX_CONFIG="$HOME/.codex/config.toml"
if [ -f "$CODEX_CONFIG" ]; then
  echo "[mcp] attempting to register with Codex..."
  if grep -q "^\[mcp_servers\.daizo\]" "$CODEX_CONFIG" 2>/dev/null; then
    echo "[mcp] daizo already configured in Codex - skipping"
  else
    echo "" >> "$CODEX_CONFIG"
    echo "[mcp_servers.daizo]" >> "$CODEX_CONFIG"
    echo "command = \"$DAIZO_DIR/bin/daizo-mcp\"" >> "$CODEX_CONFIG"
    echo "[mcp] successfully registered daizo MCP server with Codex"
  fi
else
  echo "[mcp] Codex config not found - skipping Codex registration"
fi

echo "[done] daizo installed. Try: daizo-cli doctor --verbose"
echo ""
echo "If MCP auto-registration failed, you can add manually:"
echo "  Claude Code: claude mcp add daizo $DAIZO_DIR/bin/daizo-mcp"
echo "  Codex: Add to ~/.codex/config.toml:"
echo "    [mcp_servers.daizo]"
echo "    command = \"$DAIZO_DIR/bin/daizo-mcp\""
