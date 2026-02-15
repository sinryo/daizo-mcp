# daizo-mcp

CBETA（漢文）、パーリ三蔵（ローマ字）、GRETIL（サンスクリット TEI）、SARIT（TEI P5）、SAT（オンライン）、浄土宗全書（オンライン）に加え、チベット大蔵経系のオンライン全文検索（BUDA/BDRC・Adarshah）にも対応した、高速な仏教テキスト検索・取得のための MCP サーバーおよび CLI です。Rust で実装し、高速・堅牢に動作します。

関連: [English README](README.md) | [繁體中文 README](README.zh-TW.md)

## 特長

- **ダイレクトIDアクセス**: テキストIDが分かっていれば即座に取得（最速！）
- CBETA / Tipitaka / GRETIL / SARIT に対する高速な正規表現検索（行番号つき）
- CBETA検索は新字体など“現代の表記”でもヒットするよう正規化（旧字体・簡繁などの揺れを吸収）
- タイトル検索（CBETA / Tipitaka / GRETIL / SARIT）
- 行番号や文字位置での前後コンテキスト取得
- SAT オンライン検索（スマートキャッシュ付き）
- 浄土宗全書（オンライン）の検索・本文取得（キャッシュ付き）
- チベット語のオンライン全文検索（BUDA/BDRC + Adarshah、EWTS/Wylieの簡易自動変換つき）
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

解決:
- `daizo_resolve`（タイトル/別名/ID からコーパス候補と、次に呼ぶべき取得ツール呼び出しを返す）

検索:
- `cbeta_title_search`, `cbeta_search`
- `tipitaka_title_search`, `tipitaka_search`
- `gretil_title_search`, `gretil_search`
- `sat_search`
- `jozen_search`
- `tibetan_search`（チベット語のオンライン全文検索。`sources:["buda","adarshah"]`。BUDAは `exact` でフレーズ検索、Adarshahは `wildcard`、`maxSnippetChars` でスニペット長）

取得:
- `cbeta_fetch`（`lb`, `lineNumber`, `contextBefore`, `contextAfter`, `headQuery`, `headIndex`, `format:"plain"`, `focusHighlight` をサポート。`plain` は XMLタグ除去・gaiji解決・teiHeader除外・改行保持。`focusHighlight` は最初のハイライト一致箇所付近にジャンプ）
- `tipitaka_fetch`（`lineNumber`, `contextBefore`, `contextAfter` をサポート）
- `gretil_fetch`（`lineNumber`, `contextBefore`, `contextAfter`, `headQuery`, `headIndex` をサポート）
- `sat_fetch`, `sat_pipeline`（`exact` をサポート。デフォルトはフレーズ検索）
- `jozen_fetch`（`lineno` 指定で1ページ取得。`[J..] ...` 形式で返す）

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

1. `daizo_resolve` で候補ID（コーパス）を決める
2. `*_fetch` を `{ id }`（必要なら `part` や `headQuery` など）で呼ぶ
3. フレーズ検索が必要なら `*_search` → `_meta.fetchSuggestions` → `*_fetch`（`lineNumber`）を使う
4. `*_pipeline` は多ファイル要約が必要な時のみ使用。既定で `autoFetch=false` を推奨

### crosswalk（横断解決）とは

daizo でいう **crosswalk** は、「人間のクエリ（経典名・別名・略称など）」から「実際に叩くべきコーパスID」と「次に呼ぶべき `*_fetch`」へ最短で橋渡しすることです。

- `daizo_resolve({query})` を呼ぶ
- 返ってくる候補と `_meta.fetchSuggestions` を使って、最小トークンで `*_fetch` に移る

各ツールの description にも案内を記載。`initialize` 応答の `prompts.low-token-guide` でも方針を提示します。

Tips: `DAIZO_HINT_TOP` でサジェスト件数を制御（既定 1）。

## データソース

- CBETA: https://github.com/cbeta-org/xml-p5
- Tipitaka (romanized): https://github.com/VipassanaTech/tipitaka-xml
- GRETIL (Sanskrit TEI): https://gretil.sub.uni-goettingen.de/
- SAT (online): wrap7 / detail エンドポイント
- 浄土宗全書（オンライン）: jodoshuzensho.jp
- BUDA/BDRC（チベット語オンライン）: library.bdrc.io / autocomplete.bdrc.io
- Adarshah（チベット語オンライン）: online.adarshah.org / api.adarshah.org

## ディレクトリと環境変数

- `DAIZO_DIR`（既定: `~/.daizo`）
  - データ: `xml-p5/`, `tipitaka-xml/romn/`, `GRETIL/`, `SARIT-corpus/`
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
scripts/release.sh 0.6.1 --all

# CHANGELOG をノートに使用
scripts/release.sh 0.6.1 --push --release

# ドライラン
scripts/release.sh 0.6.1 --all --dry-run
```

## ライセンス

MIT または Apache-2.0 © 2025 Shinryo Taniguchi

## コントリビューション

Issue や PR を歓迎します。バグ報告には `daizo-cli doctor --verbose` の出力を添付してください。
