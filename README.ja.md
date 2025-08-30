# daizo-mcp

このプロジェクトは、Codex、Claude Code、その他のMCP対応クライアント向けのMCPサーバーを提供します。以下のデータを検索・取得するCLIが含まれています：

- CBETA (TEI P5 XML)
- パーリ三蔵 (ローマ字化)
- SAT

## できること

このMCPサーバーを使用すると、ClaudeやAIアシスタントに直接以下のことを依頼できます：

- **仏教テキストの検索**: 「パーリ仏典でマインドフルネスについての記述を見つけて」
- **特定のテキスト取得**: 「CBETAから法華経の第1章を見せて」
- **トピック別の探索**: 「中部経典で瞑想について何と言われているか？」
- **テキスト詳細の取得**: 「T0858のメタデータを全て取得して」

AIは数千の仏教テキストをリアルタイムで検索し、正確な引用と内容を提供できます。

Rustでビルドされ、高速にお経をインデックス化。すべてのデータ、キャッシュ、ローカルインストールは単一のベースディレクトリDAIZO_DIR（デフォルト: ~/.daizo）に保存されます。

他の言語版: [English README](README.md) | [Traditional Chinese README](README.zh-TW.md)

## 前提条件

**Gitのインストールが必須です**。システムが仏教テキストリポジトリを自動ダウンロードするためです。

