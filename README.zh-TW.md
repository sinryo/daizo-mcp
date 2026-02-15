# daizo-mcp

面向 CBETA（中文）、巴利三藏（羅馬化）、GRETIL（梵文 TEI）、SARIT（TEI P5）、SAT（線上）、浄土宗全書（線上），以及透過線上語料進行的藏文全文搜尋（BUDA/BDRC、Adarshah）的高速佛典搜尋與擷取。包含 MCP 伺服器與 CLI，採用 Rust 實作，專注於速度與穩定性。

相關: [English README](README.md) | [日本語 README](README.ja.md)

## 亮點

- **直接 ID 存取**：已知文本 ID 時可即時取得（最快！）
- CBETA / Tipitaka / GRETIL / SARIT / MUKTABODHA 文字內容快速正則搜尋（附行號）
- CBETA 搜尋可接受較現代的字形（新舊字、簡繁等會被正規化以避免漏掉大正藏本文）
- 標題搜尋（CBETA / Tipitaka / GRETIL / SARIT / MUKTABODHA）
- 以行號或字元範圍精準擷取上下文
- SAT 線上搜尋（含智慧快取）
- 浄土宗全書（線上）搜尋/本文擷取（含快取）
- 藏文線上全文搜尋（BUDA/BDRC + Adarshah，EWTS/Wylie 會嘗試自動轉為藏文字）
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

### 直接 ID 存取（最快！）

已知文本 ID 時，跳過搜尋直接取得：

```bash
# CBETA：大正藏號（T + 4 位數字）
daizo-cli cbeta-fetch --id T0001      # 長阿含經
daizo-cli cbeta-fetch --id T0262      # 妙法蓮華經
daizo-cli cbeta-fetch --id T0235      # 金剛般若波羅蜜經

# Tipitaka：尼柯耶代碼（DN, MN, SN, AN, KN）
daizo-cli tipitaka-fetch --id DN1     # 梵網經
daizo-cli tipitaka-fetch --id MN1     # 根本法門經
daizo-cli tipitaka-fetch --id SN1     # 相應部第一

# GRETIL：梵文文本名稱
daizo-cli gretil-fetch --id saddharmapuNDarIka         # 法華經（梵文）
daizo-cli gretil-fetch --id vajracchedikA              # 金剛般若經（梵文）
daizo-cli gretil-fetch --id prajJApAramitAhRdayasUtra  # 般若心經（梵文）

# SARIT：TEI P5 語料（檔名 stem）
daizo-cli sarit-fetch --id asvaghosa-buddhacarita

# MUKTABODHA：梵文資料庫（檔名 stem；本機檔案置於 $DAIZO_DIR/MUKTABODHA）
daizo-cli muktabodha-fetch --id "<file-stem>"
```

### 搜尋

```bash
# 標題搜尋
daizo-cli cbeta-title-search --query "楞伽經" --json
daizo-cli tipitaka-title-search --query "dn 1" --json
daizo-cli sarit-title-search --query "buddhacarita" --json
daizo-cli muktabodha-title-search --query "yoga" --json

# 內容搜尋（附行號）
daizo-cli cbeta-search --query "阿彌陀" --max-results 10
daizo-cli tipitaka-search --query "nibbana|vipassana" --max-results 15
daizo-cli gretil-search --query "yoga" --max-results 10
daizo-cli sarit-search --query "yoga" --max-results 10
daizo-cli muktabodha-search --query "yoga" --max-results 10
```

### 附上下文取得

```bash
# 依 ID 取得（附選項）
daizo-cli cbeta-fetch --id T0858 --part 1 --max-chars 4000 --json
daizo-cli tipitaka-fetch --id s0101m.mul --max-chars 2000 --json
daizo-cli gretil-fetch --id buddhacarita --max-chars 4000 --json
daizo-cli sarit-fetch --id asvaghosa-buddhacarita --max-chars 4000 --json
daizo-cli muktabodha-fetch --id "<file-stem>" --max-chars 4000 --json

# 行號上下文（搜尋後）
daizo-cli cbeta-fetch --id T0858 --line-number 342 --context-before 10 --context-after 200
daizo-cli tipitaka-fetch --id s0305m.mul --line-number 158 --context-before 5 --context-after 100
```

