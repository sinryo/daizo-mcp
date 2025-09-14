# MCP 開発に必要なドキュメント索引

このプロジェクト（daizo-mcp）の開発・運用で参照頻度が高いMCP関連ドキュメントを厳選してまとめました。各リンクは公式仕様・実装ガイド・クライアント統合の観点で整理しています。

## 公式仕様（Model Context Protocol）
- 概要・導入: https://modelcontextprotocol.io/introduction
  - MCPの目的、用語、全体像（初期化/能力アドバタイズ/エラーモデルの位置付け含む）
- ツール（Tools）コンセプト: https://modelcontextprotocol.io/docs/concepts/tools
  - ツールの宣言スキーマ、呼び出し、引数/戻り値の取り扱い
- リソース（Resources）コンセプト: https://modelcontextprotocol.io/docs/concepts/resources
  - サーバ側が提供する読み取り可能な外部/内部リソースの公開と取得
- プロンプト（Prompts）コンセプト: https://modelcontextprotocol.io/docs/concepts/prompts
  - サーバ側でテンプレ化されたプロンプトの列挙/取得
- ロギング（Logging）コンセプト: https://modelcontextprotocol.io/docs/concepts/logging
  - クライアント→サーバのログ取り込み、レベル、メッセージ形式
- トランスポート概要: https://modelcontextprotocol.io/docs/transports/overview
  - MCPはプロトコル（JSON-RPC）とトランスポート（stdio/HTTP SSE）の分離を採用
- stdio トランスポート: https://modelcontextprotocol.io/docs/transports/stdio
  - `Content-Length` ベースのフレーミング、ヘッダー、エンコーディング、双方向I/O
- HTTP/SSE トランスポート: https://modelcontextprotocol.io/docs/transports/http
  - ストリーム応答、イベント種別、接続/切断、エラーとリトライ

### ローカル（オフライン）コピー
- docs/mcp/mcp-introduction.html:1
- docs/mcp/mcp-tools.html:1
- docs/mcp/mcp-resources.html:1
- docs/mcp/mcp-prompts.html:1
- docs/mcp/mcp-logging.html:1
- docs/mcp/mcp-transports-overview.html:1
- docs/mcp/mcp-stdio.html:1
- docs/mcp/mcp-http.html:1

（実装時の着目点）
- 初期化/Capability広告: initialize → capabilities（tools/resources/prompts/logging）
- リクエスト/レスポンス: JSON-RPC 2.0の`id/method/params`とエラー応答の設計
- コンテント表現: テキスト/リッチテキスト/メタデータの表現と上限

## Anthropic（Claude）連携ドキュメント
- MCPコネクタ（Messages APIからの直接接続）: docs/mcp-connector.md
  - ベータヘッダー `"anthropic-beta": "mcp-client-2025-04-04"`、HTTP公開サーバ前提、ツール呼び出しのみ対応、複数サーバ接続など

## 実装・運用ベストプラクティス（推奨）
- レート制御/キャッシュ: 外部サイト連携（例: SAT）時の再試行・バックオフ・キャッシュ方針
- セキュリティ: 認証のないローカルstdio公開とHTTP公開の差分、OAuth/Bearer利用可否の判断
- ロギング/計測: デバッグ可観測性（環境変数でのON/OFF、ログサイズ管理）
- 互換性: `protocolVersion`やContentフォーマットの将来互換性に注意

## daizo-mcp 実装との対応付け（抜粋）
- stdio/行区切り対応フレーミング: `daizo-mcp/src/main.rs` の `read_message` / `write_message`
- initializeとcapabilities: `handle_initialize`（tools/resources/prompts/logging空集合を広告）
- Tools定義群とディスパッチ: `tools_list` と `match method { ... }`
- 外部サイト（SAT）アクセス: バックオフ+キャッシュ有（HTTP公開時はレート配慮）

## 次アクションの提案
- 必要に応じ、上記リンクの主要ページをオフライン参照用に `docs/mcp-*.md` として順次取り込み
- サーバが`resources/prompts/logging`を提供する場合の仕様差分を検討し、対応計画を作成
- レート制御と利用規約への準拠（SAT等）の明文化をREADMEへ追記
