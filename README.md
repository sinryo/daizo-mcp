# daizo-mcp

An MCP (Model Context Protocol) server plus CLI for fast Buddhist text search and retrieval. Supports CBETA (Chinese), Pāli Tipitaka (romanized), GRETIL (Sanskrit TEI), SAT (online), and Tibetan full-text search via online corpora (BUDA/BDRC, Adarshah). Implemented in Rust for speed and reliability.

See also: [日本語 README](README.ja.md) | [繁體中文 README](README.zh-TW.md)

## Highlights

- **Direct ID Access**: Instant retrieval when you know the text ID (fastest path!)
- Fast regex/content search with line numbers (CBETA/Tipitaka/GRETIL)
- CBETA search works with modern forms too (new/old CJK variants are normalized so Taisho texts still hit)
- Title search across CBETA, Tipitaka, and GRETIL indices
- Precise context fetching by line number or character range
- Optional SAT online search with smart caching
- Tibetan online full-text search (BUDA/BDRC + Adarshah), with EWTS/Wylie best-effort auto-conversion
- One-shot bootstrap and index build

## Install

Prerequisite: Git must be installed.

Quick bootstrap:

```bash
curl -fsSL https://raw.githubusercontent.com/sinryo/daizo-mcp/main/scripts/bootstrap.sh | bash -s -- --yes --write-path
```

Manual:

```bash
cargo build --release
scripts/install.sh --prefix "$HOME/.daizo" --write-path
```

## Use With MCP Clients

Claude Code CLI:

```bash
claude mcp add daizo "$HOME/.daizo/bin/daizo-mcp"
```

Codex CLI (`~/.codex/config.toml`):

```toml
[mcp_servers.daizo]
command = "/Users/you/.daizo/bin/daizo-mcp"
```

## CLI Examples

### Direct ID Access (Fastest!)

When you know the text ID, skip search entirely:

```bash
# CBETA: Taisho number (T + 4-digit number)
daizo-cli cbeta-fetch --id T0001      # 長阿含經
daizo-cli cbeta-fetch --id T0262      # 妙法蓮華經 (Lotus Sutra)
daizo-cli cbeta-fetch --id T0235      # 金剛般若波羅蜜經 (Diamond Sutra)

# Tipitaka: Nikāya codes (DN, MN, SN, AN, KN)
daizo-cli tipitaka-fetch --id DN1     # Brahmajāla Sutta
daizo-cli tipitaka-fetch --id MN1     # Mūlapariyāya Sutta
daizo-cli tipitaka-fetch --id SN1     # First Saṃyutta

# GRETIL: Sanskrit text names
daizo-cli gretil-fetch --id saddharmapuNDarIka         # Lotus Sutra (Sanskrit)
daizo-cli gretil-fetch --id vajracchedikA              # Diamond Sutra (Sanskrit)
daizo-cli gretil-fetch --id prajJApAramitAhRdayasUtra  # Heart Sutra (Sanskrit)
```

### Search

```bash
# Title search
daizo-cli cbeta-title-search --query "楞伽經" --json
daizo-cli tipitaka-title-search --query "dn 1" --json

# Content search (with line numbers)
daizo-cli cbeta-search --query "阿弥陀" --max-results 10
daizo-cli tipitaka-search --query "nibbana|vipassana" --max-results 15
daizo-cli gretil-search --query "yoga" --max-results 10
```

### Fetch with Context

```bash
# Fetch by ID with options
daizo-cli cbeta-fetch --id T0858 --part 1 --max-chars 4000 --json
daizo-cli tipitaka-fetch --id s0101m.mul --max-chars 2000 --json
daizo-cli gretil-fetch --id buddhacarita --max-chars 4000 --json

# Context around a line (after search)
daizo-cli cbeta-fetch --id T0858 --line-number 342 --context-before 10 --context-after 200
daizo-cli tipitaka-fetch --id s0305m.mul --line-number 158 --context-before 5 --context-after 100
```

### Admin

```bash
daizo-cli init                      # first-time setup (downloads data, builds indexes)
daizo-cli doctor --verbose          # diagnose install and data
daizo-cli index-rebuild --source all
daizo-cli uninstall --purge         # remove binaries and data/cache
daizo-cli update --yes              # reinstall this CLI
```

## MCP Tools

Resolve:
- `daizo_resolve` (resolve title/alias/ID into candidate corpus IDs and recommended next fetch calls)

Search:
- `cbeta_title_search`, `cbeta_search`
- `tipitaka_title_search`, `tipitaka_search`
- `gretil_title_search`, `gretil_search`
- `sat_search`
- `tibetan_search` (online Tibetan full-text search; `sources:["buda","adarshah"]`, `exact` for phrase search on BUDA, `wildcard` for Adarshah, `maxSnippetChars` for snippet size)

