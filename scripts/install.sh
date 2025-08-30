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