Gitがインストールされていない場合：
- インストールガイド: [https://git-scm.com/book/en/v2/Getting-Started-Installing-Git](https://git-scm.com/book/ja/v2/%e4%bd%bf%e3%81%84%e5%a7%8b%e3%82%81%e3%82%8b-Git%e3%81%ae%e3%82%a4%e3%83%b3%e3%82%b9%e3%83%88%e3%83%bc%e3%83%ab)

## ワンライナーインストール（推奨）

`$DAIZO_DIR/bin`にバイナリをビルドし、全ての仏教テキストリポジトリを自動ダウンロード・更新、インデックスを構築して、利用可能な場合はClaude CodeとCodexに自動でMCPサーバーを登録します。

``` bash
curl -fsSL https://raw.githubusercontent.com/sinryo/daizo-mcp/main/scripts/bootstrap.sh | bash -s -- --yes --write-path
```

**インストール中に実行される処理:**
1. **ビルド**: daizo-cliとdaizo-mcpバイナリをコンパイル
2. **自動ダウンロード**: CBETAとTipitakaリポジトリを自動クローン（合計約2-3GB）
3. **インデックス構築**: 全テキストを処理して検索可能なインデックスを構築
4. **自動登録**: Claude CodeとCodex MCPクライアントに登録

**自動登録の動作:**
- **Claude Code**: `claude` CLIが利用可能な場合、`claude mcp add daizo /path/to/daizo-mcp` を実行
- **Codex**: `~/.codex/config.toml` が存在し、`[mcp_servers.daizo]` セクションがまだない場合に設定を追加
- 自動登録が失敗してもエラーにならず、後で手動追加可能

## オプション

- --prefix : DAIZO_DIRを設定（デフォルト: $DAIZO_DIR または ~/.daizo）
- --write-path: シェルrcにDAIZO_DIRとPATHのエクスポートを追記

## 手動インストール（開発者向け）

1. ビルド: ```cargo build --release```
2. インストール + 自動ダウンロード + インデックス作成: ```scripts/install.sh --prefix "$HOME/.daizo" --write-path```

**注意**: インストールスクリプトは必要な全データリポジトリを自動ダウンロードしてインデックスを構築します。個別に `daizo-cli init` を実行する必要はありません。

### MCPクライアントへの追加

ブートストラップスクリプトが両方のクライアントに自動登録を試みます。失敗した場合は手動で追加できます:

- **Claude Code CLI**:
  ``` bash
  claude mcp add daizo /path/to/DAIZO_DIR/bin/daizo-mcp
  ```

- **Codex CLI** — `~/.codex/config.toml` に追加:
  ``` toml
  [mcp_servers.daizo]
  command = "/path/to/DAIZO_DIR/bin/daizo-mcp"
  ```

## 有用なリンク

- Model Context Protocol: https://modelcontextprotocol.io
- Claude Code MCP: https://docs.anthropic.com/ja/docs/claude-code/mcp
- Codex MCP: https://github.com/openai/codex/blob/main/docs/advanced.md#model-context-protocol-mcp

## 環境

- DAIZO_DIR: ベースディレクトリ（デフォルト: ~/.daizo）
    - データ: $DAIZO_DIR/xml-p5, $DAIZO_DIR/tipitaka-xml/romn
    - キャッシュ: $DAIZO_DIR/cache
    - バイナリ: $DAIZO_DIR/bin

**自動データ管理**: `index-rebuild`コマンドが自動的に：
- リポジトリが存在しない場合は**クローン**
- 既存のリポジトリを `git pull --ff-only` で**更新**
- データが最新であることを確認してから**インデックス構築**
- 処理全体を通じて**進行状況を詳細表示**

## データソース（上流）

このプロジェクトは以下の上流リポジトリをクローンして使用します（それぞれのライセンス/使用条件に従ってください）：

- CBETA (TEI P5 XML): https://github.com/cbeta-org/xml-p5 → $DAIZO_DIR/xml-p5
- パーリ三蔵 (ローマ字化): https://github.com/VipassanaTech/tipitaka-xml → $DAIZO_DIR/tipitaka-xml/romn

## CLI主要機能

- CBETA
    - 検索: ```daizo-cli cbeta-search --query "楞伽經" --json```
    - 取得:  ```daizo-cli cbeta-fetch --id T0858 --part 1 --include-notes --max-chars 4000 --json```
- Tipitaka (romn)
    - 検索: ```daizo-cli tipitaka-search --query "dn 1" --json```
    - 取得:  ```daizo-cli tipitaka-fetch --id e0101n.mul --max-chars 2000 --json```
- SAT
    - 検索 (wrap7 JSON): ```daizo-cli sat-search --query 大日 --rows 100``` 
    - 自動取得 (検索→最適タイトル→取得): ```daizo-cli sat-search --query 大日 --rows 100```
    - パイプライン: ```daizo-cli sat-pipeline --query 大日 --rows 100```
    - useidによる取得: ```daizo-cli sat-fetch --useid "0015_,01,0246b01" --max-chars 3000 --json```

## ユーティリティ

- バージョン:   ```daizo-cli version```
- 診断:    ```daizo-cli doctor --verbose```
- **インデックス再構築**: ```daizo-cli index-rebuild --source all```（データ自動ダウンロード・更新）
- 更新:    ```daizo-cli update --git https://github.com/sinryo/daizo-mcp --yes```
- アンインストール: ```daizo-cli uninstall```（データ/キャッシュを削除する場合は --purge を追加）

## MCPツール (daizo-mcp)

- cbeta_search, cbeta_fetch
- tipitaka_search, tipitaka_fetch
- sat_search: { query, rows?(=100) }
- sat_detail: { useid, startChar?, maxChars? }
- sat_fetch:  { useid?, url?, startChar?, maxChars? }
- sat_pipeline: { query, rows?, offs?, fq?, startChar?, maxChars? }

## インデックス

- CBETA: teiHeader/bodyを解析してリッチなメタデータ（author/editor/respAll/translator/juanCount/headsPreview/canon/nnum）を構築。title+id+metaでのファジー検索。
- Tipitaka: メタデータ、headsPreview、別名展開（DN/MN/SN/AN/KN、複合「SN 12.2」、パーリ語発音区別符号の変種）を収集。title+id+metaでのファジー検索。

## 実装ノート

- エンコーディング検出（encoding_rs）とフォールバックを使用したXMLデコード
- SAT: 1秒スロットリング + 指数バックオフ（最大3回リトライ）；$DAIZO_DIR/cacheにキャッシュ

## ライセンス

- MIT OR Apache‑2.0
- © 2025 Shinryo Taniguchi

## コントリビューション

IssueとPRは歓迎します。```daizo-cli doctor --verbose```と最小限の再現例を含めてください。