Fetch:
- `cbeta_fetch` (supports `lb`, `lineNumber`, `contextBefore`, `contextAfter`, `headQuery`, `headIndex`, `format:"plain"`, `focusHighlight`; `plain` strips XML, resolves gaiji, excludes `teiHeader`, preserves line breaks; `focusHighlight` jumps near the first highlight match)
- `tipitaka_fetch` (supports `lineNumber`, `contextBefore`, `contextAfter`)
- `gretil_fetch` (supports `lineNumber`, `contextBefore`, `contextAfter`, `headQuery`, `headIndex`)
- `sat_fetch`, `sat_pipeline` (supports `exact`; default is phrase search)

## Low-Token Guide (AI clients)

### Fastest: Direct ID Access

When the text ID is known, **skip search entirely**:

| Corpus | ID Format | Example |
|--------|-----------|---------|
| CBETA | `T` + 4-digit number | `cbeta_fetch({id: "T0262"})` |
| Tipitaka | `DN`, `MN`, `SN`, `AN`, `KN` + number | `tipitaka_fetch({id: "DN1"})` |
| GRETIL | Sanskrit text name | `gretil_fetch({id: "saddharmapuNDarIka"})` |

### Common IDs Reference

**CBETA (Chinese Canon)**:
- T0001 = 長阿含經 (Dīrghāgama)
- T0099 = 雜阿含經 (Saṃyuktāgama)
- T0262 = 妙法蓮華經 (Lotus Sutra)
- T0235 = 金剛般若波羅蜜經 (Diamond Sutra)
- T0251 = 般若波羅蜜多心經 (Heart Sutra)

**Tipitaka (Pāli Canon)**:
- DN1-DN34 = Dīghanikāya (長部)
- MN1-MN152 = Majjhimanikāya (中部)
- SN = Saṃyuttanikāya (相応部)
- AN = Aṅguttaranikāya (増支部)

**GRETIL (Sanskrit)**:
- saddharmapuNDarIka = Lotus Sutra
- vajracchedikA = Diamond Sutra
- prajJApAramitAhRdayasUtra = Heart Sutra
- buddhacarita = Buddhacarita (Aśvaghoṣa)

### Standard Flow (when ID unknown)

1. Use `daizo_resolve` to pick corpus+id candidates
2. Call `*_fetch` with `{ id }` (and optionally `part`/`headQuery`, etc.)
3. If you need phrase search: `*_search` → read `_meta.fetchSuggestions` → `*_fetch` (`lineNumber`)
4. Use `*_pipeline` only when you need a multi-file summary; set `autoFetch=false` by default

### What “Crosswalk” Means Here

In daizo, **crosswalk** means: start from a human query (title/alias/short name) and quickly map it to concrete corpus IDs and next calls.

- Call `daizo_resolve({query})`
- Use the returned candidates and `_meta.fetchSuggestions` to jump directly to the best `*_fetch`

Tool descriptions mention these hints; `initialize` also exposes a `prompts.low-token-guide` entry for clients.

Tip: Control number of suggestions via `DAIZO_HINT_TOP` (default 1).

## Data Sources

- CBETA: https://github.com/cbeta-org/xml-p5
- Tipitaka (romanized): https://github.com/VipassanaTech/tipitaka-xml
- GRETIL (Sanskrit TEI): https://gretil.sub.uni-goettingen.de/
- SAT (online): wrap7/detail endpoints
- BUDA/BDRC (online Tibetan): library.bdrc.io / autocomplete.bdrc.io
- Adarshah (online Tibetan): online.adarshah.org / api.adarshah.org

## Directories and Env

- `DAIZO_DIR` (default: `~/.daizo`)
  - data: `xml-p5/`, `tipitaka-xml/romn/`, `GRETIL/`
  - cache: `cache/`
  - binaries: `bin/`
- `DAIZO_DEBUG=1` enables minimal MCP debug log
- Highlight envs: `DAIZO_HL_PREFIX`, `DAIZO_HL_SUFFIX`, `DAIZO_SNIPPET_PREFIX`, `DAIZO_SNIPPET_SUFFIX`
- Repo policy envs (for robots/rate-limits):
  - `DAIZO_REPO_MIN_DELAY_MS`, `DAIZO_REPO_USER_AGENT`, `DAIZO_REPO_RESPECT_ROBOTS`

## Scripts

| Script | Purpose |
|--------|---------|
| `scripts/bootstrap.sh` | One-liner installer: checks deps → clones repo → runs install.sh → auto-registers MCP |
| `scripts/install.sh` | Main installer: builds → installs binaries → downloads GRETIL → rebuilds indexes |
| `scripts/link-binaries.sh` | Dev helper: creates symlinks to release binaries in repo root |
| `scripts/release.sh` | Release helper: version bump → tag → GitHub release |

### Release Helper Examples

```bash
# Auto (bump → commit → tag → push → GitHub release with auto-notes)
scripts/release.sh 0.5.0 --all

# CHANGELOG notes instead of auto-notes
scripts/release.sh 0.5.0 --push --release

# Dry run
scripts/release.sh 0.5.0 --all --dry-run
```

## License

MIT OR Apache-2.0 © 2025 Shinryo Taniguchi

## Contributing

Issues and PRs welcome. Please include `daizo-cli doctor --verbose` output with bug reports.
