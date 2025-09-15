# Changelog

All notable changes to this project will be documented in this file.

## [0.3.1] - 2025-09-15

### Added
- GRETIL (Sanskrit TEI) support across CLI and MCP:
  - New CLI commands: `gretil-title-search`, `gretil-search`, `gretil-fetch`, `gretil-pipeline`.
  - New MCP tools: `gretil_title_search`, `gretil_search`, `gretil_fetch`, `gretil_pipeline`.
- GRETIL installer step: download `1_sanskr.zip` to `$DAIZO_DIR/GRETIL/` and unzip; skip when already present.
- GRETIL indexer (`build_gretil_index`): extract `title/author/editor/translator/publisher/date/idno`, heads preview, keywords/classCode/catRef.
- Sanskrit-friendly matching (`compute_match_score_sanskrit`) for better title search on IAST texts.
- Token-economy defaults for MCP (AI clients):
  - Hard cap on returned text via `DAIZO_MCP_MAX_CHARS` (default 6000 chars).
  - Pipeline favors highlight snippets; `DAIZO_MCP_SNIPPET_LEN` (default 120), `DAIZO_MCP_AUTO_FILES` (default 1), `DAIZO_MCP_AUTO_MATCHES` (default 1).

### Changed
- README (EN/JA/ZH-TW) updated with GRETIL usage and examples.
- MCP CBETA/Tipitaka/GRETIL fetch handlers now enforce output cap even when `full=true`.
- MCP pipeline reduces auto-fetched context by default to minimize tokens; sends snippets unless full context is explicitly needed.

### Fixed
- Installer idempotency for GRETIL (skip re-download/unzip).
- Minor warnings and small robustness improvements.

### Notes
- Version bumped: `daizo-cli` 0.3.1, `daizo-mcp` 0.3.1.
- Environment knobs for AI clients:
  - `DAIZO_MCP_MAX_CHARS`, `DAIZO_MCP_SNIPPET_LEN`, `DAIZO_MCP_AUTO_FILES`, `DAIZO_MCP_AUTO_MATCHES`.

