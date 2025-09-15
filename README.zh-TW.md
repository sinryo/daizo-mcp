# daizo-mcp

面向 CBETA（中文）、巴利三藏（羅馬化）、GRETIL（梵文 TEI）與 SAT（線上）的高速佛典搜尋與擷取。包含 MCP 伺服器與 CLI，採用 Rust 實作，專注於速度與穩定性。

相關: [English README](README.md) | [日本語 README](README.ja.md)

## 亮點

- CBETA / Tipitaka / GRETIL 文字內容快速正則搜尋（附行號）
- 標題搜尋（CBETA / Tipitaka / GRETIL）
- 以行號或字元範圍精準擷取上下文
- SAT 線上搜尋（含智慧快取）
- 一鍵初始化與索引建置

## 安裝

前置需求：請先安裝 Git。

快速安裝：

```bash
curl -fsSL https://raw.githubusercontent.com/sinryo/daizo-mcp/main/scripts/bootstrap.sh | bash -s -- --yes --write-path
```

手動安裝：

```bash
cargo build --release
scripts/install.sh --prefix "$HOME/.daizo" --write-path
```

## MCP 客戶端整合

Claude Code CLI：

```bash
claude mcp add daizo "$HOME/.daizo/bin/daizo-mcp"
```

Codex CLI（`~/.codex/config.toml`）：

```toml
[mcp_servers.daizo]
command = "/Users/you/.daizo/bin/daizo-mcp"
```

## CLI 範例

搜尋：

```bash
# 標題搜尋
daizo-cli cbeta-title-search --query "楞伽經" --json
daizo-cli tipitaka-title-search --query "dn 1" --json

# 內容搜尋（附行號）
daizo-cli cbeta-search --query "阿彌陀" --max-results 10
daizo-cli tipitaka-search --query "nibbana|vipassana" --max-results 15
daizo-cli gretil-search --query "yoga" --max-results 10
```

取得：

```bash
# 依 ID 取得
daizo-cli cbeta-fetch --id T0858 --part 1 --max-chars 4000 --json
daizo-cli tipitaka-fetch --id e0101n.mul --max-chars 2000 --json
daizo-cli gretil-fetch --query "Bhagavadgita" --max-chars 4000 --json

# 行號上下文（搜尋後）
daizo-cli cbeta-fetch --id T0858 --line-number 342 --context-before 10 --context-after 200
daizo-cli tipitaka-fetch --id s0305m.mul --line-number 158 --context-before 5 --context-after 100
```

管理：

```bash
daizo-cli init                      # 首次設定（下載資料、建立索引）
daizo-cli doctor --verbose          # 檢查安裝與資料
daizo-cli index-rebuild --source all
daizo-cli uninstall --purge         # 移除二進位與資料/快取
daizo-cli update --yes              # 重新安裝 CLI
```

## MCP 工具

搜尋：
- `cbeta_title_search`, `cbeta_search`
- `tipitaka_title_search`, `tipitaka_search`
- `gretil_title_search`, `gretil_search`
- `sat_search`

取得：
- `cbeta_fetch`（支援 `lineNumber`, `contextBefore`, `contextAfter`）
- `tipitaka_fetch`（支援 `lineNumber`, `contextBefore`, `contextAfter`）
- `gretil_fetch`（支援 `lineNumber`, `contextBefore`, `contextAfter`）
- `sat_fetch`, `sat_pipeline`

## 低代幣用法（AI 用戶端）

- 預設流程：`*_search` → 讀取 `_meta.fetchSuggestions` → 以 `{ id, lineNumber, contextBefore:1, contextAfter:3 }` 呼叫 `*_fetch`。
- 僅在需要多檔案摘要時使用 `*_pipeline`，且預設 `autoFetch=false`。搜尋工具也提供 `_meta.pipelineHint`。
- 工具描述中已標示此指引；`initialize` 亦提供 `prompts.low-token-guide` 以提示用法。

提示：以 `DAIZO_HINT_TOP` 控制建議數量（預設 1）。

## 資料來源

- CBETA: https://github.com/cbeta-org/xml-p5
- Tipitaka（羅馬化）: https://github.com/VipassanaTech/tipitaka-xml
- GRETIL（梵文 TEI）: https://gretil.sub.uni-goettingen.de/
- SAT（線上）: wrap7 / detail 端點

## 目錄與環境變數

- `DAIZO_DIR`（預設：`~/.daizo`）
  - 資料：`xml-p5/`, `tipitaka-xml/romn/`
  - 快取：`cache/`
  - 二進位：`bin/`
- `DAIZO_DEBUG=1` 啟用簡易 MCP 除錯日誌
- 高亮設定：`DAIZO_HL_PREFIX`, `DAIZO_HL_SUFFIX`, `DAIZO_SNIPPET_PREFIX`, `DAIZO_SNIPPET_SUFFIX`
- 取得策略（頻率/robots）：
  - `DAIZO_REPO_MIN_DELAY_MS`, `DAIZO_REPO_USER_AGENT`, `DAIZO_REPO_RESPECT_ROBOTS`

## 版本釋出輔助

- 腳本：`scripts/release.sh`
- 範例：
  - 全自動（bump → commit → tag → push → GitHub 釋出，自動筆記）: `scripts/release.sh 0.3.3 --all`
  - 使用 CHANGELOG 筆記：`scripts/release.sh 0.3.3 --push --release`
  - 模擬執行：`scripts/release.sh 0.3.3 --all --dry-run`

## 授權

MIT 或 Apache-2.0 © 2025 Shinryo Taniguchi
