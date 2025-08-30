# daizo-mcp

為AI助理提供直接存取CBETA、巴利三藏、SAT等佛教文獻資料庫的MCP（Model Context Protocol）伺服器。使用Rust建構，提供高效能的文本搜尋與取得功能。

## 功能

您可以向AI助理提出以下要求：

- **標題搜尋**：「在CBETA中尋找法華經」
- **內容搜尋**：「在CBETA全部文獻中搜尋提及『阿彌陀』的文本」
- **特定文本取得**：「顯示巴利經典DN 1的第一章」
- **主題探索**：「中部尼柯耶對禪修有何論述？」
- **模式搜尋**：「在Tipitaka文本中找出所有『nibbana』或『vipassana』的出現位置」
- **搜尋與聚焦**：「找出『轉法輪經』出現的位置，然後顯示前10行、後200行」

AI可以即時搜尋數千部佛教文本，並提供準確的引用。

相關：[English README](README.md) | [日本語 README](README.ja.md)

## 前提條件

下載佛教文獻儲存庫需要**安裝Git**。

Git安裝指南：https://git-scm.com/book/en/v2/Getting-Started-Installing-Git

## 快速安裝

```bash
curl -fsSL https://raw.githubusercontent.com/sinryo/daizo-mcp/main/scripts/bootstrap.sh | bash -s -- --yes --write-path
```

這將自動：
1. 建構二進位檔案
2. 下載CBETA和Tipitaka文本儲存庫（約2-3GB）
3. 建構搜尋索引
4. 註冊到Claude Code和Codex（如果可用）

## 手動設定

1. 建構：`cargo build --release`
2. 安裝：`scripts/install.sh --prefix "$HOME/.daizo" --write-path`

### 添加到MCP客戶端

**Claude Code CLI：**
```bash
claude mcp add daizo /path/to/DAIZO_DIR/bin/daizo-mcp
```

**Codex CLI** - 添加到`~/.codex/config.toml`：
```toml
[mcp_servers.daizo]
command = "/path/to/DAIZO_DIR/bin/daizo-mcp"
```

## 資料來源

- **CBETA**（中文佛教文獻）：https://github.com/cbeta-org/xml-p5
- **巴利三藏**（羅馬化）：https://github.com/VipassanaTech/tipitaka-xml
- **SAT**（線上資料庫）：額外搜尋功能

## CLI使用方法

### 搜尋指令
```bash
# 標題搜尋
daizo-cli cbeta-title-search --query "楞伽經" --json
daizo-cli tipitaka-title-search --query "dn 1" --json

# 快速內容搜尋（附行號）
daizo-cli cbeta-search --query "阿彌陀" --max-results 10
daizo-cli tipitaka-search --query "nibbana|vipassana" --max-results 15
```

### 取得指令
```bash
# 取得特定文本
daizo-cli cbeta-fetch --id T0858 --part 1 --max-chars 4000 --json
daizo-cli tipitaka-fetch --id e0101n.mul --max-chars 2000 --json

# 基於行號的情境取得（搜尋後）
daizo-cli cbeta-fetch --id T0858 --line-number 342 --context-before 10 --context-after 200
daizo-cli tipitaka-fetch --id s0305m.mul --line-number 158 --context-before 5 --context-after 100
```

### 管理
```bash
daizo-cli doctor --verbose      # 檢查安裝狀態
daizo-cli index-rebuild --source all  # 重建索引
daizo-cli version              # 顯示版本
```

## MCP工具

MCP伺服器為AI助理提供以下工具：

### 搜尋工具
- **cbeta_title_search**：CBETA語料庫的標題搜尋
- **cbeta_search**：CBETA文本的快速正規表達式內容搜尋（傳回行號）
- **tipitaka_title_search**：Tipitaka語料庫的標題搜尋
- **tipitaka_search**：Tipitaka文本的快速正規表達式內容搜尋（傳回行號）
- **sat_search**：額外線上資料庫搜尋

### 取得工具
- **cbeta_fetch**：根據ID取得CBETA文本（支援特定部分/章節選項）
  - 基於行號的取得：`lineNumber`、`contextBefore`、`contextAfter` 參數
- **tipitaka_fetch**：根據ID取得Tipitaka文本（支援章節）
  - 基於行號的取得：`lineNumber`、`contextBefore`、`contextAfter` 參數
- **sat_fetch**、**sat_pipeline**：額外資料庫取得工具

### 搜尋與聚焦工作流程
1. 使用`*_search`搜尋內容並取得行號
2. 使用`*_fetch`配合`lineNumber`取得匹配項目周圍的聚焦情境

### 實用工具
- **index_rebuild**：重建搜尋索引（必要時自動下載資料）

## 特色功能

- **快速搜尋**：在整個文本語料庫中進行平行正規表達式搜尋，並追蹤行號
- **智慧取得**：具備取得提示的情境感知文本擷取和彈性的基於行號的情境
- **搜尋與聚焦**：找出內容後取得可自訂的情境（例如前10行，後200行）
- **多種格式**：支援TEI P5 XML、純文字、結構化資料
- **自動資料管理**：自動下載和更新文本儲存庫
- **快取機制**：線上查詢的智慧快取

## 環境

- **DAIZO_DIR**：基礎目錄（預設：~/.daizo）
  - 資料：xml-p5/、tipitaka-xml/romn/
  - 快取：cache/
  - 二進位檔案：bin/

## 授權

MIT OR Apache-2.0 © 2025 Shinryo Taniguchi

## 貢獻

歡迎Issue和PR。錯誤回報請包含`daizo-cli doctor --verbose`的輸出。