# daizo-mcp

An MCP (Model Context Protocol) server that provides AI assistants with direct access to Buddhist text databases including CBETA, Pāli Tipitaka, and SAT. Built in Rust for high performance text search and retrieval.

## What You Can Do

Ask your AI assistant to:

- **Search by title**: "Find the Lotus Sutra in CBETA" 
- **Search by content**: "Search for texts mentioning '阿弥陀' across all CBETA texts"
- **Retrieve specific texts**: "Show me chapter 1 of DN 1 from the Pāli Canon"
- **Explore by topic**: "What does the Majjhima Nikaya say about meditation?"
- **Pattern search**: "Find all occurrences of 'nibbana' or 'vipassana' in Tipitaka texts"
- **Search & Focus**: "Find where 'Dhammacakkappavattana' appears, then show me the 10 lines before and 200 lines after"

The AI can search across thousands of Buddhist texts in real-time and provide accurate citations.

See also: [Japanese README](README.ja.md) | [Traditional Chinese README](README.zh-TW.md)

## Prerequisites

**Git is required** for downloading Buddhist text repositories.

Install Git: https://git-scm.com/book/en/v2/Getting-Started-Installing-Git

## Quick Install

```bash
curl -fsSL https://raw.githubusercontent.com/sinryo/daizo-mcp/main/scripts/bootstrap.sh | bash -s -- --yes --write-path
```

This automatically:
1. Builds the binaries
2. Downloads CBETA and Tipitaka text repositories (~2-3GB)
3. Builds search indexes
4. Registers with Claude Code and Codex if available

## Manual Setup

1. Build: `cargo build --release`
2. Install: `scripts/install.sh --prefix "$HOME/.daizo" --write-path`

### Add to MCP Clients

**Claude Code CLI:**
```bash
claude mcp add daizo /path/to/DAIZO_DIR/bin/daizo-mcp
```

**Codex CLI** - add to `~/.codex/config.toml`:
```toml
[mcp_servers.daizo]
command = "/path/to/DAIZO_DIR/bin/daizo-mcp"
```

## Data Sources

- **CBETA** (Chinese Buddhist texts): https://github.com/cbeta-org/xml-p5
- **Pāli Tipitaka** (romanized): https://github.com/VipassanaTech/tipitaka-xml
- **SAT** (online database): Additional search capability

## CLI Usage

### Search Commands
```bash
# Title-based search
daizo-cli cbeta-title-search --query "楞伽經" --json
daizo-cli tipitaka-title-search --query "dn 1" --json

# Fast content search (with line numbers)
daizo-cli cbeta-search --query "阿弥陀" --max-results 10
daizo-cli tipitaka-search --query "nibbana|vipassana" --max-results 15
```

### Fetch Commands
```bash
# Retrieve specific texts
daizo-cli cbeta-fetch --id T0858 --part 1 --max-chars 4000 --json
daizo-cli tipitaka-fetch --id e0101n.mul --max-chars 2000 --json

# Line-based context retrieval (after search)
daizo-cli cbeta-fetch --id T0858 --line-number 342 --context-before 10 --context-after 200
daizo-cli tipitaka-fetch --id s0305m.mul --line-number 158 --context-before 5 --context-after 100
```

### Management
```bash
daizo-cli doctor --verbose      # Check installation
daizo-cli index-rebuild --source all  # Rebuild indexes
daizo-cli version              # Show version
```

## MCP Tools

The MCP server provides these tools for AI assistants:

### Search Tools
- **cbeta_title_search**: Title-based search in CBETA corpus
- **cbeta_search**: Fast regex content search across CBETA texts (returns line numbers)
- **tipitaka_title_search**: Title-based search in Tipitaka corpus  
- **tipitaka_search**: Fast regex content search across Tipitaka texts (returns line numbers)
- **sat_search**: Additional online database search

### Fetch Tools
- **cbeta_fetch**: Retrieve CBETA text by ID with options for specific parts/sections
  - Line-based retrieval: `lineNumber`, `contextBefore`, `contextAfter` parameters
- **tipitaka_fetch**: Retrieve Tipitaka text by ID with section support
  - Line-based retrieval: `lineNumber`, `contextBefore`, `contextAfter` parameters
- **sat_fetch**, **sat_pipeline**: Additional database retrieval tools

### Search & Focus Workflow
1. Use `*_search` to find content and get line numbers
2. Use `*_fetch` with `lineNumber` to get focused context around matches

### Utility Tools
- **index_rebuild**: Rebuild search indexes (auto-downloads data if needed)

## Features

- **Fast Search**: Parallel regex search across entire text corpora with line number tracking
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