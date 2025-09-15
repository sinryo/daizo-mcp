# daizo-mcp

An MCP (Model Context Protocol) server plus CLI for fast Buddhist text search and retrieval. Supports CBETA (Chinese), Pāli Tipitaka (romanized), GRETIL (Sanskrit TEI), and SAT (online). Implemented in Rust for speed and reliability.

See also: [日本語 README](README.ja.md) | [繁體中文 README](README.zh-TW.md)

## Highlights

- Fast regex/content search with line numbers (CBETA/Tipitaka/GRETIL)
- Title search across CBETA, Tipitaka, and GRETIL indices
- Precise context fetching by line number or character range
- Optional SAT online search with smart caching
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

Search:

```bash
# Title search
daizo-cli cbeta-title-search --query "楞伽經" --json
daizo-cli tipitaka-title-search --query "dn 1" --json

# Content search (with line numbers)
daizo-cli cbeta-search --query "阿弥陀" --max-results 10
daizo-cli tipitaka-search --query "nibbana|vipassana" --max-results 15
daizo-cli gretil-search --query "yoga" --max-results 10
```

Fetch:

```bash
# Fetch by ID
daizo-cli cbeta-fetch --id T0858 --part 1 --max-chars 4000 --json
daizo-cli tipitaka-fetch --id e0101n.mul --max-chars 2000 --json
daizo-cli gretil-fetch --query "Bhagavadgita" --max-chars 4000 --json

# Context around a line (after search)
daizo-cli cbeta-fetch --id T0858 --line-number 342 --context-before 10 --context-after 200
daizo-cli tipitaka-fetch --id s0305m.mul --line-number 158 --context-before 5 --context-after 100
```

Admin:

```bash
daizo-cli init                      # first-time setup (downloads data, builds indexes)
daizo-cli doctor --verbose          # diagnose install and data
daizo-cli index-rebuild --source all
daizo-cli uninstall --purge         # remove binaries and data/cache
daizo-cli update --yes              # reinstall this CLI
```

## MCP Tools

Search:
- `cbeta_title_search`, `cbeta_search`
- `tipitaka_title_search`, `tipitaka_search`
- `gretil_title_search`, `gretil_search`
- `sat_search`

Fetch:
- `cbeta_fetch` (supports `lineNumber`, `contextBefore`, `contextAfter`)
- `tipitaka_fetch` (supports `lineNumber`, `contextBefore`, `contextAfter`)
- `gretil_fetch` (supports `lineNumber`, `contextBefore`, `contextAfter`)
- `sat_fetch`, `sat_pipeline`

## Data Sources

- CBETA: https://github.com/cbeta-org/xml-p5
- Tipitaka (romanized): https://github.com/VipassanaTech/tipitaka-xml
- GRETIL (Sanskrit TEI): https://gretil.sub.uni-goettingen.de/
- SAT (online): wrap7/detail endpoints

## Directories and Env

- `DAIZO_DIR` (default: `~/.daizo`)
  - data: `xml-p5/`, `tipitaka-xml/romn/`
  - cache: `cache/`
  - binaries: `bin/`
- `DAIZO_DEBUG=1` enables minimal MCP debug log
- Highlight envs: `DAIZO_HL_PREFIX`, `DAIZO_HL_SUFFIX`, `DAIZO_SNIPPET_PREFIX`, `DAIZO_SNIPPET_SUFFIX`
- Repo policy envs (for robots/rate-limits):
  - `DAIZO_REPO_MIN_DELAY_MS`, `DAIZO_REPO_USER_AGENT`, `DAIZO_REPO_RESPECT_ROBOTS`

## License

MIT OR Apache-2.0 © 2025 Shinryo Taniguchi
- **Smart Retrieval**: Context-aware text extraction with fetch hints and flexible line-based context
- **Search & Focus**: Find content, then retrieve customizable context (e.g., 10 lines before, 200 after)
- **Multiple Formats**: Support for TEI P5 XML, plain text, and structured data
- **Automatic Data Management**: Downloads and updates text repositories automatically
- **Caching**: Intelligent caching for online queries

## Environment

- **DAIZO_DIR**: Base directory (default: ~/.daizo)
  - Data: xml-p5/, tipitaka-xml/romn/
  - Cache: cache/
  - Binaries: bin/

## License

MIT OR Apache-2.0 © 2025 Shinryo Taniguchi

## Contributing

Issues and PRs welcome. Please include `daizo-cli doctor --verbose` output with bug reports.
