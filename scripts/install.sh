#!/bin/bash
set -eu
# Enable pipefail if supported
if set -o | grep -q pipefail 2>/dev/null; then
  set -o pipefail
fi

# daizo-mcp installer
# - Builds release binaries
# - Installs into "${DAIZO_DIR:-$HOME/.daizo}/bin"
# - Rebuilds indexes via installed CLI
# - Optionally writes PATH export to your shell rc

usage() {
  cat <<EOF
Usage: scripts/install.sh [--prefix <path>] [--write-path]

Options:
  --prefix <path>   Install base (DAIZO_DIR). Default: \$DAIZO_DIR or ~/.daizo
  --write-path      Append 'export DAIZO_DIR=...; export PATH=\"$DAIZO_DIR/bin:$PATH\"' to your shell rc (~/.zshrc or ~/.bashrc)

Environment:
  DAIZO_DIR         Install base. Overrides default if set.

This will:
  1) cargo build --release
  2) copy target/release/daizo-cli and daizo-mcp to \$DAIZO_DIR/bin
  3) run: \$DAIZO_DIR/bin/daizo-cli index-rebuild --source all (automatically downloads/updates data)
EOF
}

PREFIX="${DAIZO_DIR:-}"
WRITE_PATH=0

while [ $# -gt 0 ]; do
  case "$1" in
    --prefix)
      PREFIX="$2"; shift 2 ;;
    --write-path)
      WRITE_PATH=1; shift ;;
    -h|--help)
      usage; exit 0 ;;
    *)
      echo "Unknown arg: $1" >&2; usage; exit 1 ;;
  esac
done

if [ -z "${PREFIX}" ]; then
  PREFIX="$HOME/.daizo"
fi

REPO_DIR="$(cd "$(dirname "$0")/.." && pwd)"
BIN_OUT="$PREFIX/bin"

echo "[install] REPO_DIR=$REPO_DIR"
echo "[install] DAIZO_DIR=$PREFIX"
echo "[install] BIN_OUT=$BIN_OUT"

echo -e "\033[36m🛑 Stopping existing daizo-mcp processes... / 既存のdaizo-mcpプロセスを停止中... / 正在停止現有的daizo-mcp進程...\033[0m"

# Check if we're on Windows (Git Bash, WSL, or similar)
if command -v tasklist > /dev/null 2>&1 && command -v taskkill > /dev/null 2>&1; then
  # Windows environment
  if tasklist | grep -i "daizo-mcp" > /dev/null; then
    echo "[cleanup] killing existing daizo-mcp processes (Windows)"
    taskkill /F /IM "daizo-mcp*" > /dev/null 2>&1 || true
    echo -e "\033[32m✅ Existing processes stopped / 既存プロセス停止完了 / 現有進程已停止\033[0m"
  else
    echo "[cleanup] no daizo-mcp processes found"
  fi
else
  # Unix-like environment (Linux, macOS)
  if pgrep -f "daizo-mcp" > /dev/null; then
    echo "[cleanup] killing existing daizo-mcp processes"
    pkill -f "daizo-mcp" || true
    sleep 1
    # Force kill if still running
    if pgrep -f "daizo-mcp" > /dev/null; then
      echo "[cleanup] force killing daizo-mcp processes"
      pkill -9 -f "daizo-mcp" || true
    fi
    echo -e "\033[32m✅ Existing processes stopped / 既存プロセス停止完了 / 現有進程已停止\033[0m"
  else
    echo "[cleanup] no daizo-mcp processes found"
  fi
fi

echo -e "\033[36m🗂️  Cleaning up old installation... / 古いインストールをクリーンアップ中... / 正在清理舊安裝...\033[0m"
if [ -d "$BIN_OUT" ]; then
  echo "[cleanup] removing existing directory: $BIN_OUT"
  rm -rf "$BIN_OUT"
  echo -e "\033[32m✅ Old installation cleaned up / 古いインストールのクリーンアップ完了 / 舊安裝清理完成\033[0m"
else
  echo "[cleanup] no existing bin directory found"
fi

mkdir -p "$BIN_OUT"

echo -e "\033[36m🔨 Building Rust project... / Rustプロジェクトをビルドしています... / 正在構建Rust項目...\033[0m"
echo "[build] cargo build --release"
(
  cd "$REPO_DIR"
  cargo build --release
)
echo -e "\033[32m✅ Build completed / ビルド完了 / 構建完成\033[0m"

