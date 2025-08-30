# daizo-mcp

This project provides an MCP server for Codex, Claude Code, and other MCP-enabled clients. It includes a CLI to
search and fetch:

- CBETA (TEI P5 XML)
- Pāli Tipitaka (romanized)
- SAT

## What You Can Do

With this MCP server, you can directly ask Claude or your AI assistant to:

- **Search Buddhist texts**: "Find passages about mindfulness in the Pāli Canon"
- **Retrieve specific texts**: "Show me chapter 1 of the Lotus Sutra from CBETA"
- **Explore by topic**: "What does the Majjhima Nikaya say about meditation?"
- **Get text details**: "Fetch the complete metadata for text T0858"

The AI can search across thousands of Buddhist texts in real-time and provide you with accurate citations and content.

Built in Rust with quick-xml and request caching/backoff for SAT. All data, cache, and local installs live
under a single base directory: DAIZO_DIR (default: ~/.daizo).

See also: [Japanese README](README.ja.md) | [Traditional Chinese README](README.zh-TW.md)

## Prerequisites

**Git is required** for this installation as the system automatically downloads Buddhist text repositories.

Install Git if not already available:
- Installation guide: https://git-scm.com/book/en/v2/Getting-Started-Installing-Git

## One‑liner Install (recommended)

Builds binaries to $DAIZO_DIR/bin, automatically downloads/updates all Buddhist text repositories, builds indexes, and registers the MCP server with Claude Code and Codex if available.

``` bash
curl -fsSL https://raw.githubusercontent.com/sinryo/daizo-mcp/main/scripts/bootstrap.sh | bash -s -- --yes --write-path
```

**What happens during installation:**
1. **Build**: Compiles daizo-cli and daizo-mcp binaries
2. **Auto-download**: Automatically clones CBETA and Tipitaka repositories (~2-3GB total)
3. **Index building**: Processes all texts and builds searchable indexes
4. **Auto-registration**: Registers with Claude Code and Codex MCP clients

**Auto-registration behavior:**
- **Claude Code**: Uses `claude mcp add daizo /path/to/daizo-mcp` if `claude` CLI is available
- **Codex**: Adds configuration to `~/.codex/config.toml` if the file exists and doesn't already contain `[mcp_servers.daizo]`
- Fails silently if auto-registration doesn't work - you can always add manually later

## Options

- --prefix : sets DAIZO_DIR (default: $DAIZO_DIR or ~/.daizo)
- --write-path: appends DAIZO_DIR and PATH exports to your shell rc

## Manual Install (developers)

1. Build: ```cargo build --release```
2. Install + auto-download + index: ```scripts/install.sh --prefix "$HOME/.daizo" --write-path```

**Note**: The install script now automatically downloads all required data repositories and builds indexes. No need to run `daizo-cli init` separately.

### Add to MCP Clients

The bootstrap script tries to auto-register with both clients. If that fails, you can add manually:

- **Claude Code CLI**:
  ``` bash
  claude mcp add daizo /path/to/DAIZO_DIR/bin/daizo-mcp
  ```

- **Codex CLI** — add to `~/.codex/config.toml`:
  ``` toml
  [mcp_servers.daizo]
  command = "/path/to/DAIZO_DIR/bin/daizo-mcp"
  ```


## Useful links

- Model Context Protocol: https://modelcontextprotocol.io
- Claude Code MCP: https://docs.anthropic.com/en/docs/claude-code/mcp
- Codex MCP: https://github.com/openai/codex/blob/main/docs/advanced.md#model-context-protocol-mcp

## Environment

- DAIZO_DIR: base directory (default: ~/.daizo)
    - Data: $DAIZO_DIR/xml-p5, $DAIZO_DIR/tipitaka-xml/romn
    - Cache: $DAIZO_DIR/cache
    - Binaries: $DAIZO_DIR/bin

**Automatic Data Management**: The `index-rebuild` command automatically:
- **Clones** repositories if they don't exist
- **Updates** existing repositories with `git pull --ff-only` 
- **Builds** fresh indexes after ensuring data is current
- **Shows progress** with detailed logging throughout the process

## Data Sources (Upstream)
This project clones and uses the following upstream repositories (follow their licenses/usage):

- CBETA (TEI P5 XML): https://github.com/cbeta-org/xml-p5 → $DAIZO_DIR/xml-p5
- Pāli Tipitaka (romanized): https://github.com/VipassanaTech/tipitaka-xml → $DAIZO_DIR/tipitaka-xml/romn

## CLI Highlights

- CBETA
    - Search: ```daizo-cli cbeta-search --query "楞伽經" --json```
    - Fetch:  ```daizo-cli cbeta-fetch --id T0858 --part 1 --include-notes --max-chars 4000```
--json
- Tipitaka (romn)
    - Search: ```daizo-cli tipitaka-search --query "dn 1" --json```
    - Fetch:  ```daizo-cli tipitaka-fetch --id e0101n.mul --max-chars 2000 --json```
- SAT
    - Search (wrap7 JSON): ```daizo-cli sat-search --query 大日 --rows 100``` 
    - Auto‑fetch (search→best title→fetch): ```daizo-cli sat-search --query 大日 --rows 100```
    - Pipeline: ```daizo-cli sat-pipeline --query 大日 --rows 100```
    - Fetch by useid: ```daizo-cli sat-fetch --useid "0015_,01,0246b01" --max-chars 3000 --json```

## Utilities

- Version:   ```daizo-cli version```
- Doctor:    ```daizo-cli doctor --verbose```
- **Index Rebuild**: ```daizo-cli index-rebuild --source all``` (auto-downloads/updates data)
- Update:    ```daizo-cli update --git https://github.com/sinryo/daizo-mcp --yes```
- Uninstall: ```daizo-cli uninstall``` (add --purge to remove data/cache)

## MCP Tools (daizo-mcp)

- cbeta_search, cbeta_fetch
- tipitaka_search, tipitaka_fetch
- sat_search: { query, rows?(=100) }
- sat_detail: { useid, startChar?, maxChars? }
- sat_fetch:  { useid?, url?, startChar?, maxChars? }
- sat_pipeline: { query, rows?, offs?, fq?, startChar?, maxChars? }

## Indexes

- CBETA: parse teiHeader/body to build rich meta (author/editor/respAll/translator/juanCount/headsPreview/
canon/nnum). Fuzzy search across title+id+meta.
- Tipitaka: collect  metadata, headsPreview, and alias expansions (DN/MN/SN/AN/KN, composite “SN 12.2”, Pāli
diacritics variants). Fuzzy search across title+id+meta.

## Implementation Notes

- XML decode with encoding detection (encoding_rs) and fallbacks
- SAT: 1s throttle + exponential backoff (max 3 retries); cache to $DAIZO_DIR/cache

## License

- MIT OR Apache‑2.0
- © 2025 Shinryo Taniguchi

## Contributing
Issues and PRs are welcome. Please include ```daizo-cli doctor --verbose``` and a minimal repro.