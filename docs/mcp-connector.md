# MCPコネクタ

> Claude の Model Context Protocol (MCP) コネクタ機能により、別途 MCP クライアントを使用せずに Messages API から直接リモート MCP サーバーに接続できます。

Claude の Model Context Protocol (MCP) コネクタ機能により、別途 MCP クライアントを使用せずに Messages API から直接リモート MCP サーバーに接続できます。

<Note>
  この機能にはベータヘッダーが必要です: `"anthropic-beta": "mcp-client-2025-04-04"`
</Note>

## 主な機能

* **直接API統合**: MCP クライアントを実装せずに MCP サーバーに接続
* **ツール呼び出しサポート**: Messages API を通じて MCP ツールにアクセス
* **OAuth認証**: 認証されたサーバー用の OAuth Bearer トークンをサポート
* **複数サーバー**: 単一のリクエストで複数の MCP サーバーに接続

## 制限事項

* [MCP仕様](https://modelcontextprotocol.io/introduction#explore-mcp)の機能セットのうち、現在は[ツール呼び出し](https://modelcontextprotocol.io/docs/concepts/tools)のみがサポートされています。
* サーバーは HTTP を通じて公開されている必要があります（Streamable HTTP と SSE トランスポートの両方をサポート）。ローカル STDIO サーバーには直接接続できません。
* MCP コネクタは現在 Amazon Bedrock と Google Vertex ではサポートされていません。

## Messages API での MCP コネクタの使用

リモート MCP サーバーに接続するには、Messages API リクエストに `mcp_servers` パラメータを含めます：

<CodeGroup>
  ```bash cURL
  curl https://api.anthropic.com/v1/messages \
    -H "Content-Type: application/json" \
    -H "X-API-Key: $ANTHROPIC_API_KEY" \
    -H "anthropic-version: 2023-06-01" \
    -H "anthropic-beta: mcp-client-2025-04-04" \
    -d '{
      "model": "claude-sonnet-4-20250514",
      "max_tokens": 1000,
      "messages": [{"role": "user", "content": "What tools do you have available?"}],
      "mcp_servers": [
        {
          "type": "url",
          "url": "https://example-server.modelcontextprotocol.io/sse",
          "name": "example-mcp",
          "authorization_token": "YOUR_TOKEN"
        }
      ]
    }'
  ```

  ```typescript TypeScript
  import { Anthropic } from '@anthropic-ai/sdk';

  const anthropic = new Anthropic();

  const response = await anthropic.beta.messages.create({
    model: "claude-sonnet-4-20250514",
    max_tokens: 1000,
    messages: [
      {
        role: "user",
        content: "What tools do you have available?",
      },
    ],
    mcp_servers: [
      {
        type: "url",
        url: "https://example-server.modelcontextprotocol.io/sse",
        name: "example-mcp",
        authorization_token: "YOUR_TOKEN",
      },
    ],
    betas: ["mcp-client-2025-04-04"],
  });
  ```

  ```python Python
  import anthropic

  client = anthropic.Anthropic()

  response = client.beta.messages.create(
      model="claude-sonnet-4-20250514",
      max_tokens=1000,
      messages=[{
          "role": "user",
          "content": "What tools do you have available?"
      }],
      mcp_servers=[{
          "type": "url",
          "url": "https://mcp.example.com/sse",
          "name": "example-mcp",
          "authorization_token": "YOUR_TOKEN"
      }],
      betas=["mcp-client-2025-04-04"]
  )
  ```
</CodeGroup>

## MCP サーバー設定

`mcp_servers` 配列内の各 MCP サーバーは以下の設定をサポートします：

```json
{
  "type": "url",
  "url": "https://example-server.modelcontextprotocol.io/sse",
  "name": "example-mcp",
  "tool_configuration": {
    "enabled": true,
    "allowed_tools": ["example_tool_1", "example_tool_2"]
  },
  "authorization_token": "YOUR_TOKEN"
}
```

### フィールドの説明

| プロパティ                              | タイプ     | 必須  | 説明                                                                                                                     |
| ---------------------------------- | ------- | --- | ---------------------------------------------------------------------------------------------------------------------- |
| `type`                             | string  | はい  | 現在は "url" のみサポート                                                                                                       |
| `url`                              | string  | はい  | MCP サーバーの URL。https\:// で始まる必要があります                                                                                    |
| `name`                             | string  | はい  | この MCP サーバーの一意の識別子。`mcp_tool_call` ブロックでサーバーを識別し、モデルにツールを区別するために使用されます。                                                |
| `tool_configuration`               | object  | いいえ | ツール使用を設定                                                                                                               |
| `tool_configuration.enabled`       | boolean | いいえ | このサーバーからのツールを有効にするかどうか（デフォルト: true）                                                                                    |
| `tool_configuration.allowed_tools` | array   | いいえ | 許可するツールを制限するリスト（デフォルトでは、すべてのツールが許可されます）                                                                                |
| `authorization_token`              | string  | いいえ | MCP サーバーで必要な場合の OAuth 認証トークン。[MCP仕様](https://modelcontextprotocol.io/specification/2025-03-26/basic/authorization)を参照。 |

## レスポンスコンテンツタイプ

Claude が MCP ツールを使用する場合、レスポンスには2つの新しいコンテンツブロックタイプが含まれます：

### MCP ツール使用ブロック

```json
{
  "type": "mcp_tool_use",
  "id": "mcptoolu_014Q35RayjACSWkSj4X2yov1",
  "name": "echo",
  "server_name": "example-mcp",
  "input": { "param1": "value1", "param2": "value2" }
}
```

### MCP ツール結果ブロック

```json
{
  "type": "mcp_tool_result",
  "tool_use_id": "mcptoolu_014Q35RayjACSWkSj4X2yov1",
  "is_error": false,
  "content": [
    {
      "type": "text",
      "text": "Hello"
    }
  ]
}
```

## 複数の MCP サーバー

`mcp_servers` 配列に複数のオブジェクトを含めることで、複数の MCP サーバーに接続できます：

```json
{
  "model": "claude-sonnet-4-20250514",
  "max_tokens": 1000,
  "messages": [
    {
      "role": "user",
      "content": "Use tools from both mcp-server-1 and mcp-server-2 to complete this task"
    }
  ],
  "mcp_servers": [
    {
      "type": "url",
      "url": "https://mcp.example1.com/sse",
      "name": "mcp-server-1",
      "authorization_token": "TOKEN1"
    },
    {
      "type": "url",
      "url": "https://mcp.example2.com/sse",
      "name": "mcp-server-2",
      "authorization_token": "TOKEN2"
    }
  ]
}
```

## 認証

OAuth 認証を必要とする MCP サーバーの場合、アクセストークンを取得する必要があります。MCP コネクタベータは、MCP サーバー定義で `authorization_token` パラメータの渡しをサポートしています。
API コンシューマーは OAuth フローを処理し、API 呼び出しを行う前にアクセストークンを取得し、必要に応じてトークンを更新することが期待されています。

### テスト用のアクセストークンの取得

MCP インスペクターは、テスト目的でアクセストークンを取得するプロセスをガイドできます。

1. 以下のコマンドでインスペクターを実行します。マシンに Node.js がインストールされている必要があります。

   ```bash
   npx @modelcontextprotocol/inspector
   ```

2. 左側のサイドバーで、「Transport type」に対して「SSE」または「Streamable HTTP」のいずれかを選択します。

3. MCP サーバーの URL を入力します。

4. 右側のエリアで、「Need to configure authentication?」の後の「Open Auth Settings」ボタンをクリックします。

5. 「Quick OAuth Flow」をクリックし、OAuth 画面で認証します。

6. インスペクターの「OAuth Flow Progress」セクションの手順に従い、「Authentication complete」に到達するまで「Continue」をクリックします。

7. `access_token` の値をコピーします。

8. MCP サーバー設定の `authorization_token` フィールドに貼り付けます。

### アクセストークンの使用

上記のいずれかの OAuth フローを使用してアクセストークンを取得したら、MCP サーバー設定で使用できます：

```json
{
  "mcp_servers": [
    {
      "type": "url",
      "url": "https://example-server.modelcontextprotocol.io/sse",
      "name": "authenticated-server",
      "authorization_token": "YOUR_ACCESS_TOKEN_HERE"
    }
  ]
}
```

OAuth フローの詳細な説明については、MCP 仕様の[認証セクション](https://modelcontextprotocol.io/docs/concepts/authentication)を参照してください。
