# daizo-mcp

CBETA（漢文）、パーリ三蔵（ローマ字）、GRETIL（サンスクリット TEI）、SAT（オンライン）に対応した、高速な仏教テキスト検索・取得のための MCP サーバーおよび CLI です。Rust で実装し、高速・堅牢に動作します。

関連: [English README](README.md) | [繁體中文 README](README.zh-TW.md)

## 特長

- CBETA / Tipitaka / GRETIL に対する高速な正規表現検索（行番号つき）
- タイトル検索（CBETA / Tipitaka / GRETIL）
- 行番号や文字位置での前後コンテキスト取得
- SAT オンライン検索（スマートキャッシュ付き）
- ワンコマンド・ブートストラップとインデックス構築

## インストール

前提: Git が必要です。

クイックインストール:

```bash
curl -fsSL https://raw.githubusercontent.com/sinryo/daizo-mcp/main/scripts/bootstrap.sh | bash -s -- --yes --write-path
```

手動セットアップ:

```bash
cargo build --release
scripts/install.sh --prefix "$HOME/.daizo" --write-path
```

## MCP クライアント連携

Claude Code CLI:

```bash
claude mcp add daizo "$HOME/.daizo/bin/daizo-mcp"
```

Codex CLI（`~/.codex/config.toml`）:

```toml
[mcp_servers.daizo]
command = "/Users/you/.daizo/bin/daizo-mcp"
```

## CLI 例

検索:

```bash
# タイトル検索
daizo-cli cbeta-title-search --query "楞伽經" --json
daizo-cli tipitaka-title-search --query "dn 1" --json

# 内容検索（行番号つき）
daizo-cli cbeta-search --query "阿弥陀" --max-results 10
daizo-cli tipitaka-search --query "nibbana|vipassana" --max-results 15
daizo-cli gretil-search --query "yoga" --max-results 10
```

取得:

```bash
# ID 指定で取得
daizo-cli cbeta-fetch --id T0858 --part 1 --max-chars 4000 --json
daizo-cli tipitaka-fetch --id e0101n.mul --max-chars 2000 --json
daizo-cli gretil-fetch --query "Bhagavadgita" --max-chars 4000 --json

# 行番号の前後コンテキスト
daizo-cli cbeta-fetch --id T0858 --line-number 342 --context-before 10 --context-after 200
daizo-cli tipitaka-fetch --id s0305m.mul --line-number 158 --context-before 5 --context-after 100
```

管理:

```bash
daizo-cli init                      # 初期セットアップ（データ取得とインデックス構築）
daizo-cli doctor --verbose          # インストール/データ診断
daizo-cli index-rebuild --source all
daizo-cli uninstall --purge         # バイナリとデータ/キャッシュを削除
daizo-cli update --yes              # CLI の再インストール
```

## MCP ツール

検索:
- `cbeta_title_search`, `cbeta_search`
- `tipitaka_title_search`, `tipitaka_search`
- `gretil_title_search`, `gretil_search`
- `sat_search`

取得:
- `cbeta_fetch`（`lineNumber`, `contextBefore`, `contextAfter` をサポート）
- `tipitaka_fetch`（`lineNumber`, `contextBefore`, `contextAfter` をサポート）
- `gretil_fetch`（`lineNumber`, `contextBefore`, `contextAfter` をサポート）
- `sat_fetch`, `sat_pipeline`

## 低トークン運用（AI クライアント向け）

- 既定の導線: `*_search` → `_meta.fetchSuggestions` を読む → `*_fetch` を `{ id, lineNumber, contextBefore:1, contextAfter:3 }` で呼ぶ。
- `*_pipeline` は多ファイル要約が必要な時のみ使用。既定で `autoFetch=false` を推奨。search は `_meta.pipelineHint` も返します。
- 各ツールの description にも案内を記載。`initialize` 応答の `prompts.low-token-guide` でも方針を提示します。

Tips: `DAIZO_HINT_TOP` でサジェスト件数を制御（既定 1）。

## データソース

- CBETA: https://github.com/cbeta-org/xml-p5
- Tipitaka (romanized): https://github.com/VipassanaTech/tipitaka-xml
- GRETIL (Sanskrit TEI): https://gretil.sub.uni-goettingen.de/
- SAT (online): wrap7 / detail エンドポイント

## ディレクトリと環境変数

- `DAIZO_DIR`（既定: `~/.daizo`）
  - データ: `xml-p5/`, `tipitaka-xml/romn/`
  - キャッシュ: `cache/`
  - バイナリ: `bin/`
- `DAIZO_DEBUG=1` で簡易 MCP デバッグログ
- ハイライト関連: `DAIZO_HL_PREFIX`, `DAIZO_HL_SUFFIX`, `DAIZO_SNIPPET_PREFIX`, `DAIZO_SNIPPET_SUFFIX`
- 取得ポリシー（レート/robots 配慮）:
  - `DAIZO_REPO_MIN_DELAY_MS`, `DAIZO_REPO_USER_AGENT`, `DAIZO_REPO_RESPECT_ROBOTS`

## リリース補助

- スクリプト: `scripts/release.sh`
- 例:
  - 自動一括（バンプ → コミット → タグ → プッシュ → GitHub リリース自動ノート）: `scripts/release.sh 0.3.3 --all`
  - CHANGELOG をノートに使用: `scripts/release.sh 0.3.3 --push --release`
  - ドライラン: `scripts/release.sh 0.3.3 --all --dry-run`

## ライセンス

MIT または Apache-2.0 © 2025 Shinryo Taniguchi