### 管理

```bash
daizo-cli init                      # 首次設定（下載資料、建立索引）
daizo-cli doctor --verbose          # 檢查安裝與資料
daizo-cli index-rebuild --source all
daizo-cli uninstall --purge         # 移除二進位與資料/快取
daizo-cli update --yes              # 重新安裝 CLI
```

## MCP 工具

核心：
- `daizo_version`（伺服器版本/建置資訊）
- `daizo_usage`（AI 用戶端使用指南；低代幣流程）
- `daizo_profile`（工具呼叫的簡易效能量測）

解決：
- `daizo_resolve`（將標題/別名/ID 解析為跨語料庫的候選 ID 與建議下一步 fetch 呼叫；範圍：cbeta/tipitaka/gretil/sarit/muktabodha）

搜尋：
- `cbeta_title_search`, `cbeta_search`
- `tipitaka_title_search`, `tipitaka_search`
- `gretil_title_search`, `gretil_search`
- `sarit_title_search`, `sarit_search`
- `muktabodha_title_search`, `muktabodha_search`
- `sat_search`
- `jozen_search`
- `tibetan_search`（藏文線上全文搜尋；`sources:["buda","adarshah"]`，BUDA 支援 `exact` 短語搜尋，Adarshah 支援 `wildcard`，`maxSnippetChars` 控制片段長度）

取得：
- `cbeta_fetch`（支援 `lb`, `lineNumber`, `contextBefore`, `contextAfter`, `headQuery`, `headIndex`, `format:"plain"`, `focusHighlight`；`plain` 會移除 XML 標籤、解決 gaiji、排除 `teiHeader`，並保留換行；`focusHighlight` 會跳到第一個高亮匹配附近）
- `tipitaka_fetch`（支援 `lineNumber`, `contextBefore`, `contextAfter`）
- `gretil_fetch`（支援 `lineNumber`, `contextBefore`, `contextAfter`）
- `sarit_fetch`（支援 `lineNumber`, `contextBefore`, `contextAfter`）
- `muktabodha_fetch`（支援 `lineNumber`, `contextBefore`, `contextAfter`）
- `sat_fetch`, `sat_detail`, `sat_pipeline`
- `jozen_fetch`（以 `lineno` 擷取單頁；回傳格式為 `[J..] ...`）

管線：
- `cbeta_pipeline`, `gretil_pipeline`, `sarit_pipeline`, `muktabodha_pipeline`, `sat_pipeline`（若要先摘要，建議 `autoFetch=false`）

## 低代幣用法（AI 用戶端）

### 最快：直接 ID 存取

已知文本 ID 時，**跳過搜尋**：

| 語料庫 | ID 格式 | 範例 |
|--------|---------|------|
| CBETA | `T` + 4 位數字 | `cbeta_fetch({id: "T0262"})` |
| Tipitaka | `DN`, `MN`, `SN`, `AN`, `KN` + 數字 | `tipitaka_fetch({id: "DN1"})` |
| GRETIL | 梵文文本名稱 | `gretil_fetch({id: "saddharmapuNDarIka"})` |
| SARIT | TEI 檔名 stem | `sarit_fetch({id: "asvaghosa-buddhacarita"})` |
| MUKTABODHA | 檔名 stem | `muktabodha_fetch({id: "FILE_STEM"})` |

### 常用 ID 參考

**CBETA（中文大藏經）**：
- T0001 = 長阿含經
- T0099 = 雜阿含經
- T0262 = 妙法蓮華經（法華經）
- T0235 = 金剛般若波羅蜜經（金剛經）
- T0251 = 般若波羅蜜多心經（心經）

