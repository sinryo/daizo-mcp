# daizo-mcp

CBETA、パーリ三蔵、SATなどの仏教文献データベースへの直接アクセスをAIアシスタントに提供するMCP（Model Context Protocol）サーバーです。高性能なテキスト検索と取得のためにRustで構築されています。

## できること

AIアシスタントに以下のようにお願いできます：

- **タイトル検索**: 「CBETAで法華経を探して」
- **内容検索**: 「CBETA全体で『阿弥陀』に言及するテキストを検索して」
- **特定テキスト取得**: 「パーリ経典のDN 1の第1章を見せて」
- **トピック探索**: 「中部経典で瞑想について何と言っているか」
- **パターン検索**: 「Tipitakaテキストで'nibbana'や'vipassana'の出現箇所をすべて見つけて」
- **検索＆フォーカス**: 「『転法輪経』が出現する箇所を見つけて、その前10行、後200行を表示して」

AIは数千の仏教テキストをリアルタイムで検索し、正確な引用を提供できます。

関連: [English README](README.md) | [繁體中文 README](README.zh-TW.md)

## 必要なもの

仏教文献リポジトリのダウンロードのため**Gitが必要**です。

Git インストール: https://git-scm.com/book/ja/v2/Getting-Started-Installing-Git

## クイックインストール

```bash
curl -fsSL https://raw.githubusercontent.com/sinryo/daizo-mcp/main/scripts/bootstrap.sh | bash -s -- --yes --write-path
```

これにより自動で：
1. バイナリをビルド
2. CBETAとTipitakaテキストリポジトリをダウンロード（約2-3GB）
3. 検索インデックスを構築
4. Claude CodeやCodexに登録（利用可能な場合）

## 手動セットアップ

1. ビルド: `cargo build --release`
2. インストール: `scripts/install.sh --prefix "$HOME/.daizo" --write-path`

### MCPクライアントに追加

**Claude Code CLI:**
```bash
claude mcp add daizo /path/to/DAIZO_DIR/bin/daizo-mcp
```

**Codex CLI** - `~/.codex/config.toml`に追加:
```toml
[mcp_servers.daizo]
command = "/path/to/DAIZO_DIR/bin/daizo-mcp"
```

## データソース

- **CBETA**（中国仏教文献）: https://github.com/cbeta-org/xml-p5
- **パーリ三蔵**（ローマ字版）: https://github.com/VipassanaTech/tipitaka-xml
- **SAT**（オンラインデータベース）: 追加検索機能

## CLI使用法

### 検索コマンド
```bash
# タイトルベース検索
daizo-cli cbeta-title-search --query "楞伽經" --json
daizo-cli tipitaka-title-search --query "dn 1" --json

# 高速内容検索（行番号付き）
daizo-cli cbeta-search --query "阿弥陀" --max-results 10
daizo-cli tipitaka-search --query "nibbana|vipassana" --max-results 15
```

### 取得コマンド
```bash
# 特定テキストの取得
daizo-cli cbeta-fetch --id T0858 --part 1 --max-chars 4000 --json
daizo-cli tipitaka-fetch --id e0101n.mul --max-chars 2000 --json

# 行ベースのコンテキスト取得（検索後）
daizo-cli cbeta-fetch --id T0858 --line-number 342 --context-before 10 --context-after 200
daizo-cli tipitaka-fetch --id s0305m.mul --line-number 158 --context-before 5 --context-after 100
```

### 管理
```bash
daizo-cli doctor --verbose      # インストール状況チェック
daizo-cli index-rebuild --source all  # インデックス再構築
daizo-cli version              # バージョン表示
```

## MCPツール

MCPサーバーはAIアシスタント向けに以下のツールを提供：

### 検索ツール
- **cbeta_title_search**: CBETAコーパスのタイトルベース検索
- **cbeta_search**: CBETAテキスト全体での高速正規表現内容検索（行番号付き）
- **tipitaka_title_search**: Tipitakaコーパスのタイトルベース検索
- **tipitaka_search**: Tipitakaテキスト全体での高速正規表現内容検索（行番号付き）
- **sat_search**: 追加オンラインデータベース検索

### 取得ツール
- **cbeta_fetch**: IDによるCBETAテキスト取得（特定部分/セクションオプション付き）
  - 行ベース取得: `lineNumber`, `contextBefore`, `contextAfter` パラメータ
- **tipitaka_fetch**: IDによるTipitakaテキスト取得（セクション対応）
  - 行ベース取得: `lineNumber`, `contextBefore`, `contextAfter` パラメータ
- **sat_fetch**, **sat_pipeline**: 追加データベース取得ツール

### 検索＆フォーカス ワークフロー
1. `*_search` で内容を検索し行番号を取得
2. `*_fetch` で `lineNumber` を使ってマッチ箇所周辺のフォーカスされたコンテキストを取得

### ユーティリティツール
- **index_rebuild**: 検索インデックス再構築（必要に応じてデータ自動ダウンロード）

## 機能

- **高速検索**: テキストコーパス全体での並列正規表現検索
- **スマート取得**: 取得ヒント付きのコンテキスト対応テキスト抽出
- **複数フォーマット**: TEI P5 XML、プレーンテキスト、構造化データ対応
- **自動データ管理**: テキストリポジトリの自動ダウンロードと更新
- **キャッシュ**: オンラインクエリのインテリジェントキャッシュ

## 環境

- **DAIZO_DIR**: ベースディレクトリ（デフォルト: ~/.daizo）
  - データ: xml-p5/, tipitaka-xml/romn/
  - キャッシュ: cache/
  - バイナリ: bin/

## ライセンス

MIT OR Apache-2.0 © 2025 Shinryo Taniguchi

## 貢献

IssueとPRを歓迎します。バグ報告には`daizo-cli doctor --verbose`の出力を含めてください。