echo -e "\033[36m📦 Installing binaries... / バイナリをインストール中... / 正在安裝二進制文件...\033[0m"
for b in daizo-cli daizo-mcp; do
  src="$REPO_DIR/target/release/$b"
  if [ ! -x "$src" ]; then
    echo "[error] missing binary: $src" >&2
    exit 1
  fi
  echo "[install] copy $b -> $BIN_OUT"
  cp -f "$src" "$BIN_OUT/"
done
echo -e "\033[32m✅ Binary installation completed / バイナリインストール完了 / 二進制文件安裝完成\033[0m"

echo -e "\033[36m📚 Fetching GRETIL Sanskrit corpus... / GRETILサンスクリット語コーパスを取得中... / 正在下載 GRETIL 梵文語料庫...\033[0m"
GRETIL_URL="https://gretil.sub.uni-goettingen.de/gretil/1_sanskr.zip"
GRETIL_DIR="$PREFIX/GRETIL"
GRETIL_ZIP="$GRETIL_DIR/1_sanskr.zip"
mkdir -p "$GRETIL_DIR"

if [ -f "$GRETIL_ZIP" ]; then
  echo "[gretil] zip already present, skip download: $GRETIL_ZIP"
else
  echo "[gretil] download -> $GRETIL_ZIP"
  if command -v curl >/dev/null 2>&1; then
    curl -L --fail --retry 3 -o "$GRETIL_ZIP" "$GRETIL_URL"
  elif command -v wget >/dev/null 2>&1; then
    wget -O "$GRETIL_ZIP" "$GRETIL_URL"
  else
    echo "[error] neither curl nor wget is available to download $GRETIL_URL" >&2
    exit 1
  fi
fi

STAMP_FILE="$GRETIL_DIR/.extracted-1_sanskr"
if [ -f "$STAMP_FILE" ] || find "$GRETIL_DIR" -mindepth 1 -not -name "$(basename "$GRETIL_ZIP")" -print -quit | grep -q . ; then
  echo "[gretil] already extracted, skip unzip"
else
  echo "[gretil] unzip into $GRETIL_DIR"
  if command -v unzip >/dev/null 2>&1; then
    unzip -oq "$GRETIL_ZIP" -d "$GRETIL_DIR"
    touch "$STAMP_FILE"
  else
    echo "[error] 'unzip' command not found; please install it and re-run" >&2
    exit 1
  fi
  echo -e "\033[32m✅ GRETIL fetched and extracted / GRETILの取得と展開が完了 / GRETIL 下載並解壓完成\033[0m"
fi

echo -e "\033[36m📥 Downloading Buddhist texts and building indexes... / お経データのダウンロードとインデックス構築中... / 正在下載佛經文本並構建索引...\033[0m"
echo "[index] rebuilding indexes (this will automatically download/update data)"
DAIZO_DIR="$PREFIX" "$BIN_OUT/daizo-cli" index-rebuild --source all || {
  echo "[warn] index rebuild failed; you can run: DAIZO_DIR=$PREFIX $BIN_OUT/daizo-cli index-rebuild --source all" >&2
}
echo -e "\033[32m✅ Index building completed / インデックス構築完了 / 索引構建完成\033[0m"

echo -e "\033[36m⚙️  Configuring environment... / 環境設定中... / 正在配置環境...\033[0m"
if [ $WRITE_PATH -eq 1 ]; then
  SHELL_NAME="$(basename "${SHELL:-}")"
  RC=""
  case "$SHELL_NAME" in
    zsh) RC="$HOME/.zshrc" ;;
    bash) RC="$HOME/.bashrc" ;;
    *) RC="$HOME/.profile" ;;
  esac
  echo "[path] append DAIZO_DIR/PATH exports to $RC"
  {
    echo "export DAIZO_DIR=\"$PREFIX\""
    echo "export PATH=\"$PREFIX/bin:\$PATH\""
  } >> "$RC"
  echo "[path] done. Reload your shell or 'source $RC'"
else
  echo "[path] To use the tools, ensure in your shell rc:"
  echo "       export DAIZO_DIR=\"\$HOME/.daizo\""
  echo "       export PATH=\"\$HOME/.daizo/bin:\$PATH\""
fi

echo -e "\033[32m🎉 Installation completed! / インストール完了！ / 安裝完成！\033[0m"
echo "[ok] Installed daizo-cli and daizo-mcp to $BIN_OUT"
