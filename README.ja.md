# daizo-mcp

CBETA（漢文）、パーリ三蔵（ローマ字）、GRETIL（サンスクリット TEI）、SAT（オンライン）に対応した、高速な仏教テキスト検索・取得のための MCP サーバーおよび CLI です。Rust で実装し、高速・堅牢に動作します。

関連: [English README](README.md) | [繁體中文 README](README.zh-TW.md)

## 特長

- **ダイレクトIDアクセス**: テキストIDが分かっていれば即座に取得（最速！）
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

### ダイレクトIDアクセス（最速！）

テキストIDが分かっていれば、検索をスキップして直接取得:

```bash
# CBETA: 大正番号（T + 4桁の数字）
daizo-cli cbeta-fetch --id T0001      # 長阿含經
daizo-cli cbeta-fetch --id T0262      # 妙法蓮華經
daizo-cli cbeta-fetch --id T0235      # 金剛般若波羅蜜經

# Tipitaka: ニカーヤコード（DN, MN, SN, AN, KN）
daizo-cli tipitaka-fetch --id DN1     # 梵網経
daizo-cli tipitaka-fetch --id MN1     # 根本法門経
daizo-cli tipitaka-fetch --id SN1     # 相応部第1

# GRETIL: サンスクリットテキスト名
daizo-cli gretil-fetch --id saddharmapuNDarIka         # 法華経（梵文）
daizo-cli gretil-fetch --id vajracchedikA              # 金剛般若経（梵文）
daizo-cli gretil-fetch --id prajJApAramitAhRdayasUtra  # 般若心経（梵文）
```

### 検索

```bash
# タイトル検索
daizo-cli cbeta-title-search --query "楞伽經" --json
daizo-cli tipitaka-title-search --query "dn 1" --json

# 内容検索（行番号つき）
daizo-cli cbeta-search --query "阿弥陀" --max-results 10
daizo-cli tipitaka-search --query "nibbana|vipassana" --max-results 15
daizo-cli gretil-search --query "yoga" --max-results 10
```

### コンテキスト付き取得

```bash
# IDとオプション指定で取得
daizo-cli cbeta-fetch --id T0858 --part 1 --max-chars 4000 --json
daizo-cli tipitaka-fetch --id s0101m.mul --max-chars 2000 --json
daizo-cli gretil-fetch --id buddhacarita --max-chars 4000 --json

# 行番号の前後コンテキスト
daizo-cli cbeta-fetch --id T0858 --line-number 342 --context-before 10 --context-after 200
daizo-cli tipitaka-fetch --id s0305m.mul --line-number 158 --context-before 5 --context-after 100
```

### 管理

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

### 最速: ダイレクトIDアクセス

テキストIDが分かっている場合は **検索をスキップ**:

| コーパス | ID形式 | 例 |
|----------|--------|-----|
| CBETA | `T` + 4桁数字 | `cbeta_fetch({id: "T0262"})` |
| Tipitaka | `DN`, `MN`, `SN`, `AN`, `KN` + 番号 | `tipitaka_fetch({id: "DN1"})` |
| GRETIL | サンスクリットテキスト名 | `gretil_fetch({id: "saddharmapuNDarIka"})` |

### よく使うID一覧

**CBETA（漢文大蔵経）**:
- T0001 = 長阿含經
- T0099 = 雜阿含經
- T0262 = 妙法蓮華經（法華経）
- T0235 = 金剛般若波羅蜜經（金剛経）
- T0251 = 般若波羅蜜多心經（般若心経）

**Tipitaka（パーリ三蔵）**:
- DN1-DN34 = 長部（Dīghanikāya）
- MN1-MN152 = 中部（Majjhimanikāya）
- SN = 相応部（Saṃyuttanikāya）
- AN = 増支部（Aṅguttaranikāya）

**GRETIL（梵文）**:
- saddharmapuNDarIka = 法華経
- vajracchedikA = 金剛般若経
- prajJApAramitAhRdayasUtra = 般若心経
- buddhacarita = 仏所行讃（馬鳴）

### 通常フロー（IDが不明な場合）

1. `*_search` → `_meta.fetchSuggestions` を読む
2. `*_fetch` を `{ id, lineNumber, contextBefore:1, contextAfter:3 }` で呼ぶ
3. `*_pipeline` は多ファイル要約が必要な時のみ使用。既定で `autoFetch=false` を推奨

各ツールの description にも案内を記載。`initialize` 応答の `prompts.low-token-guide` でも方針を提示します。

Tips: `DAIZO_HINT_TOP` でサジェスト件数を制御（既定 1）。

## データソース

- CBETA: https://github.com/cbeta-org/xml-p5
- Tipitaka (romanized): https://github.com/VipassanaTech/tipitaka-xml
- GRETIL (Sanskrit TEI): https://gretil.sub.uni-goettingen.de/
- SAT (online): wrap7 / detail エンドポイント

## ディレクトリと環境変数

- `DAIZO_DIR`（既定: `~/.daizo`）
  - データ: `xml-p5/`, `tipitaka-xml/romn/`, `GRETIL/`
  - キャッシュ: `cache/`
  - バイナリ: `bin/`
- `DAIZO_DEBUG=1` で簡易 MCP デバッグログ
- ハイライト関連: `DAIZO_HL_PREFIX`, `DAIZO_HL_SUFFIX`, `DAIZO_SNIPPET_PREFIX`, `DAIZO_SNIPPET_SUFFIX`
- 取得ポリシー（レート/robots 配慮）:
  - `DAIZO_REPO_MIN_DELAY_MS`, `DAIZO_REPO_USER_AGENT`, `DAIZO_REPO_RESPECT_ROBOTS`

## スクリプト

| スクリプト | 役割 |
|------------|------|
| `scripts/bootstrap.sh` | ワンライナーインストーラー: 依存チェック → リポジトリclone → install.sh実行 → MCP自動登録 |
| `scripts/install.sh` | メインインストーラー: ビルド → バイナリ配置 → GRETILダウンロード → インデックス構築 |
| `scripts/link-binaries.sh` | 開発用: リリースバイナリへのシンボリックリンク作成 |
| `scripts/release.sh` | リリース用: バージョンバンプ → タグ作成 → GitHub Release |

### リリース補助の例

```bash
# 自動一括（バンプ → コミット → タグ → プッシュ → GitHub リリース自動ノート）
scripts/release.sh 0.3.3 --all

# CHANGELOG をノートに使用
scripts/release.sh 0.3.3 --push --release

# ドライラン
scripts/release.sh 0.3.3 --all --dry-run
```

## ライセンス

MIT または Apache-2.0 © 2025 Shinryo Taniguchi

## コントリビューション

Issue や PR を歓迎します。バグ報告には `daizo-cli doctor --verbose` の出力を添付してください。
