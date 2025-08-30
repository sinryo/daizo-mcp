# daizo-mcp

此專案為 Codex、Claude Code 及其他支援 MCP 的客戶端提供 MCP 伺服器。包含用於搜尋和取得以下資料的 CLI：

- CBETA（TEI P5 XML）
- 巴利三藏（羅馬化）
- SAT

## 功能

使用此 MCP 伺服器，您可以直接詢問 Claude 或您的 AI 助理：

- **搜尋佛經文本**：「在巴利三藏中尋找關於正念的段落」
- **取得特定文本**：「從 CBETA 顯示法華經第一章」
- **按主題探索**：「中部尼柯耶對禪修有什麼論述？」
- **取得文本詳情**：「獲取文本 T0858 的完整元資料」

AI 可以即時搜尋數千部佛經文本，並提供準確的引用和內容。

使用 Rust 建構，採用 quick-xml 和 SAT 請求快取/退避機制。所有資料、快取和本地安裝都存放在單一基礎目錄：DAIZO_DIR（預設：~/.daizo）。

**此版本新功能**：`index-rebuild` 指令現在會在建構索引前自動下載和更新所有資料儲存庫，確保您始終擁有最新的佛經文本，無需手動干預。

另見：英文版 README.md 和日文版 README.ja.md。

## 前提條件

**需要安裝 Git**，因為系統會自動下載佛經文本儲存庫。

如果尚未安裝 Git：
- 安裝指南：https://git-scm.com/book/en/v2/Getting-Started-Installing-Git

## 一鍵安裝（推薦）

將二進制檔案建構至 $DAIZO_DIR/bin，自動下載/更新所有佛經文本儲存庫，建構索引，並在可用時向 Claude Code 和 Codex 註冊 MCP 伺服器。

``` bash
curl -fsSL https://raw.githubusercontent.com/sinryo/daizo-mcp/main/scripts/bootstrap.sh | bash -s -- --yes --write-path
```

**安裝期間執行的操作：**
1. **建構**：編譯 daizo-cli 和 daizo-mcp 二進制檔案
2. **自動下載**：自動複製 CBETA 和 Tipitaka 儲存庫（總計約 2-3GB）
3. **索引建構**：處理所有文本並建構可搜尋的索引
4. **自動註冊**：向 Claude Code 和 Codex MCP 客戶端註冊

**自動註冊行為：**
- **Claude Code**：如果 `claude` CLI 可用，使用 `claude mcp add daizo /path/to/daizo-mcp`
- **Codex**：如果 `~/.codex/config.toml` 檔案存在且尚未包含 `[mcp_servers.daizo]`，則添加設定
- 如果自動註冊失效，會靜默失敗 - 您可以稍後手動添加

## 選項

- --prefix：設定 DAIZO_DIR（預設：$DAIZO_DIR 或 ~/.daizo）
- --write-path：將 DAIZO_DIR 和 PATH 匯出附加到您的 shell rc

## 手動安裝（開發者）

1. 建構：```cargo build --release```
2. 安裝 + 自動下載 + 索引：```scripts/install.sh --prefix "$HOME/.daizo" --write-path```

**注意**：安裝腳本現在會自動下載所有必需的資料儲存庫並建構索引。無需單獨執行 `daizo-cli init`。

### 添加到 MCP 客戶端

啟動腳本會嘗試自動註冊到兩個客戶端。如果失敗，您可以手動添加：

- **Claude Code CLI**：
  ``` bash
  claude mcp add daizo /path/to/DAIZO_DIR/bin/daizo-mcp
  ```

- **Codex CLI** — 添加到 `~/.codex/config.toml`：
  ``` toml
  [mcp_servers.daizo]
  command = "/path/to/DAIZO_DIR/bin/daizo-mcp"
  ```

## 實用連結

- Model Context Protocol：https://modelcontextprotocol.io
- Claude Code MCP：https://docs.anthropic.com/en/docs/claude-code/mcp
- Codex MCP：https://github.com/openai/codex/blob/main/docs/advanced.md#model-context-protocol-mcp

## 環境

- DAIZO_DIR：基礎目錄（預設：~/.daizo）
  - bin/：daizo-cli 和 daizo-mcp 二進制檔案
  - xml-p5/：CBETA TEI P5 XML 檔案
  - tipitaka-xml/romn/：巴利三藏（羅馬化）XML 檔案
  - cache/：索引檔案和 SAT 快取

## 指令

安裝後，您可以使用：

```bash
# 搜尋 CBETA
daizo-cli cbeta-search --query "法華經"

# 取得 CBETA 文本
daizo-cli cbeta-fetch --id "T0262" --part "1"

# 搜尋巴利三藏
daizo-cli tipitaka-search --query "mindfulness"

# 取得巴利文本
daizo-cli tipitaka-fetch --query "Satipatthana"

# 搜尋 SAT
daizo-cli sat-search --query "般若"

# 重建索引
daizo-cli index-rebuild --source all
```

## 故障排除

### Git 未找到錯誤

確保已安裝 Git：https://git-scm.com/book/en/v2/Getting-Started-Installing-Git

### 權限錯誤

確保 `$DAIZO_DIR/bin` 在您的 PATH 中，並且二進制檔案可執行：

```bash
chmod +x ~/.daizo/bin/daizo-cli ~/.daizo/bin/daizo-mcp
```

### 磁碟空間

CBETA + Tipitaka 需要約 2-3GB 的磁碟空間。

### MCP 連接問題

檢查您的 MCP 客戶端設定：

```bash
# Claude Code
claude mcp list

# Codex
cat ~/.codex/config.toml
```

## 開發

使用標準 Rust 工作流程：

```bash
# 執行測試
cargo test

# 檢查程式碼品質
cargo clippy

# 格式化程式碼
cargo fmt
```

## 授權

此專案採用 MIT 授權 - 詳見 LICENSE 檔案。

## 貢獻

歡迎貢獻！請先開 issue 討論您想要進行的更改。

## 致謝

- [CBETA](http://cbeta.org/) 提供中文佛經數位化文本
- [VipassanaTech](https://github.com/VipassanaTech) 提供巴利三藏數位化
- [SAT](https://21dzk.l.u-tokyo.ac.jp/SAT2018/) 提供大正新脩大藏經數位資源