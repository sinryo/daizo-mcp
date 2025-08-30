# Architecture Overview

## Goals
- Provide MCP tools to retrieve Buddhist texts from local CBETA TEI and SAT website.

## Components
- `src/index.ts`: MCP server exposing tools `cbeta.fetch` and `sat.fetch`.
- `src/cbeta.ts`: Locates TEI XML in `DAIZO_DIR/xml-p5` and extracts readable text.
- `src/sat.ts`: Fetches SAT pages and extracts main text content via Cheerio.
- `src/config.ts`: Environment configuration helpers.

## Data Flow (CBETA)
`cbeta.fetch` -> resolve file by id -> read XML -> mark lb/pb -> strip tags -> normalize -> return text.

## Data Flow (SAT)
`sat.fetch` -> build URL -> HTTP GET -> select probable content container -> normalize -> return text.

## Future Work
- Rich TEI traversal (preserve sections, notes as footnotes, variants).
- SAT ID to URL mapping and caching.
- Tests and fixtures for representative TEI and HTML samples.