**Tipitaka（巴利三藏）**：
- DN1-DN34 = 長部（Dīghanikāya）
- MN1-MN152 = 中部（Majjhimanikāya）
- SN = 相應部（Saṃyuttanikāya）
- AN = 增支部（Aṅguttaranikāya）

**GRETIL（梵文）**：
- saddharmapuNDarIka = 法華經
- vajracchedikA = 金剛般若經
- prajJApAramitAhRdayasUtra = 般若心經
- buddhacarita = 佛所行讚（馬鳴）

### 標準流程（ID 未知時）

1. 先用 `daizo_resolve` 做 crosswalk（橫向解析），挑出語料庫與候選 ID
2. 直接呼叫 `*_fetch({id})`（必要時加上 `lineNumber`/`contextBefore`/`contextAfter` 等）
3. 若要精確片段：`*_search` → 讀取 `_meta.fetchSuggestions` → `*_fetch(lineNumber)`
4. 僅在需要多檔案摘要時使用 `*_pipeline`，且預設 `autoFetch=false`

工具描述中已標示此指引；`initialize` 亦提供 `prompts.low-token-guide` 以提示用法。

提示：以 `DAIZO_HINT_TOP` 控制建議數量（預設 1）。

## 資料來源

- CBETA: https://github.com/cbeta-org/xml-p5
- Tipitaka（羅馬化）: https://github.com/VipassanaTech/tipitaka-xml
- GRETIL（梵文 TEI）: https://gretil.sub.uni-goettingen.de/
- SARIT（TEI P5）: https://github.com/sarit/SARIT-corpus
- MUKTABODHA（梵文；本機檔案）: 將文本放在 `$DAIZO_DIR/MUKTABODHA/`
- SAT（線上）: wrap7 / detail 端點
- 浄土宗全書（線上）: jodoshuzensho.jp
- BUDA/BDRC（藏文線上）: library.bdrc.io / autocomplete.bdrc.io
- Adarshah（藏文線上）: online.adarshah.org / api.adarshah.org

## 目錄與環境變數

- `DAIZO_DIR`（預設：`~/.daizo`）
  - 資料：`xml-p5/`, `tipitaka-xml/romn/`, `GRETIL/`, `SARIT-corpus/`, `MUKTABODHA/`
  - 快取：`cache/`
  - 二進位：`bin/`
- `DAIZO_DEBUG=1` 啟用簡易 MCP 除錯日誌
- 高亮設定：`DAIZO_HL_PREFIX`, `DAIZO_HL_SUFFIX`, `DAIZO_SNIPPET_PREFIX`, `DAIZO_SNIPPET_SUFFIX`
- 取得策略（頻率/robots）：
  - `DAIZO_REPO_MIN_DELAY_MS`, `DAIZO_REPO_USER_AGENT`, `DAIZO_REPO_RESPECT_ROBOTS`

## 腳本

| 腳本 | 用途 |
|------|------|
| `scripts/bootstrap.sh` | 一鍵安裝：檢查依賴 → clone 倉庫 → 執行 install.sh → 自動註冊 MCP |
| `scripts/install.sh` | 主安裝程式：建置 → 安裝二進位 → 下載 GRETIL → 重建索引 |
| `scripts/link-binaries.sh` | 開發用：建立指向 release 二進位的符號連結 |
| `scripts/release.sh` | 釋出用：版本升級 → 建立標籤 → GitHub Release |

### 版本釋出範例

```bash
# 全自動（bump → commit → tag → push → GitHub 釋出，自動筆記）
scripts/release.sh 0.6.1 --all

# 使用 CHANGELOG 筆記
scripts/release.sh 0.6.1 --push --release

# 模擬執行
scripts/release.sh 0.6.1 --all --dry-run
```

## 授權

MIT 或 Apache-2.0 © 2025 Shinryo Taniguchi

## 貢獻

歡迎 Issue 與 PR。提交 bug 報告時請附上 `daizo-cli doctor --verbose` 輸出。
