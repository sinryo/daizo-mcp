use anyhow::Result;
use daizo_core::text_utils::{
    compute_match_score_sanskrit, find_highlight_positions, is_subsequence, jaccard, normalized,
    token_jaccard, ws_cjk_variant_fuzzy_regex_literal,
};
use daizo_core::{
    build_cbeta_index, build_gretil_index, build_muktabodha_index, build_sarit_index,
    build_tipitaka_index, cbeta_gaiji_map_fast, cbeta_grep, extract_cbeta_juan,
    extract_cbeta_juan_plain, extract_cbeta_plain_from_snippet, extract_text,
    extract_text_around_line_asymmetric, extract_text_opts, gretil_grep, list_heads_cbeta,
    list_heads_generic, muktabodha_grep, sarit_grep, tipitaka_grep, IndexEntry,
};
use encoding_rs::Encoding;
use ewts::EwtsConverter;
use regex::Regex;
use reqwest::blocking::Client;
use scraper::{Html, Selector};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha1::{Digest, Sha1};
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

/// Version constant for the MCP server
const VERSION: &str = env!("CARGO_PKG_VERSION");
use daizo_core::path_resolver::{
    cache_dir, cbeta_root, daizo_home, find_exact_file_by_name, find_tipitaka_content_for_base,
    gretil_root, muktabodha_root, resolve_cbeta_path_by_id, resolve_muktabodha_by_id,
    resolve_muktabodha_path_direct, resolve_sarit_by_id, resolve_sarit_path_direct,
    resolve_tipitaka_by_id, sarit_root, tipitaka_root,
};

fn to_whitespace_fuzzy_literal(s: &str) -> String {
    // 連続した空白（改行含む）を \\s* に畳み込み、それ以外はリテラルとしてエスケープ
    let mut out = String::new();
    let mut in_ws = false;
    for ch in s.chars() {
        if ch.is_whitespace() {
            if !in_ws {
                out.push_str("\\s*");
                in_ws = true;
            }
        } else {
            in_ws = false;
            out.push_str(&regex::escape(&ch.to_string()));
        }
    }
    out
}

fn cbeta_extract_lb_from_line(line: &str) -> Option<String> {
    static LB_RE: OnceLock<Regex> = OnceLock::new();
    let re = LB_RE.get_or_init(|| Regex::new(r#"<lb\b[^>]*\bn\s*=\s*["']([^"']+)["']"#).unwrap());
    re.captures(line)
        .and_then(|c| c.get(1).map(|m| m.as_str().to_string()))
}

// ============ MCP stdio framing ============

fn dbg_enabled() -> bool {
    std::env::var("DAIZO_DEBUG").ok().as_deref() == Some("1")
}
fn dbg_log(msg: &str) {
    if !dbg_enabled() {
        return;
    }
    let path = daizo_home().join("daizo-mcp.log");
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        let _ = writeln!(f, "{}", msg);
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum FramingMode {
    Lsp,
    Lines,
}

static MODE: OnceLock<FramingMode> = OnceLock::new();

fn set_mode(m: FramingMode) {
    let _ = MODE.set(m);
}
fn get_mode() -> FramingMode {
    *MODE.get().unwrap_or(&FramingMode::Lsp)
}

fn read_message(stdin: &mut impl BufRead) -> Result<Option<serde_json::Value>> {
    // Try to read one logical unit. Support two modes:
    // 1) LSP-style headers with Content-Length and blank line
    // 2) Single-line JSON (newline-delimited JSON)

    let mut line = String::new();
    let n = stdin.read_line(&mut line)?;
    if n == 0 {
        return Ok(None);
    }

    let trimmed = line.trim_start();
    if trimmed.starts_with('{') {
        // newline-delimited JSON
        set_mode(FramingMode::Lines);
        dbg_log(&format!("[lines] {}", line.trim_end()));
        let v: serde_json::Value = serde_json::from_str(line.trim_end())?;
        return Ok(Some(v));
    }

    // Otherwise, collect headers until blank line, include the first line we read
    let mut headers = String::new();
    headers.push_str(&line);
    loop {
        if line == "\n" || line == "\r\n" || line.trim().is_empty() {
            break;
        }
        line.clear();
        let n = stdin.read_line(&mut line)?;
        if n == 0 {
            break;
        }
        headers.push_str(&line);
        if line == "\n" || line == "\r\n" || line.trim().is_empty() {
            break;
        }
    }
    set_mode(FramingMode::Lsp);
    dbg_log(&format!(
        "[hdr]{}",
        headers.replace('\r', "\\r").replace('\n', "\\n")
    ));

    // parse Content-Length
    let mut content_length = 0usize;
    for hline in headers.lines() {
        let h = hline.trim();
        if h.to_lowercase().starts_with("content-length:") {
            if let Some(v) = h.split(':').nth(1) {
                content_length = v.trim().parse().unwrap_or(0);
            }
        }
    }
    if content_length == 0 {
        dbg_log("[body] skip len=0");
        return Ok(Some(serde_json::Value::Null));
    }
    let mut content = vec![0u8; content_length];
    stdin.read_exact(&mut content)?;
    dbg_log(&format!("[body-bytes]{}", content_length));
    let v: serde_json::Value = serde_json::from_slice(&content)?;
    Ok(Some(v))
}

fn write_message(stdout: &mut impl Write, v: &serde_json::Value) -> Result<()> {
    match get_mode() {
        FramingMode::Lines => {
            let body = serde_json::to_string(v)?;
            writeln!(stdout, "{}", body)?;
            stdout.flush()?;
            dbg_log(&format!("[send-lines] {} chars", body.len()));
        }
        FramingMode::Lsp => {
            let body = serde_json::to_vec(v)?;
            write!(
                stdout,
                "Content-Length: {}\r\nContent-Type: application/vscode-jsonrpc; charset=utf-8\r\n\r\n",
                body.len()
            )?;
            stdout.write_all(&body)?;
            stdout.flush()?;
            dbg_log(&format!("[send-lsp] {} bytes", body.len()));
        }
    }
    Ok(())
}

fn env_usize(key: &str, default_v: usize) -> usize {
    std::env::var(key)
        .ok()
        .and_then(|s| s.parse::<usize>().ok())
        .unwrap_or(default_v)
}

fn default_max_chars() -> usize {
    env_usize("DAIZO_MCP_MAX_CHARS", 6000)
}
fn default_snippet_len() -> usize {
    env_usize("DAIZO_MCP_SNIPPET_LEN", 120)
}
fn default_auto_files() -> usize {
    env_usize("DAIZO_MCP_AUTO_FILES", 1)
}
fn default_auto_matches() -> usize {
    env_usize("DAIZO_MCP_AUTO_MATCHES", 1)
}

#[derive(Deserialize)]
struct Request {
    id: serde_json::Value,
    method: String,
    #[serde(default)]
    params: serde_json::Value,
}

// ============ Paths & cache (via daizo-core::path_resolver) ============
fn ensure_dir(p: &Path) {
    let _ = fs::create_dir_all(p);
}

fn ensure_cbeta_data() {
    let _ = daizo_core::repo::ensure_cbeta_data_at(&cbeta_root());
}

fn ensure_tipitaka_data() {
    let _ = daizo_core::repo::ensure_tipitaka_data_at(&daizo_home().join("tipitaka-xml"));
}

fn ensure_sarit_data() {
    let _ = daizo_core::repo::ensure_sarit_data_at(&sarit_root());
}

fn ensure_muktabodha_dir() {
    daizo_core::repo::ensure_muktabodha_dir(&muktabodha_root());
}

fn load_index(path: &Path) -> Option<Vec<IndexEntry>> {
    fs::read(path)
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
}

fn save_index(path: &Path, entries: &Vec<IndexEntry>) -> Result<()> {
    ensure_dir(path.parent().unwrap());
    fs::write(path, serde_json::to_vec(entries)?)?;
    Ok(())
}

// ============ Tool handlers ============

fn handle_initialize(id: serde_json::Value) -> serde_json::Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "result": {
            "protocolVersion": "2024-11-05",
            "capabilities": {
                "tools": {},
                "resources": {},
                "prompts": {
                    "low-token-guide": {
                        "name": "low-token-guide",
                        "description": "Guidance for AI to use search→fetch (lineNumber) instead of pipeline by default to minimize tokens.",
                        "messages": [
                            {"role": "system", "content": "Prefer low-token flow: 0) If corpus/ID is unknown, call daizo_resolve({query}) and follow its recommended fetch tool+args. 1) If ID is known, call *_fetch directly. 2) Otherwise use *_search, read _meta.fetchSuggestions, then call *_fetch with {id, lineNumber, contextBefore:1, contextAfter:3, highlight:<query>, format:\"plain\"}. 3) Use *_pipeline only for cross-file summary; set autoFetch=false by default."}
                        ]
                    }
                },
                "logging": {}
            },
            "serverInfo": { "name": "daizo-mcp", "version": VERSION }
        }
    })
}

fn tools_list() -> Vec<serde_json::Value> {
    vec![
        tool("daizo_version", "Get daizo-mcp server version and build information. Use this to check compatibility and troubleshoot issues.", json!({"type":"object","properties":{}})),
        tool("daizo_usage", "Usage guidance for AI: FAST PATH - use direct IDs! CBETA: T0001, T0262 (cbeta_fetch). Tipitaka: DN1, MN1 (tipitaka_fetch). GRETIL: saddharmapuNDarIka, vajracchedikA (gretil_fetch). No search needed when ID is known!", json!({"type":"object","properties":{}})),
        tool("daizo_profile", "Run an in-process benchmark for a tool call and return timing stats (warm cache). Use for performance measurement.", json!({"type":"object","properties":{
            "tool":{"type":"string","description":"Tool name to call (e.g., cbeta_search, cbeta_fetch, daizo_resolve)."},
            "arguments":{"type":"object","description":"Arguments object passed to the tool."},
            "iterations":{"type":"number","description":"Measured iterations (default: 10)."},
            "warmup":{"type":"number","description":"Warmup iterations (default: 1)."},
            "includeSamples":{"type":"boolean","description":"Include per-iteration samples in _meta (default: false)."}
        },"required":["tool","arguments"]})),
        tool("daizo_resolve", "Resolve a user query (title/alias/ID) to candidate corpus IDs and recommended next tool calls. Use this when you don't know which corpus/ID to use.", json!({"type":"object","properties":{
            "query":{"type":"string","description":"User query (title/alias/ID). Examples: '法華経', 'T0262', 'DN1', 'vajracchedikA'."},
            "sources":{"type":"array","items":{"type":"string","enum":["cbeta","tipitaka","gretil","sarit"]},"description":"Search scope. Default: ['cbeta','tipitaka','gretil','sarit']."},
            "limitPerSource":{"type":"number","description":"Max candidates per source (default: 5)"},
            "limit":{"type":"number","description":"Max total candidates (default: 10)"},
            "preferSource":{"type":"string","description":"Optional bias: cbeta|tipitaka|gretil|sarit"},
            "minScore":{"type":"number","description":"Filter out candidates below this score (default: 0.1)"}
        },"required":["query"]})),
        tool("cbeta_fetch", "Retrieve CBETA text by ID/part. FAST: If Taisho number is known (e.g. T0001, T0262 for Lotus Sutra), use id directly without search. Supports low-cost slices via id+lb (preferred) or id+lineNumber (XML line). TIP: Always pass 'highlight' with search term when fetching context!", json!({"type":"object","properties":{
            "id":{"type":"string","description":"Taisho number (e.g. T0001, T0262). Use this directly if known - much faster than query!"},
            "query":{"type":"string","description":"Fuzzy title search (slower). Prefer id if Taisho number is known."},
            "part":{"type":"string","description":"Juan/part number (e.g. '001'). Use for long texts."},
            "lb":{"type":"string","description":"CBETA line break marker n=... (e.g. '0114b27'). More stable than XML lineNumber."},
            "headIndex":{"type":"number","description":"Extract section by <head> index (0-based)."},
            "headQuery":{"type":"string","description":"Extract section by <head> substring match (e.g., '方便品')."},
            "includeNotes":{"type":"boolean"},
            "format":{"type":"string","description":"Output format. Use 'plain' for readable plain text (gaiji resolved, teiHeader excluded, line breaks preserved). Default keeps current behavior."},
            "full":{"type":"boolean","description":"Return full text without slicing"},
            "focusHighlight":{"type":"boolean","description":"If highlight is provided and no lb/lineNumber is specified, focus output around the first highlight match (default true)."},
            "highlight":{"type":"string","description":"Highlight string or regex pattern (used with lineNumber-based context)"},
            "highlightRegex":{"type":"boolean","description":"Interpret highlight as regex (default false)"},
            "highlightPrefix":{"type":"string","description":"Prefix marker for highlights (default \">>> \")"},
            "highlightSuffix":{"type":"string","description":"Suffix marker for highlights (default \" <<<\")"},
            "headingsLimit":{"type":"number"},
            "startChar":{"type":"number"},"endChar":{"type":"number"},"maxChars":{"type":"number"},
            "page":{"type":"number"},"pageSize":{"type":"number"},
            "lineNumber":{"type":"number","description":"Target XML line number for context extraction (from *_search). Prefer lb when available."},
            "contextBefore":{"type":"number","description":"Number of lines before target line (default: 10)"},
            "contextAfter":{"type":"number","description":"Number of lines after target line (default: 100)"},
            "contextLines":{"type":"number","description":"Number of lines before/after target line (deprecated, use contextBefore/contextAfter)"}
        }})),
        tool("cbeta_search", "Fast regex search over CBETA; returns _meta.fetchSuggestions (use cbeta_fetch with id+lineNumber+highlight). IMPORTANT: When fetching, always include highlight param with search term!", json!({"type":"object","properties":{
            "query":{"type":"string","description":"Regular expression pattern to search for"},
            "maxResults":{"type":"number","description":"Maximum number of files to return (default: 20)"},
            "maxMatchesPerFile":{"type":"number","description":"Maximum matches per file (default: 5)"}
        },"required":["query"]})),
        tool("cbeta_title_search", "Title-based search in CBETA corpus. Note: If Taisho number is already known (e.g. T0262), skip search and use cbeta_fetch directly with id!", json!({"type":"object","properties":{"query":{"type":"string","description":"Title to search. If you already know Taisho number, use cbeta_fetch with id instead."},"limit":{"type":"number"}},"required":["query"]})),
        tool("cbeta_pipeline", "CBETA summarize/context pipeline; set autoFetch=false for summary-only (see cbeta_search _meta.pipelineHint)", json!({"type":"object","properties":{
            "query":{"type":"string"},
            "maxResults":{"type":"number"},
            "maxMatchesPerFile":{"type":"number"},
            "contextBefore":{"type":"number"},
            "contextAfter":{"type":"number"},
            "autoFetch":{"type":"boolean"},
            "autoFetchFiles":{"type":"number","description":"Auto-fetch top N files (default 1 when autoFetch=true)"},
            "includeMatchLine":{"type":"boolean","description":"Include the matched line in auto-fetched context (default true)"},
            "includeHighlightSnippet":{"type":"boolean","description":"Include a short highlight snippet before each context (default true)"},
            "snippetPrefix":{"type":"string","description":"Prefix for highlight snippets in pipeline (default \">>> \")"},
            "snippetSuffix":{"type":"string","description":"Suffix for highlight snippets in pipeline (default '')"},
            "highlight":{"type":"string","description":"Highlight string or regex pattern inside contexts"},
            "highlightRegex":{"type":"boolean","description":"Interpret highlight as regex (default false)"},
            "highlightPrefix":{"type":"string","description":"Prefix marker for highlights (default from env or \">>> \")"},
            "highlightSuffix":{"type":"string","description":"Suffix marker for highlights (default from env or \" <<<\")"},
            "full":{"type":"boolean"},
            "includeNotes":{"type":"boolean"}
        },"required":["query"]})),
        tool("sat_detail", "Fetch SAT detail by useid", json!({"type":"object","properties":{"useid":{"type":"string"},"key":{"type":"string"},"startChar":{"type":"number"},"maxChars":{"type":"number"}},"required":["useid"]})),
        tool("sat_fetch", "Fetch SAT page (prefer useid to detail URL)", json!({"type":"object","properties":{
            "url":{"type":"string"},
            "useid":{"type":"string"},
            "startChar":{"type":"number"},
            "maxChars":{"type":"number"}
        }})),
        tool("sat_pipeline", "Search wrap7, pick best title, then fetch detail", json!({"type":"object","properties":{
            "query":{"type":"string"},
            "exact":{"type":"boolean","description":"If true (default), quote the query for phrase search."},
            "rows":{"type":"number"},
            "offs":{"type":"number"},
            "fields":{"type":"string"},
            "fq":{"type":"array","items":{"type":"string"}},
            "startChar":{"type":"number"},
            "maxChars":{"type":"number"}
        },"required":["query"]})),
	        tool("sat_search", "Search SAT wrap7.php", json!({"type":"object","properties":{
	            "query":{"type":"string"},
	            "rows":{"type":"number"},
	            "offs":{"type":"number"},
	            "exact":{"type":"boolean"},
	            "titlesOnly":{"type":"boolean"},
	            "fields":{"type":"string"},
	            "fq":{"type":"array","items":{"type":"string"}},
	            "autoFetch":{"type":"boolean"}
	        },"required":["query"]})),
	        tool("jozen_search", "Search Jodo Shu Zensho text database (online). Returns _meta.results and _meta.fetchSuggestions (use jozen_fetch with lineno).", json!({"type":"object","properties":{
	            "query":{"type":"string","description":"Search keyword(s). Separate words by space for AND search."},
	            "page":{"type":"number","description":"Page number (1-based). Default: 1."},
	            "maxResults":{"type":"number","description":"Max results to return from that page (<=50). Default: 20."},
	            "maxSnippetChars":{"type":"number","description":"Max snippet length in characters. Default: DAIZO_MCP_SNIPPET_LEN or 120."}
	        },"required":["query"]})),
	        tool("jozen_fetch", "Fetch Jodo Shu Zensho detail page by lineno (online). Returns page text with line IDs.", json!({"type":"object","properties":{
	            "lineno":{"type":"string","description":"Line/page id (e.g., 'J01_0200B19' or 'J01_0200')"},
	            "startChar":{"type":"number"},
	            "maxChars":{"type":"number"}
	        },"required":["lineno"]})),
	        tool("tibetan_search", "Full-text search over Tibetan corpora (online). Use this when you want Tibetan full-text search without downloading corpora. Sources: adarshah, buda.", json!({"type":"object","properties":{
	            "query":{"type":"string","description":"Search query. Tibetan Unicode or EWTS/Wylie accepted (we may auto-convert EWTS to Unicode)."},
	            "sources":{"type":"array","items":{"type":"string","enum":["adarshah","buda"]},"description":"Search backends. Default: ['adarshah','buda']."},
	            "limit":{"type":"number","description":"Max total results (default: 10)"},
            "exact":{"type":"boolean","description":"If true (default), prefer phrase/exact behavior when supported by the backend (currently BUDA)."},
            "maxSnippetChars":{"type":"number","description":"Max snippet length in characters (default: 240). Use 0 to disable truncation."},
            "wildcard":{"type":"boolean","description":"Adarshah-only: wildcard search (default false)."}
        },"required":["query"]})),
        tool("tipitaka_fetch", "Retrieve Tipitaka text. FAST: Use Nikāya codes directly (DN, MN, SN, AN, KN) without search. Examples: DN1, MN1, SN1, AN1. Or use file stems like s0101m.mul.", json!({"type":"object","properties":{
            "id":{"type":"string","description":"Nikāya code (DN, MN, SN, AN, KN) with optional number (e.g., DN1, MN1) or file stem (e.g., s0101m.mul). Use directly for fast access!"},
            "query":{"type":"string","description":"Fuzzy title search (slower). Prefer id if Nikāya code is known."},
            "headIndex":{"type":"number"},
            "headQuery":{"type":"string"},
            "headingsLimit":{"type":"number"},
            "highlight":{"type":"string","description":"Highlight string or regex pattern (used with lineNumber-based context)"},
            "highlightRegex":{"type":"boolean","description":"Interpret highlight as regex (default false)"},
            "highlightPrefix":{"type":"string","description":"Prefix marker for highlights (default from env or '>>> ')"},
            "highlightSuffix":{"type":"string","description":"Suffix marker for highlights (default from env or ' <<<')"},
            "startChar":{"type":"number"},
            "endChar":{"type":"number"},
            "maxChars":{"type":"number"},
            "page":{"type":"number"},
            "pageSize":{"type":"number"},
            "lineNumber":{"type":"number","description":"Target line number for context extraction"},
            "contextBefore":{"type":"number","description":"Number of lines before target line (default: 10)"},
            "contextAfter":{"type":"number","description":"Number of lines after target line (default: 100)"},
            "contextLines":{"type":"number","description":"Number of lines before/after target line (deprecated, use contextBefore/contextAfter)"}
        }})),
        tool("tipitaka_search", "Fast regex search over Tipitaka; returns _meta.fetchSuggestions (use tipitaka_fetch with id+lineNumber+highlight). Always include highlight param when fetching!", json!({"type":"object","properties":{
            "query":{"type":"string","description":"Regular expression pattern to search for"},
            "maxResults":{"type":"number","description":"Maximum number of files to return (default: 20)"},
            "maxMatchesPerFile":{"type":"number","description":"Maximum matches per file (default: 5)"}
        },"required":["query"]})),
        tool("tipitaka_title_search", "Title-based search in Tipitaka corpus. Note: If Nikāya code is known (DN, MN, SN, AN, KN), skip search and use tipitaka_fetch directly with id!", json!({"type":"object","properties":{"query":{"type":"string","description":"Title to search. If you know Nikāya code, use tipitaka_fetch with id instead."},"limit":{"type":"number"}},"required":["query"]})),
        // GRETIL (Sanskrit TEI)
        tool("gretil_title_search", "Title-based search in GRETIL corpus. Note: If text name is known, skip search and use gretil_fetch directly with id!", json!({"type":"object","properties":{"query":{"type":"string","description":"Title to search. If you know the file stem (e.g., 'saddharmapuNDarIka'), use gretil_fetch with id instead."},"limit":{"type":"number"}},"required":["query"]})),
        tool("gretil_search", "Fast regex search over GRETIL; returns _meta.fetchSuggestions (use gretil_fetch with id+lineNumber+highlight). Always include highlight param when fetching!", json!({"type":"object","properties":{
            "query":{"type":"string","description":"Regular expression pattern to search for"},
            "maxResults":{"type":"number","description":"Maximum number of files to return (default: 20)"},
            "maxMatchesPerFile":{"type":"number","description":"Maximum matches per file (default: 5)"}
        },"required":["query"]})),
        tool("gretil_fetch", "Retrieve GRETIL Sanskrit text by ID. FAST ACCESS: Use id directly (e.g., 'saddharmapuNDarIka', 'vajracchedikA', 'prajJApAramitAhRdayasUtra'). File stems follow sa_<textname>.xml pattern; you can omit 'sa_' prefix.", json!({"type":"object","properties":{
            "id":{"type":"string"},
            "query":{"type":"string"},
            "headIndex":{"type":"number","description":"Extract section by <head> index (0-based)."},
            "headQuery":{"type":"string","description":"Extract section by <head> substring match."},
            "includeNotes":{"type":"boolean"},
            "full":{"type":"boolean","description":"Return full text without slicing"},
            "highlight":{"type":"string","description":"Highlight string or regex pattern (used with lineNumber-based context)"},
            "highlightRegex":{"type":"boolean","description":"Interpret highlight as regex (default false)"},
            "highlightPrefix":{"type":"string","description":"Prefix marker for highlights (default '>>> ')"},
            "highlightSuffix":{"type":"string","description":"Suffix marker for highlights (default ' <<<')"},
            "headingsLimit":{"type":"number"},
            "startChar":{"type":"number"},"endChar":{"type":"number"},"maxChars":{"type":"number"},
            "page":{"type":"number"},"pageSize":{"type":"number"},
            "lineNumber":{"type":"number","description":"Target line number for context extraction"},
            "contextBefore":{"type":"number","description":"Number of lines before target line (default: 10)"},
            "contextAfter":{"type":"number","description":"Number of lines after target line (default: 100)"},
            "contextLines":{"type":"number","description":"Number of lines before/after target line (deprecated, use contextBefore/contextAfter)"}
        }})),
        tool("gretil_pipeline", "GRETIL summarize/context pipeline; set autoFetch=false for summary-only (see gretil_search _meta.pipelineHint)", json!({"type":"object","properties":{
            "query":{"type":"string"},
            "maxResults":{"type":"number"},
            "maxMatchesPerFile":{"type":"number"},
            "contextBefore":{"type":"number"},
            "contextAfter":{"type":"number"},
            "autoFetch":{"type":"boolean"},
            "autoFetchFiles":{"type":"number","description":"Auto-fetch top N files (default 1 when autoFetch=true)"},
            "includeMatchLine":{"type":"boolean","description":"Include the matched line in auto-fetched context (default true)"},
            "includeHighlightSnippet":{"type":"boolean","description":"Include a short highlight snippet before each context (default true)"},
            "snippetPrefix":{"type":"string","description":"Prefix for highlight snippets in pipeline (default '>>> ')"},
            "snippetSuffix":{"type":"string","description":"Suffix for highlight snippets in pipeline (default '')"},
            "highlight":{"type":"string","description":"Highlight string or regex pattern inside contexts"},
            "highlightRegex":{"type":"boolean","description":"Interpret highlight as regex (default false)"},
            "highlightPrefix":{"type":"string","description":"Prefix marker for highlights (default from env or '>>> ')"},
            "highlightSuffix":{"type":"string","description":"Suffix marker for highlights (default from env or ' <<<')"},
            "full":{"type":"boolean"},
            "includeNotes":{"type":"boolean"}
        },"required":["query"]})),
        // SARIT (TEI P5)
        tool("sarit_title_search", "Title-based search in SARIT corpus. Note: If file stem is known, skip search and use sarit_fetch directly with id!", json!({"type":"object","properties":{"query":{"type":"string","description":"Title to search. If you know the file stem (e.g., 'asvaghosa-buddhacarita'), use sarit_fetch with id instead."},"limit":{"type":"number"}},"required":["query"]})),
        tool("sarit_search", "Fast regex search over SARIT; returns _meta.fetchSuggestions (use sarit_fetch with id+lineNumber+highlight). Always include highlight param when fetching!", json!({"type":"object","properties":{
            "query":{"type":"string","description":"Regular expression pattern to search for"},
            "maxResults":{"type":"number","description":"Maximum number of files to return (default: 20)"},
            "maxMatchesPerFile":{"type":"number","description":"Maximum matches per file (default: 5)"}
        },"required":["query"]})),
        tool("sarit_fetch", "Retrieve SARIT TEI P5 text by ID. FAST ACCESS: Use id directly (file stem). Tries both repository root and transliterated/ subdir.", json!({"type":"object","properties":{
            "id":{"type":"string"},
            "query":{"type":"string"},
            "headIndex":{"type":"number","description":"Extract section by <head> index (0-based)."},
            "headQuery":{"type":"string","description":"Extract section by <head> substring match."},
            "includeNotes":{"type":"boolean"},
            "full":{"type":"boolean","description":"Return full text without slicing"},
            "highlight":{"type":"string","description":"Highlight string or regex pattern (used with lineNumber-based context)"},
            "highlightRegex":{"type":"boolean","description":"Interpret highlight as regex (default false)"},
            "highlightPrefix":{"type":"string","description":"Prefix marker for highlights (default '>>> ')"},
            "highlightSuffix":{"type":"string","description":"Suffix marker for highlights (default ' <<<')"},
            "headingsLimit":{"type":"number"},
            "startChar":{"type":"number"},"endChar":{"type":"number"},"maxChars":{"type":"number"},
            "page":{"type":"number"},"pageSize":{"type":"number"},
            "lineNumber":{"type":"number","description":"Target line number for context extraction"},
            "contextBefore":{"type":"number","description":"Number of lines before target line (default: 10)"},
            "contextAfter":{"type":"number","description":"Number of lines after target line (default: 100)"},
            "contextLines":{"type":"number","description":"Number of lines before/after target line (deprecated, use contextBefore/contextAfter)"}
        }})),
        tool("sarit_pipeline", "SARIT summarize/context pipeline; set autoFetch=false for summary-only (see sarit_search _meta.pipelineHint)", json!({"type":"object","properties":{
            "query":{"type":"string"},
            "maxResults":{"type":"number"},
            "maxMatchesPerFile":{"type":"number"},
            "contextBefore":{"type":"number"},
            "contextAfter":{"type":"number"},
            "autoFetch":{"type":"boolean"},
            "autoFetchFiles":{"type":"number","description":"Auto-fetch top N files (default 1 when autoFetch=true)"},
            "includeMatchLine":{"type":"boolean","description":"Include the matched line in auto-fetched context (default true)"},
            "includeHighlightSnippet":{"type":"boolean","description":"Include a short highlight snippet before each context (default true)"},
            "snippetPrefix":{"type":"string","description":"Prefix for highlight snippets in pipeline (default '>>> ')"},
            "snippetSuffix":{"type":"string","description":"Suffix for highlight snippets in pipeline (default '')"},
            "highlight":{"type":"string","description":"Highlight string or regex pattern inside contexts"},
            "highlightRegex":{"type":"boolean","description":"Interpret highlight as regex (default false)"},
            "highlightPrefix":{"type":"string","description":"Prefix marker for highlights (default from env or '>>> ')"},
            "highlightSuffix":{"type":"string","description":"Suffix marker for highlights (default from env or ' <<<')"},
            "full":{"type":"boolean"},
            "includeNotes":{"type":"boolean"}
        },"required":["query"]})),
        // MUKTABODHA
        tool("muktabodha_title_search", "Title-based search in MUKTABODHA Sanskrit library (IAST). If file stem is known, use muktabodha_fetch with id.", json!({"type":"object","properties":{"query":{"type":"string"},"limit":{"type":"number"}},"required":["query"]})),
        tool("muktabodha_search", "Fast regex search over MUKTABODHA; returns _meta.fetchSuggestions (use muktabodha_fetch with id+lineNumber+highlight).", json!({"type":"object","properties":{
            "query":{"type":"string","description":"Regular expression pattern to search for"},
            "maxResults":{"type":"number","description":"Maximum number of files to return (default: 20)"},
            "maxMatchesPerFile":{"type":"number","description":"Maximum matches per file (default: 5)"}
        },"required":["query"]})),
        tool("muktabodha_fetch", "Retrieve MUKTABODHA text by ID (file stem). Supports both .xml (TEI) and .txt files.", json!({"type":"object","properties":{
            "id":{"type":"string"},
            "query":{"type":"string"},
            "includeNotes":{"type":"boolean"},
            "full":{"type":"boolean"},
            "highlight":{"type":"string"},
            "highlightRegex":{"type":"boolean"},
            "highlightPrefix":{"type":"string"},
            "highlightSuffix":{"type":"string"},
            "headingsLimit":{"type":"number"},
            "startChar":{"type":"number"},"endChar":{"type":"number"},"maxChars":{"type":"number"},
            "page":{"type":"number"},"pageSize":{"type":"number"},
            "lineNumber":{"type":"number"},
            "contextBefore":{"type":"number"},
            "contextAfter":{"type":"number"},
            "contextLines":{"type":"number"}
        }})),
        tool("muktabodha_pipeline", "MUKTABODHA summarize/context pipeline; set autoFetch=false for summary-only.", json!({"type":"object","properties":{
            "query":{"type":"string"},
            "maxResults":{"type":"number"},
            "maxMatchesPerFile":{"type":"number"},
            "contextBefore":{"type":"number"},
            "contextAfter":{"type":"number"},
            "autoFetch":{"type":"boolean"},
            "autoFetchFiles":{"type":"number"},
            "includeMatchLine":{"type":"boolean"},
            "includeHighlightSnippet":{"type":"boolean"},
            "snippetPrefix":{"type":"string"},
            "snippetSuffix":{"type":"string"},
            "highlight":{"type":"string"},
            "highlightRegex":{"type":"boolean"},
            "highlightPrefix":{"type":"string"},
            "highlightSuffix":{"type":"string"},
            "full":{"type":"boolean"},
            "includeNotes":{"type":"boolean"}
        },"required":["query"]})),
    ]
}

fn tool(name: &str, description: &str, input_schema: serde_json::Value) -> serde_json::Value {
    json!({"name": name, "description": description, "inputSchema": input_schema })
}

fn handle_tools_list(id: serde_json::Value) -> serde_json::Value {
    json!({"jsonrpc":"2.0","id":id,"result": {"tools": tools_list()}})
}

// normalization and token similarity helpers are provided by daizo_core::text_utils

// メモリキャッシュ: プロセス内でインデックスを再利用し、毎回のJSONパースを回避
static CBETA_INDEX_CACHE: OnceLock<Vec<IndexEntry>> = OnceLock::new();

fn load_or_build_cbeta_index() -> &'static Vec<IndexEntry> {
    // NOTE: Do not clone the entire index on every call; keep a single in-process instance.
    CBETA_INDEX_CACHE.get_or_init(|| {
        let out = cache_dir().join("cbeta-index.json");
        if let Some(v) = load_index(&out) {
            // 既存インデックスの健全性を軽くチェック（パスの存在 + メタの有無）
            let missing = v
                .iter()
                .take(10)
                .filter(|e| !Path::new(&e.path).exists())
                .count();
            let lacks_meta = v.iter().take(10).any(|e| e.meta.is_none());
            let lacks_ver = v.iter().take(10).any(|e| {
                e.meta
                    .as_ref()
                    .and_then(|m| m.get("indexVersion"))
                    .map(|s| s.as_str() != "cbeta_index_v2")
                    .unwrap_or(true)
            });
            if !v.is_empty() && missing == 0 && !lacks_meta && !lacks_ver {
                return v;
            }
        }
        // Ensure data exists (clone if needed)
        ensure_cbeta_data();
        let entries = build_cbeta_index(&cbeta_root());
        let _ = save_index(&out, &entries);
        entries
    })
}

struct TitleHayCache {
    hay_norm: Vec<String>,
    hay_ws: Vec<String>,
}

static CBETA_TITLE_HAY_CACHE: OnceLock<TitleHayCache> = OnceLock::new();

fn cbeta_title_hay_cache(entries: &[IndexEntry]) -> Option<&'static TitleHayCache> {
    if let Some(c) = CBETA_TITLE_HAY_CACHE.get() {
        if c.hay_norm.len() == entries.len() && c.hay_ws.len() == entries.len() {
            return Some(c);
        }
        return None;
    }
    let c = CBETA_TITLE_HAY_CACHE.get_or_init(|| {
        let mut hay_norm: Vec<String> = Vec::with_capacity(entries.len());
        let mut hay_ws: Vec<String> = Vec::with_capacity(entries.len());
        for e in entries.iter() {
            let meta_str = e
                .meta
                .as_ref()
                .map(|m| {
                    let mut s = String::new();
                    for v in m.values() {
                        if !s.is_empty() {
                            s.push(' ');
                        }
                        s.push_str(v);
                    }
                    s
                })
                .unwrap_or_default();
            let hay_all = format!("{} {} {}", e.title, e.id, meta_str);
            hay_norm.push(daizo_core::text_utils::normalized(&hay_all));
            hay_ws.push(daizo_core::text_utils::normalized_with_spaces(&hay_all));
        }
        TitleHayCache { hay_norm, hay_ws }
    });
    if c.hay_norm.len() == entries.len() && c.hay_ws.len() == entries.len() {
        Some(c)
    } else {
        None
    }
}
// メモリキャッシュ: Tipitakaインデックス
static TIPITAKA_INDEX_CACHE: OnceLock<Vec<IndexEntry>> = OnceLock::new();

fn load_or_build_tipitaka_index() -> &'static Vec<IndexEntry> {
    // NOTE: Do not clone the entire index on every call; keep a single in-process instance.
    TIPITAKA_INDEX_CACHE.get_or_init(|| {
        let out = cache_dir().join("tipitaka-index.json");
        if let Some(mut v) = load_index(&out) {
            v.retain(|e| !e.path.ends_with(".toc.xml"));
            let missing = v
                .iter()
                .take(10)
                .filter(|e| !Path::new(&e.path).exists())
                .count();
            let lacks_meta = v.iter().take(10).any(|e| e.meta.is_none());
            let lacks_heads = v.iter().take(20).any(|e| {
                e.meta
                    .as_ref()
                    .map(|m| !m.contains_key("headsPreview"))
                    .unwrap_or(true)
            });
            let lacks_ver = v.iter().take(10).any(|e| {
                e.meta
                    .as_ref()
                    .and_then(|m| m.get("indexVersion"))
                    .map(|s| s.as_str() != "tipitaka_index_v2")
                    .unwrap_or(true)
            });
            let lacks_composite = v.iter().take(50).any(|e| {
                if let Some(m) = &e.meta {
                    let p = m.get("alias_prefix").map(|s| s.as_str()).unwrap_or("");
                    if p == "SN" || p == "AN" {
                        return !m.get("alias").map(|a| a.contains('.')).unwrap_or(false);
                    }
                }
                false
            });
            if !v.is_empty()
                && missing == 0
                && !lacks_meta
                && !lacks_heads
                && !lacks_ver
                && !lacks_composite
            {
                return v;
            }
        }
        ensure_tipitaka_data();
        let mut entries = build_tipitaka_index(&tipitaka_root());
        entries.retain(|e| !e.path.ends_with(".toc.xml"));
        let _ = save_index(&out, &entries);
        entries
    })
}
// メモリキャッシュ: GRETILインデックス
static GRETIL_INDEX_CACHE: OnceLock<Vec<IndexEntry>> = OnceLock::new();

fn load_or_build_gretil_index() -> &'static Vec<IndexEntry> {
    // NOTE: Do not clone the entire index on every call; keep a single in-process instance.
    GRETIL_INDEX_CACHE.get_or_init(|| {
        let out = cache_dir().join("gretil-index.json");
        if let Some(v) = load_index(&out) {
            let missing = v
                .iter()
                .take(10)
                .filter(|e| !Path::new(&e.path).exists())
                .count();
            if !v.is_empty() && missing == 0 {
                return v;
            }
        }
        let entries = build_gretil_index(&gretil_root());
        let _ = save_index(&out, &entries);
        entries
    })
}

// メモリキャッシュ: SARITインデックス
static SARIT_INDEX_CACHE: OnceLock<Vec<IndexEntry>> = OnceLock::new();

fn load_or_build_sarit_index() -> &'static Vec<IndexEntry> {
    SARIT_INDEX_CACHE.get_or_init(|| {
        let out = cache_dir().join("sarit-index.json");
        if let Some(v) = load_index(&out) {
            let missing = v
                .iter()
                .take(10)
                .filter(|e| !Path::new(&e.path).exists())
                .count();
            if !v.is_empty() && missing == 0 {
                return v;
            }
        }
        ensure_sarit_data();
        let entries = build_sarit_index(&sarit_root());
        let _ = save_index(&out, &entries);
        entries
    })
}

// メモリキャッシュ: MUKTABODHAインデックス
static MUKTABODHA_INDEX_CACHE: OnceLock<Vec<IndexEntry>> = OnceLock::new();

fn load_or_build_muktabodha_index() -> &'static Vec<IndexEntry> {
    MUKTABODHA_INDEX_CACHE.get_or_init(|| {
        let out = cache_dir().join("muktabodha-index.json");
        if let Some(v) = load_index(&out) {
            let missing = v
                .iter()
                .take(10)
                .filter(|e| !Path::new(&e.path).exists())
                .count();
            if !v.is_empty() && missing == 0 {
                return v;
            }
        }
        ensure_muktabodha_dir();
        let entries = build_muktabodha_index(&muktabodha_root());
        let _ = save_index(&out, &entries);
        entries
    })
}

struct GretilHayCache {
    hay_norm: Vec<String>,
    hay_ws: Vec<String>,
    hay_fold: Vec<String>,
}

static GRETIL_TITLE_HAY_CACHE: OnceLock<GretilHayCache> = OnceLock::new();

fn gretil_title_hay_cache(entries: &[IndexEntry]) -> Option<&'static GretilHayCache> {
    if let Some(c) = GRETIL_TITLE_HAY_CACHE.get() {
        if c.hay_norm.len() == entries.len()
            && c.hay_ws.len() == entries.len()
            && c.hay_fold.len() == entries.len()
        {
            return Some(c);
        }
        return None;
    }
    let c = GRETIL_TITLE_HAY_CACHE.get_or_init(|| {
        let mut hay_norm: Vec<String> = Vec::with_capacity(entries.len());
        let mut hay_ws: Vec<String> = Vec::with_capacity(entries.len());
        let mut hay_fold: Vec<String> = Vec::with_capacity(entries.len());
        for e in entries.iter() {
            let meta_str = e
                .meta
                .as_ref()
                .map(|m| {
                    let mut s = String::new();
                    for v in m.values() {
                        if !s.is_empty() {
                            s.push(' ');
                        }
                        s.push_str(v);
                    }
                    s
                })
                .unwrap_or_default();
            let hay_all = format!("{} {} {}", e.title, e.id, meta_str);
            hay_norm.push(daizo_core::text_utils::normalized(&hay_all));
            hay_ws.push(daizo_core::text_utils::normalized_with_spaces(&hay_all));
            hay_fold.push(daizo_core::text_utils::normalized_sanskrit(&hay_all));
        }
        GretilHayCache {
            hay_norm,
            hay_ws,
            hay_fold,
        }
    });
    if c.hay_norm.len() == entries.len()
        && c.hay_ws.len() == entries.len()
        && c.hay_fold.len() == entries.len()
    {
        Some(c)
    } else {
        None
    }
}

static SARIT_TITLE_HAY_CACHE: OnceLock<GretilHayCache> = OnceLock::new();

fn sarit_title_hay_cache(entries: &[IndexEntry]) -> Option<&'static GretilHayCache> {
    if let Some(c) = SARIT_TITLE_HAY_CACHE.get() {
        if c.hay_norm.len() == entries.len()
            && c.hay_ws.len() == entries.len()
            && c.hay_fold.len() == entries.len()
        {
            return Some(c);
        }
        return None;
    }
    let c = SARIT_TITLE_HAY_CACHE.get_or_init(|| {
        let mut hay_norm: Vec<String> = Vec::with_capacity(entries.len());
        let mut hay_ws: Vec<String> = Vec::with_capacity(entries.len());
        let mut hay_fold: Vec<String> = Vec::with_capacity(entries.len());
        for e in entries.iter() {
            let meta_str = e
                .meta
                .as_ref()
                .map(|m| {
                    let mut s = String::new();
                    for v in m.values() {
                        if !s.is_empty() {
                            s.push(' ');
                        }
                        s.push_str(v);
                    }
                    s
                })
                .unwrap_or_default();
            let hay_all = format!("{} {} {}", e.title, e.id, meta_str);
            hay_norm.push(daizo_core::text_utils::normalized(&hay_all));
            hay_ws.push(daizo_core::text_utils::normalized_with_spaces(&hay_all));
            hay_fold.push(daizo_core::text_utils::normalized_sanskrit(&hay_all));
        }
        GretilHayCache {
            hay_norm,
            hay_ws,
            hay_fold,
        }
    });
    if c.hay_norm.len() == entries.len()
        && c.hay_ws.len() == entries.len()
        && c.hay_fold.len() == entries.len()
    {
        Some(c)
    } else {
        None
    }
}

static MUKTABODHA_TITLE_HAY_CACHE: OnceLock<GretilHayCache> = OnceLock::new();

fn muktabodha_title_hay_cache(entries: &[IndexEntry]) -> Option<&'static GretilHayCache> {
    if let Some(c) = MUKTABODHA_TITLE_HAY_CACHE.get() {
        if c.hay_norm.len() == entries.len()
            && c.hay_ws.len() == entries.len()
            && c.hay_fold.len() == entries.len()
        {
            return Some(c);
        }
        return None;
    }
    let c = MUKTABODHA_TITLE_HAY_CACHE.get_or_init(|| {
        let mut hay_norm: Vec<String> = Vec::with_capacity(entries.len());
        let mut hay_ws: Vec<String> = Vec::with_capacity(entries.len());
        let mut hay_fold: Vec<String> = Vec::with_capacity(entries.len());
        for e in entries.iter() {
            let meta_str = e
                .meta
                .as_ref()
                .map(|m| {
                    let mut s = String::new();
                    for v in m.values() {
                        if !s.is_empty() {
                            s.push(' ');
                        }
                        s.push_str(v);
                    }
                    s
                })
                .unwrap_or_default();
            let hay_all = format!("{} {} {}", e.title, e.id, meta_str);
            hay_norm.push(daizo_core::text_utils::normalized(&hay_all));
            hay_ws.push(daizo_core::text_utils::normalized_with_spaces(&hay_all));
            hay_fold.push(daizo_core::text_utils::normalized_sanskrit(&hay_all));
        }
        GretilHayCache {
            hay_norm,
            hay_ws,
            hay_fold,
        }
    });
    if c.hay_norm.len() == entries.len()
        && c.hay_ws.len() == entries.len()
        && c.hay_fold.len() == entries.len()
    {
        Some(c)
    } else {
        None
    }
}

#[derive(Clone)]
struct CbetaFileCacheEntry {
    path: PathBuf,
    xml: Arc<String>,
    gaiji: Option<Arc<std::collections::HashMap<String, String>>>,
    heads: Option<Arc<Vec<String>>>,
}

static CBETA_FILE_CACHE: OnceLock<Mutex<Vec<CbetaFileCacheEntry>>> = OnceLock::new();

fn cbeta_file_cache_cap() -> usize {
    std::env::var("DAIZO_CBETA_FILE_CACHE")
        .ok()
        .and_then(|s| s.parse::<usize>().ok())
        .unwrap_or(2)
        .clamp(0, 16)
}

fn cbeta_xml_cached(path: &Path) -> Arc<String> {
    let cache = CBETA_FILE_CACHE.get_or_init(|| Mutex::new(Vec::new()));
    // Fast path: cache hit
    if let Some(xml) = {
        let mut guard = cache.lock().unwrap();
        if let Some(pos) = guard.iter().position(|e| e.path == path) {
            let entry = guard.remove(pos);
            let xml = entry.xml.clone();
            guard.insert(0, entry);
            Some(xml)
        } else {
            None
        }
    } {
        return xml;
    }

    // Miss: read outside lock
    let xml_s = fs::read_to_string(path).unwrap_or_default();
    let xml = Arc::new(xml_s);

    let mut guard = cache.lock().unwrap();
    guard.retain(|e| e.path != path);
    guard.insert(
        0,
        CbetaFileCacheEntry {
            path: path.to_path_buf(),
            xml: xml.clone(),
            gaiji: None,
            heads: None,
        },
    );
    let cap = cbeta_file_cache_cap();
    if cap == 0 {
        guard.clear();
    } else if guard.len() > cap {
        guard.truncate(cap);
    }
    xml
}

fn cbeta_gaiji_cached(path: &Path, xml: &str) -> Arc<std::collections::HashMap<String, String>> {
    let cache = CBETA_FILE_CACHE.get_or_init(|| Mutex::new(Vec::new()));
    if let Some(g) = {
        let mut guard = cache.lock().unwrap();
        if let Some(pos) = guard.iter().position(|e| e.path == path) {
            let entry = guard.remove(pos);
            let g = entry.gaiji.clone();
            guard.insert(0, entry);
            g
        } else {
            None
        }
    } {
        return g;
    }

    // Miss: compute outside lock
    let g_map = cbeta_gaiji_map_fast(xml);
    let g = Arc::new(g_map);

    let mut guard = cache.lock().unwrap();
    if let Some(pos) = guard.iter().position(|e| e.path == path) {
        guard[pos].gaiji = Some(g.clone());
        // keep LRU-ish by moving to front
        let entry = guard.remove(pos);
        guard.insert(0, entry);
    }
    g
}

fn cbeta_heads_cached(path: &Path, xml: &str) -> Arc<Vec<String>> {
    let cache = CBETA_FILE_CACHE.get_or_init(|| Mutex::new(Vec::new()));
    if let Some(h) = {
        let mut guard = cache.lock().unwrap();
        if let Some(pos) = guard.iter().position(|e| e.path == path) {
            let entry = guard.remove(pos);
            let h = entry.heads.clone();
            guard.insert(0, entry);
            h
        } else {
            None
        }
    } {
        return h;
    }

    // Miss: compute outside lock
    let heads = list_heads_cbeta(xml);
    let h = Arc::new(heads);

    let mut guard = cache.lock().unwrap();
    if let Some(pos) = guard.iter().position(|e| e.path == path) {
        guard[pos].heads = Some(h.clone());
        let entry = guard.remove(pos);
        guard.insert(0, entry);
    }
    h
}

#[derive(Clone, Debug, Serialize)]
struct ScoredHit<'a> {
    #[serde(skip_serializing)]
    entry: &'a IndexEntry,
    score: f32,
}

fn scored_cmp(a: &(f32, &IndexEntry), b: &(f32, &IndexEntry)) -> std::cmp::Ordering {
    match b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal) {
        std::cmp::Ordering::Equal => a.1.id.cmp(&b.1.id),
        other => other,
    }
}

fn topk_insert<'a>(
    top: &mut Vec<(f32, &'a IndexEntry)>,
    cand: (f32, &'a IndexEntry),
    limit: usize,
) {
    if limit == 0 {
        return;
    }
    if top.len() == limit {
        if let Some(worst) = top.last() {
            // Keep `top` sorted best-first; ignore candidates not better than worst.
            if scored_cmp(&cand, worst) != std::cmp::Ordering::Less {
                return;
            }
        }
    }
    // Find insertion point (limit is small; linear scan is faster than full sort).
    let mut pos = top.len();
    for i in 0..top.len() {
        if scored_cmp(&cand, &top[i]) == std::cmp::Ordering::Less {
            pos = i;
            break;
        }
    }
    if pos == top.len() {
        top.push(cand);
    } else {
        top.insert(pos, cand);
    }
    if top.len() > limit {
        top.truncate(limit);
    }
}

fn best_match<'a>(entries: &'a [IndexEntry], q: &str, limit: usize) -> Vec<ScoredHit<'a>> {
    let pq = daizo_core::text_utils::PrecomputedQuery::new(q, false);
    let nq = pq.normalized();
    let hay_cache = cbeta_title_hay_cache(entries);
    let mut top: Vec<(f32, &IndexEntry)> = Vec::with_capacity(limit.min(32));
    for (i, e) in entries.iter().enumerate() {
        let mut s = if let Some(cache) = hay_cache {
            daizo_core::text_utils::compute_match_score_precomputed_with_hay(
                e,
                &cache.hay_norm[i],
                &cache.hay_ws[i],
                &pq,
            )
        } else {
            daizo_core::text_utils::compute_match_score_precomputed(e, &pq)
        };
        if let Some(meta) = &e.meta {
            // CBETA: bias toward Taisho canon by default (common user expectation).
            if meta.get("canon").map(|c| c.as_str()) == Some("T") {
                s = (s + 0.02).min(1.2);
            } else if e.id.starts_with('T') {
                s = (s + 0.01).min(1.2);
            }
            for k in ["author", "editor", "translator", "publisher"].iter() {
                if let Some(v) = meta.get(*k) {
                    let nv = normalized(v);
                    if !nv.is_empty() && (nv.contains(nq) || nq.contains(&nv)) {
                        s = s.max(0.93);
                    }
                }
            }
        }
        topk_insert(&mut top, (s, e), limit);
    }
    top.into_iter()
        .map(|(s, e)| ScoredHit { entry: e, score: s })
        .collect()
}

fn best_match_tipitaka<'a>(entries: &'a [IndexEntry], q: &str, limit: usize) -> Vec<ScoredHit<'a>> {
    let mut top: Vec<(f32, &IndexEntry)> = Vec::with_capacity(limit.min(32));
    let pq = daizo_core::text_utils::PrecomputedQuery::new(q, true);
    for e in entries.iter() {
        let s = daizo_core::text_utils::compute_match_score_precomputed(e, &pq);
        topk_insert(&mut top, (s, e), limit);
    }
    top.into_iter()
        .map(|(s, e)| ScoredHit { entry: e, score: s })
        .collect()
}

fn best_match_gretil<'a>(entries: &'a [IndexEntry], q: &str, limit: usize) -> Vec<ScoredHit<'a>> {
    let nq = normalized(q);
    let nq_ws = daizo_core::text_utils::normalized_with_spaces(q);
    let nq_nospace = nq_ws.replace(' ', "");
    let nq_fold = daizo_core::text_utils::normalized_sanskrit(q);
    let q_tokens: std::collections::HashSet<String> =
        nq_ws.split_whitespace().map(|w| w.to_string()).collect();
    let has_digit = q.chars().any(|c| c.is_ascii_digit());
    let hay_cache = gretil_title_hay_cache(entries);

    let mut top: Vec<(f32, &IndexEntry)> = Vec::with_capacity(limit.min(32));
    for (i, e) in entries.iter().enumerate() {
        let mut s = if let Some(cache) = hay_cache {
            let hay = &cache.hay_norm[i];
            let hay_ws = &cache.hay_ws[i];
            let hay_fold = &cache.hay_fold[i];

            let mut score = if hay.contains(&nq) {
                1.0
            } else {
                let s_char = jaccard(hay, &nq);
                let s_tok = if q_tokens.is_empty() {
                    0.0
                } else {
                    // token jaccard from pre-normalized whitespace form (avoid per-entry NFKD)
                    let mut uniq: Vec<&str> = Vec::new();
                    let mut inter = 0usize;
                    for tok in hay_ws.split_whitespace() {
                        if uniq.iter().any(|t| *t == tok) {
                            continue;
                        }
                        if q_tokens.contains(tok) {
                            inter += 1;
                        }
                        uniq.push(tok);
                    }
                    let sa_len = uniq.len();
                    let sb_len = q_tokens.len();
                    if sa_len == 0 || sb_len == 0 {
                        0.0
                    } else {
                        let uni = (sa_len + sb_len).saturating_sub(inter);
                        if uni == 0 {
                            0.0
                        } else {
                            inter as f32 / uni as f32
                        }
                    }
                };
                s_char.max(s_tok).max(jaccard(hay_fold, &nq_fold))
            };

            if score < 0.95 {
                let subseq = is_subsequence(hay, &nq)
                    || is_subsequence(&nq, hay)
                    || is_subsequence(hay_fold, &nq_fold);
                if subseq {
                    score = score.max(0.85);
                }
            }

            let alias = e
                .meta
                .as_ref()
                .and_then(|m| m.get("alias").map(|s| s.as_str()))
                .unwrap_or("");
            let nalias = daizo_core::text_utils::normalized_with_spaces(alias).replace(' ', "");
            let nalias_fold = daizo_core::text_utils::normalized_sanskrit(alias);
            if !nalias.is_empty() {
                if nalias.split_whitespace().any(|a| a == nq_nospace)
                    || nalias.contains(&nq_nospace)
                    || (!nalias_fold.is_empty() && nalias_fold.contains(&nq_fold))
                {
                    score = score.max(0.95);
                }
            }

            if has_digit && !nq_ws.is_empty() && hay_ws.contains(&nq_ws) {
                score = (score + 0.05).min(1.0);
            }
            score
        } else {
            compute_match_score_sanskrit(e, q)
        };

        if let Some(meta) = &e.meta {
            for k in ["author", "editor", "translator", "publisher"].iter() {
                if let Some(v) = meta.get(*k) {
                    let nv = normalized(v);
                    if !nv.is_empty() && (nv.contains(&nq) || nq.contains(&nv)) {
                        s = s.max(0.93);
                    }
                }
            }
        }
        topk_insert(&mut top, (s, e), limit);
    }
    top.into_iter()
        .map(|(s, e)| ScoredHit { entry: e, score: s })
        .collect()
}

fn best_match_sarit<'a>(entries: &'a [IndexEntry], q: &str, limit: usize) -> Vec<ScoredHit<'a>> {
    // SARIT は多表記（デーヴァナーガリー/ローマナイズ等）を含むため、
    // GRETIL と同等の正規化・折り畳みロジックを使う。
    let nq = normalized(q);
    let nq_ws = daizo_core::text_utils::normalized_with_spaces(q);
    let nq_nospace = nq_ws.replace(' ', "");
    let nq_fold = daizo_core::text_utils::normalized_sanskrit(q);
    let q_tokens: std::collections::HashSet<String> =
        nq_ws.split_whitespace().map(|w| w.to_string()).collect();
    let has_digit = q.chars().any(|c| c.is_ascii_digit());
    let hay_cache = sarit_title_hay_cache(entries);

    let mut top: Vec<(f32, &IndexEntry)> = Vec::with_capacity(limit.min(32));
    for (i, e) in entries.iter().enumerate() {
        let mut s = if let Some(cache) = hay_cache {
            let hay = &cache.hay_norm[i];
            let hay_ws = &cache.hay_ws[i];
            let hay_fold = &cache.hay_fold[i];

            let mut score = if hay.contains(&nq) {
                1.0
            } else {
                let s_char = jaccard(hay, &nq);
                let s_tok = if q_tokens.is_empty() {
                    0.0
                } else {
                    let mut uniq: Vec<&str> = Vec::new();
                    let mut inter = 0usize;
                    for tok in hay_ws.split_whitespace() {
                        if uniq.iter().any(|t| *t == tok) {
                            continue;
                        }
                        if q_tokens.contains(tok) {
                            inter += 1;
                        }
                        uniq.push(tok);
                    }
                    let sa_len = uniq.len();
                    let sb_len = q_tokens.len();
                    if sa_len == 0 || sb_len == 0 {
                        0.0
                    } else {
                        let uni = (sa_len + sb_len).saturating_sub(inter);
                        if uni == 0 {
                            0.0
                        } else {
                            inter as f32 / uni as f32
                        }
                    }
                };
                s_char.max(s_tok).max(jaccard(hay_fold, &nq_fold))
            };

            if score < 0.95 {
                let subseq = is_subsequence(hay, &nq)
                    || is_subsequence(&nq, hay)
                    || is_subsequence(hay_fold, &nq_fold);
                if subseq {
                    score = score.max(0.85);
                }
            }

            let alias = e
                .meta
                .as_ref()
                .and_then(|m| m.get("alias").map(|s| s.as_str()))
                .unwrap_or("");
            let nalias = daizo_core::text_utils::normalized_with_spaces(alias).replace(' ', "");
            let nalias_fold = daizo_core::text_utils::normalized_sanskrit(alias);
            if !nalias.is_empty() {
                if nalias.split_whitespace().any(|a| a == nq_nospace)
                    || nalias.contains(&nq_nospace)
                    || (!nalias_fold.is_empty() && nalias_fold.contains(&nq_fold))
                {
                    score = score.max(0.95);
                }
            }

            if has_digit && !nq_ws.is_empty() && hay_ws.contains(&nq_ws) {
                score = (score + 0.05).min(1.0);
            }
            score
        } else {
            compute_match_score_sanskrit(e, q)
        };

        if let Some(meta) = &e.meta {
            for k in ["author", "editor", "translator", "publisher"].iter() {
                if let Some(v) = meta.get(*k) {
                    let nv = normalized(v);
                    if !nv.is_empty() && (nv.contains(&nq) || nq.contains(&nv)) {
                        s = s.max(0.93);
                    }
                }
            }
        }
        topk_insert(&mut top, (s, e), limit);
    }
    top.into_iter()
        .map(|(s, e)| ScoredHit { entry: e, score: s })
        .collect()
}

fn best_match_muktabodha<'a>(
    entries: &'a [IndexEntry],
    q: &str,
    limit: usize,
) -> Vec<ScoredHit<'a>> {
    // サンスクリット向け（IAST）として GRETIL と同じ方式でスコアリング。
    // MUKTABODHA は .txt も混ざる想定だが、タイトル/ID/メタでマッチさせる。
    let nq = normalized(q);
    let nq_ws = daizo_core::text_utils::normalized_with_spaces(q);
    let nq_nospace = nq_ws.replace(' ', "");
    let nq_fold = daizo_core::text_utils::normalized_sanskrit(q);
    let q_tokens: std::collections::HashSet<String> =
        nq_ws.split_whitespace().map(|w| w.to_string()).collect();
    let has_digit = q.chars().any(|c| c.is_ascii_digit());
    let hay_cache = muktabodha_title_hay_cache(entries);

    let mut top: Vec<(f32, &IndexEntry)> = Vec::with_capacity(limit.min(32));
    for (i, e) in entries.iter().enumerate() {
        let mut s = if let Some(cache) = hay_cache {
            let hay = &cache.hay_norm[i];
            let hay_ws = &cache.hay_ws[i];
            let hay_fold = &cache.hay_fold[i];

            let mut score = if hay.contains(&nq) {
                1.0
            } else {
                let s_char = jaccard(hay, &nq);
                let s_tok = if q_tokens.is_empty() {
                    0.0
                } else {
                    let mut uniq: Vec<&str> = Vec::new();
                    let mut inter = 0usize;
                    for tok in hay_ws.split_whitespace() {
                        if uniq.iter().any(|t| *t == tok) {
                            continue;
                        }
                        if q_tokens.contains(tok) {
                            inter += 1;
                        }
                        uniq.push(tok);
                    }
                    let sa_len = uniq.len();
                    let sb_len = q_tokens.len();
                    if sa_len == 0 || sb_len == 0 {
                        0.0
                    } else {
                        let uni = (sa_len + sb_len).saturating_sub(inter);
                        if uni == 0 {
                            0.0
                        } else {
                            inter as f32 / uni as f32
                        }
                    }
                };
                s_char.max(s_tok).max(jaccard(hay_fold, &nq_fold))
            };

            if score < 0.95 {
                let subseq = is_subsequence(hay, &nq)
                    || is_subsequence(&nq, hay)
                    || is_subsequence(hay_fold, &nq_fold);
                if subseq {
                    score = score.max(0.85);
                }
            }

            let alias = e
                .meta
                .as_ref()
                .and_then(|m| m.get("alias").map(|s| s.as_str()))
                .unwrap_or("");
            let nalias = daizo_core::text_utils::normalized_with_spaces(alias).replace(' ', "");
            let nalias_fold = daizo_core::text_utils::normalized_sanskrit(alias);
            if !nalias.is_empty() {
                if nalias.split_whitespace().any(|a| a == nq_nospace)
                    || nalias.contains(&nq_nospace)
                    || (!nalias_fold.is_empty() && nalias_fold.contains(&nq_fold))
                {
                    score = score.max(0.95);
                }
            }

            if has_digit && !nq_ws.is_empty() && hay_ws.contains(&nq_ws) {
                score = (score + 0.05).min(1.0);
            }
            score
        } else {
            compute_match_score_sanskrit(e, q)
        };

        if let Some(meta) = &e.meta {
            for k in ["author", "editor", "translator", "publisher"].iter() {
                if let Some(v) = meta.get(*k) {
                    let nv = normalized(v);
                    if !nv.is_empty() && (nv.contains(&nq) || nq.contains(&nv)) {
                        s = s.max(0.93);
                    }
                }
            }
        }
        topk_insert(&mut top, (s, e), limit);
    }
    top.into_iter()
        .map(|(s, e)| ScoredHit { entry: e, score: s })
        .collect()
}

#[derive(Clone, Copy)]
struct ResolveCrosswalk {
    key: &'static str,
    aliases: &'static [&'static str],
    cbeta_id: Option<&'static str>,
    cbeta_title: Option<&'static str>,
    tipitaka_id: Option<&'static str>,
    tipitaka_title: Option<&'static str>,
    gretil_id: Option<&'static str>,
    gretil_title: Option<&'static str>,
}

// Curated cross-corpus aliases for daizo_resolve. Keep this small and high precision.
static RESOLVE_CROSSWALK: &[ResolveCrosswalk] = &[
    ResolveCrosswalk {
        key: "heart-sutra",
        aliases: &[
            "般若心経",
            "般若心經",
            "般若波羅蜜多心経",
            "般若波羅蜜多心經",
            "T0251",
            "T08n0251",
            "prajJApAramitAhRdayasUtra",
            "sa_prajJApAramitAhRdayasUtra",
            "Heart Sutra",
        ],
        cbeta_id: Some("T0251"),
        cbeta_title: Some("般若波羅蜜多心經"),
        tipitaka_id: None,
        tipitaka_title: None,
        gretil_id: Some("prajJApAramitAhRdayasUtra"),
        gretil_title: Some("Prajñāpāramitāhṛdayasūtra"),
    },
    ResolveCrosswalk {
        key: "diamond-sutra",
        aliases: &[
            "金剛経",
            "金剛經",
            "金剛般若経",
            "金剛般若經",
            "金剛般若波羅蜜経",
            "金剛般若波羅蜜經",
            "T0235",
            "T08n0235",
            "vajracchedikA",
            "sa_vajracchedikA-prajJApAramitA",
            "Diamond Sutra",
        ],
        cbeta_id: Some("T0235"),
        cbeta_title: Some("金剛般若波羅蜜經"),
        tipitaka_id: None,
        tipitaka_title: None,
        gretil_id: Some("vajracchedikA"),
        gretil_title: Some("Vajracchedikā"),
    },
    ResolveCrosswalk {
        key: "lotus-sutra",
        aliases: &[
            "法華経",
            "法華經",
            "妙法蓮華経",
            "妙法蓮華經",
            "T0262",
            "T09n0262",
            "saddharmapuNDarIka",
            "sa_saddharmapuNDarIka",
            "Lotus Sutra",
        ],
        cbeta_id: Some("T0262"),
        cbeta_title: Some("妙法蓮華經"),
        tipitaka_id: None,
        tipitaka_title: None,
        gretil_id: Some("saddharmapuNDarIka"),
        gretil_title: Some("Saddharmapuṇḍarīka"),
    },
    ResolveCrosswalk {
        key: "lankavatara-sutra",
        aliases: &[
            "楞伽経",
            "楞伽經",
            "楞伽阿跋多羅宝経",
            "楞伽阿跋多羅寶經",
            "T0670",
            "T16n0670",
            "laGkAvatArasUtra",
            "saddharmalaGkAvatArasUtra",
        ],
        cbeta_id: Some("T0670"),
        cbeta_title: Some("楞伽阿跋多羅寶經"),
        tipitaka_id: None,
        tipitaka_title: None,
        gretil_id: Some("laGkAvatArasUtra"),
        gretil_title: Some("Laṅkāvatārasūtra"),
    },
];

fn resolve_crosswalk_candidates(
    q: &str,
    sources: &[String],
    prefer_source: Option<&str>,
) -> Vec<(f32, serde_json::Value)> {
    let nq = normalized(q);
    if nq.is_empty() {
        return Vec::new();
    }
    let mut out: Vec<(f32, serde_json::Value)> = Vec::new();
    for cw in RESOLVE_CROSSWALK.iter() {
        let mut hit = false;
        for a in cw.aliases.iter() {
            let na = normalized(a);
            if na.is_empty() {
                continue;
            }
            // High-precision: exact match OR containment with a minimum length.
            if na == nq || (nq.len() >= 6 && (na.contains(&nq) || nq.contains(&na))) {
                hit = true;
                break;
            }
        }
        if !hit {
            continue;
        }

        if sources.iter().any(|s| s == "cbeta") {
            if let Some(id) = cw.cbeta_id {
                let bias = if prefer_source == Some("cbeta") {
                    0.02
                } else {
                    0.0
                };
                let boost = if normalized(id) == nq { 0.05 } else { 0.0 };
                out.push((
                    1.05 + bias + boost,
                    json!({
                        "source": "cbeta",
                        "id": id,
                        "title": cw.cbeta_title,
                        "score": 1.0,
                        "fetch": {"tool": "cbeta_fetch", "args": {"id": id}},
                        "resolvedBy": "crosswalk",
                        "key": cw.key
                    }),
                ));
            }
        }
        if sources.iter().any(|s| s == "tipitaka") {
            if let Some(id) = cw.tipitaka_id {
                let bias = if prefer_source == Some("tipitaka") {
                    0.02
                } else {
                    0.0
                };
                let boost = if normalized(id) == nq { 0.05 } else { 0.0 };
                out.push((
                    1.05 + bias + boost,
                    json!({
                        "source": "tipitaka",
                        "id": id,
                        "title": cw.tipitaka_title,
                        "score": 1.0,
                        "fetch": {"tool": "tipitaka_fetch", "args": {"id": id}},
                        "resolvedBy": "crosswalk",
                        "key": cw.key
                    }),
                ));
            }
        }
        if sources.iter().any(|s| s == "gretil") {
            if let Some(id) = cw.gretil_id {
                let bias = if prefer_source == Some("gretil") {
                    0.02
                } else {
                    0.0
                };
                let boost = if normalized(id) == nq { 0.05 } else { 0.0 };
                out.push((
                    1.05 + bias + boost,
                    json!({
                        "source": "gretil",
                        "id": id,
                        "title": cw.gretil_title,
                        "score": 1.0,
                        "fetch": {"tool": "gretil_fetch", "args": {"id": id}},
                        "resolvedBy": "crosswalk",
                        "key": cw.key
                    }),
                ));
            }
        }
    }
    out
}

fn resolve_title_candidates_cbeta(
    q: &str,
    limit_per_source: usize,
    min_score: f32,
    prefer_source: Option<&str>,
) -> Vec<(f32, serde_json::Value)> {
    let idx = load_or_build_cbeta_index();
    let mut out: Vec<(f32, serde_json::Value)> = Vec::new();
    for h in best_match(idx, q, limit_per_source) {
        if h.score < min_score {
            continue;
        }
        let bias = if prefer_source == Some("cbeta") {
            0.02
        } else {
            0.0
        };
        let v = json!({
            "source": "cbeta",
            "id": &h.entry.id,
            "title": &h.entry.title,
            "score": h.score,
            "path": &h.entry.path,
            "fetch": {"tool": "cbeta_fetch", "args": {"id": &h.entry.id}},
            "resolvedBy": "title-index"
        });
        out.push((h.score + bias, v));
    }
    out
}

fn resolve_title_candidates_tipitaka(
    q: &str,
    limit_per_source: usize,
    min_score: f32,
    prefer_source: Option<&str>,
) -> Vec<(f32, serde_json::Value)> {
    let idx = load_or_build_tipitaka_index();
    let mut out: Vec<(f32, serde_json::Value)> = Vec::new();
    for h in best_match_tipitaka(idx, q, limit_per_source) {
        if h.score < min_score {
            continue;
        }
        let bias = if prefer_source == Some("tipitaka") {
            0.02
        } else {
            0.0
        };
        let stem = Path::new(&h.entry.path)
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or(&h.entry.id)
            .to_string();
        let v = json!({
            "source": "tipitaka",
            "id": &stem,
            "title": &h.entry.title,
            "score": h.score,
            "path": &h.entry.path,
            "meta": &h.entry.meta,
            "fetch": {"tool": "tipitaka_fetch", "args": {"id": &stem}},
            "resolvedBy": "title-index"
        });
        out.push((h.score + bias, v));
    }
    out
}

fn resolve_title_candidates_gretil(
    q: &str,
    limit_per_source: usize,
    min_score: f32,
    prefer_source: Option<&str>,
) -> Vec<(f32, serde_json::Value)> {
    let idx = load_or_build_gretil_index();
    let mut out: Vec<(f32, serde_json::Value)> = Vec::new();
    for h in best_match_gretil(idx, q, limit_per_source) {
        if h.score < min_score {
            continue;
        }
        let bias = if prefer_source == Some("gretil") {
            0.02
        } else {
            0.0
        };
        let v = json!({
            "source": "gretil",
            "id": &h.entry.id,
            "title": &h.entry.title,
            "score": h.score,
            "path": &h.entry.path,
            "meta": &h.entry.meta,
            "fetch": {"tool": "gretil_fetch", "args": {"id": &h.entry.id}},
            "resolvedBy": "title-index"
        });
        out.push((h.score + bias, v));
    }
    out
}

fn resolve_title_candidates_sarit(
    q: &str,
    limit_per_source: usize,
    min_score: f32,
    prefer_source: Option<&str>,
) -> Vec<(f32, serde_json::Value)> {
    let idx = load_or_build_sarit_index();
    let mut out: Vec<(f32, serde_json::Value)> = Vec::new();
    for h in best_match_sarit(idx, q, limit_per_source) {
        if h.score < min_score {
            continue;
        }
        let bias = if prefer_source == Some("sarit") {
            0.02
        } else {
            0.0
        };
        let v = json!({
            "source": "sarit",
            "id": &h.entry.id,
            "title": &h.entry.title,
            "score": h.score,
            "path": &h.entry.path,
            "meta": &h.entry.meta,
            "fetch": {"tool": "sarit_fetch", "args": {"id": &h.entry.id}},
            "resolvedBy": "title-index"
        });
        out.push((h.score + bias, v));
    }
    out
}

fn resolve_title_candidates_muktabodha(
    q: &str,
    limit_per_source: usize,
    min_score: f32,
    prefer_source: Option<&str>,
) -> Vec<(f32, serde_json::Value)> {
    let idx = load_or_build_muktabodha_index();
    let mut out: Vec<(f32, serde_json::Value)> = Vec::new();
    for h in best_match_muktabodha(idx, q, limit_per_source) {
        if h.score < min_score {
            continue;
        }
        let bias = if prefer_source == Some("muktabodha") {
            0.02
        } else {
            0.0
        };
        let v = json!({
            "source": "muktabodha",
            "id": &h.entry.id,
            "title": &h.entry.title,
            "score": h.score,
            "path": &h.entry.path,
            "meta": &h.entry.meta,
            "fetch": {"tool": "muktabodha_fetch", "args": {"id": &h.entry.id}},
            "resolvedBy": "title-index"
        });
        out.push((h.score + bias, v));
    }
    out
}

// jaccard and is_subsequence moved to daizo_core::text_utils

// resolve_cbeta_path moved to daizo_core::path_resolver

// removed: unused helper

fn slice_text(text: &str, args: &serde_json::Value) -> String {
    // Slice by character positions (safe for UTF-8).
    let default_max = 8000usize;
    let start_char = args
        .get("page")
        .and_then(|v| v.as_u64())
        .and_then(|p| {
            args.get("pageSize")
                .and_then(|s| s.as_u64())
                .map(|ps| (p as usize) * (ps as usize))
        })
        .or_else(|| {
            args.get("startChar")
                .and_then(|v| v.as_u64().map(|x| x as usize))
        })
        .unwrap_or(0);
    let total_chars = text.chars().count();
    let start_char = std::cmp::min(start_char, total_chars);
    let end_char = if let (Some(p), Some(ps)) = (
        args.get("page").and_then(|v| v.as_u64()),
        args.get("pageSize").and_then(|v| v.as_u64()),
    ) {
        Some((p as usize) * (ps as usize) + (ps as usize))
    } else if let Some(ec) = args.get("endChar").and_then(|v| v.as_u64()) {
        Some(ec as usize)
    } else if let Some(mc) = args.get("maxChars").and_then(|v| v.as_u64()) {
        Some(start_char + mc as usize)
    } else {
        None
    };
    let end_char = end_char
        .map(|e| std::cmp::min(e, total_chars))
        .unwrap_or_else(|| std::cmp::min(start_char + default_max, total_chars));
    if start_char >= end_char {
        return String::new();
    }
    // Convert char indices to byte indices
    let s_byte = text
        .char_indices()
        .nth(start_char)
        .map(|(b, _)| b)
        .unwrap_or(text.len());
    let e_byte = text
        .char_indices()
        .nth(end_char)
        .map(|(b, _)| b)
        .unwrap_or(text.len());
    if s_byte > e_byte {
        return String::new();
    }
    text[s_byte..e_byte].to_string()
}

fn slice_text_bounds(
    text: &str,
    start_char: usize,
    max_chars: usize,
) -> (String, usize, usize, usize) {
    let total_chars = text.chars().count();
    let effective_start = std::cmp::min(start_char, total_chars);
    let target_end = effective_start.saturating_add(max_chars);
    let effective_end = std::cmp::min(target_end, total_chars);

    let start_byte = if effective_start == total_chars {
        text.len()
    } else {
        text.char_indices()
            .nth(effective_start)
            .map(|(idx, _)| idx)
            .unwrap_or(text.len())
    };
    let end_byte = if effective_end == total_chars {
        text.len()
    } else {
        text.char_indices()
            .nth(effective_end)
            .map(|(idx, _)| idx)
            .unwrap_or(text.len())
    };

    let slice = if start_byte <= end_byte {
        text[start_byte..end_byte].to_string()
    } else {
        String::new()
    };

    (slice, total_chars, effective_start, effective_end)
}

fn handle_call(id: serde_json::Value, params: &serde_json::Value) -> serde_json::Value {
    let name = params.get("name").and_then(|v| v.as_str()).unwrap_or("");
    let args = params.get("arguments").cloned().unwrap_or(json!({}));
    let content_text = match name {
        "daizo_version" => {
            let data_status = json!({
                "cbeta": cbeta_root().exists(),
                "tipitaka": tipitaka_root().exists(),
                "gretil": gretil_root().exists(),
                "sarit": sarit_root().exists(),
                "muktabodha": muktabodha_root().exists(),
            });
            let version_info = json!({
                "version": VERSION,
                "name": "daizo-mcp",
                "description": "MCP server for Buddhist scripture retrieval (CBETA, Tipitaka, GRETIL, SARIT, MUKTABODHA, SAT, JOZEN)",
                "homepage": "https://github.com/sinryo/daizo-mcp",
                "data_available": data_status,
                "data_path": daizo_home().to_string_lossy(),
                "optimizations": [
                    "ripgrep-based regex search",
                    "ignore-based parallel file walking",
                    "memory-mapped I/O",
                    "LTO enabled"
                ],
                "features": [
                    "CBETA Chinese Buddhist Canon (大正藏)",
                    "Tipitaka Pāli Canon (パーリ聖典)",
                    "GRETIL Sanskrit Texts (梵語文献)",
                    "SARIT TEI P5 corpus",
                    "MUKTABODHA Sanskrit library (IAST)",
                    "SAT Database search",
                    "Jodo Shu Zensho (浄土宗全書) search/fetch (online)"
                ]
            });
            return json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": {
                    "content": [{"type": "text", "text": serde_json::to_string_pretty(&version_info).unwrap_or_default()}],
                    "_meta": version_info
                }
            });
        }
        "daizo_usage" => {
            let guide = "Daizo usage guide:\n\n## FASTEST: Direct ID access (no search needed!)\n\n### CBETA (Chinese Buddhist Canon)\nIf you know the Taisho number, use cbeta_fetch with id directly:\n- T0001 = 長阿含經 (Dirghagama)\n- T0099 = 雜阿含經 (Samyuktagama)\n- T0125 = 增壹阿含經 (Ekottaragama)\n- T0262 = 妙法蓮華經 (Lotus Sutra)\n- T0235 = 金剛般若波羅蜜經 (Diamond Sutra)\n- T0251 = 般若波羅蜜多心經 (Heart Sutra)\n- T0945 = 大佛頂首楞嚴經 (Shurangama Sutra)\n- T2076 = 景德傳燈錄\n\nExample: cbeta_fetch({id: \"T0262\"}) - instant!\n\n### Tipitaka (Pāli Canon)\nUse Nikāya codes directly with tipitaka_fetch:\n- DN = Dīghanikāya (長部) - e.g., DN1, DN2\n- MN = Majjhimanikāya (中部) - e.g., MN1, MN2\n- SN = Saṃyuttanikāya (相応部) - e.g., SN1\n- AN = Aṅguttaranikāya (増支部) - e.g., AN1\n- KN = Khuddakanikāya (小部)\n\nExamples:\n- tipitaka_fetch({id: \"DN1\"}) - Brahmajāla Sutta (梵網経)\n- tipitaka_fetch({id: \"MN1\"}) - Mūlapariyāya Sutta\n- tipitaka_fetch({id: \"s0101m.mul\"}) - Direct file access\n\n### GRETIL (Sanskrit Texts)\nUse file stem directly with gretil_fetch:\n- saddharmapuNDarIka = 法華經 (Lotus Sutra)\n- vajracchedikA = 金剛般若經 (Diamond Sutra)\n- prajJApAramitAhRdayasUtra = 般若心經 (Heart Sutra)\n- azvaghoSa-buddhacarita = 佛所行讚 (Buddhacarita)\n- laGkAvatArasUtra = 楞伽經 (Lankavatara Sutra)\n- mahAparinirvANasUtra = 大般涅槃經 (Mahaparinirvana Sutra)\n\nExamples:\n- gretil_fetch({id: \"saddharmapuNDarIka\"}) - Lotus Sutra Sanskrit\n- gretil_fetch({id: \"vajracchedikA\"}) - Diamond Sutra Sanskrit\n- gretil_fetch({id: \"sa_azvaghoSa-buddhacarita\"}) - with sa_ prefix also works\n\n## If corpus/ID is unknown\n- Use daizo_resolve({query: \"法華経\"}) first, then follow _meta.pick.fetch.\n\n## Standard flow (when ID unknown)\n1. Use *_search -> read _meta.fetchSuggestions\n2. Call *_fetch with {id, lineNumber, contextBefore:3, contextAfter:10, highlight: \"検索語\", format:\"plain\"}\n3. IMPORTANT: Always include 'highlight' parameter with the search term!\n   - If lineNumber doesn't show expected text, the highlight ensures correct context\n   - Example: cbeta_fetch({id:\"T0279\", lineNumber:1650, highlight:\"金剛杵\", contextBefore:5, contextAfter:10, format:\"plain\"})\n4. Use *_pipeline only for multi-file summary; set autoFetch=false by default\n\n## Troubleshooting: If lineNumber doesn't work\n- ALWAYS pass 'highlight' param with search term for verification\n- Use larger contextBefore/contextAfter (e.g., 10-20 lines)\n- For very long texts, use part/juan parameter instead: cbeta_fetch({id:\"T0279\", part:\"001\"})";
            return json!({
                "jsonrpc":"2.0",
                "id": id,
                "result": { "content": [{"type":"text","text": guide}], "_meta": {"source": "daizo_usage"} }
            });
        }
        "daizo_profile" => {
            let tool = args
                .get("tool")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .trim()
                .to_string();
            let call_args = args.get("arguments").cloned().unwrap_or(json!({}));
            let iterations = args
                .get("iterations")
                .and_then(|v| v.as_u64())
                .unwrap_or(10)
                .clamp(1, 10_000) as usize;
            let warmup = args
                .get("warmup")
                .and_then(|v| v.as_u64())
                .unwrap_or(1)
                .clamp(0, 10_000) as usize;
            let include_samples = args
                .get("includeSamples")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);

            if tool.is_empty() || tool == "daizo_profile" {
                return json!({"jsonrpc":"2.0","id": id, "result": { "content": [{"type":"text","text": "invalid tool"}], "_meta": {"tool": tool, "ok": false} }});
            }

            let params = json!({"name": tool, "arguments": call_args});

            // Warmup (ignore timings)
            for _ in 0..warmup {
                let _ = handle_call(json!(0), &params);
            }

            let mut samples_ms: Vec<f64> = Vec::with_capacity(iterations);
            for i in 0..iterations {
                let t0 = Instant::now();
                let _ = handle_call(json!(i as u64), &params);
                let dt = t0.elapsed().as_secs_f64() * 1000.0;
                samples_ms.push(dt);
            }

            let mut sorted = samples_ms.clone();
            sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
            let mean_ms = if samples_ms.is_empty() {
                0.0
            } else {
                samples_ms.iter().sum::<f64>() / (samples_ms.len() as f64)
            };
            let pct = |p: f64| -> f64 {
                if sorted.is_empty() {
                    return 0.0;
                }
                let n = sorted.len();
                let idx = ((p * (n as f64 - 1.0)).round() as usize).min(n - 1);
                sorted[idx]
            };
            let min_ms = *sorted.first().unwrap_or(&0.0);
            let max_ms = *sorted.last().unwrap_or(&0.0);
            let p50 = pct(0.50);
            let p90 = pct(0.90);
            let p95 = pct(0.95);
            let p99 = pct(0.99);

            let summary = format!(
                "daizo_profile tool={}\niterations={} warmup={}\nmin={:.3}ms p50={:.3}ms mean={:.3}ms p90={:.3}ms p95={:.3}ms p99={:.3}ms max={:.3}ms\n",
                tool, iterations, warmup, min_ms, p50, mean_ms, p90, p95, p99, max_ms
            );

            let meta = json!({
                "tool": tool,
                "iterations": iterations,
                "warmup": warmup,
                "minMs": min_ms,
                "p50Ms": p50,
                "meanMs": mean_ms,
                "p90Ms": p90,
                "p95Ms": p95,
                "p99Ms": p99,
                "maxMs": max_ms,
                "samplesMs": if include_samples { Some(samples_ms) } else { None::<Vec<f64>> }
            });

            return json!({"jsonrpc":"2.0","id": id, "result": { "content": [{"type":"text","text": summary}], "_meta": meta }});
        }
        "daizo_resolve" => {
            let q = args
                .get("query")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .trim();
            if q.is_empty() {
                return json!({"jsonrpc":"2.0","id": id, "result": { "content": [{"type":"text","text": "query is empty"}], "_meta": {"query": q, "count": 0, "candidates": []} }});
            }

            let sources: Vec<String> = args
                .get("sources")
                .and_then(|v| v.as_array())
                .map(|a| {
                    a.iter()
                        .filter_map(|x| x.as_str().map(|s| s.to_string()))
                        .collect::<Vec<_>>()
                })
                .unwrap_or_else(|| {
                    vec![
                        "cbeta".to_string(),
                        "tipitaka".to_string(),
                        "gretil".to_string(),
                        "sarit".to_string(),
                        "muktabodha".to_string(),
                    ]
                });
            let limit_per_source = args
                .get("limitPerSource")
                .and_then(|v| v.as_u64())
                .unwrap_or(5) as usize;
            let limit_total = args.get("limit").and_then(|v| v.as_u64()).unwrap_or(10) as usize;
            let prefer_source = args
                .get("preferSource")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
            let min_score = args.get("minScore").and_then(|v| v.as_f64()).unwrap_or(0.1) as f32;

            let mut cands_scored: Vec<(f32, serde_json::Value)> = Vec::new();

            // Direct ID detection (fast path)
            let mut direct_id_mode = false;
            let q_nospace = q.split_whitespace().collect::<String>();
            let q_upper = q_nospace.to_ascii_uppercase();
            // CBETA: T + 1-4 digits
            if sources.iter().any(|s| s == "cbeta") && q_upper.starts_with('T') {
                let digits: String = q_upper
                    .chars()
                    .skip(1)
                    .take_while(|c| c.is_ascii_digit())
                    .collect();
                if !digits.is_empty() {
                    if let Ok(n) = digits.parse::<u32>() {
                        let id_norm = format!("T{:04}", n);
                        let bias = if prefer_source.as_deref() == Some("cbeta") {
                            0.02
                        } else {
                            0.0
                        };
                        let cand = json!({
                                "source": "cbeta",
                                "id": id_norm,
                                "title": null,
                                "score": 1.0,
                                "fetch": {"tool": "cbeta_fetch", "args": {"id": format!("T{:04}", n)}},
                                "resolvedBy": "direct-id"
                        });
                        cands_scored.push((1.20 + bias, cand));
                        direct_id_mode = true;
                    }
                }
            }
            // Tipitaka: DN/MN/SN/AN/KN + digits
            if sources.iter().any(|s| s == "tipitaka") {
                for pref in ["DN", "MN", "SN", "AN", "KN"] {
                    if q_upper.starts_with(pref) {
                        let rest = q_upper[pref.len()..].to_string();
                        let digits: String = rest.chars().filter(|c| c.is_ascii_digit()).collect();
                        if !digits.is_empty() {
                            if let Ok(n) = digits.parse::<u32>() {
                                let id_norm = format!("{}{}", pref, n);
                                let bias = if prefer_source.as_deref() == Some("tipitaka") {
                                    0.02
                                } else {
                                    0.0
                                };
                                let cand = json!({
                                    "source": "tipitaka",
                                    "id": id_norm,
                                    "title": null,
                                    "score": 1.0,
                                    "fetch": {"tool": "tipitaka_fetch", "args": {"id": format!("{}{}", pref, n)}},
                                    "resolvedBy": "direct-id"
                                });
                                cands_scored.push((1.20 + bias, cand));
                                direct_id_mode = true;
                            }
                        }
                    }
                }
            }
            // Tipitaka file stem hint
            if sources.iter().any(|s| s == "tipitaka")
                && q_nospace.contains(".mul")
                && !q_nospace.contains(' ')
            {
                let stem = q_nospace.clone();
                let bias = if prefer_source.as_deref() == Some("tipitaka") {
                    0.02
                } else {
                    0.0
                };
                let cand = json!({
                    "source": "tipitaka",
                    "id": stem,
                    "title": null,
                    "score": 1.0,
                    "fetch": {"tool": "tipitaka_fetch", "args": {"id": q_nospace}},
                    "resolvedBy": "direct-id"
                });
                cands_scored.push((1.20 + bias, cand));
                direct_id_mode = true;
            }

            // Cheap cross-corpus aliases (no index load).
            cands_scored.extend(resolve_crosswalk_candidates(
                q,
                &sources,
                prefer_source.as_deref(),
            ));

            // Title index resolution (CPU only). Parallelize across corpora to reduce tail latency.
            if !direct_id_mode {
                let prefer = prefer_source.as_deref();
                let do_cbeta = sources.iter().any(|s| s == "cbeta");
                let do_tipitaka = sources.iter().any(|s| s == "tipitaka");
                let do_gretil = sources.iter().any(|s| s == "gretil");
                let do_sarit = sources.iter().any(|s| s == "sarit");
                let do_muktabodha = sources.iter().any(|s| s == "muktabodha");
                let jobs = (do_cbeta as usize)
                    + (do_tipitaka as usize)
                    + (do_gretil as usize)
                    + (do_sarit as usize)
                    + (do_muktabodha as usize);
                if jobs >= 2 {
                    std::thread::scope(|scope| {
                        let h_cbeta = if do_cbeta {
                            Some(scope.spawn(|| {
                                resolve_title_candidates_cbeta(
                                    q,
                                    limit_per_source,
                                    min_score,
                                    prefer,
                                )
                            }))
                        } else {
                            None
                        };
                        let h_tipitaka = if do_tipitaka {
                            Some(scope.spawn(|| {
                                resolve_title_candidates_tipitaka(
                                    q,
                                    limit_per_source,
                                    min_score,
                                    prefer,
                                )
                            }))
                        } else {
                            None
                        };
                        let h_gretil = if do_gretil {
                            Some(scope.spawn(|| {
                                resolve_title_candidates_gretil(
                                    q,
                                    limit_per_source,
                                    min_score,
                                    prefer,
                                )
                            }))
                        } else {
                            None
                        };
                        let h_sarit = if do_sarit {
                            Some(scope.spawn(|| {
                                resolve_title_candidates_sarit(
                                    q,
                                    limit_per_source,
                                    min_score,
                                    prefer,
                                )
                            }))
                        } else {
                            None
                        };
                        let h_muktabodha = if do_muktabodha {
                            Some(scope.spawn(|| {
                                resolve_title_candidates_muktabodha(
                                    q,
                                    limit_per_source,
                                    min_score,
                                    prefer,
                                )
                            }))
                        } else {
                            None
                        };
                        if let Some(h) = h_cbeta {
                            cands_scored.extend(h.join().unwrap_or_default());
                        }
                        if let Some(h) = h_tipitaka {
                            cands_scored.extend(h.join().unwrap_or_default());
                        }
                        if let Some(h) = h_gretil {
                            cands_scored.extend(h.join().unwrap_or_default());
                        }
                        if let Some(h) = h_sarit {
                            cands_scored.extend(h.join().unwrap_or_default());
                        }
                        if let Some(h) = h_muktabodha {
                            cands_scored.extend(h.join().unwrap_or_default());
                        }
                    });
                } else {
                    if do_cbeta {
                        cands_scored.extend(resolve_title_candidates_cbeta(
                            q,
                            limit_per_source,
                            min_score,
                            prefer,
                        ));
                    }
                    if do_tipitaka {
                        cands_scored.extend(resolve_title_candidates_tipitaka(
                            q,
                            limit_per_source,
                            min_score,
                            prefer,
                        ));
                    }
                    if do_gretil {
                        cands_scored.extend(resolve_title_candidates_gretil(
                            q,
                            limit_per_source,
                            min_score,
                            prefer,
                        ));
                    }
                    if do_sarit {
                        cands_scored.extend(resolve_title_candidates_sarit(
                            q,
                            limit_per_source,
                            min_score,
                            prefer,
                        ));
                    }
                    if do_muktabodha {
                        cands_scored.extend(resolve_title_candidates_muktabodha(
                            q,
                            limit_per_source,
                            min_score,
                            prefer,
                        ));
                    }
                }
            }

            cands_scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
            let mut candidates: Vec<serde_json::Value> = Vec::new();
            let mut seen: std::collections::HashSet<(String, String)> =
                std::collections::HashSet::new();
            for (_s, v) in cands_scored.into_iter() {
                let src = v
                    .get("source")
                    .and_then(|x| x.as_str())
                    .unwrap_or("")
                    .to_string();
                let cid = v
                    .get("id")
                    .and_then(|x| x.as_str())
                    .unwrap_or("")
                    .to_string();
                if src.is_empty() || cid.is_empty() {
                    continue;
                }
                if seen.insert((src, cid)) {
                    candidates.push(v);
                }
                if candidates.len() >= limit_total {
                    break;
                }
            }
            let pick = candidates.first().cloned();

            let mut summary = format!("Candidates for '{}':\n", q);
            for (i, c) in candidates.iter().enumerate() {
                let src = c.get("source").and_then(|v| v.as_str()).unwrap_or("");
                let cid = c.get("id").and_then(|v| v.as_str()).unwrap_or("");
                let title = c.get("title").and_then(|v| v.as_str()).unwrap_or("");
                let sc = c.get("score").and_then(|v| v.as_f64()).unwrap_or(0.0);
                if title.is_empty() {
                    summary.push_str(&format!("{}. [{}] {} (score {:.3})\n", i + 1, src, cid, sc));
                } else {
                    summary.push_str(&format!(
                        "{}. [{}] {}  {} (score {:.3})\n",
                        i + 1,
                        src,
                        cid,
                        title,
                        sc
                    ));
                }
            }
            if candidates.is_empty() {
                summary.push_str("0 candidates\n");
            }

            let meta = json!({
                "query": q,
                "sources": sources,
                "count": candidates.len(),
                "candidates": candidates,
                "pick": pick
            });
            return json!({"jsonrpc":"2.0","id": id, "result": { "content": [{"type":"text","text": summary}], "_meta": meta }});
        }
        "cbeta_title_search" => {
            let q = args
                .get("query")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .trim()
                .to_string();
            let limit = args.get("limit").and_then(|v| v.as_u64()).unwrap_or(10) as usize;
            let idx = load_or_build_cbeta_index();
            let hits = best_match(idx, &q, limit);
            let summary = hits
                .iter()
                .enumerate()
                .map(|(i, h)| {
                    let tr = h
                        .entry
                        .meta
                        .as_ref()
                        .and_then(|m| m.get("translator"))
                        .map(|s| s.as_str())
                        .unwrap_or("");
                    if tr.is_empty() {
                        format!("{}. {}  {}", i + 1, h.entry.id, h.entry.title)
                    } else {
                        format!("{}. {}  {}  [{}]", i + 1, h.entry.id, h.entry.title, tr)
                    }
                })
                .collect::<Vec<_>>()
                .join("\n");
            let results: Vec<_> = hits
                .iter()
                .map(|h| {
                    let meta = h.entry.meta.as_ref();
                    let author = meta.and_then(|m| m.get("author").cloned());
                    let translator = meta.and_then(|m| m.get("translator").cloned());
                    json!({
                        "id": h.entry.id,
                        "title": h.entry.title,
                        "path": h.entry.path,
                        "score": h.score,
                        "author": author,
                        "translator": translator,
                        "meta": {
                            "author": meta.and_then(|m| m.get("author").cloned()),
                            "editor": meta.and_then(|m| m.get("editor").cloned()),
                            "translator": meta.and_then(|m| m.get("translator").cloned()),
                            "publisher": meta.and_then(|m| m.get("publisher").cloned()),
                            "date": meta.and_then(|m| m.get("date").cloned()),
                            "idno": meta.and_then(|m| m.get("idno").cloned()),
                            "canon": meta.and_then(|m| m.get("canon").cloned()),
                            "nnum": meta.and_then(|m| m.get("nnum").cloned()),
                            "juanCount": meta.and_then(|m| m.get("juanCount").cloned()),
                            "headsPreview": meta.and_then(|m| m.get("headsPreview").cloned()),
                            "respAll": meta.and_then(|m| m.get("respAll").cloned()),
                        }
                    })
                })
                .collect();
            let meta = json!({
                "count": results.len(),
                "results": results
            });
            return json!({"jsonrpc":"2.0","id": id, "result": { "content": [{"type":"text","text": summary }], "_meta": meta }});
        }
        "cbeta_fetch" => {
            ensure_cbeta_data();
            let mut matched_id: Option<String> = None;
            let mut matched_title: Option<String> = None;
            let mut matched_score: Option<f32> = None;
            let mut path: PathBuf = PathBuf::new();
            // 高速化: IDが指定されている場合は直接パス解決を試み、インデックスのロードを回避
            if let Some(id) = args.get("id").and_then(|v| v.as_str()) {
                // まず直接パス解決を試みる（インデックス不要、高速）
                if let Some(direct_path) = daizo_core::path_resolver::resolve_cbeta_path_direct(id)
                {
                    matched_id = Some(id.to_string());
                    path = direct_path;
                    // タイトルは後でインデックスから取得（オプショナル、遅延ロード）
                } else {
                    // フォールバック: インデックスから検索
                    let idx = load_or_build_cbeta_index();
                    if let Some(hit) = idx.iter().find(|e| e.id == id) {
                        matched_id = Some(hit.id.clone());
                        matched_title = Some(hit.title.clone());
                        path = PathBuf::from(&hit.path);
                    } else {
                        // 最終フォールバック: WalkDir検索
                        path = resolve_cbeta_path_by_id(id).unwrap_or_else(|| PathBuf::from(""));
                        matched_id = Some(id.to_string());
                    }
                }
            } else if let Some(q) = args.get("query").and_then(|v| v.as_str()) {
                let idx = load_or_build_cbeta_index();
                if let Some(hit) = best_match(idx, q, 1).into_iter().next() {
                    matched_id = Some(hit.entry.id.clone());
                    matched_title = Some(hit.entry.title.clone());
                    matched_score = Some(hit.score);
                    path = PathBuf::from(&hit.entry.path);
                }
            }
            let xml_arc = cbeta_xml_cached(&path);
            let xml = xml_arc.as_str();
            // includeNotes support
            let include_notes = args
                .get("includeNotes")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            let is_plain = args
                .get("format")
                .and_then(|v| v.as_str())
                .map(|s| s.eq_ignore_ascii_case("plain"))
                .unwrap_or(false);

            // Effective highlight pattern (also used for focusHighlight).
            let hl_in = args
                .get("highlight")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
            let mut hl_use_re = args
                .get("highlightRegex")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            let mut hl_pat: Option<String> = None;
            if let Some(h0) = &hl_in {
                let looks_like = h0.chars().any(|c| ".+*?[](){}|\\".contains(c));
                if looks_like && !hl_use_re {
                    hl_use_re = true;
                    hl_pat = Some(h0.clone());
                } else if h0.chars().any(|c| c.is_whitespace()) && !looks_like && !hl_use_re {
                    hl_use_re = true;
                    hl_pat = Some(ws_cjk_variant_fuzzy_regex_literal(h0));
                } else if !hl_use_re && !looks_like {
                    // Expand variants and escape as regex for CBETA by default.
                    hl_use_re = true;
                    hl_pat = Some(ws_cjk_variant_fuzzy_regex_literal(h0));
                } else {
                    hl_pat = Some(h0.clone());
                }
            }

            let mut gaiji: Option<Arc<std::collections::HashMap<String, String>>> = None;
            let mut ensure_gaiji = || {
                if gaiji.is_none() {
                    gaiji = Some(cbeta_gaiji_cached(&path, xml));
                }
            };

            // lineNumber/lb/part/head指定時の処理
            let (mut text, mut extraction_method, part_matched) = if let Some(lb) = args
                .get("lb")
                .and_then(|v| v.as_str())
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
            {
                let context_before = args
                    .get("contextBefore")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(
                        args.get("contextLines")
                            .and_then(|v| v.as_u64())
                            .unwrap_or(10),
                    ) as usize;
                let context_after = args.get("contextAfter").and_then(|v| v.as_u64()).unwrap_or(
                    args.get("contextLines")
                        .and_then(|v| v.as_u64())
                        .unwrap_or(100),
                ) as usize;
                let pat = format!(r#"<lb\b[^>]*\bn\s*=\s*["']{}["']"#, regex::escape(&lb));
                if let Ok(re) = Regex::new(&pat) {
                    if let Some(m) = re.find(&xml) {
                        let xml_line = xml[..m.start()].lines().count() + 1;
                        if is_plain {
                            ensure_gaiji();
                            let raw = extract_text_around_line_asymmetric(
                                &xml,
                                xml_line,
                                context_before,
                                context_after,
                            );
                            let context_text = extract_cbeta_plain_from_snippet(
                                &raw,
                                gaiji.as_ref().unwrap(),
                                include_notes,
                            );
                            (
                                context_text,
                                format!(
                                    "plain-lb-context-{}-{}-{}",
                                    lb, context_before, context_after
                                ),
                                false,
                            )
                        } else {
                            let context_text = daizo_core::extract_xml_around_line_asymmetric(
                                &xml,
                                xml_line,
                                context_before,
                                context_after,
                            );
                            (
                                context_text,
                                format!("lb-context-{}-{}-{}", lb, context_before, context_after),
                                false,
                            )
                        }
                    } else {
                        if is_plain {
                            ensure_gaiji();
                            let t = extract_cbeta_plain_from_snippet(
                                &xml,
                                gaiji.as_ref().unwrap(),
                                include_notes,
                            );
                            (t, "plain-full".to_string(), false)
                        } else {
                            (
                                extract_text_opts(&xml, include_notes),
                                "full".to_string(),
                                false,
                            )
                        }
                    }
                } else {
                    if is_plain {
                        ensure_gaiji();
                        let t = extract_cbeta_plain_from_snippet(
                            &xml,
                            gaiji.as_ref().unwrap(),
                            include_notes,
                        );
                        (t, "plain-full".to_string(), false)
                    } else {
                        (
                            extract_text_opts(&xml, include_notes),
                            "full".to_string(),
                            false,
                        )
                    }
                }
            } else if let Some(line_num) = args.get("lineNumber").and_then(|v| v.as_u64()) {
                // 新しいパラメータを優先、fallbackで古いパラメータを使用
                let context_before = args
                    .get("contextBefore")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(
                        args.get("contextLines")
                            .and_then(|v| v.as_u64())
                            .unwrap_or(10),
                    ) as usize;
                let context_after = args.get("contextAfter").and_then(|v| v.as_u64()).unwrap_or(
                    args.get("contextLines")
                        .and_then(|v| v.as_u64())
                        .unwrap_or(100),
                ) as usize;
                if is_plain {
                    ensure_gaiji();
                    let raw = extract_text_around_line_asymmetric(
                        &xml,
                        line_num as usize,
                        context_before,
                        context_after,
                    );
                    let context_text = extract_cbeta_plain_from_snippet(
                        &raw,
                        gaiji.as_ref().unwrap(),
                        include_notes,
                    );
                    (
                        context_text,
                        format!(
                            "plain-line-context-{}-{}-{}",
                            line_num, context_before, context_after
                        ),
                        false,
                    )
                } else {
                    let context_text = daizo_core::extract_xml_around_line_asymmetric(
                        &xml,
                        line_num as usize,
                        context_before,
                        context_after,
                    );
                    (
                        context_text,
                        format!(
                            "line-context-{}-{}-{}",
                            line_num, context_before, context_after
                        ),
                        false,
                    )
                }
            } else if let Some(part) = args.get("part").and_then(|v| v.as_str()) {
                if is_plain {
                    if let Some(sec) = extract_cbeta_juan_plain(&xml, part, include_notes) {
                        (sec, "plain-cbeta-juan".to_string(), true)
                    } else {
                        ensure_gaiji();
                        let t = extract_cbeta_plain_from_snippet(
                            &xml,
                            gaiji.as_ref().unwrap(),
                            include_notes,
                        );
                        (t, "plain-full".to_string(), false)
                    }
                } else if let Some(sec) = extract_cbeta_juan(&xml, part) {
                    (sec, "cbeta-juan".to_string(), true)
                } else {
                    (
                        extract_text_opts(&xml, include_notes),
                        "full".to_string(),
                        false,
                    )
                }
            } else if let Some(hq) = args.get("headQuery").and_then(|v| v.as_str()) {
                if is_plain {
                    if let Some((start, end)) = section_by_head_bounds(&xml, None, Some(hq)) {
                        ensure_gaiji();
                        let sec_xml = &xml[start..end];
                        let t = extract_cbeta_plain_from_snippet(
                            sec_xml,
                            gaiji.as_ref().unwrap(),
                            include_notes,
                        );
                        (t, "plain-head-query".to_string(), false)
                    } else {
                        ensure_gaiji();
                        let t = extract_cbeta_plain_from_snippet(
                            &xml,
                            gaiji.as_ref().unwrap(),
                            include_notes,
                        );
                        (t, "plain-full".to_string(), false)
                    }
                } else {
                    (
                        extract_section_by_head(&xml, None, Some(hq), include_notes)
                            .unwrap_or_else(|| extract_text_opts(&xml, include_notes)),
                        "head-query".to_string(),
                        false,
                    )
                }
            } else if let Some(hi) = args.get("headIndex").and_then(|v| v.as_u64()) {
                if is_plain {
                    if let Some((start, end)) =
                        section_by_head_bounds(&xml, Some(hi as usize), None)
                    {
                        ensure_gaiji();
                        let sec_xml = &xml[start..end];
                        let t = extract_cbeta_plain_from_snippet(
                            sec_xml,
                            gaiji.as_ref().unwrap(),
                            include_notes,
                        );
                        (t, "plain-head-index".to_string(), false)
                    } else {
                        ensure_gaiji();
                        let t = extract_cbeta_plain_from_snippet(
                            &xml,
                            gaiji.as_ref().unwrap(),
                            include_notes,
                        );
                        (t, "plain-full".to_string(), false)
                    }
                } else {
                    (
                        extract_section_by_head(&xml, Some(hi as usize), None, include_notes)
                            .unwrap_or_else(|| extract_text_opts(&xml, include_notes)),
                        "head-index".to_string(),
                        false,
                    )
                }
            } else {
                if is_plain {
                    ensure_gaiji();
                    let t = extract_cbeta_plain_from_snippet(
                        &xml,
                        gaiji.as_ref().unwrap(),
                        include_notes,
                    );
                    (t, "plain-full".to_string(), false)
                } else {
                    (
                        extract_text_opts(&xml, include_notes),
                        "full".to_string(),
                        false,
                    )
                }
            };

            // If highlight is provided but lb/lineNumber isn't, focus output around the first match.
            // This avoids "start of text only" when the match is far from the beginning.
            let full_flag = args.get("full").and_then(|v| v.as_bool()).unwrap_or(false);
            let focus_hl = args
                .get("focusHighlight")
                .and_then(|v| v.as_bool())
                .unwrap_or(true);
            let has_target = args.get("lb").is_some() || args.get("lineNumber").is_some();
            let has_slice = args.get("startChar").is_some()
                || args.get("endChar").is_some()
                || args.get("page").is_some()
                || args.get("pageSize").is_some()
                || args.get("maxChars").is_some();
            let mut focused_meta: Option<serde_json::Value> = None;
            if !full_flag && focus_hl && !has_target && !has_slice {
                if let Some(pat) = hl_pat.as_deref() {
                    let before = args
                        .get("contextBefore")
                        .and_then(|v| v.as_u64())
                        .unwrap_or(
                            args.get("contextLines")
                                .and_then(|v| v.as_u64())
                                .unwrap_or(3),
                        ) as usize;
                    let after = args.get("contextAfter").and_then(|v| v.as_u64()).unwrap_or(
                        args.get("contextLines")
                            .and_then(|v| v.as_u64())
                            .unwrap_or(20),
                    ) as usize;
                    let span = if hl_use_re {
                        Regex::new(pat)
                            .ok()
                            .and_then(|re| re.find(&text).map(|m| (m.start(), m.end())))
                    } else {
                        text.find(pat).map(|sb| (sb, sb + pat.len()))
                    };
                    if let Some((sb, _eb)) = span {
                        let line_no = text[..sb].lines().count() + 1;
                        let ctx = daizo_core::extract_text_around_line_asymmetric(
                            &text, line_no, before, after,
                        );
                        if !ctx.trim().is_empty() {
                            focused_meta = Some(json!({
                                "pattern": pat,
                                "patternIsRegex": hl_use_re,
                                "line": line_no,
                                "contextBefore": before,
                                "contextAfter": after
                            }));
                            text = ctx;
                            extraction_method =
                                format!("highlight-context-{}-{}-{}", line_no, before, after);
                        }
                    }
                }
            }

            let mut sliced = if full_flag {
                text.clone()
            } else {
                slice_text(&text, &args)
            };
            // Optional highlight across sliced text
            let mut highlight_count = 0usize;
            let mut highlight_positions: Vec<serde_json::Value> = Vec::new();
            if let Some(hpat) = hl_pat.as_deref().or_else(|| hl_in.as_deref()) {
                let use_re = hl_use_re;
                let hpre = args
                    .get("highlightPrefix")
                    .and_then(|v| v.as_str())
                    .unwrap_or(">>> ");
                let hsuf = args
                    .get("highlightSuffix")
                    .and_then(|v| v.as_str())
                    .unwrap_or(" <<<");
                let original = sliced.clone();
                if use_re {
                    if let Ok(re) = regex::Regex::new(hpat) {
                        for m in re.find_iter(&original) {
                            let sb = m.start();
                            let eb = m.end();
                            let sc = original[..sb].chars().count();
                            let ec = sc + original[sb..eb].chars().count();
                            highlight_positions.push(json!({"startChar": sc, "endChar": ec}));
                        }
                        let mut count = 0usize;
                        let replaced = re.replace_all(&sliced, |caps: &regex::Captures| {
                            count += 1;
                            format!("{}{}{}", hpre, &caps[0], hsuf)
                        });
                        sliced = replaced.into_owned();
                        highlight_count = count;
                    }
                } else if !hpat.is_empty() {
                    let mut i = 0usize;
                    while let Some(pos) = original[i..].find(hpat) {
                        let abs = i + pos;
                        let sc = original[..abs].chars().count();
                        let ec = sc + hpat.chars().count();
                        highlight_positions.push(json!({"startChar": sc, "endChar": ec}));
                        i = abs + hpat.len();
                    }
                    let mut out = String::with_capacity(sliced.len());
                    let mut j = 0usize;
                    while let Some(pos) = sliced[j..].find(hpat) {
                        let abs = j + pos;
                        out.push_str(&sliced[j..abs]);
                        out.push_str(hpre);
                        out.push_str(hpat);
                        out.push_str(hsuf);
                        j = abs + hpat.len();
                        highlight_count += 1;
                    }
                    out.push_str(&sliced[j..]);
                    sliced = out;
                }
            }
            let heads = cbeta_heads_cached(&path, xml);
            let hl = args
                .get("headingsLimit")
                .and_then(|v| v.as_u64())
                .unwrap_or(10) as usize;
            // enforce output cap
            let cap = default_max_chars();
            if sliced.chars().count() > cap {
                sliced = sliced.chars().take(cap).collect();
            }
            let meta = json!({
                "totalLength": text.len(),
                "returnedStart": args.get("startChar").and_then(|v| v.as_u64()).unwrap_or( args.get("page").and_then(|v| v.as_u64()).and_then(|p| args.get("pageSize").and_then(|s| s.as_u64()).map(|ps| p*ps)).unwrap_or(0) ) ,
                "returnedEnd": args.get("startChar").and_then(|v| v.as_u64()).unwrap_or( args.get("page").and_then(|v| v.as_u64()).and_then(|p| args.get("pageSize").and_then(|s| s.as_u64()).map(|ps| p*ps)).unwrap_or(0) ) + (sliced.len() as u64),
                "truncated": if full_flag { (sliced.len() as u64) < (text.len() as u64) } else { (sliced.len() as u64) < (text.len() as u64) },
                "sourcePath": path.to_string_lossy(),
                "format": if is_plain { "plain" } else { "default" },
                "extractionMethod": extraction_method,
                "partMatched": part_matched,
                "headingsTotal": heads.len(),
                "headingsPreview": heads.iter().take(hl).cloned().collect::<Vec<_>>(),
                "matchedId": matched_id,
                "matchedTitle": matched_title,
                "matchedScore": matched_score,
                "focused": focused_meta,
                "highlighted": if highlight_count > 0 { Some(highlight_count) } else { None::<usize> },
                "highlightPositions": if highlight_positions.is_empty() { None::<Vec<serde_json::Value>> } else { Some(highlight_positions) },
            });
            return json!({"jsonrpc":"2.0","id": id, "result": { "content": [{"type":"text","text": sliced}], "_meta": meta }});
        }
        "tipitaka_title_search" => {
            let q = args
                .get("query")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .trim();
            let limit = args.get("limit").and_then(|v| v.as_u64()).unwrap_or(10) as usize;
            let idx = load_or_build_tipitaka_index();
            let hits = best_match_tipitaka(idx, q, limit);
            let summary = hits
                .iter()
                .enumerate()
                .map(|(i, h)| {
                    let stem = Path::new(&h.entry.path)
                        .file_stem()
                        .and_then(|s| s.to_str())
                        .unwrap_or(&h.entry.id);
                    format!("{}. {}  {}", i + 1, stem, h.entry.title)
                })
                .collect::<Vec<_>>()
                .join("\n");
            let results: Vec<_> = hits
                .iter()
                .map(|h| {
                    let stem = Path::new(&h.entry.path)
                        .file_stem()
                        .and_then(|s| s.to_str())
                        .unwrap_or(&h.entry.id)
                        .to_string();
                    json!({
                        "id": stem,
                        "title": h.entry.title,
                        "path": h.entry.path,
                        "score": h.score,
                        "meta": h.entry.meta
                    })
                })
                .collect();
            let meta = json!({ "count": results.len(), "results": results });
            return json!({"jsonrpc":"2.0","id": id, "result": { "content": [{"type":"text","text": summary }], "_meta": meta }});
        }
        "tipitaka_fetch" => {
            ensure_tipitaka_data();
            let mut matched_id: Option<String> = None;
            let mut matched_title: Option<String> = None;
            let mut matched_score: Option<f32> = None;
            // 高速化: IDが指定されている場合は直接パス解決を試み、インデックスのロードを回避
            let mut path: PathBuf = if let Some(id) = args.get("id").and_then(|v| v.as_str()) {
                // まず直接パス解決を試みる（インデックス不要、高速）
                if let Some(direct_path) =
                    daizo_core::path_resolver::resolve_tipitaka_path_direct(id)
                {
                    matched_id = Path::new(&direct_path)
                        .file_stem()
                        .map(|s| s.to_string_lossy().into_owned());
                    direct_path
                } else {
                    // フォールバック: インデックスから検索
                    let idx = load_or_build_tipitaka_index();
                    if let Some(p) = resolve_tipitaka_by_id(&idx, id) {
                        matched_id = Path::new(&p)
                            .file_stem()
                            .map(|s| s.to_string_lossy().into_owned());
                        if let Some(e) = idx.iter().find(|e| e.path == p.to_string_lossy()) {
                            matched_title = Some(e.title.clone());
                        }
                        p
                    } else {
                        PathBuf::new()
                    }
                }
            } else if let Some(q) = args.get("query").and_then(|v| v.as_str()) {
                let idx = load_or_build_tipitaka_index();
                if let Some(hit) = best_match_tipitaka(idx, q, 1).into_iter().next() {
                    matched_title = Some(hit.entry.title.clone());
                    matched_score = Some(hit.score);
                    matched_id = Path::new(&hit.entry.path)
                        .file_stem()
                        .map(|s| s.to_string_lossy().into_owned());
                    PathBuf::from(&hit.entry.path)
                } else {
                    PathBuf::new()
                }
            } else {
                PathBuf::new()
            };

            // まだ見つからない場合は、厳密に `<id>.xml` を探す
            if path.as_os_str().is_empty() || !path.exists() {
                let fname = format!(
                    "{}.xml",
                    args.get("id").and_then(|v| v.as_str()).unwrap_or_default()
                );
                if let Some(p) = find_exact_file_by_name(&tipitaka_root(), &fname) {
                    path = p;
                }
            }
            // 見つからない場合は空に近い応答
            if path.as_os_str().is_empty() {
                path = PathBuf::from("");
            }
            // If we matched a TOC file (e.g., s0404m1.mul.toc.xml), try to open the first content part (e.g., s0404m1.mul0.xml)
            if let Some(name) = path.file_name().and_then(|s| s.to_str()) {
                if name.ends_with(".toc.xml") {
                    if let Some(stem) = Path::new(name).file_stem().and_then(|s| s.to_str()) {
                        let base = stem.trim_end_matches(".toc");
                        // Prefer base0.xml, then any base*.xml (non-TOC)
                        let dir: PathBuf = path
                            .parent()
                            .map(|p| p.to_path_buf())
                            .unwrap_or_else(|| tipitaka_root());
                        let mut candidate: Option<PathBuf> = None;
                        let base0 = dir.join(format!("{}0.xml", base));
                        if base0.exists() {
                            candidate = Some(base0);
                        }
                        if candidate.is_none() {
                            // Scan directory for base*.xml excluding *.toc.xml
                            if let Ok(read_dir) = fs::read_dir(&dir) {
                                for entry in read_dir.flatten() {
                                    let p = entry.path();
                                    if p.extension().and_then(|s| s.to_str()) == Some("xml") {
                                        if let Some(stem2) = p.file_stem().and_then(|s| s.to_str())
                                        {
                                            if stem2.starts_with(base) && !stem2.ends_with(".toc") {
                                                candidate = Some(p.clone());
                                                break;
                                            }
                                        }
                                    }
                                }
                            }
                        }
                        if let Some(c) = candidate {
                            path = c;
                        }
                    }
                }
            }
            // 読み取り時にエンコーディング問題で空になるのを避けるため、バイト読み + UTF-8(代替) に変更
            let mut cur_path = path.clone();
            let mut xml = fs::read(&cur_path)
                .map(|b| decode_xml_bytes(&b))
                .unwrap_or_default();
            let (mut text, mut extraction_method) =
                if let Some(line_num) = args.get("lineNumber").and_then(|v| v.as_u64()) {
                    // 新しいパラメータを優先、fallbackで古いパラメータを使用
                    let context_before = args
                        .get("contextBefore")
                        .and_then(|v| v.as_u64())
                        .unwrap_or(
                            args.get("contextLines")
                                .and_then(|v| v.as_u64())
                                .unwrap_or(10),
                        ) as usize;
                    let context_after = args.get("contextAfter").and_then(|v| v.as_u64()).unwrap_or(
                        args.get("contextLines")
                            .and_then(|v| v.as_u64())
                            .unwrap_or(100),
                    ) as usize;
                    let context_text = daizo_core::extract_xml_around_line_asymmetric(
                        &xml,
                        line_num as usize,
                        context_before,
                        context_after,
                    );
                    (
                        context_text,
                        format!(
                            "line-context-{}-{}-{}",
                            line_num, context_before, context_after
                        ),
                    )
                } else if let Some(hq) = args.get("headQuery").and_then(|v| v.as_str()) {
                    (
                        extract_section_by_head(&xml, None, Some(hq), false)
                            .unwrap_or_else(|| extract_text(&xml)),
                        "head-query".to_string(),
                    )
                } else if let Some(hi) = args.get("headIndex").and_then(|v| v.as_u64()) {
                    (
                        extract_section_by_head(&xml, Some(hi as usize), None, false)
                            .unwrap_or_else(|| extract_text(&xml)),
                        "head-index".to_string(),
                    )
                } else {
                    (extract_text(&xml), "full".to_string())
                };
            // フォールバックA：抽出が空で、同ベースの連番ファイルがある場合は最小番号を開く
            if text.trim().is_empty() {
                if let Some(stem) = cur_path.file_stem().and_then(|s| s.to_str()) {
                    let ends_with_digit = stem
                        .chars()
                        .last()
                        .map(|c| c.is_ascii_digit())
                        .unwrap_or(false);
                    if !ends_with_digit {
                        if let Some(candidate) = find_tipitaka_content_for_base(stem) {
                            if candidate != cur_path {
                                cur_path = candidate.clone();
                                xml = fs::read(&candidate)
                                    .map(|b| decode_xml_bytes(&b))
                                    .unwrap_or_default();
                                let (t2, m2) = if let Some(hq) =
                                    args.get("headQuery").and_then(|v| v.as_str())
                                {
                                    (
                                        extract_section_by_head(&xml, None, Some(hq), false)
                                            .unwrap_or_else(|| extract_text(&xml)),
                                        "head-query+base-fallback".to_string(),
                                    )
                                } else if let Some(hi) =
                                    args.get("headIndex").and_then(|v| v.as_u64())
                                {
                                    (
                                        extract_section_by_head(
                                            &xml,
                                            Some(hi as usize),
                                            None,
                                            false,
                                        )
                                        .unwrap_or_else(|| extract_text(&xml)),
                                        "head-index+base-fallback".to_string(),
                                    )
                                } else {
                                    (extract_text(&xml), "full+base-fallback".to_string())
                                };
                                text = t2;
                                extraction_method = m2;
                            }
                        }
                    }
                }
            }
            // フォールバックB：それでも空ならタグ除去の素朴処理
            if text.trim().is_empty() && !xml.trim().is_empty() {
                if let Ok(re) = regex::Regex::new(r"<[^>]+>") {
                    let t = re.replace_all(&xml, " ");
                    let t = t.split_whitespace().collect::<Vec<_>>().join(" ");
                    if !t.trim().is_empty() {
                        text = t;
                        extraction_method = "plain-strip-tags".to_string();
                    }
                }
            }
            let mut sliced = slice_text(&text, &args);
            // enforce output cap
            let cap = default_max_chars();
            if sliced.chars().count() > cap {
                sliced = sliced.chars().take(cap).collect();
            }
            // Optional highlight for Tipitaka
            let hl_in = args.get("highlight").and_then(|v| v.as_str());
            let mut hl_regex = args
                .get("highlightRegex")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            let hpre = args
                .get("highlightPrefix")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string())
                .or_else(|| std::env::var("DAIZO_HL_PREFIX").ok())
                .unwrap_or_else(|| ">>> ".to_string());
            let hsuf = args
                .get("highlightSuffix")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string())
                .or_else(|| std::env::var("DAIZO_HL_SUFFIX").ok())
                .unwrap_or_else(|| " <<<".to_string());
            let mut highlight_positions: Vec<serde_json::Value> = Vec::new();
            let mut highlight_count = 0usize;
            if let Some(hpat0) = hl_in {
                let looks_like_regex = hpat0.chars().any(|c| ".+*?[](){}|\\".contains(c));
                let hpat =
                    if hpat0.chars().any(|c| c.is_whitespace()) && !looks_like_regex && !hl_regex {
                        hl_regex = true;
                        to_whitespace_fuzzy_literal(hpat0)
                    } else {
                        hpat0.to_string()
                    };
                if hl_regex {
                    if let Ok(re) = Regex::new(&hpat) {
                        for m in re.find_iter(&sliced) {
                            let sb = m.start();
                            let eb = m.end();
                            let sc = sliced[..sb].chars().count();
                            let ec = sc + sliced[sb..eb].chars().count();
                            highlight_positions.push(json!({"startChar": sc, "endChar": ec}));
                        }
                        let mut count = 0usize;
                        let replaced = re.replace_all(&sliced, |caps: &regex::Captures| {
                            count += 1;
                            format!("{}{}{}", hpre, &caps[0], hsuf)
                        });
                        sliced = replaced.into_owned();
                        highlight_count = count;
                    }
                } else if !hpat.is_empty() {
                    let mut i = 0usize;
                    while let Some(pos) = sliced[i..].find(&hpat) {
                        let abs = i + pos;
                        let sc = sliced[..abs].chars().count();
                        let ec = sc + hpat.chars().count();
                        highlight_positions.push(json!({"startChar": sc, "endChar": ec}));
                        i = abs + hpat.len();
                    }
                    let mut out = String::with_capacity(sliced.len());
                    let mut j = 0usize;
                    while let Some(pos) = sliced[j..].find(&hpat) {
                        let abs = j + pos;
                        out.push_str(&sliced[j..abs]);
                        out.push_str(&hpre);
                        out.push_str(&hpat);
                        out.push_str(&hsuf);
                        j = abs + hpat.len();
                        highlight_count += 1;
                    }
                    out.push_str(&sliced[j..]);
                    sliced = out;
                }
            }
            let heads = list_heads_generic(&xml);
            let hl = args
                .get("headingsLimit")
                .and_then(|v| v.as_u64())
                .unwrap_or(10) as usize;
            let meta = json!({
                "totalLength": text.len(),
                "returnedStart": args.get("startChar").and_then(|v| v.as_u64()).unwrap_or( args.get("page").and_then(|v| v.as_u64()).and_then(|p| args.get("pageSize").and_then(|s| s.as_u64()).map(|ps| p*ps)).unwrap_or(0) ) ,
                "returnedEnd": args.get("startChar").and_then(|v| v.as_u64()).unwrap_or( args.get("page").and_then(|v| v.as_u64()).and_then(|p| args.get("pageSize").and_then(|s| s.as_u64()).map(|ps| p*ps)).unwrap_or(0) ) + (sliced.len() as u64),
                "truncated": (sliced.len() as u64) < (text.len() as u64),
                "sourcePath": cur_path.to_string_lossy(),
                "extractionMethod": extraction_method,
                "headingsTotal": heads.len(),
                "headingsPreview": heads.into_iter().take(hl).collect::<Vec<_>>(),
                "matchedId": matched_id,
                "matchedTitle": matched_title,
                "matchedScore": matched_score,
                "biblio": tipitaka_biblio(&xml),
                "highlighted": if highlight_count > 0 { Some(highlight_count) } else { None::<usize> },
                "highlightPositions": if highlight_positions.is_empty() { None::<Vec<serde_json::Value>> } else { Some(highlight_positions) },
            });
            return json!({"jsonrpc":"2.0","id": id, "result": { "content": [{"type":"text","text": sliced}], "_meta": meta }});
        }
        "sat_search" => {
            let q = args.get("query").and_then(|v| v.as_str()).unwrap_or("");
            let rows = args.get("rows").and_then(|v| v.as_u64()).unwrap_or(100) as usize;
            let offs = args.get("offs").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
            let exact = args.get("exact").and_then(|v| v.as_bool()).unwrap_or(true);
            let titles_only = args
                .get("titlesOnly")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            let fields = args
                .get("fields")
                .and_then(|v| v.as_str())
                .unwrap_or("id,fascnm,startid,endid");
            let fq: Vec<String> = args
                .get("fq")
                .and_then(|v| v.as_array())
                .map(|a| {
                    a.iter()
                        .filter_map(|x| x.as_str().map(|s| s.to_string()))
                        .collect()
                })
                .unwrap_or_default();
            let qt = q.trim();
            let q_param = if exact && !qt.is_empty() {
                if qt.starts_with('\"') && qt.ends_with('\"') && qt.len() >= 2 {
                    qt.to_string()
                } else {
                    format!("\"{}\"", qt)
                }
            } else {
                qt.to_string()
            };
            if let Some(jsonv) = sat_wrap7_search_json(&q_param, rows, offs, fields, &fq) {
                let docs_v = jsonv
                    .get("response")
                    .and_then(|r| r.get("docs"))
                    .cloned()
                    .unwrap_or(json!([]));
                let count = jsonv
                    .get("response")
                    .and_then(|r| r.get("numFound"))
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0);
                let meta_base = json!({ "count": count, "results": docs_v, "titlesOnly": titles_only, "q": qt, "qSent": q_param, "exact": exact, "fl": fields, "fq": fq });
                let auto = args
                    .get("autoFetch")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                if auto {
                    let docs = jsonv
                        .get("response")
                        .and_then(|r| r.get("docs"))
                        .and_then(|v| v.as_array())
                        .cloned()
                        .unwrap_or_default();
                    if docs.is_empty() {
                        let summary = "0 results".to_string();
                        return json!({ "jsonrpc":"2.0","id": id, "result": { "content": [{"type":"text","text": summary }], "_meta": meta_base }});
                    }
                    let mut best_i = 0usize;
                    let mut best_sc = -1f32;
                    for (i, d) in docs.iter().enumerate() {
                        let title = d.get("fascnm").and_then(|v| v.as_str()).unwrap_or("");
                        let sc = title_score(title, q);
                        if sc > best_sc {
                            best_sc = sc;
                            best_i = i;
                        }
                    }
                    let chosen = &docs[best_i];
                    let useid = chosen.get("startid").and_then(|v| v.as_str()).unwrap_or("");
                    let url = sat_detail_build_url(useid);
                    let t = sat_fetch(&url);
                    let start =
                        args.get("startChar").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
                    let maxc = args
                        .get("maxChars")
                        .and_then(|v| v.as_u64())
                        .unwrap_or(8000) as usize;
                    let (sliced, total_chars, returned_start, returned_end) =
                        slice_text_bounds(&t, start, maxc);
                    let mut meta = meta_base;
                    meta["chosen"] = chosen.clone();
                    meta["titleScore"] = json!(best_sc);
                    meta["sourceUrl"] = json!(url);
                    meta["returnedStart"] = json!(returned_start as u64);
                    meta["returnedEnd"] = json!(returned_end as u64);
                    meta["totalLength"] = json!(total_chars as u64);
                    meta["truncated"] = json!(returned_end < total_chars);
                    meta["extractionMethod"] = json!("sat-detail-extract");
                    return json!({ "jsonrpc":"2.0","id": id, "result": { "content": [{"type":"text","text": sliced }], "_meta": meta }});
                } else {
                    let summary = if titles_only {
                        format!("{} titles; see _meta.results", count)
                    } else {
                        format!("{} results; see _meta.results", count)
                    };
                    return json!({ "jsonrpc":"2.0","id": id, "result": { "content": [{"type":"text","text": summary }], "_meta": meta_base }});
                }
            } else {
                let hits = sat_search_results(q, rows, offs, exact, titles_only);
                let meta =
                    json!({ "count": hits.len(), "results": hits, "titlesOnly": titles_only });
                let summary = if titles_only {
                    format!(
                        "{} titles; see _meta.results",
                        meta["count"].as_u64().unwrap_or(0)
                    )
                } else {
                    format!(
                        "{} results; see _meta.results",
                        meta["count"].as_u64().unwrap_or(0)
                    )
                };
                return json!({ "jsonrpc":"2.0","id": id, "result": { "content": [{"type":"text","text": summary }], "_meta": meta }});
            }
        }
        "sat_fetch" => {
            // Prefer building URL from useid (startid). Fallback to direct url.
            let url = if let Some(uid) = args.get("useid").and_then(|v| v.as_str()) {
                sat_detail_build_url(uid)
            } else {
                args.get("url")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string()
            };
            let start = args.get("startChar").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
            let maxc = args
                .get("maxChars")
                .and_then(|v| v.as_u64())
                .unwrap_or(8000) as usize;
            let t = sat_fetch(&url);
            let (sliced, total_chars, returned_start, returned_end) =
                slice_text_bounds(&t, start, maxc);
            let meta = json!({
                "totalLength": total_chars as u64,
                "returnedStart": returned_start as u64,
                "returnedEnd": returned_end as u64,
                "truncated": returned_end < total_chars,
                "sourceUrl": url,
                "extractionMethod": "sat-detail-extract"
            });
            return json!({"jsonrpc":"2.0","id": id, "result": { "content": [{"type":"text","text": sliced}], "_meta": meta }});
        }
        "sat_detail" => {
            let useid = args.get("useid").and_then(|v| v.as_str()).unwrap_or("");
            // Fixed params per observation: mode=detail, ob=1, mode2=2. useid is the key.
            let url = format!("https://21dzk.l.u-tokyo.ac.jp/SAT2018/satdb2018pre.php?mode=detail&ob=1&mode2=2&useid={}", urlencoding::encode(useid));
            let start = args.get("startChar").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
            let maxc = args
                .get("maxChars")
                .and_then(|v| v.as_u64())
                .unwrap_or(8000) as usize;
            let t = sat_fetch(&url);
            let (sliced, total_chars, returned_start, returned_end) =
                slice_text_bounds(&t, start, maxc);
            let meta = json!({
                "totalLength": total_chars as u64,
                "returnedStart": returned_start as u64,
                "returnedEnd": returned_end as u64,
                "truncated": returned_end < total_chars,
                "sourceUrl": url,
                "extractionMethod": "sat-detail-extract"
            });
            return json!({"jsonrpc":"2.0","id": id, "result": { "content": [{"type":"text","text": sliced}], "_meta": meta }});
        }
        "tibetan_search" => {
            let q = args
                .get("query")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .trim()
                .to_string();
            if q.is_empty() {
                return json!({"jsonrpc":"2.0","id": id, "result": { "content": [{"type":"text","text": "query is empty"}], "_meta": {"query": q, "count": 0, "results": []} }});
            }
            let sources: Vec<String> = args
                .get("sources")
                .and_then(|v| v.as_array())
                .map(|a| {
                    a.iter()
                        .filter_map(|x| x.as_str().map(|s| s.to_string()))
                        .collect::<Vec<_>>()
                })
                .unwrap_or_else(|| vec!["adarshah".to_string(), "buda".to_string()]);
            let limit = args.get("limit").and_then(|v| v.as_u64()).unwrap_or(10) as usize;
            let exact = args.get("exact").and_then(|v| v.as_bool()).unwrap_or(true);
            let max_snippet_chars = args
                .get("maxSnippetChars")
                .and_then(|v| v.as_u64())
                .unwrap_or(240) as usize;
            let wildcard = args
                .get("wildcard")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);

            let mut variants: Vec<String> = vec![q.clone()];
            if let Some(u) = ewts_to_unicode_best_effort(&q) {
                if !variants.iter().any(|x| x == &u) {
                    variants.push(u);
                }
            }

            let mut results: Vec<serde_json::Value> = Vec::new();
            let warnings: Vec<String> = Vec::new();

            for src in sources.iter() {
                match src.as_str() {
                    "adarshah" => {
                        for vq in variants.iter() {
                            let mut r =
                                adarshah_search_fulltext(vq, wildcard, limit, max_snippet_chars);
                            results.append(&mut r);
                        }
                    }
                    "buda" => {
                        for vq in variants.iter() {
                            let mut r = buda_search_fulltext(vq, exact, limit, max_snippet_chars);
                            results.append(&mut r);
                        }
                    }
                    _ => {}
                }
            }

            let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
            let mut uniq: Vec<serde_json::Value> = Vec::new();
            for r in results.into_iter() {
                let key = r
                    .get("url")
                    .and_then(|u| u.as_str())
                    .map(|s| s.to_string())
                    .unwrap_or_else(|| serde_json::to_string(&r).unwrap_or_default());
                if seen.insert(key) {
                    uniq.push(r);
                }
                if uniq.len() >= limit {
                    break;
                }
            }

            let mut summary = format!("Tibetan search for '{}'\n", q);
            if uniq.is_empty() {
                summary.push_str("0 results\n");
            } else {
                for (i, r) in uniq.iter().enumerate() {
                    let src = r.get("source").and_then(|v| v.as_str()).unwrap_or("");
                    let title = r.get("title").and_then(|v| v.as_str()).unwrap_or("");
                    let snip = r.get("snippet").and_then(|v| v.as_str()).unwrap_or("");
                    let url = r.get("url").and_then(|v| v.as_str()).unwrap_or("");
                    if !title.is_empty() {
                        summary.push_str(&format!("{}. [{}] {}  {}\n", i + 1, src, title, url));
                    } else {
                        summary.push_str(&format!("{}. [{}] {}\n", i + 1, src, url));
                    }
                    if !snip.is_empty() {
                        summary.push_str(&format!("   {}\n", snip.replace('\n', " ")));
                    }
                }
            }

            let meta = json!({
                "query": q,
                "variants": variants,
                "sources": sources,
                "exact": exact,
                "maxSnippetChars": max_snippet_chars,
                "warnings": if warnings.is_empty() { None::<Vec<String>> } else { Some(warnings) },
                "count": uniq.len(),
                "results": uniq
            });
            return json!({"jsonrpc":"2.0","id": id, "result": { "content": [{"type":"text","text": summary}], "_meta": meta }});
        }
	        "sat_pipeline" => {
            let q = args.get("query").and_then(|v| v.as_str()).unwrap_or("");
            let exact = args.get("exact").and_then(|v| v.as_bool()).unwrap_or(true);
            let rows = args.get("rows").and_then(|v| v.as_u64()).unwrap_or(100) as usize;
            let offs = args.get("offs").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
            let fields_requested = args
                .get("fields")
                .and_then(|v| v.as_str())
                .unwrap_or("id,fascnm,startid,endid,body");
            // sat_pipeline selection relies on `body` and fetching relies on `startid`.
            // Ensure required fields are present even if the user customizes `fields`.
            let fields_used = sat_wrap7_ensure_fields(
                fields_requested,
                &["id", "fascnm", "startid", "endid", "body"],
            );
            let fq: Vec<String> = args
                .get("fq")
                .and_then(|v| v.as_array())
                .map(|a| {
                    a.iter()
                        .filter_map(|x| x.as_str().map(|s| s.to_string()))
                        .collect()
                })
                .unwrap_or_default();
            let start_char_opt = args
                .get("startChar")
                .and_then(|v| v.as_u64())
                .map(|v| v as usize);
            let start_char_provided = start_char_opt.is_some();
            let start_requested = start_char_opt.unwrap_or(0);
            let maxc = args
                .get("maxChars")
                .and_then(|v| v.as_u64())
                .unwrap_or(8000) as usize;
            let qt = q.trim();
            let q_param = if exact && !qt.is_empty() {
                if qt.starts_with('"') && qt.ends_with('"') && qt.len() >= 2 {
                    qt.to_string()
                } else {
                    format!("\"{}\"", qt)
                }
            } else {
                qt.to_string()
            };
            if let Some(jsonv) = sat_wrap7_search_json(&q_param, rows, offs, &fields_used, &fq) {
                let docs = jsonv
                    .get("response")
                    .and_then(|r| r.get("docs"))
                    .and_then(|v| v.as_array())
                    .cloned()
                    .unwrap_or_default();
                if docs.is_empty() {
                    return json!({"jsonrpc":"2.0","id": id, "result": { "content": [{"type":"text","text": "no results"}], "_meta": {"count": 0} }});
                }
                let (best_i, chosen_by, best_sc) = sat_pick_best_doc(&docs, qt);
                let chosen = &docs[best_i];
                let useid = chosen.get("startid").and_then(|v| v.as_str()).unwrap_or("");
                let url = sat_detail_build_url(useid);
                let t = sat_fetch(&url);
                let q_focus = qt
                    .trim()
                    .strip_prefix('"')
                    .and_then(|s| s.strip_suffix('"'))
                    .unwrap_or(qt)
                    .trim();
                let mut focus = json!({"enabled": false});
                let start_eff = if start_char_provided || q_focus.is_empty() {
                    start_requested
                } else {
                    let pat = ws_cjk_variant_fuzzy_regex_literal(q_focus);
                    let pos = find_highlight_positions(&t, &pat, true).into_iter().next();
                    if let Some(p0) = pos {
                        let s = p0.start_char.saturating_sub(50);
                        focus = json!({
                            "enabled": true,
                            "query": q_focus,
                            "pattern": pat,
                            "matchStartChar": p0.start_char as u64,
                            "matchEndChar": p0.end_char as u64,
                            "startChar": s as u64
                        });
                        s
                    } else {
                        start_requested
                    }
                };
                let (sliced, total_chars, returned_start, returned_end) =
                    slice_text_bounds(&t, start_eff, maxc);
                let count = jsonv
                    .get("response")
                    .and_then(|r| r.get("numFound"))
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0);
                let meta = json!({
                    "totalLength": total_chars as u64,
                    "returnedStart": returned_start as u64,
                    "returnedEnd": returned_end as u64,
                    "truncated": returned_end < total_chars,
                    "sourceUrl": url,
                    "extractionMethod": "sat-detail-extract",
                    "search": {"q": qt, "qSent": q_param, "exact": exact, "rows": rows, "offs": offs, "flRequested": fields_requested, "flUsed": fields_used, "fq": fq, "count": count},
                    "chosen": chosen,
                    "chosenBy": chosen_by,
                    "titleScore": best_sc,
                    "focus": focus,
                    "startCharRequested": start_requested as u64
                });
                return json!({"jsonrpc":"2.0","id": id, "result": { "content": [{"type":"text","text": sliced}], "_meta": meta }});
            } else {
                return json!({"jsonrpc":"2.0","id": id, "result": { "content": [{"type":"text","text": "no results"}], "_meta": {"count": 0} }});
	            }
	        }
	        "jozen_search" => {
	            let q_raw0 = args.get("query").and_then(|v| v.as_str()).unwrap_or("");
	            let q = q_raw0.trim().to_string();
	            let page = args.get("page").and_then(|v| v.as_u64()).unwrap_or(1) as usize;
	            let page = page.max(1);
	            let max_results = args
	                .get("maxResults")
	                .and_then(|v| v.as_u64())
	                .unwrap_or(20) as usize;
	            let max_results = std::cmp::min(max_results, 50);
	            let max_snippet_chars = args
	                .get("maxSnippetChars")
	                .and_then(|v| v.as_u64())
	                .map(|v| v as usize)
	                .unwrap_or(default_snippet_len());
	
	            if q.is_empty() {
	                let meta = json!({
	                    "source": "jozen",
	                    "query": q,
	                    "page": page,
	                    "pageSize": 50,
	                    "totalCount": 0,
	                    "totalPages": 0,
	                    "results": [],
	                    "fetchSuggestions": []
	                });
	                return json!({"jsonrpc":"2.0","id": id, "result": { "content": [{"type":"text","text": "0 results"}], "_meta": meta }});
	            }
	
	            let Some(html) = jozen_search_html(&q, page) else {
	                let meta = json!({
	                    "source": "jozen",
	                    "query": q,
	                    "page": page,
	                    "results": [],
	                    "error": "fetch failed"
	                });
	                return json!({"jsonrpc":"2.0","id": id, "result": { "content": [{"type":"text","text": "0 results"}], "_meta": meta }});
	            };
	
	            let parsed = jozen_parse_search_html(&html, &q, max_results, max_snippet_chars);
	            let hint_top = std::env::var("DAIZO_HINT_TOP")
	                .ok()
	                .and_then(|s| s.parse::<usize>().ok())
	                .unwrap_or(1);
	            let mut fetch_suggestions: Vec<serde_json::Value> = Vec::new();
	            for r in parsed.results.iter().take(hint_top) {
	                if !r.lineno.is_empty() {
	                    fetch_suggestions.push(json!({
	                        "tool": "jozen_fetch",
	                        "args": {"lineno": r.lineno.clone()}
	                    }));
	                }
	            }
	
	            let total = parsed.total_count.unwrap_or(parsed.results.len());
	            let total_pages = parsed.total_pages.unwrap_or(0);
	            let summary = if total_pages > 0 {
	                format!(
	                    "{} results (page {}/{}); see _meta.results",
	                    total, page, total_pages
	                )
	            } else {
	                format!("{} results (page {}); see _meta.results", total, page)
	            };
	            let meta = json!({
	                "source": "jozen",
	                "query": q,
	                "page": page,
	                "pageSize": parsed.page_size.unwrap_or(50),
	                "displayedCount": parsed.displayed_count,
	                "totalCount": parsed.total_count.unwrap_or(0),
	                "totalPages": parsed.total_pages.unwrap_or(0),
	                "results": parsed.results,
	                "fetchSuggestions": fetch_suggestions
	            });
	            return json!({"jsonrpc":"2.0","id": id, "result": { "content": [{"type":"text","text": summary}], "_meta": meta }});
	        }
	        "jozen_fetch" => {
	            let lineno0 = args.get("lineno").and_then(|v| v.as_str()).unwrap_or("");
	            let lineno = lineno0.trim().to_string();
	            if lineno.is_empty() {
	                return json!({"jsonrpc":"2.0","id": id, "result": { "content": [{"type":"text","text": "lineno is empty"}], "_meta": {"source":"jozen"} }});
	            }
	            let start = args.get("startChar").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
	            let maxc = args
	                .get("maxChars")
	                .and_then(|v| v.as_u64())
	                .map(|v| v as usize)
	                .unwrap_or(default_max_chars());
	
	            let source_url = jozen_detail_url(&lineno);
	            let Some(html) = jozen_detail_html(&lineno) else {
	                let meta = json!({
	                    "source": "jozen",
	                    "lineno": lineno,
	                    "sourceUrl": source_url,
	                    "error": "fetch failed"
	                });
	                return json!({"jsonrpc":"2.0","id": id, "result": { "content": [{"type":"text","text": ""}], "_meta": meta }});
	            };
	            let detail = jozen_extract_detail(&html, &source_url);
	            let (sliced, total_chars, returned_start, returned_end) =
	                slice_text_bounds(&detail.content, start, maxc);
	            let meta = json!({
	                "source": "jozen",
	                "sourceUrl": detail.source_url,
	                "workHeader": detail.work_header,
	                "textno": detail.textno,
	                "pagePrev": detail.page_prev,
	                "pageNext": detail.page_next,
	                "lineCount": detail.line_ids.len(),
	                "lineIds": detail.line_ids,
	                "totalLength": total_chars as u64,
	                "returnedStart": returned_start as u64,
	                "returnedEnd": returned_end as u64,
	                "truncated": returned_end < total_chars,
	                "extractionMethod": "jozen-detail-extract"
	            });
	            return json!({"jsonrpc":"2.0","id": id, "result": { "content": [{"type":"text","text": sliced}], "_meta": meta }});
	        }
	        "cbeta_search" => {
	            let q_raw0 = args.get("query").and_then(|v| v.as_str()).unwrap_or("");
	            let q_raw = q_raw0.trim();
	            let looks_like_regex = q_raw.chars().any(|c| ".+*?[](){}|\\".contains(c));
            let (q, q_display, hl_pat, hl_regex) = if looks_like_regex {
                (
                    q_raw.to_string(),
                    q_raw.to_string(),
                    q_raw.to_string(),
                    true,
                )
            } else {
                let qv = ws_cjk_variant_fuzzy_regex_literal(q_raw);
                (qv.clone(), q_raw.to_string(), qv, true)
            };
            let max_results = args
                .get("maxResults")
                .and_then(|v| v.as_u64())
                .unwrap_or(20) as usize;
            let max_matches_per_file = args
                .get("maxMatchesPerFile")
                .and_then(|v| v.as_u64())
                .unwrap_or(5) as usize;

            ensure_cbeta_data();
            let results = cbeta_grep(&cbeta_root(), &q, max_results, max_matches_per_file);

            let mut summary = format!(
                "Found {} files with matches for '{}':\n\n",
                results.len(),
                q_display
            );
            for (i, result) in results.iter().enumerate() {
                summary.push_str(&format!(
                    "{}. {} ({})\n",
                    i + 1,
                    result.title,
                    result.file_id
                ));
                summary.push_str(&format!(
                    "   {} matches, {}\n",
                    result.total_matches,
                    result
                        .fetch_hints
                        .total_content_size
                        .as_deref()
                        .unwrap_or("unknown size")
                ));

                for (j, m) in result.matches.iter().enumerate().take(2) {
                    summary.push_str(&format!(
                        "   Match {}: ...{}...\n",
                        j + 1,
                        m.context.chars().take(100).collect::<String>()
                    ));
                }
                if result.matches.len() > 2 {
                    summary.push_str(&format!(
                        "   ... and {} more matches\n",
                        result.matches.len() - 2
                    ));
                }

                if !result.fetch_hints.recommended_parts.is_empty() {
                    summary.push_str(&format!(
                        "   Recommended parts: {}\n",
                        result.fetch_hints.recommended_parts.join(", ")
                    ));
                }
                summary.push('\n');
            }
            if results.len() >= max_results {
                summary.push_str("NOTE: Results may be truncated by maxResults. Increase maxResults to see more.\n");
            }
            // Lightweight next-call hints for AI clients (low token cost)
            let hint_top = std::env::var("DAIZO_HINT_TOP")
                .ok()
                .and_then(|s| s.parse::<usize>().ok())
                .unwrap_or(1);
            let mut fetch_suggestions: Vec<serde_json::Value> = Vec::new();
            for r in results.iter().take(hint_top) {
                if let Some(m) = r.matches.first() {
                    if let Some(lb) = cbeta_extract_lb_from_line(&m.context) {
                        fetch_suggestions.push(json!({
                            "tool": "cbeta_fetch",
                            "args": {"id": r.file_id, "lb": lb, "contextBefore": 1, "contextAfter": 3, "highlight": hl_pat, "highlightRegex": hl_regex, "format": "plain"},
                            "mode": "low-cost"
                        }));
                    } else if let Some(ln) = m.line_number {
                        fetch_suggestions.push(json!({
                            "tool": "cbeta_fetch",
                            "args": {"id": r.file_id, "lineNumber": ln, "contextBefore": 1, "contextAfter": 3, "highlight": hl_pat, "highlightRegex": hl_regex, "format": "plain"},
                            "mode": "low-cost"
                        }));
                    }
                }
            }

            // Enrich structured results with CBETA lb (more stable than XML line numbers).
            // Keep the original `results` structs for summary rendering; only mutate JSON for `_meta`.
            let mut results_meta = serde_json::to_value(&results).unwrap_or(json!([]));
            if let Some(arr) = results_meta.as_array_mut() {
                for r in arr.iter_mut() {
                    if let Some(ms) = r.get_mut("matches").and_then(|v| v.as_array_mut()) {
                        for m in ms.iter_mut() {
                            if m.get("lb").is_some() {
                                continue;
                            }
                            let ctx = m.get("context").and_then(|v| v.as_str()).unwrap_or("");
                            if let Some(lb) = cbeta_extract_lb_from_line(ctx) {
                                m["lb"] = json!(lb);
                            }
                        }
                    }
                }
            }
            let mut meta = json!({
                "queryRaw": q_display,
                "searchPattern": q,
                "totalFiles": results.len(),
                "results": results_meta,
                "hint": "Use cbeta_fetch (id + lineNumber) for low-cost context; cbeta_pipeline with autoFetch=false to summarize",
                "fetchSuggestions": fetch_suggestions,
                "truncatedByMaxResults": results.len() >= max_results
            });
            // Optional pipeline hint (kept minimal)
            meta["pipelineHint"] = json!({
                "tool": "cbeta_pipeline",
                "args": {"query": q_display, "autoFetch": false, "maxResults": 5, "maxMatchesPerFile": 1, "includeHighlightSnippet": false, "includeMatchLine": true }
            });

            return json!({"jsonrpc":"2.0","id": id, "result": { "content": [{"type":"text","text": summary}], "_meta": meta }});
        }
        "cbeta_pipeline" => {
            let q_raw0 = args.get("query").and_then(|v| v.as_str()).unwrap_or("");
            let q_raw = q_raw0.trim();
            let looks_like_regex_q = q_raw.chars().any(|c| ".+*?[](){}|\\".contains(c));
            let q = if looks_like_regex_q {
                q_raw.to_string()
            } else {
                ws_cjk_variant_fuzzy_regex_literal(q_raw)
            };
            let max_results = args
                .get("maxResults")
                .and_then(|v| v.as_u64())
                .unwrap_or(10) as usize;
            let max_matches_per_file = args
                .get("maxMatchesPerFile")
                .and_then(|v| v.as_u64())
                .unwrap_or(3) as usize;
            let context_before = args
                .get("contextBefore")
                .and_then(|v| v.as_u64())
                .unwrap_or(10) as usize;
            let context_after = args
                .get("contextAfter")
                .and_then(|v| v.as_u64())
                .unwrap_or(100) as usize;
            let mut auto_fetch = args
                .get("autoFetch")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            let force_no_auto = std::env::var("DAIZO_FORCE_NO_AUTO").ok().as_deref() == Some("1");
            let auto_fetch_files = args
                .get("autoFetchFiles")
                .and_then(|v| v.as_u64())
                .map(|x| x as usize)
                .unwrap_or_else(|| if auto_fetch { default_auto_files() } else { 0 });
            let auto_fetch_matches = args
                .get("autoFetchMatches")
                .and_then(|v| v.as_u64())
                .map(|x| x as usize);
            let include_match_line = args
                .get("includeMatchLine")
                .and_then(|v| v.as_bool())
                .unwrap_or(true);
            let include_highlight_snippet = args
                .get("includeHighlightSnippet")
                .and_then(|v| v.as_bool())
                .unwrap_or(true);
            let min_snippet_len = args
                .get("minSnippetLen")
                .and_then(|v| v.as_u64())
                .map(|x| x as usize)
                .unwrap_or(default_snippet_len());
            // highlight/snippet markers with env fallbacks
            let hl_in = args.get("highlight").and_then(|v| v.as_str());
            let mut hl_regex = args
                .get("highlightRegex")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            let hl_pat: Option<String> = hl_in.map(|p| {
                let looks_like_regex_hl = p.chars().any(|c| ".+*?[](){}|\\".contains(c));
                if !hl_regex && !looks_like_regex_hl {
                    // Expand variants + whitespace and escape as regex by default for CBETA.
                    hl_regex = true;
                    ws_cjk_variant_fuzzy_regex_literal(p)
                } else {
                    p.to_string()
                }
            });
            let hl_pre = args
                .get("highlightPrefix")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string())
                .or_else(|| std::env::var("DAIZO_HL_PREFIX").ok())
                .unwrap_or_else(|| ">>> ".to_string());
            let hl_suf = args
                .get("highlightSuffix")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string())
                .or_else(|| std::env::var("DAIZO_HL_SUFFIX").ok())
                .unwrap_or_else(|| " <<<".to_string());
            let snip_pre = args
                .get("snippetPrefix")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string())
                .or_else(|| std::env::var("DAIZO_SNIPPET_PREFIX").ok())
                .unwrap_or_else(|| ">>> ".to_string());
            let snip_suf = args
                .get("snippetSuffix")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string())
                .or_else(|| std::env::var("DAIZO_SNIPPET_SUFFIX").ok())
                .unwrap_or_else(String::new);
            let full = args.get("full").and_then(|v| v.as_bool()).unwrap_or(false);
            let include_notes = args
                .get("includeNotes")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);

            ensure_cbeta_data();
            let results = cbeta_grep(&cbeta_root(), &q, max_results, max_matches_per_file);

            // Build summary and suggestions
            let mut summary = format!(
                "Found {} files with matches for '{}':\n\n",
                results.len(),
                q_raw
            );
            let mut suggestions: Vec<serde_json::Value> = Vec::new();
            for (i, result) in results.iter().enumerate() {
                summary.push_str(&format!(
                    "{}. {} ({})\n",
                    i + 1,
                    result.title,
                    result.file_id
                ));
                summary.push_str(&format!("   {} matches\n", result.total_matches));
                for (j, m) in result.matches.iter().enumerate().take(2) {
                    summary.push_str(&format!(
                        "   Match {}: ...{}...\n",
                        j + 1,
                        m.context.chars().take(100).collect::<String>()
                    ));
                }
                if let Some(m) = result.matches.first() {
                    if let Some(lb) = cbeta_extract_lb_from_line(&m.context) {
                        let sug_hl = hl_pat.clone().unwrap_or_else(|| q.to_string());
                        let sug_hl_regex = if hl_pat.is_some() { hl_regex } else { true };
                        suggestions.push(json!({
                            "tool": "cbeta_fetch",
                            "args": {"id": result.file_id, "lb": lb, "contextBefore": context_before, "contextAfter": context_after, "highlight": sug_hl, "highlightRegex": sug_hl_regex}
                        }));
                    } else if let Some(ln) = m.line_number {
                        let sug_hl = hl_pat.clone().unwrap_or_else(|| q.to_string());
                        let sug_hl_regex = if hl_pat.is_some() {
                            hl_regex
                        } else {
                            q.chars().any(|c| ".+*?[](){}|\\".contains(c))
                                || q.chars().any(|c| c.is_whitespace())
                        };
                        suggestions.push(json!({
                            "tool": "cbeta_fetch",
                            "args": {"id": result.file_id, "lineNumber": ln, "contextBefore": context_before, "contextAfter": context_after, "highlight": sug_hl, "highlightRegex": sug_hl_regex}
                        }));
                    }
                }
                summary.push('\n');
            }

            let mut content_items: Vec<serde_json::Value> =
                vec![json!({"type":"text","text": summary})];
            let mut meta = json!({
                "searchPattern": q,
                "queryRaw": q_raw,
                "totalFiles": results.len(),
                "results": results,
                "fetchSuggestions": suggestions
            });
            if force_no_auto && auto_fetch {
                auto_fetch = false;
                meta["autoFetchOverridden"] = json!(true);
            }

            if auto_fetch && auto_fetch_files > 0 {
                let take_files = std::cmp::min(auto_fetch_files, results.len());
                let mut fetched: Vec<serde_json::Value> = Vec::new();
                for r in results.iter().take(take_files) {
                    let xml = fs::read_to_string(&r.file_path).unwrap_or_default();
                    if full {
                        let text = if include_notes {
                            extract_text_opts(&xml, true)
                        } else {
                            extract_text(&xml)
                        };
                        let cap = default_max_chars();
                        let sliced: String = text.chars().take(cap).collect();
                        content_items.push(json!({"type":"text","text": sliced}));
                        fetched.push(json!({"id": r.file_id, "full": true, "returnedChars": sliced.chars().count()}));
                    } else {
                        let mut combined = String::new();
                        let mut count = 0usize;
                        let mut highlight_counts: Vec<usize> = Vec::new();
                        let mut file_highlights: Vec<Vec<serde_json::Value>> = Vec::new();
                        let mut per_file_limit = auto_fetch_matches.unwrap_or(max_matches_per_file);
                        if include_highlight_snippet {
                            per_file_limit = per_file_limit.min(default_auto_matches());
                        }
                        for m in r.matches.iter().take(per_file_limit) {
                            if let Some(ln) = m.line_number {
                                let mut ctx = daizo_core::extract_xml_around_line_asymmetric(
                                    &xml,
                                    ln,
                                    context_before,
                                    context_after,
                                );
                                if !include_match_line {
                                    // best-effort: remove central line (position = context_before)
                                    let mut lines: Vec<&str> = ctx.lines().collect();
                                    if context_before < lines.len() {
                                        lines.remove(context_before);
                                    }
                                    ctx = lines.join("\n");
                                }
                                if !ctx.trim().is_empty() {
                                    if !combined.is_empty() {
                                        combined.push_str("\n\n---\n\n");
                                    }
                                    // Prefer compact snippet; avoid dumping full context unless explicitly requested
                                    if include_highlight_snippet
                                        && !m.highlight.trim().is_empty()
                                        && m.highlight.trim().chars().count() >= min_snippet_len
                                    {
                                        combined.push_str(&format!(
                                            "{}{}{}",
                                            snip_pre,
                                            m.highlight.trim(),
                                            snip_suf
                                        ));
                                    }
                                    // optional in-context highlighting and positions per context
                                    let mut chigh: Vec<serde_json::Value> = Vec::new();
                                    if let Some(pats) = hl_pat.as_deref() {
                                        if hl_regex {
                                            if let Ok(re) = regex::Regex::new(pats) {
                                                for mm in re.find_iter(&ctx) {
                                                    let sb = mm.start();
                                                    let eb = mm.end();
                                                    let sc = ctx[..sb].chars().count();
                                                    let ec = sc + ctx[sb..eb].chars().count();
                                                    chigh.push(
                                                        json!({"startChar": sc, "endChar": ec}),
                                                    );
                                                }
                                                ctx = re
                                                    .replace_all(&ctx, |caps: &regex::Captures| {
                                                        format!("{}{}{}", hl_pre, &caps[0], hl_suf)
                                                    })
                                                    .into_owned();
                                            }
                                        } else if !pats.is_empty() {
                                            let mut i = 0usize;
                                            while let Some(pos) = ctx[i..].find(pats) {
                                                let abs = i + pos;
                                                let sc = ctx[..abs].chars().count();
                                                let ec = sc + pats.chars().count();
                                                chigh.push(json!({"startChar": sc, "endChar": ec}));
                                                i = abs + pats.len();
                                            }
                                            let mut out = String::with_capacity(ctx.len());
                                            let mut j = 0usize;
                                            while let Some(pos) = ctx[j..].find(pats) {
                                                let abs = j + pos;
                                                out.push_str(&ctx[j..abs]);
                                                out.push_str(&hl_pre);
                                                out.push_str(pats);
                                                out.push_str(&hl_suf);
                                                j = abs + pats.len();
                                            }
                                            out.push_str(&ctx[j..]);
                                            ctx = out;
                                        }
                                    }
                                    if !include_highlight_snippet {
                                        combined.push_str(&format!(
                                            "# {} (line {})\n\n{}",
                                            r.file_id, ln, ctx
                                        ));
                                    }
                                    highlight_counts.push(chigh.len());
                                    file_highlights.push(chigh);
                                    count += 1;
                                }
                            }
                        }
                        if !combined.is_empty() {
                            content_items.push(json!({"type":"text","text": combined}));
                            let mut fobj = json!({
                                "id": r.file_id,
                                "full": false,
                                "contexts": count,
                                "contextBefore": context_before,
                                "contextAfter": context_after,
                                "includeMatchLine": include_match_line,
                            });
                            if highlight_counts.iter().any(|&c| c > 0) {
                                fobj["highlightCounts"] = json!(highlight_counts);
                            }
                            fobj["highlightPositions"] = json!(file_highlights);
                            fetched.push(fobj);
                        }
                    }
                }
                if !fetched.is_empty() {
                    meta["autoFetched"] = json!(fetched);
                }
            }

            // Compatibility: some clients only display the first content item.
            // Inline all text content into a single item so autoFetch is always visible.
            if content_items.len() > 1 {
                let mut joined = String::new();
                for item in content_items.iter() {
                    if let Some(t) = item.get("text").and_then(|v| v.as_str()) {
                        if !joined.is_empty() {
                            joined.push_str("\n\n");
                        }
                        joined.push_str(t);
                    }
                }
                content_items = vec![json!({"type":"text","text": joined})];
            }

            return json!({"jsonrpc":"2.0","id": id, "result": { "content": content_items, "_meta": meta }});
        }
        "gretil_title_search" => {
            let q = args
                .get("query")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .trim()
                .to_string();
            let limit = args.get("limit").and_then(|v| v.as_u64()).unwrap_or(10) as usize;
            let idx = load_or_build_gretil_index();
            let hits = best_match_gretil(idx, &q, limit);
            let summary = hits
                .iter()
                .enumerate()
                .map(|(i, h)| format!("{}. {}  {}", i + 1, h.entry.id, h.entry.title))
                .collect::<Vec<_>>()
                .join("\n");
            let results: Vec<_> = hits
                .iter()
                .map(|h| {
                    json!({
                        "id": h.entry.id,
                        "title": h.entry.title,
                        "path": h.entry.path,
                        "score": h.score
                    })
                })
                .collect();
            let meta = json!({ "count": results.len(), "results": results });
            return json!({"jsonrpc":"2.0","id": id, "result": { "content": [{"type":"text","text": summary }], "_meta": meta }});
        }
        "gretil_fetch" => {
            let mut matched_id: Option<String> = None;
            let mut matched_title: Option<String> = None;
            let mut matched_score: Option<f32> = None;
            let mut path: PathBuf = PathBuf::new();
            if let Some(id_str) = args.get("id").and_then(|v| v.as_str()) {
                // 直接パス解決を最初に試行（インデックスロード不要で最速）
                if let Some(p) = daizo_core::path_resolver::resolve_gretil_path_direct(id_str) {
                    path = p.clone();
                    matched_id = Some(id_str.to_string());
                    // タイトルはキャッシュがあれば取得（なくても問題なし）
                    if let Some(idx) = GRETIL_INDEX_CACHE.get() {
                        if let Some(e) = idx.iter().find(|e| Path::new(&e.path) == &p) {
                            matched_title = Some(e.title.clone());
                        }
                    }
                } else {
                    // フォールバック: インデックスベースの解決
                    let idx = load_or_build_gretil_index();
                    if let Some(p) = daizo_core::path_resolver::resolve_gretil_by_id(&idx, id_str) {
                        matched_id = Path::new(&p)
                            .file_stem()
                            .map(|s| s.to_string_lossy().into_owned());
                        if let Some(e) = idx.iter().find(|e| e.path == p.to_string_lossy()) {
                            matched_title = Some(e.title.clone());
                        }
                        path = p;
                    }
                }
            } else if let Some(q) = args.get("query").and_then(|v| v.as_str()) {
                let idx = load_or_build_gretil_index();
                if let Some(hit) = best_match_gretil(idx, q, 1).into_iter().next() {
                    matched_title = Some(hit.entry.title.clone());
                    matched_score = Some(hit.score);
                    matched_id = Path::new(&hit.entry.path)
                        .file_stem()
                        .map(|s| s.to_string_lossy().into_owned());
                    path = PathBuf::from(&hit.entry.path);
                }
            }
            if path.as_os_str().is_empty() {
                return json!({"jsonrpc":"2.0","id": id, "result": { "content": [{"type":"text","text": "not found"}] }});
            }
            let xml = fs::read_to_string(&path).unwrap_or_default();
            let include_notes = args
                .get("includeNotes")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            let (text, extraction_method) =
                if let Some(line_num) = args.get("lineNumber").and_then(|v| v.as_u64()) {
                    let before = args
                        .get("contextBefore")
                        .and_then(|v| v.as_u64())
                        .unwrap_or(
                            args.get("contextLines")
                                .and_then(|v| v.as_u64())
                                .unwrap_or(10),
                        ) as usize;
                    let after = args.get("contextAfter").and_then(|v| v.as_u64()).unwrap_or(
                        args.get("contextLines")
                            .and_then(|v| v.as_u64())
                            .unwrap_or(100),
                    ) as usize;
                    let context_text = daizo_core::extract_xml_around_line_asymmetric(
                        &xml,
                        line_num as usize,
                        before,
                        after,
                    );
                    (
                        context_text,
                        format!("line-context-{}-{}-{}", line_num, before, after),
                    )
                } else if let Some(hq) = args.get("headQuery").and_then(|v| v.as_str()) {
                    (
                        extract_section_by_head(&xml, None, Some(hq), include_notes)
                            .unwrap_or_else(|| extract_text_opts(&xml, include_notes)),
                        "head-query".to_string(),
                    )
                } else if let Some(hi) = args.get("headIndex").and_then(|v| v.as_u64()) {
                    (
                        extract_section_by_head(&xml, Some(hi as usize), None, include_notes)
                            .unwrap_or_else(|| extract_text_opts(&xml, include_notes)),
                        "head-index".to_string(),
                    )
                } else {
                    (extract_text_opts(&xml, include_notes), "full".to_string())
                };
            let full_flag = args.get("full").and_then(|v| v.as_bool()).unwrap_or(false);
            let mut sliced = if full_flag {
                text.clone()
            } else {
                slice_text(&text, &args)
            };
            let mut highlight_count = 0usize;
            let mut highlight_positions: Vec<serde_json::Value> = Vec::new();
            if let Some(hpat) = args.get("highlight").and_then(|v| v.as_str()) {
                let use_re = args
                    .get("highlightRegex")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                let hpre = args
                    .get("highlightPrefix")
                    .and_then(|v| v.as_str())
                    .unwrap_or(">>> ");
                let hsuf = args
                    .get("highlightSuffix")
                    .and_then(|v| v.as_str())
                    .unwrap_or(" <<<");
                let original = sliced.clone();
                if use_re {
                    if let Ok(re) = regex::Regex::new(hpat) {
                        for m in re.find_iter(&original) {
                            let sb = m.start();
                            let eb = m.end();
                            let sc = original[..sb].chars().count();
                            let ec = sc + original[sb..eb].chars().count();
                            highlight_positions.push(json!({"startChar": sc, "endChar": ec}));
                        }
                        let mut count = 0usize;
                        let replaced = re.replace_all(&sliced, |caps: &regex::Captures| {
                            count += 1;
                            format!("{}{}{}", hpre, &caps[0], hsuf)
                        });
                        sliced = replaced.into_owned();
                        highlight_count = count;
                    }
                } else if !hpat.is_empty() {
                    let mut i = 0usize;
                    while let Some(pos) = original[i..].find(hpat) {
                        let abs = i + pos;
                        let sc = original[..abs].chars().count();
                        let ec = sc + hpat.chars().count();
                        highlight_positions.push(json!({"startChar": sc, "endChar": ec}));
                        i = abs + hpat.len();
                    }
                    let mut out = String::with_capacity(sliced.len());
                    let mut j = 0usize;
                    while let Some(pos) = sliced[j..].find(hpat) {
                        let abs = j + pos;
                        out.push_str(&sliced[j..abs]);
                        out.push_str(hpre);
                        out.push_str(hpat);
                        out.push_str(hsuf);
                        j = abs + hpat.len();
                        highlight_count += 1;
                    }
                    out.push_str(&sliced[j..]);
                    sliced = out;
                }
            }
            // cap output
            let cap = default_max_chars();
            if sliced.chars().count() > cap {
                sliced = sliced.chars().take(cap).collect();
            }
            let heads = list_heads_generic(&xml);
            let hl = args
                .get("headingsLimit")
                .and_then(|v| v.as_u64())
                .unwrap_or(10) as usize;
            let meta = json!({
                "totalLength": text.len(),
                "returnedStart": args.get("startChar").and_then(|v| v.as_u64()).unwrap_or( args.get("page").and_then(|v| v.as_u64()).and_then(|p| args.get("pageSize").and_then(|s| s.as_u64()).map(|ps| p*ps)).unwrap_or(0) ),
                "returnedEnd": args.get("startChar").and_then(|v| v.as_u64()).unwrap_or( args.get("page").and_then(|v| v.as_u64()).and_then(|p| args.get("pageSize").and_then(|s| s.as_u64()).map(|ps| p*ps)).unwrap_or(0) ) + (sliced.len() as u64),
                "truncated": if full_flag { false } else { (sliced.len() as u64) < (text.len() as u64) },
                "sourcePath": path.to_string_lossy(),
                "extractionMethod": extraction_method,
                "headingsTotal": heads.len(),
                "headingsPreview": heads.into_iter().take(hl).collect::<Vec<_>>(),
                "matchedId": matched_id,
                "matchedTitle": matched_title,
                "matchedScore": matched_score,
                "highlighted": if highlight_count > 0 { Some(highlight_count) } else { None::<usize> },
                "highlightPositions": if highlight_positions.is_empty() { None::<Vec<serde_json::Value>> } else { Some(highlight_positions) },
            });
            return json!({"jsonrpc":"2.0","id": id, "result": { "content": [{"type":"text","text": sliced}], "_meta": meta }});
        }
        "gretil_search" => {
            let q_raw = args.get("query").and_then(|v| v.as_str()).unwrap_or("");
            let looks_like_regex = q_raw.chars().any(|c| ".+*?[](){}|\\".contains(c));
            let q = if q_raw.chars().any(|c| c.is_whitespace()) && !looks_like_regex {
                to_whitespace_fuzzy_literal(q_raw)
            } else {
                q_raw.to_string()
            };
            let max_results = args
                .get("maxResults")
                .and_then(|v| v.as_u64())
                .unwrap_or(20) as usize;
            let max_matches_per_file = args
                .get("maxMatchesPerFile")
                .and_then(|v| v.as_u64())
                .unwrap_or(5) as usize;
            let results = gretil_grep(&gretil_root(), &q, max_results, max_matches_per_file);
            let mut summary = format!(
                "Found {} files with matches for '{}':\n\n",
                results.len(),
                q
            );
            for (i, result) in results.iter().enumerate() {
                summary.push_str(&format!(
                    "{}. {} ({})\n",
                    i + 1,
                    result.title,
                    result.file_id
                ));
                summary.push_str(&format!(
                    "   {} matches, {}\n",
                    result.total_matches,
                    result
                        .fetch_hints
                        .total_content_size
                        .as_deref()
                        .unwrap_or("unknown size")
                ));
                for (j, m) in result.matches.iter().enumerate().take(2) {
                    summary.push_str(&format!(
                        "   Match {}: ...{}...\n",
                        j + 1,
                        m.context.chars().take(100).collect::<String>()
                    ));
                }
                if result.matches.len() > 2 {
                    summary.push_str(&format!(
                        "   ... and {} more matches\n",
                        result.matches.len() - 2
                    ));
                }
                summary.push('\n');
            }
            // Lightweight next-call hints (low token) for GRETIL
            let hint_top = std::env::var("DAIZO_HINT_TOP")
                .ok()
                .and_then(|s| s.parse::<usize>().ok())
                .unwrap_or(1);
            let hl_regex = looks_like_regex || (q != q_raw);
            let mut fetch_suggestions: Vec<serde_json::Value> = Vec::new();
            for r in results.iter().take(hint_top) {
                if let Some(m) = r.matches.first() {
                    if let Some(ln) = m.line_number {
                        fetch_suggestions.push(json!({
                        "tool": "gretil_fetch",
                        "args": {"id": r.file_id, "lineNumber": ln, "contextBefore": 1, "contextAfter": 3, "highlight": q, "highlightRegex": hl_regex},
                        "mode": "low-cost"
                    }));
                    }
                }
            }
            let mut meta = json!({
                "searchPattern": q,
                "totalFiles": results.len(),
                "results": results,
                "hint": "Use gretil_fetch (id + lineNumber) for low-cost context; gretil_pipeline with autoFetch=false to summarize",
                "fetchSuggestions": fetch_suggestions
            });
            meta["pipelineHint"] = json!({
                "tool": "gretil_pipeline",
                "args": {"query": q, "autoFetch": false, "maxResults": 5, "maxMatchesPerFile": 1, "includeMatchLine": true }
            });
            return json!({"jsonrpc":"2.0","id": id, "result": { "content": [{"type":"text","text": summary}], "_meta": meta }});
        }
        "gretil_pipeline" => {
            let q_raw = args.get("query").and_then(|v| v.as_str()).unwrap_or("");
            let looks_like_regex = q_raw.chars().any(|c| ".+*?[](){}|\\".contains(c));
            let q = if q_raw.chars().any(|c| c.is_whitespace()) && !looks_like_regex {
                to_whitespace_fuzzy_literal(q_raw)
            } else {
                q_raw.to_string()
            };
            let context_before = args
                .get("contextBefore")
                .and_then(|v| v.as_u64())
                .unwrap_or(10) as usize;
            let context_after = args
                .get("contextAfter")
                .and_then(|v| v.as_u64())
                .unwrap_or(100) as usize;
            let max_results = args
                .get("maxResults")
                .and_then(|v| v.as_u64())
                .unwrap_or(10) as usize;
            let max_matches_per_file = args
                .get("maxMatchesPerFile")
                .and_then(|v| v.as_u64())
                .unwrap_or(3) as usize;
            let include_match_line = args
                .get("includeMatchLine")
                .and_then(|v| v.as_bool())
                .unwrap_or(true);
            let results = gretil_grep(&gretil_root(), &q, max_results, max_matches_per_file);
            let mut content_items: Vec<serde_json::Value> = Vec::new();
            let mut meta =
                json!({ "searchPattern": q, "totalFiles": results.len(), "results": results });
            let summary = format!("Found {} files with matches for '{}'", results.len(), q);
            content_items.push(json!({"type":"text","text": summary}));
            let force_no_auto = std::env::var("DAIZO_FORCE_NO_AUTO").ok().as_deref() == Some("1");
            let mut auto_fetch = args
                .get("autoFetch")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            if force_no_auto && auto_fetch {
                auto_fetch = false;
                meta["autoFetchOverridden"] = json!(true);
            }
            if auto_fetch {
                let full = args.get("full").and_then(|v| v.as_bool()).unwrap_or(false);
                let include_notes = args
                    .get("includeNotes")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                let tf = args
                    .get("autoFetchFiles")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(1) as usize;
                let tf = tf.min(results.len());
                let mut fetched: Vec<serde_json::Value> = Vec::new();
                let hl_pre = args
                    .get("highlightPrefix")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string())
                    .or_else(|| std::env::var("DAIZO_HL_PREFIX").ok())
                    .unwrap_or_else(|| ">>> ".to_string());
                let hl_suf = args
                    .get("highlightSuffix")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string())
                    .or_else(|| std::env::var("DAIZO_HL_SUFFIX").ok())
                    .unwrap_or_else(|| " <<<".to_string());
                let sn_pre = args
                    .get("snippetPrefix")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string())
                    .or_else(|| std::env::var("DAIZO_SNIPPET_PREFIX").ok())
                    .unwrap_or_else(|| ">>> ".to_string());
                let sn_suf = args
                    .get("snippetSuffix")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string())
                    .or_else(|| std::env::var("DAIZO_SNIPPET_SUFFIX").ok())
                    .unwrap_or_else(|| "".to_string());
                let mut file_highlights_all: Vec<Vec<serde_json::Value>> = Vec::new();
                for r in results.iter().take(tf) {
                    let per_file_limit = args
                        .get("autoFetchMatches")
                        .and_then(|v| v.as_u64())
                        .unwrap_or(max_matches_per_file as u64)
                        as usize;
                    let xml = fs::read_to_string(&r.file_path).unwrap_or_default();
                    if full {
                        let text = extract_text_opts(&xml, include_notes);
                        content_items.push(json!({"type":"text","text": text}));
                        fetched.push(json!({"id": r.file_id, "full": true}));
                    } else {
                        let mut combined = String::new();
                        let mut highlight_counts: Vec<usize> = Vec::new();
                        let mut file_highlights: Vec<serde_json::Value> = Vec::new();
                        for m in r.matches.iter().take(per_file_limit) {
                            if let Some(ln) = m.line_number {
                                let mut ctx = daizo_core::extract_xml_around_line_asymmetric(
                                    &xml,
                                    ln,
                                    context_before,
                                    context_after,
                                );
                                let mut chigh: Vec<serde_json::Value> = Vec::new();
                                if let Some(pat) = args.get("highlight").and_then(|v| v.as_str()) {
                                    let looks_like =
                                        pat.chars().any(|c| ".+*?[](){}|\\".contains(c));
                                    let mut hlr = args
                                        .get("highlightRegex")
                                        .and_then(|v| v.as_bool())
                                        .unwrap_or(false);
                                    let pat = if pat.chars().any(|c| c.is_whitespace())
                                        && !looks_like
                                        && !hlr
                                    {
                                        hlr = true;
                                        to_whitespace_fuzzy_literal(pat)
                                    } else {
                                        pat.to_string()
                                    };
                                    if hlr {
                                        if let Ok(re) = regex::Regex::new(&pat) {
                                            for mm in re.find_iter(&ctx) {
                                                let sb = mm.start();
                                                let eb = mm.end();
                                                let sc = ctx[..sb].chars().count();
                                                let ec = sc + ctx[sb..eb].chars().count();
                                                chigh.push(json!({"startChar": sc, "endChar": ec}));
                                            }
                                            let mut ct = 0usize;
                                            let rep =
                                                re.replace_all(&ctx, |caps: &regex::Captures| {
                                                    ct += 1;
                                                    format!("{}{}{}", hl_pre, &caps[0], hl_suf)
                                                });
                                            ctx = rep.into_owned();
                                            highlight_counts.push(ct);
                                        }
                                    } else if !pat.is_empty() {
                                        let mut i = 0usize;
                                        while let Some(pos) = ctx[i..].find(&pat) {
                                            let abs = i + pos;
                                            let sc = ctx[..abs].chars().count();
                                            let ec = sc + pat.chars().count();
                                            chigh.push(json!({"startChar": sc, "endChar": ec}));
                                            i = abs + pat.len();
                                        }
                                        let mut out = String::with_capacity(ctx.len());
                                        let mut j = 0usize;
                                        let mut ct = 0usize;
                                        while let Some(pos) = ctx[j..].find(&pat) {
                                            let abs = j + pos;
                                            out.push_str(&ctx[j..abs]);
                                            out.push_str(&hl_pre);
                                            out.push_str(&pat);
                                            out.push_str(&hl_suf);
                                            j = abs + pat.len();
                                            ct += 1;
                                        }
                                        out.push_str(&ctx[j..]);
                                        ctx = out;
                                        highlight_counts.push(ct);
                                    }
                                }
                                if args
                                    .get("includeHighlightSnippet")
                                    .and_then(|v| v.as_bool())
                                    .unwrap_or(true)
                                {
                                    let min_len = args
                                        .get("minSnippetLen")
                                        .and_then(|v| v.as_u64())
                                        .unwrap_or(0)
                                        as usize;
                                    let snip = ctx
                                        .chars()
                                        .take(std::cmp::max(min_len, 120))
                                        .collect::<String>();
                                    combined.push_str(&format!("{}{}{}\n", &sn_pre, snip, &sn_suf));
                                } else {
                                    combined.push_str(&format!(
                                        "# {}{}\n\n{}",
                                        r.file_id,
                                        if include_match_line {
                                            format!(" (line {})", ln)
                                        } else {
                                            String::new()
                                        },
                                        ctx
                                    ));
                                }
                                file_highlights.push(json!(chigh));
                            }
                        }
                        if !combined.is_empty() {
                            content_items.push(json!({"type":"text","text": combined}));
                            let mut fobj = json!({"id": r.file_id, "full": false, "contextBefore": context_before, "contextAfter": context_after, "includeMatchLine": include_match_line});
                            if highlight_counts.iter().any(|&c| c > 0) {
                                fobj["highlightCounts"] = json!(highlight_counts);
                            }
                            fobj["highlightPositions"] = json!(file_highlights);
                            fetched.push(fobj);
                        }
                        file_highlights_all.push(file_highlights);
                    }
                }
                if !fetched.is_empty() {
                    meta["autoFetched"] = json!(fetched);
                }
            }
            // Compatibility: inline all text items so autoFetch is visible in clients that only show the first item.
            if content_items.len() > 1 {
                let mut joined = String::new();
                for item in content_items.iter() {
                    if let Some(t) = item.get("text").and_then(|v| v.as_str()) {
                        if !joined.is_empty() {
                            joined.push_str("\n\n");
                        }
                        joined.push_str(t);
                    }
                }
                content_items = vec![json!({"type":"text","text": joined})];
            }
            return json!({"jsonrpc":"2.0","id": id, "result": { "content": content_items, "_meta": meta }});
        }
        "sarit_title_search" => {
            let q = args
                .get("query")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .trim()
                .to_string();
            let limit = args.get("limit").and_then(|v| v.as_u64()).unwrap_or(10) as usize;
            let idx = load_or_build_sarit_index();
            let hits = best_match_sarit(idx, &q, limit);
            let summary = hits
                .iter()
                .enumerate()
                .map(|(i, h)| format!("{}. {}  {}", i + 1, h.entry.id, h.entry.title))
                .collect::<Vec<_>>()
                .join("\n");
            let results: Vec<_> = hits
                .iter()
                .map(|h| {
                    json!({
                        "id": h.entry.id,
                        "title": h.entry.title,
                        "path": h.entry.path,
                        "score": h.score,
                        "meta": h.entry.meta
                    })
                })
                .collect();
            let meta = json!({ "count": results.len(), "results": results });
            return json!({"jsonrpc":"2.0","id": id, "result": { "content": [{"type":"text","text": summary }], "_meta": meta }});
        }
        "sarit_fetch" => {
            ensure_sarit_data();
            let mut matched_id: Option<String> = None;
            let mut matched_title: Option<String> = None;
            let mut matched_score: Option<f32> = None;
            let mut path: PathBuf = PathBuf::new();

            if let Some(id_str) = args.get("id").and_then(|v| v.as_str()) {
                if let Some(p) = resolve_sarit_path_direct(id_str) {
                    path = p.clone();
                    matched_id = Some(id_str.to_string());
                    if let Some(idx) = SARIT_INDEX_CACHE.get() {
                        if let Some(e) = idx.iter().find(|e| Path::new(&e.path) == &p) {
                            matched_title = Some(e.title.clone());
                        }
                    }
                } else {
                    let idx = load_or_build_sarit_index();
                    if let Some(p) = resolve_sarit_by_id(&idx, id_str) {
                        matched_id = Some(
                            Path::new(&p)
                                .file_stem()
                                .map(|s| s.to_string_lossy().into_owned())
                                .unwrap_or_else(|| id_str.to_string()),
                        );
                        if let Some(e) = idx.iter().find(|e| Path::new(&e.path) == &p) {
                            matched_title = Some(e.title.clone());
                        }
                        path = p;
                    }
                }
            } else if let Some(q) = args.get("query").and_then(|v| v.as_str()) {
                let idx = load_or_build_sarit_index();
                if let Some(hit) = best_match_sarit(idx, q, 1).into_iter().next() {
                    matched_title = Some(hit.entry.title.clone());
                    matched_score = Some(hit.score);
                    matched_id = Some(hit.entry.id.clone());
                    path = PathBuf::from(&hit.entry.path);
                }
            }

            if path.as_os_str().is_empty() {
                return json!({"jsonrpc":"2.0","id": id, "result": { "content": [{"type":"text","text": "not found"}] }});
            }

            let xml = fs::read_to_string(&path).unwrap_or_default();
            let include_notes = args
                .get("includeNotes")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            let (text, extraction_method) =
                if let Some(line_num) = args.get("lineNumber").and_then(|v| v.as_u64()) {
                    let before = args
                        .get("contextBefore")
                        .and_then(|v| v.as_u64())
                        .unwrap_or(
                            args.get("contextLines")
                                .and_then(|v| v.as_u64())
                                .unwrap_or(10),
                        ) as usize;
                    let after = args.get("contextAfter").and_then(|v| v.as_u64()).unwrap_or(
                        args.get("contextLines")
                            .and_then(|v| v.as_u64())
                            .unwrap_or(100),
                    ) as usize;
                    let context_text = daizo_core::extract_xml_around_line_asymmetric(
                        &xml,
                        line_num as usize,
                        before,
                        after,
                    );
                    (
                        context_text,
                        format!("line-context-{}-{}-{}", line_num, before, after),
                    )
                } else if let Some(hq) = args.get("headQuery").and_then(|v| v.as_str()) {
                    (
                        extract_section_by_head(&xml, None, Some(hq), include_notes)
                            .unwrap_or_else(|| extract_text_opts(&xml, include_notes)),
                        "head-query".to_string(),
                    )
                } else if let Some(hi) = args.get("headIndex").and_then(|v| v.as_u64()) {
                    (
                        extract_section_by_head(&xml, Some(hi as usize), None, include_notes)
                            .unwrap_or_else(|| extract_text_opts(&xml, include_notes)),
                        "head-index".to_string(),
                    )
                } else {
                    (extract_text_opts(&xml, include_notes), "full".to_string())
                };

            let full_flag = args.get("full").and_then(|v| v.as_bool()).unwrap_or(false);
            let mut sliced = if full_flag {
                text.clone()
            } else {
                slice_text(&text, &args)
            };

            let mut highlight_count = 0usize;
            let mut highlight_positions: Vec<serde_json::Value> = Vec::new();
            if let Some(hpat) = args.get("highlight").and_then(|v| v.as_str()) {
                let use_re = args
                    .get("highlightRegex")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                let hpre = args
                    .get("highlightPrefix")
                    .and_then(|v| v.as_str())
                    .unwrap_or(">>> ");
                let hsuf = args
                    .get("highlightSuffix")
                    .and_then(|v| v.as_str())
                    .unwrap_or(" <<<");
                let original = sliced.clone();
                if use_re {
                    if let Ok(re) = regex::Regex::new(hpat) {
                        for m in re.find_iter(&original) {
                            let sb = m.start();
                            let eb = m.end();
                            let sc = original[..sb].chars().count();
                            let ec = sc + original[sb..eb].chars().count();
                            highlight_positions.push(json!({"startChar": sc, "endChar": ec}));
                        }
                        let rep = re.replace_all(&sliced, |caps: &regex::Captures| {
                            highlight_count += 1;
                            format!("{}{}{}", hpre, &caps[0], hsuf)
                        });
                        sliced = rep.into_owned();
                    }
                } else if !hpat.is_empty() {
                    let (decorated, count, positions) =
                        daizo_core::text_utils::highlight_text(&sliced, hpat, false, hpre, hsuf);
                    sliced = decorated;
                    highlight_count = count;
                    highlight_positions = positions
                        .into_iter()
                        .map(|p| json!({"startChar": p.start_char, "endChar": p.end_char}))
                        .collect();
                }
            }

            let heads = list_heads_generic(&xml);
            let headings_limit = args
                .get("headingsLimit")
                .and_then(|v| v.as_u64())
                .unwrap_or(20) as usize;
            let meta = json!({
                "totalLength": text.chars().count(),
                "returnedStart": args.get("startChar").and_then(|v| v.as_u64()).unwrap_or(0),
                "returnedEnd": args.get("endChar").and_then(|v| v.as_u64()).unwrap_or(sliced.chars().count() as u64),
                "sourcePath": path.to_string_lossy(),
                "extractionMethod": extraction_method,
                "headingsTotal": heads.len(),
                "headingsPreview": heads.into_iter().take(headings_limit).collect::<Vec<_>>(),
                "matchedId": matched_id,
                "matchedTitle": matched_title,
                "matchedScore": matched_score,
                "highlighted": if highlight_count > 0 { Some(highlight_count) } else { None::<usize> },
                "highlightPositions": if !highlight_positions.is_empty() { Some(highlight_positions) } else { None::<Vec<serde_json::Value>> },
            });
            return json!({"jsonrpc":"2.0","id": id, "result": { "content": [{"type":"text","text": sliced}], "_meta": meta }});
        }
        "sarit_search" => {
            let q_raw = args.get("query").and_then(|v| v.as_str()).unwrap_or("");
            let looks_like_regex = q_raw.chars().any(|c| ".+*?[](){}|\\".contains(c));
            let q = if q_raw.chars().any(|c| c.is_whitespace()) && !looks_like_regex {
                to_whitespace_fuzzy_literal(q_raw)
            } else {
                q_raw.to_string()
            };
            let max_results = args
                .get("maxResults")
                .and_then(|v| v.as_u64())
                .unwrap_or(20) as usize;
            let max_matches_per_file = args
                .get("maxMatchesPerFile")
                .and_then(|v| v.as_u64())
                .unwrap_or(5) as usize;

            ensure_sarit_data();
            let results = sarit_grep(&sarit_root(), &q, max_results, max_matches_per_file);

            let mut summary = format!(
                "Found {} files with matches for '{}':\n\n",
                results.len(),
                q_raw
            );
            for (i, result) in results.iter().enumerate() {
                summary.push_str(&format!(
                    "{}. {} ({})\n",
                    i + 1,
                    result.title,
                    result.file_id
                ));
                summary.push_str(&format!(
                    "   {} matches, {}\n",
                    result.total_matches,
                    result
                        .fetch_hints
                        .total_content_size
                        .as_deref()
                        .unwrap_or("unknown size")
                ));
                for (j, m) in result.matches.iter().enumerate().take(2) {
                    summary.push_str(&format!(
                        "   Match {}: ...{}...\n",
                        j + 1,
                        m.context.chars().take(100).collect::<String>()
                    ));
                }
                if result.matches.len() > 2 {
                    summary.push_str(&format!(
                        "   ... and {} more matches\n",
                        result.matches.len() - 2
                    ));
                }
                summary.push('\n');
            }

            // lightweight next-call hints
            let hint_top = std::env::var("DAIZO_MCP_SUGGEST_TOP")
                .ok()
                .and_then(|s| s.parse::<usize>().ok())
                .unwrap_or(1);
            let hl_regex = looks_like_regex || (q != q_raw);
            let mut fetch_suggestions: Vec<serde_json::Value> = Vec::new();
            for r in results.iter().take(hint_top) {
                if let Some(m) = r.matches.first() {
                    if let Some(ln) = m.line_number {
                        fetch_suggestions.push(json!({
                            "tool": "sarit_fetch",
                            "args": {"id": r.file_id, "lineNumber": ln, "contextBefore": 1, "contextAfter": 3, "highlight": q, "highlightRegex": hl_regex},
                            "mode": "low-cost"
                        }));
                    }
                }
            }
            let mut meta = json!({
                "searchPattern": q,
                "totalFiles": results.len(),
                "results": results,
                "hint": "Use sarit_fetch (id + lineNumber) for low-cost context; sarit_pipeline with autoFetch=false to summarize",
                "fetchSuggestions": fetch_suggestions
            });
            meta["pipelineHint"] = json!({
                "tool": "sarit_pipeline",
                "args": {"query": q, "autoFetch": false, "maxResults": 5, "maxMatchesPerFile": 1, "includeMatchLine": true }
            });
            return json!({"jsonrpc":"2.0","id": id, "result": { "content": [{"type":"text","text": summary}], "_meta": meta }});
        }
        "sarit_pipeline" => {
            let q_raw = args.get("query").and_then(|v| v.as_str()).unwrap_or("");
            let looks_like_regex = q_raw.chars().any(|c| ".+*?[](){}|\\".contains(c));
            let q = if q_raw.chars().any(|c| c.is_whitespace()) && !looks_like_regex {
                to_whitespace_fuzzy_literal(q_raw)
            } else {
                q_raw.to_string()
            };
            let context_before = args
                .get("contextBefore")
                .and_then(|v| v.as_u64())
                .unwrap_or(10) as usize;
            let context_after = args
                .get("contextAfter")
                .and_then(|v| v.as_u64())
                .unwrap_or(100) as usize;
            let max_results = args
                .get("maxResults")
                .and_then(|v| v.as_u64())
                .unwrap_or(10) as usize;
            let max_matches_per_file = args
                .get("maxMatchesPerFile")
                .and_then(|v| v.as_u64())
                .unwrap_or(3) as usize;
            let include_match_line = args
                .get("includeMatchLine")
                .and_then(|v| v.as_bool())
                .unwrap_or(true);
            ensure_sarit_data();
            let results = sarit_grep(&sarit_root(), &q, max_results, max_matches_per_file);
            let mut content_items: Vec<serde_json::Value> = Vec::new();
            let mut meta =
                json!({ "searchPattern": q, "totalFiles": results.len(), "results": results });
            let summary = format!("Found {} files with matches for '{}'", results.len(), q);
            content_items.push(json!({"type":"text","text": summary}));
            let force_no_auto = std::env::var("DAIZO_FORCE_NO_AUTO").ok().as_deref() == Some("1");
            let mut auto_fetch = args
                .get("autoFetch")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            if force_no_auto && auto_fetch {
                auto_fetch = false;
                meta["autoFetchOverridden"] = json!(true);
            }
            if auto_fetch {
                let full = args.get("full").and_then(|v| v.as_bool()).unwrap_or(false);
                let include_notes = args
                    .get("includeNotes")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                let tf = args
                    .get("autoFetchFiles")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(1) as usize;
                let tf = tf.min(results.len());
                let mut fetched: Vec<serde_json::Value> = Vec::new();
                let hl_pre = args
                    .get("highlightPrefix")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string())
                    .or_else(|| std::env::var("DAIZO_HL_PREFIX").ok())
                    .unwrap_or_else(|| ">>> ".to_string());
                let hl_suf = args
                    .get("highlightSuffix")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string())
                    .or_else(|| std::env::var("DAIZO_HL_SUFFIX").ok())
                    .unwrap_or_else(|| " <<<".to_string());
                let sn_pre = args
                    .get("snippetPrefix")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string())
                    .or_else(|| std::env::var("DAIZO_SNIPPET_PREFIX").ok())
                    .unwrap_or_else(|| ">>> ".to_string());
                let sn_suf = args
                    .get("snippetSuffix")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string())
                    .or_else(|| std::env::var("DAIZO_SNIPPET_SUFFIX").ok())
                    .unwrap_or_else(|| "".to_string());
                for r in results.iter().take(tf) {
                    let per_file_limit = args
                        .get("autoFetchMatches")
                        .and_then(|v| v.as_u64())
                        .unwrap_or(max_matches_per_file as u64)
                        as usize;
                    let xml = fs::read_to_string(&r.file_path).unwrap_or_default();
                    if full {
                        let text = extract_text_opts(&xml, include_notes);
                        content_items.push(json!({"type":"text","text": text}));
                        fetched.push(json!({"id": r.file_id, "full": true}));
                    } else {
                        let mut combined = String::new();
                        let mut highlight_counts: Vec<usize> = Vec::new();
                        let mut file_highlights: Vec<serde_json::Value> = Vec::new();
                        for m in r.matches.iter().take(per_file_limit) {
                            if let Some(ln) = m.line_number {
                                let mut ctx = daizo_core::extract_xml_around_line_asymmetric(
                                    &xml,
                                    ln,
                                    context_before,
                                    context_after,
                                );
                                let mut chigh: Vec<serde_json::Value> = Vec::new();
                                if let Some(pat) = args.get("highlight").and_then(|v| v.as_str()) {
                                    let looks_like =
                                        pat.chars().any(|c| ".+*?[](){}|\\".contains(c));
                                    let mut hlr = args
                                        .get("highlightRegex")
                                        .and_then(|v| v.as_bool())
                                        .unwrap_or(false);
                                    let pat = if pat.chars().any(|c| c.is_whitespace())
                                        && !looks_like
                                        && !hlr
                                    {
                                        hlr = true;
                                        to_whitespace_fuzzy_literal(pat)
                                    } else {
                                        pat.to_string()
                                    };
                                    if hlr {
                                        if let Ok(re) = regex::Regex::new(&pat) {
                                            for mm in re.find_iter(&ctx) {
                                                let sb = mm.start();
                                                let eb = mm.end();
                                                let sc = ctx[..sb].chars().count();
                                                let ec = sc + ctx[sb..eb].chars().count();
                                                chigh.push(json!({"startChar": sc, "endChar": ec}));
                                            }
                                            let mut ct = 0usize;
                                            let rep =
                                                re.replace_all(&ctx, |caps: &regex::Captures| {
                                                    ct += 1;
                                                    format!("{}{}{}", hl_pre, &caps[0], hl_suf)
                                                });
                                            ctx = rep.into_owned();
                                            highlight_counts.push(ct);
                                        }
                                    } else if !pat.is_empty() {
                                        let mut i = 0usize;
                                        while let Some(pos) = ctx[i..].find(&pat) {
                                            let abs = i + pos;
                                            let sc = ctx[..abs].chars().count();
                                            let ec = sc + pat.chars().count();
                                            chigh.push(json!({"startChar": sc, "endChar": ec}));
                                            i = abs + pat.len();
                                        }
                                        let mut out = String::with_capacity(ctx.len());
                                        let mut j = 0usize;
                                        let mut ct = 0usize;
                                        while let Some(pos) = ctx[j..].find(&pat) {
                                            let abs = j + pos;
                                            out.push_str(&ctx[j..abs]);
                                            out.push_str(&hl_pre);
                                            out.push_str(&pat);
                                            out.push_str(&hl_suf);
                                            j = abs + pat.len();
                                            ct += 1;
                                        }
                                        out.push_str(&ctx[j..]);
                                        ctx = out;
                                        highlight_counts.push(ct);
                                    }
                                }
                                if args
                                    .get("includeHighlightSnippet")
                                    .and_then(|v| v.as_bool())
                                    .unwrap_or(true)
                                {
                                    let min_len = args
                                        .get("minSnippetLen")
                                        .and_then(|v| v.as_u64())
                                        .unwrap_or(0)
                                        as usize;
                                    let snip = ctx
                                        .chars()
                                        .take(std::cmp::max(min_len, 120))
                                        .collect::<String>();
                                    combined.push_str(&format!("{}{}{}\n", &sn_pre, snip, &sn_suf));
                                } else {
                                    combined.push_str(&format!(
                                        "# {}{}\n\n{}",
                                        r.file_id,
                                        if include_match_line {
                                            format!(" (line {})", ln)
                                        } else {
                                            String::new()
                                        },
                                        ctx
                                    ));
                                }
                                file_highlights.push(json!(chigh));
                            }
                        }
                        if !combined.is_empty() {
                            content_items.push(json!({"type":"text","text": combined}));
                            let mut fobj = json!({"id": r.file_id, "full": false, "contextBefore": context_before, "contextAfter": context_after, "includeMatchLine": include_match_line});
                            if highlight_counts.iter().any(|&c| c > 0) {
                                fobj["highlightCounts"] = json!(highlight_counts);
                            }
                            fobj["highlightPositions"] = json!(file_highlights);
                            fetched.push(fobj);
                        }
                    }
                }
                if !fetched.is_empty() {
                    meta["autoFetched"] = json!(fetched);
                }
            }
            if content_items.len() > 1 {
                let mut joined = String::new();
                for item in content_items.iter() {
                    if let Some(t) = item.get("text").and_then(|v| v.as_str()) {
                        if !joined.is_empty() {
                            joined.push_str("\n\n");
                        }
                        joined.push_str(t);
                    }
                }
                content_items = vec![json!({"type":"text","text": joined})];
            }
            return json!({"jsonrpc":"2.0","id": id, "result": { "content": content_items, "_meta": meta }});
        }
        "muktabodha_title_search" => {
            ensure_muktabodha_dir();
            let q = args
                .get("query")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .trim()
                .to_string();
            let limit = args.get("limit").and_then(|v| v.as_u64()).unwrap_or(10) as usize;
            let idx = load_or_build_muktabodha_index();
            let hits = best_match_muktabodha(idx, &q, limit);
            let summary = hits
                .iter()
                .enumerate()
                .map(|(i, h)| format!("{}. {}  {}", i + 1, h.entry.id, h.entry.title))
                .collect::<Vec<_>>()
                .join("\n");
            let results: Vec<_> = hits
                .iter()
                .map(|h| {
                    json!({
                        "id": h.entry.id,
                        "title": h.entry.title,
                        "path": h.entry.path,
                        "score": h.score,
                        "meta": h.entry.meta
                    })
                })
                .collect();
            let meta = json!({ "count": results.len(), "results": results });
            return json!({"jsonrpc":"2.0","id": id, "result": { "content": [{"type":"text","text": summary }], "_meta": meta }});
        }
        "muktabodha_fetch" => {
            ensure_muktabodha_dir();
            let mut matched_id: Option<String> = None;
            let mut matched_title: Option<String> = None;
            let mut matched_score: Option<f32> = None;
            let mut path: PathBuf = PathBuf::new();

            if let Some(id_str) = args.get("id").and_then(|v| v.as_str()) {
                if let Some(p) = resolve_muktabodha_path_direct(id_str) {
                    path = p.clone();
                    matched_id = Some(id_str.to_string());
                    if let Some(idx) = MUKTABODHA_INDEX_CACHE.get() {
                        if let Some(e) = idx.iter().find(|e| Path::new(&e.path) == &p) {
                            matched_title = Some(e.title.clone());
                        }
                    }
                } else {
                    let idx = load_or_build_muktabodha_index();
                    if let Some(p) = resolve_muktabodha_by_id(&idx, id_str) {
                        matched_id = Some(
                            Path::new(&p)
                                .file_stem()
                                .map(|s| s.to_string_lossy().into_owned())
                                .unwrap_or_else(|| id_str.to_string()),
                        );
                        if let Some(e) = idx.iter().find(|e| Path::new(&e.path) == &p) {
                            matched_title = Some(e.title.clone());
                        }
                        path = p;
                    }
                }
            } else if let Some(q) = args.get("query").and_then(|v| v.as_str()) {
                let idx = load_or_build_muktabodha_index();
                if let Some(hit) = best_match_muktabodha(idx, q, 1).into_iter().next() {
                    matched_title = Some(hit.entry.title.clone());
                    matched_score = Some(hit.score);
                    matched_id = Some(hit.entry.id.clone());
                    path = PathBuf::from(&hit.entry.path);
                }
            }

            if path.as_os_str().is_empty() {
                return json!({"jsonrpc":"2.0","id": id, "result": { "content": [{"type":"text","text": "not found"}] }});
            }

            let bytes = fs::read(&path).unwrap_or_default();
            let xml = decode_xml_bytes(&bytes);
            let include_notes = args
                .get("includeNotes")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            let is_xml = path
                .extension()
                .and_then(|s| s.to_str())
                .map(|s| s.eq_ignore_ascii_case("xml"))
                .unwrap_or(false);

            let (text, extraction_method) =
                if let Some(line_num) = args.get("lineNumber").and_then(|v| v.as_u64()) {
                    let before = args
                        .get("contextBefore")
                        .and_then(|v| v.as_u64())
                        .unwrap_or(
                            args.get("contextLines")
                                .and_then(|v| v.as_u64())
                                .unwrap_or(10),
                        ) as usize;
                    let after = args.get("contextAfter").and_then(|v| v.as_u64()).unwrap_or(
                        args.get("contextLines")
                            .and_then(|v| v.as_u64())
                            .unwrap_or(100),
                    ) as usize;
                    let context_text = daizo_core::extract_xml_around_line_asymmetric(
                        &xml,
                        line_num as usize,
                        before,
                        after,
                    );
                    (
                        context_text,
                        format!("line-context-{}-{}-{}", line_num, before, after),
                    )
                } else if is_xml {
                    (
                        extract_text_opts(&xml, include_notes),
                        "full-xml".to_string(),
                    )
                } else {
                    (xml.clone(), "full-txt".to_string())
                };

            let full_flag = args.get("full").and_then(|v| v.as_bool()).unwrap_or(false);
            let mut sliced = if full_flag {
                text.clone()
            } else {
                slice_text(&text, &args)
            };

            let mut highlight_count = 0usize;
            let mut highlight_positions: Vec<serde_json::Value> = Vec::new();
            if let Some(hpat) = args.get("highlight").and_then(|v| v.as_str()) {
                let use_re = args
                    .get("highlightRegex")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                let hpre = args
                    .get("highlightPrefix")
                    .and_then(|v| v.as_str())
                    .unwrap_or(">>> ");
                let hsuf = args
                    .get("highlightSuffix")
                    .and_then(|v| v.as_str())
                    .unwrap_or(" <<<");
                let (decorated, count, positions) =
                    daizo_core::text_utils::highlight_text(&sliced, hpat, use_re, hpre, hsuf);
                sliced = decorated;
                highlight_count = count;
                highlight_positions = positions
                    .into_iter()
                    .map(|p| json!({"startChar": p.start_char, "endChar": p.end_char}))
                    .collect();
            }

            let heads = if is_xml {
                list_heads_generic(&xml)
            } else {
                Vec::new()
            };
            let headings_limit = args
                .get("headingsLimit")
                .and_then(|v| v.as_u64())
                .unwrap_or(20) as usize;
            let meta = json!({
                "totalLength": text.chars().count(),
                "sourcePath": path.to_string_lossy(),
                "extractionMethod": extraction_method,
                "headingsTotal": heads.len(),
                "headingsPreview": heads.into_iter().take(headings_limit).collect::<Vec<_>>(),
                "matchedId": matched_id,
                "matchedTitle": matched_title,
                "matchedScore": matched_score,
                "highlighted": if highlight_count > 0 { Some(highlight_count) } else { None::<usize> },
                "highlightPositions": if !highlight_positions.is_empty() { Some(highlight_positions) } else { None::<Vec<serde_json::Value>> },
            });
            return json!({"jsonrpc":"2.0","id": id, "result": { "content": [{"type":"text","text": sliced}], "_meta": meta }});
        }
        "muktabodha_search" => {
            let q_raw = args.get("query").and_then(|v| v.as_str()).unwrap_or("");
            let looks_like_regex = q_raw.chars().any(|c| ".+*?[](){}|\\".contains(c));
            let q = if q_raw.chars().any(|c| c.is_whitespace()) && !looks_like_regex {
                to_whitespace_fuzzy_literal(q_raw)
            } else {
                q_raw.to_string()
            };
            let max_results = args
                .get("maxResults")
                .and_then(|v| v.as_u64())
                .unwrap_or(20) as usize;
            let max_matches_per_file = args
                .get("maxMatchesPerFile")
                .and_then(|v| v.as_u64())
                .unwrap_or(5) as usize;

            ensure_muktabodha_dir();
            let results =
                muktabodha_grep(&muktabodha_root(), &q, max_results, max_matches_per_file);

            let mut summary = format!(
                "Found {} files with matches for '{}':\n\n",
                results.len(),
                q_raw
            );
            for (i, result) in results.iter().enumerate() {
                summary.push_str(&format!(
                    "{}. {} ({})\n",
                    i + 1,
                    result.title,
                    result.file_id
                ));
                summary.push_str(&format!(
                    "   {} matches, {}\n",
                    result.total_matches,
                    result
                        .fetch_hints
                        .total_content_size
                        .as_deref()
                        .unwrap_or("unknown size")
                ));
                for (j, m) in result.matches.iter().enumerate().take(2) {
                    summary.push_str(&format!(
                        "   Match {}: ...{}...\n",
                        j + 1,
                        m.context.chars().take(100).collect::<String>()
                    ));
                }
                if result.matches.len() > 2 {
                    summary.push_str(&format!(
                        "   ... and {} more matches\n",
                        result.matches.len() - 2
                    ));
                }
                summary.push('\n');
            }
            let hl_regex = looks_like_regex || (q != q_raw);
            let mut fetch_suggestions: Vec<serde_json::Value> = Vec::new();
            if let Some(r) = results.first() {
                if let Some(m) = r.matches.first() {
                    if let Some(ln) = m.line_number {
                        fetch_suggestions.push(json!({
                            "tool": "muktabodha_fetch",
                            "args": {"id": r.file_id, "lineNumber": ln, "contextBefore": 1, "contextAfter": 3, "highlight": q, "highlightRegex": hl_regex},
                            "mode": "low-cost"
                        }));
                    }
                }
            }
            let mut meta = json!({
                "searchPattern": q,
                "totalFiles": results.len(),
                "results": results,
                "hint": "Use muktabodha_fetch (id + lineNumber) for low-cost context; muktabodha_pipeline with autoFetch=false to summarize",
                "fetchSuggestions": fetch_suggestions
            });
            meta["pipelineHint"] = json!({
                "tool": "muktabodha_pipeline",
                "args": {"query": q, "autoFetch": false, "maxResults": 5, "maxMatchesPerFile": 1, "includeMatchLine": true }
            });
            return json!({"jsonrpc":"2.0","id": id, "result": { "content": [{"type":"text","text": summary}], "_meta": meta }});
        }
        "muktabodha_pipeline" => {
            let q_raw = args.get("query").and_then(|v| v.as_str()).unwrap_or("");
            let looks_like_regex = q_raw.chars().any(|c| ".+*?[](){}|\\".contains(c));
            let q = if q_raw.chars().any(|c| c.is_whitespace()) && !looks_like_regex {
                to_whitespace_fuzzy_literal(q_raw)
            } else {
                q_raw.to_string()
            };
            let context_before = args
                .get("contextBefore")
                .and_then(|v| v.as_u64())
                .unwrap_or(10) as usize;
            let context_after = args
                .get("contextAfter")
                .and_then(|v| v.as_u64())
                .unwrap_or(100) as usize;
            let max_results = args
                .get("maxResults")
                .and_then(|v| v.as_u64())
                .unwrap_or(10) as usize;
            let max_matches_per_file = args
                .get("maxMatchesPerFile")
                .and_then(|v| v.as_u64())
                .unwrap_or(3) as usize;
            let include_match_line = args
                .get("includeMatchLine")
                .and_then(|v| v.as_bool())
                .unwrap_or(true);

            ensure_muktabodha_dir();
            let results =
                muktabodha_grep(&muktabodha_root(), &q, max_results, max_matches_per_file);
            let mut content_items: Vec<serde_json::Value> = Vec::new();
            let mut meta =
                json!({ "searchPattern": q, "totalFiles": results.len(), "results": results });
            let summary = format!("Found {} files with matches for '{}'", results.len(), q);
            content_items.push(json!({"type":"text","text": summary}));

            let force_no_auto = std::env::var("DAIZO_FORCE_NO_AUTO").ok().as_deref() == Some("1");
            let mut auto_fetch = args
                .get("autoFetch")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            if force_no_auto && auto_fetch {
                auto_fetch = false;
                meta["autoFetchOverridden"] = json!(true);
            }
            if auto_fetch {
                let full = args.get("full").and_then(|v| v.as_bool()).unwrap_or(false);
                let include_notes = args
                    .get("includeNotes")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                let tf = args
                    .get("autoFetchFiles")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(1) as usize;
                let tf = tf.min(results.len());
                let mut fetched: Vec<serde_json::Value> = Vec::new();
                for r in results.iter().take(tf) {
                    let per_file_limit = args
                        .get("autoFetchMatches")
                        .and_then(|v| v.as_u64())
                        .unwrap_or(max_matches_per_file as u64)
                        as usize;
                    let bytes = fs::read(&r.file_path).unwrap_or_default();
                    let xml = decode_xml_bytes(&bytes);
                    let is_xml = r.file_path.to_lowercase().ends_with(".xml");
                    if full {
                        let text = if is_xml {
                            extract_text_opts(&xml, include_notes)
                        } else {
                            xml.clone()
                        };
                        content_items.push(json!({"type":"text","text": text}));
                        fetched.push(json!({"id": r.file_id, "full": true}));
                    } else {
                        let mut combined = String::new();
                        for m in r.matches.iter().take(per_file_limit) {
                            if let Some(ln) = m.line_number {
                                let ctx = daizo_core::extract_xml_around_line_asymmetric(
                                    &xml,
                                    ln,
                                    context_before,
                                    context_after,
                                );
                                if !combined.is_empty() {
                                    combined.push_str("\n\n---\n\n");
                                }
                                combined.push_str(&format!(
                                    "# {}{}\n\n{}",
                                    r.file_id,
                                    if include_match_line {
                                        format!(" (line {})", ln)
                                    } else {
                                        String::new()
                                    },
                                    ctx
                                ));
                            }
                        }
                        if !combined.is_empty() {
                            content_items.push(json!({"type":"text","text": combined}));
                            fetched.push(json!({"id": r.file_id, "full": false}));
                        }
                    }
                }
                if !fetched.is_empty() {
                    meta["autoFetched"] = json!(fetched);
                }
            }

            if content_items.len() > 1 {
                let mut joined = String::new();
                for item in content_items.iter() {
                    if let Some(t) = item.get("text").and_then(|v| v.as_str()) {
                        if !joined.is_empty() {
                            joined.push_str("\n\n");
                        }
                        joined.push_str(t);
                    }
                }
                content_items = vec![json!({"type":"text","text": joined})];
            }
            return json!({"jsonrpc":"2.0","id": id, "result": { "content": content_items, "_meta": meta }});
        }
        "tipitaka_search" => {
            let q_raw = args.get("query").and_then(|v| v.as_str()).unwrap_or("");
            let looks_like_regex = q_raw.chars().any(|c| ".+*?[](){}|\\".contains(c));
            let q = if q_raw.chars().any(|c| c.is_whitespace()) && !looks_like_regex {
                to_whitespace_fuzzy_literal(q_raw)
            } else {
                q_raw.to_string()
            };
            let max_results = args
                .get("maxResults")
                .and_then(|v| v.as_u64())
                .unwrap_or(20) as usize;
            let max_matches_per_file = args
                .get("maxMatchesPerFile")
                .and_then(|v| v.as_u64())
                .unwrap_or(5) as usize;

            ensure_tipitaka_data();
            let results = tipitaka_grep(&tipitaka_root(), &q, max_results, max_matches_per_file);

            let mut summary = format!(
                "Found {} files with matches for '{}':\n\n",
                results.len(),
                q
            );
            for (i, result) in results.iter().enumerate() {
                summary.push_str(&format!(
                    "{}. {} ({})\n",
                    i + 1,
                    result.title,
                    result.file_id
                ));
                summary.push_str(&format!(
                    "   {} matches, {}\n",
                    result.total_matches,
                    result
                        .fetch_hints
                        .total_content_size
                        .as_deref()
                        .unwrap_or("unknown size")
                ));

                for (j, m) in result.matches.iter().enumerate().take(2) {
                    summary.push_str(&format!(
                        "   Match {}: ...{}...\n",
                        j + 1,
                        m.context.chars().take(100).collect::<String>()
                    ));
                }
                if result.matches.len() > 2 {
                    summary.push_str(&format!(
                        "   ... and {} more matches\n",
                        result.matches.len() - 2
                    ));
                }

                if !result.fetch_hints.structure_info.is_empty() {
                    summary.push_str(&format!(
                        "   Structure: {}\n",
                        result.fetch_hints.structure_info.join(", ")
                    ));
                }
                summary.push('\n');
            }
            // Lightweight next-call hints for Tipitaka (no pipeline tool)
            let hint_top = std::env::var("DAIZO_HINT_TOP")
                .ok()
                .and_then(|s| s.parse::<usize>().ok())
                .unwrap_or(1);
            let hl_regex = looks_like_regex || (q != q_raw);
            let mut fetch_suggestions: Vec<serde_json::Value> = Vec::new();
            for r in results.iter().take(hint_top) {
                if let Some(m) = r.matches.first() {
                    if let Some(ln) = m.line_number {
                        fetch_suggestions.push(json!({
                        "tool": "tipitaka_fetch",
                        "args": {"id": r.file_id, "lineNumber": ln, "contextBefore": 1, "contextAfter": 3, "highlight": q, "highlightRegex": hl_regex},
                        "mode": "low-cost"
                    }));
                    }
                }
            }
            let meta = json!({
                "searchPattern": q,
                "totalFiles": results.len(),
                "results": results,
                "hint": "Use tipitaka_fetch (id + lineNumber) for low-cost context",
                "fetchSuggestions": fetch_suggestions
            });

            return json!({"jsonrpc":"2.0","id": id, "result": { "content": [{"type":"text","text": summary}], "_meta": meta }});
        }
        _ => format!("unknown tool: {}", name),
    };
    json!({
        "jsonrpc":"2.0",
        "id": id,
        "result": { "content": [{"type":"text","text": content_text }] }
    })
}

// find_tipitaka_content_for_base and find_exact_file_by_name moved to daizo_core::path_resolver

fn cache_path_for(url: &str) -> PathBuf {
    let mut hasher = Sha1::new();
    hasher.update(url.as_bytes());
    let h = hasher.finalize();
    let fname = format!("{:x}.txt", h);
    let dir = cache_dir().join("sat");
    ensure_dir(&dir);
    dir.join(fname)
}

fn decode_xml_bytes(bytes: &[u8]) -> String {
    // BOM-based detection first
    if bytes.len() >= 3 && bytes[..3] == [0xEF, 0xBB, 0xBF] {
        return String::from_utf8_lossy(&bytes[3..]).to_string();
    }
    if bytes.len() >= 2 && bytes[..2] == [0xFE, 0xFF] {
        let (cow, _, _) = encoding_rs::UTF_16BE.decode(bytes);
        return cow.into_owned();
    }
    if bytes.len() >= 2 && bytes[..2] == [0xFF, 0xFE] {
        let (cow, _, _) = encoding_rs::UTF_16LE.decode(bytes);
        return cow.into_owned();
    }
    // UTF-32 is rare for XML and not supported by encoding_rs helpers; omit.
    // XML declaration sniffing
    let sniff_len = std::cmp::min(512, bytes.len());
    let head = &bytes[..sniff_len];
    if let Some(enc) = sniff_xml_encoding(head) {
        if let Some(encod) = Encoding::for_label(enc.as_bytes()) {
            let (cow, _, _) = encod.decode(bytes);
            return cow.into_owned();
        }
    }
    // UTF-8 or fallback to Windows-1252 with replacements
    match String::from_utf8(bytes.to_vec()) {
        Ok(s) => s,
        Err(_) => {
            let (cow, _, _) = encoding_rs::WINDOWS_1252.decode(bytes);
            cow.into_owned()
        }
    }
}

fn sniff_xml_encoding(head: &[u8]) -> Option<String> {
    // crude scan for encoding="..." or encoding='...'
    let lower: Vec<u8> = head.iter().map(|b| b.to_ascii_lowercase()).collect();
    if let Some(pos) = find_subslice(&lower, b"encoding") {
        let rest = &lower[pos + 8..];
        let rest_orig = &head[pos + 8..];
        let mut i = 0usize;
        while i < rest.len() && (rest[i] as char).is_ascii_whitespace() {
            i += 1;
        }
        if i < rest.len() && rest[i] == b'=' {
            i += 1;
        }
        while i < rest.len() && (rest[i] as char).is_ascii_whitespace() {
            i += 1;
        }
        if i < rest.len() && (rest[i] == b'"' || rest[i] == b'\'') {
            let quote = rest[i];
            i += 1;
            let mut j = i;
            while j < rest.len() && rest[j] != quote {
                j += 1;
            }
            if j <= rest_orig.len() {
                let val = &rest_orig[i..j];
                return Some(String::from_utf8_lossy(val).trim().to_string());
            }
        }
    }
    None
}

fn find_subslice(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

fn http_client() -> &'static Client {
    static CLIENT: OnceLock<Client> = OnceLock::new();
    CLIENT.get_or_init(|| {
        Client::builder()
            .user_agent("daizo-mcp/0.1 (+https://github.com/sinryo/daizo-mcp)")
            .connect_timeout(Duration::from_secs(5))
            .timeout(Duration::from_secs(12))
            .build()
            .expect("reqwest client")
    })
}

fn throttle(ms: u64) {
    static LAST: OnceLock<Mutex<Instant>> = OnceLock::new();
    let m = LAST.get_or_init(|| Mutex::new(Instant::now() - Duration::from_millis(ms)));
    let mut last = m.lock().unwrap();
    let elapsed = last.elapsed();
    if elapsed < Duration::from_millis(ms) {
        std::thread::sleep(Duration::from_millis(ms) - elapsed);
    }
    *last = Instant::now();
}

fn http_get_with_retry(url: &str, max_retries: u32) -> Option<String> {
    let client = http_client();
    let mut attempt = 0u32;
    let mut backoff = 500u64; // ms
    loop {
        throttle(500);
        match client.get(url).send() {
            Ok(resp) => {
                let status = resp.status();
                if status.is_success() {
                    match resp.text() {
                        Ok(t) => return Some(t),
                        Err(_) => {}
                    }
                }
                if status.as_u16() == 429 || status.is_server_error() {
                    // retry
                } else {
                    return None;
                }
            }
            Err(e) => {
                dbg_log(&format!("[http] error attempt={} err={}", attempt + 1, e));
            }
        }
        attempt += 1;
        if attempt > max_retries {
            return None;
        }
        std::thread::sleep(Duration::from_millis(backoff));
        backoff = (backoff.saturating_mul(2)).min(8000);
    }
}

fn tibetan_ewts_converter() -> &'static EwtsConverter {
    static CONV: OnceLock<EwtsConverter> = OnceLock::new();
    CONV.get_or_init(EwtsConverter::create)
}

fn looks_like_ewts(s: &str) -> bool {
    let t = s.trim();
    if t.is_empty() {
        return false;
    }
    // Heuristic: ASCII-only and not just digits/symbols.
    let mut letters = 0usize;
    for ch in t.chars() {
        if !ch.is_ascii() {
            return false;
        }
        if ch.is_ascii_alphabetic() {
            letters += 1;
        }
    }
    letters >= 2
}

fn ewts_to_unicode_best_effort(s: &str) -> Option<String> {
    if !looks_like_ewts(s) {
        return None;
    }
    let conv = tibetan_ewts_converter();
    let out = conv.ewts_to_unicode(s);
    if out.trim().is_empty() || out == s {
        return None;
    }
    Some(out)
}

fn strip_html_mark(s: &str) -> String {
    s.replace("<mark>", "").replace("</mark>", "")
}

fn truncate_chars(s: &str, max_chars: usize) -> String {
    if max_chars == 0 {
        return "".to_string();
    }
    let mut out = String::new();
    for (i, ch) in s.chars().enumerate() {
        if i >= max_chars {
            break;
        }
        out.push(ch);
    }
    out
}

fn adarshah_build_link(
    kdb: &str,
    sutra: &str,
    pb: &str,
    sutra_type: Option<&str>,
    highlight: &str,
) -> Option<String> {
    let base = "https://online.adarshah.org/index.html";
    let kdb_enc = urlencoding::encode(kdb);
    let sutra_enc = urlencoding::encode(sutra);
    let pb_enc = urlencoding::encode(pb);
    let hl_enc = urlencoding::encode(highlight);
    match sutra_type.unwrap_or("sutra") {
        "voltext" => Some(format!(
            "{base}?kdb={kdb_enc}&voltext={sutra_enc}&page={pb_enc}&highlight={hl_enc}"
        )),
        _ => Some(format!(
            "{base}?kdb={kdb_enc}&sutra={sutra_enc}&page={pb_enc}&highlight={hl_enc}"
        )),
    }
}

fn adarshah_search_fulltext(
    query: &str,
    wildcard: bool,
    limit: usize,
    max_snippet_chars: usize,
) -> Vec<serde_json::Value> {
    // Reverse-engineered from https://online.adarshah.org/js/api.js + token.js
    const API_KEY: &str = "ZTI3Njg0NTNkZDRlMTJjMWUzNGM3MmM5ZGI3ZDUxN2E=";
    const URL: &str =
        "https://api.adarshah.org/plugins/adarshaplugin/file_servlet/search/esSearch?";

    let client = http_client();
    throttle(200);

    let params: Vec<(&str, String)> = vec![
        ("apiKey", API_KEY.to_string()),
        ("token", "".to_string()),
        ("text", query.to_string()),
        (
            "wildcard",
            if wildcard { "true" } else { "false" }.to_string(),
        ),
    ];

    let resp = client.post(URL).form(&params).send();
    let Ok(resp) = resp else {
        return Vec::new();
    };
    if !resp.status().is_success() {
        return Vec::new();
    }
    let Ok(body) = resp.text() else {
        return Vec::new();
    };
    let Ok(v) = serde_json::from_str::<serde_json::Value>(&body) else {
        return Vec::new();
    };

    let mut out: Vec<serde_json::Value> = Vec::new();
    let hits = v
        .get("hits")
        .and_then(|h| h.get("hits"))
        .and_then(|x| x.as_array())
        .cloned()
        .unwrap_or_default();

    for h in hits.into_iter().take(limit) {
        let score = h.get("_score").and_then(|x| x.as_f64()).unwrap_or(0.0);
        let fields = h.get("fields").cloned().unwrap_or(json!({}));
        let kdb = fields
            .get("kdb")
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .to_string();
        let sutra = fields
            .get("sutra")
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .to_string();
        let pb = fields
            .get("pb")
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .to_string();
        let sutra_type = fields
            .get("sutraType")
            .and_then(|x| x.as_str())
            .map(|s| s.to_string());

        let highlight_obj = h.get("highlight").cloned().unwrap_or(json!({}));
        let mut snippet = String::new();
        if let Some(arr) = highlight_obj.get("text").and_then(|x| x.as_array()) {
            for s in arr.iter().filter_map(|x| x.as_str()) {
                snippet.push_str(s);
            }
        } else if let Some(arr) = highlight_obj.get("textWildcard").and_then(|x| x.as_array()) {
            for s in arr.iter().filter_map(|x| x.as_str()) {
                snippet.push_str(s);
            }
        } else if let Some(s) = highlight_obj.get("text").and_then(|x| x.as_str()) {
            snippet.push_str(s);
        } else if let Some(s) = highlight_obj.get("textWildcard").and_then(|x| x.as_str()) {
            snippet.push_str(s);
        }
        snippet = strip_html_mark(&snippet);
        if max_snippet_chars > 0 && snippet.chars().count() > max_snippet_chars {
            snippet = truncate_chars(&snippet, max_snippet_chars);
        }

        let title = fields
            .get("tname")
            .and_then(|x| x.as_str())
            .or_else(|| fields.get("cname").and_then(|x| x.as_str()))
            .unwrap_or("")
            .to_string();

        let url = if !kdb.is_empty() && !sutra.is_empty() && !pb.is_empty() {
            adarshah_build_link(&kdb, &sutra, &pb, sutra_type.as_deref(), query)
        } else {
            None
        };

        out.push(json!({
            "source": "adarshah",
            "score": score,
            "query": query,
            "title": title,
            "kdb": kdb,
            "sutra": sutra,
            "pb": pb,
            "sutraType": sutra_type,
            "snippet": snippet,
            "url": url
        }));
    }
    out
}

fn buda_norm_id(s: &str) -> String {
    let t = s.trim();
    t.strip_prefix("bdr:").unwrap_or(t).to_string()
}

fn buda_extract_id_from_hit(h: &serde_json::Value) -> Option<String> {
    // Prefer explicit id fields, then routing, then parse from _id.
    let src = h.get("_source")?;
    if let Some(id) = src
        .get("inRootInstance")
        .and_then(|x| x.as_array())
        .and_then(|a| a.first())
        .and_then(|x| x.as_str())
    {
        let idn = buda_norm_id(id);
        if !idn.is_empty() {
            return Some(idn);
        }
    }
    if let Some(r) = h.get("_routing").and_then(|x| x.as_str()) {
        let idn = buda_norm_id(r);
        if !idn.is_empty() {
            return Some(idn);
        }
    }
    if let Some(raw) = h.get("_id").and_then(|x| x.as_str()) {
        // Often looks like: MW3MS701_O3MS701_...
        if let Some(prefix) = raw.split('_').next() {
            let idn = buda_norm_id(prefix);
            if idn.starts_with("MW") && idn.len() >= 4 {
                return Some(idn);
            }
        }
    }
    None
}

fn buda_query_string(q: &str, exact: bool) -> String {
    let t = q.trim();
    if t.is_empty() {
        return "".to_string();
    }
    // query_string syntax: escape backslash and quotes.
    let escaped = t.replace('\\', "\\\\").replace('"', "\\\"");
    if exact {
        format!("\"{}\"", escaped)
    } else {
        escaped
    }
}

fn buda_search_fulltext(
    query: &str,
    exact: bool,
    limit: usize,
    max_snippet_chars: usize,
) -> Vec<serde_json::Value> {
    // BUDA (BDRC) search proxy. Reverse-engineered from library.bdrc.io bundles:
    // POST https://autocomplete.bdrc.io/_msearch with Basic auth + NDJSON body.
    //
    // Notes:
    // - The backend seems to accept query_string queries for Tibetan Unicode.
    // - Returned docs are not raw etext chunks; they often include `comment` (context-ish),
    //   `prefLabel_bo_x_ewts` (title-ish), and `inRootInstance` (work-ish id).
    const URL: &str = "https://autocomplete.bdrc.io/_msearch";
    const AUTH_BASIC: &str = "Basic cHVibGljcXVlcnk6MFZzZzFRdmpMa1RDenZ0bA==";

    let q = query.trim();
    if q.is_empty() || limit == 0 {
        return Vec::new();
    }

    // Use query_string; avoid complicated DSL that sometimes 500s.
    let qsent = buda_query_string(q, exact);
    if qsent.is_empty() {
        return Vec::new();
    }
    let q_obj = json!({
        "size": limit,
        "query": {
            "query_string": { "query": qsent }
        }
    });
    let body = format!(
        "{{}}\n{}\n",
        serde_json::to_string(&q_obj).unwrap_or_else(|_| "{}".to_string())
    );

    let client = http_client();
    throttle(200);
    let resp = client
        .post(URL)
        .header("Authorization", AUTH_BASIC)
        .header("Content-Type", "application/x-ndjson")
        .body(body)
        .send();
    let Ok(resp) = resp else {
        return Vec::new();
    };
    if !resp.status().is_success() {
        return Vec::new();
    }
    let Ok(text) = resp.text() else {
        return Vec::new();
    };
    let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) else {
        return Vec::new();
    };

    let mut out: Vec<serde_json::Value> = Vec::new();
    let hits = v
        .get("responses")
        .and_then(|r| r.as_array())
        .and_then(|arr| arr.first())
        .and_then(|r0| r0.get("hits"))
        .and_then(|h| h.get("hits"))
        .and_then(|x| x.as_array())
        .cloned()
        .unwrap_or_default();

    for h in hits.into_iter().take(limit) {
        let score = h.get("_score").and_then(|x| x.as_f64()).unwrap_or(0.0);
        let src = h.get("_source").cloned().unwrap_or(json!({}));

        let title_ewts = src
            .get("prefLabel_bo_x_ewts")
            .and_then(|x| x.as_array())
            .and_then(|a| a.first())
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .to_string();
        let title_bo = if title_ewts.is_empty() {
            "".to_string()
        } else {
            tibetan_ewts_converter().ewts_to_unicode(&title_ewts)
        };

        let snippet = src
            .get("comment")
            .and_then(|x| x.as_array())
            .and_then(|a| a.first())
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .to_string();
        let snippet = if max_snippet_chars > 0 && snippet.chars().count() > max_snippet_chars {
            truncate_chars(&snippet, max_snippet_chars)
        } else {
            snippet
        };

        let root = buda_extract_id_from_hit(&h).unwrap_or_default();

        let url = if !root.is_empty() {
            Some(format!(
                "https://library.bdrc.io/show/bdr:{}",
                urlencoding::encode(&root)
            ))
        } else {
            None
        };

        out.push(json!({
            "source": "buda",
            "score": score,
            "query": q,
            "qSent": qsent,
            "exact": exact,
            "title": if !title_bo.is_empty() { title_bo } else { title_ewts.clone() },
            "title_ewts": if title_ewts.is_empty() { None::<String> } else { Some(title_ewts) },
            "id": root,
            "snippet": snippet,
            "url": url
        }));
	    }
	    out
	}

	#[derive(Serialize, Clone)]
	struct JozenHit {
	    lineno: String,
	    title: String,
	    author: String,
	    snippet: String,
	    #[serde(rename = "detailUrl")]
	    detail_url: String,
	    #[serde(rename = "imageUrl")]
	    image_url: String,
	}
	
	struct JozenSearchParsed {
	    total_count: Option<usize>,
	    displayed_count: Option<usize>,
	    total_pages: Option<usize>,
	    page_size: Option<usize>,
	    results: Vec<JozenHit>,
	}
	
	struct JozenDetail {
	    source_url: String,
	    work_header: String,
	    textno: Option<String>,
	    page_prev: Option<String>,
	    page_next: Option<String>,
	    line_ids: Vec<String>,
	    content: String,
	}
	
	fn jozen_cache_path_for(key: &str) -> PathBuf {
	    let mut hasher = Sha1::new();
	    hasher.update(key.as_bytes());
	    let h = hasher.finalize();
	    let fname = format!("{:x}.html", h);
	    let dir = cache_dir().join("jozen");
	    ensure_dir(&dir);
	    dir.join(fname)
	}
	
	fn http_post_form_with_retry(
	    url: &str,
	    params: &[(&str, String)],
	    max_retries: u32,
	) -> Option<String> {
	    let client = http_client();
	    let mut attempt = 0u32;
	    let mut backoff = 500u64; // ms
	    loop {
	        throttle(500);
	        match client.post(url).form(&params).send() {
	            Ok(resp) => {
	                let status = resp.status();
	                if status.is_success() {
	                    if let Ok(t) = resp.text() {
	                        return Some(t);
	                    }
	                }
	                if status.as_u16() == 429 || status.is_server_error() {
	                    // retry
	                } else {
	                    return None;
	                }
	            }
	            Err(e) => {
	                dbg_log(&format!("[http] post error attempt={} err={}", attempt + 1, e));
	            }
	        }
	        attempt += 1;
	        if attempt > max_retries {
	            return None;
	        }
	        std::thread::sleep(Duration::from_millis(backoff));
	        backoff = (backoff.saturating_mul(2)).min(8000);
	    }
	}
	
	fn jozen_search_html(query: &str, page: usize) -> Option<String> {
	    const URL: &str = "https://jodoshuzensho.jp/jozensearch_post/search/connect_jozen_DB.php";
	    let key = format!("POST|{}|keywd={}|page={}", URL, query, page);
	    let cpath = jozen_cache_path_for(&key);
	    if let Ok(s) = fs::read_to_string(&cpath) {
	        return Some(s);
	    }
	    let params: Vec<(&str, String)> = vec![("keywd", query.to_string()), ("page", page.to_string())];
	    if let Some(txt) = http_post_form_with_retry(URL, &params, 3) {
	        let _ = fs::write(&cpath, &txt);
	        return Some(txt);
	    }
	    None
	}
	
		fn jozen_detail_url(lineno: &str) -> String {
		    format!(
		        "https://jodoshuzensho.jp/jozensearch_post/search/detail.php?lineno={}",
		        urlencoding::encode(lineno.trim())
		    )
		}
		
		fn jozen_image_url(lineno: &str) -> String {
		    format!(
		        "https://jodoshuzensho.jp/jozensearch_post/search/image.php?lineno={}",
		        urlencoding::encode(lineno.trim())
		    )
		}
	
	fn jozen_detail_html(lineno: &str) -> Option<String> {
	    let url = jozen_detail_url(lineno);
	    let key = format!("GET|{}", url);
	    let cpath = jozen_cache_path_for(&key);
	    if let Ok(s) = fs::read_to_string(&cpath) {
	        return Some(s);
	    }
	    if let Some(txt) = http_get_with_retry(&url, 3) {
	        let _ = fs::write(&cpath, &txt);
	        return Some(txt);
	    }
	    None
	}
	
	fn jozen_join_url(href: &str) -> String {
	    let t = href.trim();
	    if t.is_empty() {
	        return String::new();
	    }
	    if t.starts_with("http://") || t.starts_with("https://") {
	        return t.to_string();
	    }
	    let base = url::Url::parse("https://jodoshuzensho.jp/jozensearch_post/search/").unwrap();
	    base.join(t)
	        .map(|u| u.to_string())
	        .unwrap_or_else(|_| format!("https://jodoshuzensho.jp/jozensearch_post/search/{}", t))
	}
	
	fn jozen_extract_lineno_from_href(href: &str) -> Option<String> {
	    let full = jozen_join_url(href);
	    if full.is_empty() {
	        return None;
	    }
	    let Ok(u) = url::Url::parse(&full) else {
	        return None;
	    };
	    for (k, v) in u.query_pairs() {
	        if k == "lineno" {
	            let t = v.trim().to_string();
	            if !t.is_empty() {
	                return Some(t);
	            }
	        }
	    }
	    None
	}
	
	fn jozen_html_fragment_to_text_keep_lb(inner_html: &str) -> String {
	    static LB_RE: OnceLock<Regex> = OnceLock::new();
	    let re = LB_RE.get_or_init(|| Regex::new(r"(?is)<lb[^>]*>").unwrap());
	    let replaced = re.replace_all(inner_html, "\n");
	    let frag = Html::parse_fragment(replaced.as_ref());
	    let t = frag.root_element().text().collect::<Vec<_>>().join("");
	    normalize_ws(&t)
	}
	
	fn jozen_collect_text_compact(node: &scraper::ElementRef) -> String {
	    node.text()
	        .collect::<Vec<_>>()
	        .join("")
	        .split_whitespace()
	        .collect::<Vec<_>>()
	        .join(" ")
	        .trim()
	        .to_string()
	}
	
	fn jozen_parse_search_html(
	    html: &str,
	    _query: &str,
	    max_results: usize,
	    max_snippet_chars: usize,
	) -> JozenSearchParsed {
	    let dom = Html::parse_document(html);
	
	    let mut total_count: Option<usize> = None;
	    let mut displayed_count: Option<usize> = None;
	    let mut total_pages: Option<usize> = None;
	
	    if let Ok(sel) = Selector::parse("p.rlt1") {
	        if let Some(p) = dom.select(&sel).next() {
	            let t = p.text().collect::<Vec<_>>().join("");
	            static TOTAL_RE: OnceLock<Regex> = OnceLock::new();
	            static DISP_RE: OnceLock<Regex> = OnceLock::new();
		            let re_total = TOTAL_RE.get_or_init(|| Regex::new(r"全\s*([0-9,]+)\s*件").unwrap());
		            let re_disp =
		                DISP_RE.get_or_init(|| Regex::new(r"([0-9,]+)\s*件を表示").unwrap());
	            if let Some(c) = re_total.captures(&t) {
	                if let Some(m) = c.get(1) {
	                    let n = m.as_str().replace(',', "");
	                    total_count = n.parse::<usize>().ok();
	                }
	            }
	            if let Some(c) = re_disp.captures(&t) {
	                if let Some(m) = c.get(1) {
	                    let n = m.as_str().replace(',', "");
	                    displayed_count = n.parse::<usize>().ok();
	                }
	            }
	        }
	    }
	
	    if let Ok(sel) = Selector::parse("form[name=lastpage] input[name=page]") {
	        if let Some(inp) = dom.select(&sel).next() {
	            if let Some(v) = inp.value().attr("value") {
	                total_pages = v.trim().parse::<usize>().ok();
	            }
	        }
	    }
	
	    let mut results: Vec<JozenHit> = Vec::new();
	    if max_results == 0 {
	        return JozenSearchParsed {
	            total_count,
	            displayed_count,
	            total_pages,
	            page_size: displayed_count.or(Some(50)),
	            results,
	        };
	    }
	
	    let tr_sel = Selector::parse("table.result_table tr").unwrap();
	    let th_sel = Selector::parse("th").unwrap();
	    let td_sel = Selector::parse("td").unwrap();
	    let a_sel = Selector::parse("a").unwrap();
	    for tr in dom.select(&tr_sel) {
	        if tr.select(&th_sel).next().is_some() {
	            continue;
	        }
	        let tds: Vec<_> = tr.select(&td_sel).collect();
	        if tds.len() < 5 {
	            continue;
	        }
	        let detail_href = tds[0]
	            .select(&a_sel)
	            .next()
	            .and_then(|a| a.value().attr("href"))
	            .unwrap_or("")
	            .to_string();
	        let image_href = tds[1]
	            .select(&a_sel)
	            .next()
	            .and_then(|a| a.value().attr("href"))
	            .unwrap_or("")
	            .to_string();
		        let mut lineno = jozen_collect_text_compact(&tds[0]);
		        if lineno.is_empty() {
		            lineno = jozen_extract_lineno_from_href(&detail_href).unwrap_or_default();
		        }
		        let title = jozen_collect_text_compact(&tds[2]);
		        let author = jozen_collect_text_compact(&tds[3]);
		        let snippet_html = tds[4].inner_html();
		        let mut snippet = jozen_html_fragment_to_text_keep_lb(&snippet_html);
		        if max_snippet_chars > 0 && snippet.chars().count() > max_snippet_chars {
		            snippet = truncate_chars(&snippet, max_snippet_chars);
		        }
		        let detail_url = if !lineno.is_empty() {
		            jozen_detail_url(&lineno)
		        } else {
		            jozen_join_url(&detail_href)
		        };
		        let image_url = if !lineno.is_empty() {
		            jozen_image_url(&lineno)
		        } else {
		            jozen_join_url(&image_href)
		        };
		        results.push(JozenHit {
		            lineno,
		            title,
		            author,
	            snippet,
	            detail_url,
	            image_url,
	        });
	        if results.len() >= max_results {
	            break;
	        }
	    }
	
	    JozenSearchParsed {
	        total_count,
	        displayed_count,
	        total_pages,
	        page_size: displayed_count.or(Some(50)),
	        results,
	    }
	}
	
	fn jozen_is_textno(token: &str) -> bool {
	    let b = token.as_bytes();
	    if b.len() != 5 {
	        return false;
	    }
	    b[0].is_ascii_alphabetic()
	        && b[1].is_ascii_digit()
	        && b[2].is_ascii_digit()
	        && b[3].is_ascii_digit()
	        && b[4].is_ascii_digit()
	}
	
	fn jozen_extract_detail(html: &str, source_url: &str) -> JozenDetail {
	    let dom = Html::parse_document(html);
	
	    let mut work_header = String::new();
	    if let Ok(sel) = Selector::parse("p.sdt01") {
	        if let Some(p) = dom.select(&sel).next() {
	            work_header = p.text().collect::<Vec<_>>().join("").trim().to_string();
	            if work_header.ends_with("画像") {
	                work_header = work_header.trim_end_matches("画像").trim().to_string();
	            }
	        }
	    }
	
	    let textno = work_header
	        .split_whitespace()
	        .next()
	        .and_then(|t| if jozen_is_textno(t) { Some(t.to_string()) } else { None });
	
	    let page_prev = Selector::parse("a.tnbn_prev")
	        .ok()
	        .and_then(|sel| dom.select(&sel).next())
	        .and_then(|a| a.value().attr("href"))
	        .and_then(jozen_extract_lineno_from_href);
	    let page_next = Selector::parse("a.tnbn_next")
	        .ok()
	        .and_then(|sel| dom.select(&sel).next())
	        .and_then(|a| a.value().attr("href"))
	        .and_then(jozen_extract_lineno_from_href);
	
	    let mut line_ids: Vec<String> = Vec::new();
	    let mut out_lines: Vec<String> = Vec::new();
	    let tr_sel = Selector::parse("table.sd_table tr").unwrap();
	    let td1_sel = Selector::parse("td.sd_td1").unwrap();
	    let td2_sel = Selector::parse("td.sd_td2").unwrap();
	    for tr in dom.select(&tr_sel) {
	        let Some(td1) = tr.select(&td1_sel).next() else {
	            continue;
	        };
	        let Some(td2) = tr.select(&td2_sel).next() else {
	            continue;
	        };
	        let id = jozen_collect_text_compact(&td1);
	        let id = id.trim_end_matches(':').trim().to_string();
	        if id.is_empty() {
	            continue;
	        }
	        let text = td2.text().collect::<Vec<_>>().join("").trim().to_string();
	        if text.is_empty() {
	            continue;
	        }
	        line_ids.push(id.clone());
	        out_lines.push(format!("[{}] {}", id, text));
	    }
	    let content = normalize_ws(&out_lines.join("\n"));
	
	    JozenDetail {
	        source_url: source_url.to_string(),
	        work_header,
	        textno,
	        page_prev,
	        page_next,
	        line_ids,
	        content,
	    }
	}
	
	fn sat_fetch(url: &str) -> String {
	    let cpath = cache_path_for(url);
	    if let Ok(s) = fs::read_to_string(&cpath) {
	        return s;
    }
    if let Some(txt) = http_get_with_retry(url, 3) {
        let text = extract_sat_text(&txt);
        let _ = fs::write(&cpath, &text);
        return text;
    }
    "".to_string()
}

fn sat_wrap7_build_url(
    q: &str,
    rows: usize,
    offs: usize,
    fields: &str,
    fq: &Vec<String>,
) -> String {
    let mut base = url::Url::parse("https://21dzk.l.u-tokyo.ac.jp/SAT2018/wrap7.php").unwrap();
    base.query_pairs_mut().append_pair("regex", "off");
    // Send the query as-is to wrap7 (caller may include quotes if needed)
    base.query_pairs_mut().append_pair("q", q);
    base.query_pairs_mut()
        .append_pair("rows", &rows.to_string());
    base.query_pairs_mut()
        .append_pair("offs", &offs.to_string());
    base.query_pairs_mut().append_pair("schop", "AND");
    if !fields.trim().is_empty() {
        base.query_pairs_mut().append_pair("fl", fields);
    }
    for f in fq {
        if !f.trim().is_empty() {
            base.query_pairs_mut().append_pair("fq", f);
        }
    }
    base.to_string()
}

fn sat_wrap7_ensure_fields(fields: &str, required: &[&str]) -> String {
    let mut out: Vec<String> = fields
        .split(',')
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .collect();
    for r in required {
        if !out.iter().any(|x| x == r) {
            out.push((*r).to_string());
        }
    }
    out.join(",")
}

fn sat_wrap7_search_json(
    q: &str,
    rows: usize,
    offs: usize,
    fields: &str,
    fq: &Vec<String>,
) -> Option<serde_json::Value> {
    let url = sat_wrap7_build_url(q, rows, offs, fields, fq);
    let cpath = cache_path_for(&url);
    let body = if let Ok(s) = fs::read_to_string(&cpath) {
        s
    } else {
        if let Some(txt) = http_get_with_retry(&url, 3) {
            let _ = fs::write(&cpath, &txt);
            txt
        } else {
            String::new()
        }
    };
    if body.is_empty() {
        return None;
    }
    serde_json::from_str::<serde_json::Value>(&body).ok()
}

#[cfg(test)]
mod tibetan_tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_truncate_chars_unicode() {
        assert_eq!(truncate_chars("abc", 2), "ab");
        assert_eq!(truncate_chars("あいう", 2), "あい");
    }

    #[test]
    fn test_buda_query_string_exact_escapes() {
        assert_eq!(buda_query_string(" foo ", true), "\"foo\"");
        assert_eq!(buda_query_string("a\"b", true), "\"a\\\"b\"");
        assert_eq!(buda_query_string("a\\b", true), "\"a\\\\b\"");
        assert_eq!(buda_query_string("x", false), "x");
    }

    #[test]
    fn test_buda_extract_id_from_hit_prefers_in_root_instance() {
        let h = json!({
            "_id": "MW3MS701_O3MS701_foo",
            "_routing": "bdr:MWROUTING",
            "_source": { "inRootInstance": ["bdr:MWINROOT"] }
        });
        assert_eq!(buda_extract_id_from_hit(&h).as_deref(), Some("MWINROOT"));
    }

    #[test]
    fn test_buda_extract_id_from_hit_falls_back_to_routing() {
        let h = json!({
            "_routing": "bdr:MWROUTING",
            "_source": { "inRootInstance": [] }
        });
        assert_eq!(buda_extract_id_from_hit(&h).as_deref(), Some("MWROUTING"));
    }

    #[test]
    fn test_buda_extract_id_from_hit_falls_back_to_id_prefix() {
        let h = json!({
            "_id": "MW3MS701_O3MS701_foo",
            "_source": {}
        });
        assert_eq!(buda_extract_id_from_hit(&h).as_deref(), Some("MW3MS701"));
    }
}

fn sat_detail_build_url(useid: &str) -> String {
    format!(
        "https://21dzk.l.u-tokyo.ac.jp/SAT2018/satdb2018pre.php?mode=detail&ob=1&mode2=2&useid={}",
        urlencoding::encode(useid)
    )
}

fn title_score(title: &str, query: &str) -> f32 {
    let a = normalized(title);
    let b = normalized(query);
    let s_char = jaccard(&a, &b);
    let s_tok = token_jaccard(title, query);
    let mut sc = s_char.max(s_tok);
    if sc < 0.95 && (is_subsequence(&a, &b) || is_subsequence(&b, &a)) {
        sc = sc.max(0.85);
    }
    sc
}

fn sat_pick_best_doc(docs: &[serde_json::Value], query: &str) -> (usize, &'static str, f32) {
    if docs.is_empty() {
        return (0, "noDocs", 0.0);
    }
    let nq = normalized(query);
    let mut best_any: (usize, f32) = (0, -1.0);
    let mut first_body: Option<(usize, f32)> = None;
    let mut first_title_contains: Option<(usize, f32)> = None;
    for (i, d) in docs.iter().enumerate() {
        let title = d.get("fascnm").and_then(|v| v.as_str()).unwrap_or("");
        let sc = title_score(title, query);
        if sc > best_any.1 {
            best_any = (i, sc);
        }
        if nq.is_empty() {
            continue;
        }
        if first_title_contains.is_none() && normalized(title).contains(&nq) {
            // Preserve wrap7 relevance order; avoid picking later titles only because of overlap.
            first_title_contains = Some((i, sc));
        }
        let body = d.get("body").and_then(|v| v.as_str()).unwrap_or("");
        if first_body.is_none() && !body.is_empty() && normalized(body).contains(&nq) {
            first_body = Some((i, sc));
        }
    }

    if let Some((i, sc)) = first_body {
        (i, "bodyContains", sc)
    } else if let Some((i, sc)) = first_title_contains {
        (i, "titleContains", sc)
    } else {
        (best_any.0, "titleScore", best_any.1)
    }
}

fn extract_sat_text(html: &str) -> String {
    let doc = Html::parse_document(html);
    // Prefer SAT detail structure: lines are in span.tx; skip line numbers (.ln) and anchors
    if let Ok(sel) = Selector::parse("span.tx") {
        let mut lines: Vec<String> = Vec::new();
        for node in doc.select(&sel) {
            let t = node.text().collect::<Vec<_>>().join("");
            let t = t.trim();
            if !t.is_empty() {
                lines.push(t.to_string());
            }
        }
        let joined = lines.join("\n");
        let joined = normalize_ws(&joined);
        if joined.len() > 50 {
            return joined;
        }
    }
    // Fallbacks
    let candidates = vec![
        "#text", "#viewer", "#content", "main", ".content", ".article", "#main", "#result",
        "#detail", "#sattext", "pre#text", "pre", ".text", "body",
    ];
    for sel in candidates {
        if let Ok(selector) = Selector::parse(sel) {
            if let Some(node) = doc.select(&selector).next() {
                let t = node.text().collect::<Vec<_>>().join("\n");
                let t = normalize_ws(&t);
                if t.len() > 50 {
                    return t;
                }
            }
        }
    }
    String::new()
}

#[derive(Serialize, Clone)]
struct SatHit {
    title: String,
    url: String,
    startid: String,
    id: Option<String>,
    snippet: String,
}

fn sat_search_results(
    q: &str,
    rows: usize,
    offs: usize,
    exact: bool,
    titles_only: bool,
) -> Vec<SatHit> {
    // Build JSON API URL
    let mut base = url::Url::parse("https://21dzk.l.u-tokyo.ac.jp/SAT2018/wrap7.php").unwrap();
    base.query_pairs_mut().append_pair("regex", "off");
    let q_param = if exact {
        format!("\"{}\"", q)
    } else {
        q.to_string()
    };
    base.query_pairs_mut().append_pair("q", &q_param);
    base.query_pairs_mut().append_pair("ttype", "undefined");
    base.query_pairs_mut().append_pair("near", "");
    base.query_pairs_mut().append_pair("amb", "undefined");
    // If titles_only, overfetch to improve chance of collecting enough unique titles
    let rows_query = if titles_only {
        std::cmp::max(rows * 5, rows)
    } else {
        rows
    };
    base.query_pairs_mut()
        .append_pair("rows", &rows_query.to_string());
    base.query_pairs_mut()
        .append_pair("offs", &offs.to_string());
    base.query_pairs_mut().append_pair("schop", "AND");
    base.query_pairs_mut().append_pair("fq", "");
    let url = base.to_string();

    // Cache raw JSON text with throttle + retry
    let cpath = cache_path_for(&url);
    let body = if let Ok(s) = fs::read_to_string(&cpath) {
        s
    } else {
        if let Some(txt) = http_get_with_retry(&url, 3) {
            let _ = fs::write(&cpath, &txt);
            txt
        } else {
            String::new()
        }
    };
    if body.is_empty() {
        return Vec::new();
    }

    // Parse JSON and format simple text output
    let v: serde_json::Value = match serde_json::from_str(&body) {
        Ok(v) => v,
        Err(_) => return Vec::new(),
    };
    let docs = v
        .get("response")
        .and_then(|r| r.get("docs"))
        .and_then(|d| d.as_array())
        .cloned()
        .unwrap_or_default();
    let mut out: Vec<SatHit> = Vec::new();
    for d in docs.into_iter() {
        let title = d
            .get("fascnm")
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .to_string();
        let startid = d
            .get("startid")
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .to_string();
        let detail = format!(
            "https://21dzk.l.u-tokyo.ac.jp/SAT2018/satdb2018pre.php?mode=detail&ob=1&mode2=2&mode4=&useid={}&cpos=undefined&regsw=off&key={}",
            urlencoding::encode(&startid), urlencoding::encode(q)
        );
        let snippet = if titles_only {
            String::new()
        } else {
            d.get("body")
                .and_then(|x| x.as_str())
                .unwrap_or("")
                .to_string()
        };
        let id = d.get("id").and_then(|x| x.as_str()).map(|s| s.to_string());
        out.push(SatHit {
            title,
            url: detail,
            startid,
            id,
            snippet,
        });
    }
    if titles_only {
        // Filter by title match against the query (normalized contains), then unique by title
        let nq = normalized(q);
        let mut filtered: Vec<SatHit> = out
            .into_iter()
            .filter(|h| {
                let ht = normalized(&h.title);
                ht.contains(&nq)
            })
            .collect();
        // Unique by title while preserving order
        let mut seen = std::collections::HashSet::new();
        filtered.retain(|h| seen.insert(h.title.clone()));
        let start = std::cmp::min(offs, filtered.len());
        let end = std::cmp::min(start + rows, filtered.len());
        filtered[start..end].to_vec()
    } else {
        // Apply offs/rows on docs
        let start = std::cmp::min(offs, out.len());
        let end = std::cmp::min(start + rows, out.len());
        out[start..end].to_vec()
    }
}

fn section_by_head_bounds(
    xml: &str,
    head_index: Option<usize>,
    head_query: Option<&str>,
) -> Option<(usize, usize)> {
    let re = Regex::new(r"(?is)<head\b[^>]*>(.*?)</head>").ok()?;
    let mut heads: Vec<(usize, usize, String)> = Vec::new();
    for cap in re.captures_iter(xml) {
        let m = cap.get(0).unwrap();
        let text = daizo_core::strip_tags(&cap[1]);
        heads.push((m.start(), m.end(), text));
    }
    if heads.is_empty() {
        return None;
    }
    let idx = if let Some(q) = head_query {
        let ql = q.to_lowercase();
        heads
            .iter()
            .position(|(_, _, t)| t.to_lowercase().contains(&ql))?
    } else {
        head_index?
    };
    let start = heads[idx].1;
    let end = heads
        .get(idx + 1)
        .map(|(s, _, _)| *s)
        .unwrap_or_else(|| xml.len());
    Some((start, end))
}

fn extract_section_by_head(
    xml: &str,
    head_index: Option<usize>,
    head_query: Option<&str>,
    include_notes: bool,
) -> Option<String> {
    let (start, end) = section_by_head_bounds(xml, head_index, head_query)?;
    let sect = &xml[start..end];
    Some(extract_text_opts(sect, include_notes))
}

fn tipitaka_biblio(xml: &str) -> serde_json::Value {
    // 軽量版: <p rend="..."> から見つかった要素だけ返す
    let mut reader = quick_xml::Reader::from_str(xml);
    reader.config_mut().trim_text_start = true;
    reader.config_mut().trim_text_end = true;
    let mut buf = Vec::new();
    let mut in_p = false;
    let mut current_rend: Option<String> = None;
    let mut current_buf = String::new();
    let mut out: std::collections::BTreeMap<String, String> = std::collections::BTreeMap::new();
    let wanted = [
        "nikaya",
        "title",
        "book",
        "subhead",
        "subsubhead",
        "chapter",
    ];
    let mut count = 0usize;
    let max = 10_000usize;
    let mut seen = 0usize;
    loop {
        match reader.read_event_into(&mut buf) {
            Ok(quick_xml::events::Event::Start(e)) | Ok(quick_xml::events::Event::Empty(e)) => {
                let name_owned = e.name().as_ref().to_owned();
                let lname = name_owned
                    .rsplit(|b| *b == b':')
                    .next()
                    .unwrap_or(&name_owned);
                if lname == b"p" {
                    in_p = true;
                    current_buf.clear();
                    current_rend = e
                        .try_get_attribute("rend")
                        .ok()
                        .flatten()
                        .and_then(|a| String::from_utf8(a.value.into_owned()).ok())
                        .map(|s| s.to_ascii_lowercase());
                }
            }
            Ok(quick_xml::events::Event::End(e)) => {
                let name_owned = e.name().as_ref().to_owned();
                let lname = name_owned
                    .rsplit(|b| *b == b':')
                    .next()
                    .unwrap_or(&name_owned);
                if lname == b"p" && in_p {
                    if let Some(r) = current_rend.take() {
                        if wanted.contains(&r.as_str()) {
                            let val = current_buf.trim();
                            if !val.is_empty() {
                                out.entry(r).or_insert_with(|| val.to_string());
                                count = out.len();
                            }
                        }
                    }
                    in_p = false;
                    current_buf.clear();
                }
            }
            Ok(quick_xml::events::Event::Text(t)) => {
                if in_p {
                    if let Ok(tx) = t.decode() {
                        let s = tx.to_string();
                        if !s.trim().is_empty() {
                            current_buf.push_str(&s);
                        }
                    }
                }
            }
            Ok(quick_xml::events::Event::Eof) => break,
            Err(_) => break,
            _ => {}
        }
        buf.clear();
        seen += 1;
        if seen > max || count >= wanted.len() {
            break;
        }
    }
    serde_json::to_value(out).unwrap_or(serde_json::json!({}))
}

fn normalize_ws(s: &str) -> String {
    let mut t = s.replace("\r", "");
    t = t
        .split('\n')
        .map(|l| l.trim())
        .collect::<Vec<_>>()
        .join("\n");
    while t.contains("\n\n\n") {
        t = t.replace("\n\n\n", "\n\n");
    }
    t
}

#[cfg(test)]
mod tests {
    use super::{jozen_extract_detail, jozen_parse_search_html, sat_pick_best_doc, slice_text_bounds};
    use serde_json::json;

    #[test]
    fn slice_text_bounds_handles_multibyte_characters() {
        let text = "大般若經初會序";
        let (slice, total_chars, start, end) = slice_text_bounds(text, 0, 4);
        assert_eq!(slice, "大般若經");
        assert_eq!(total_chars, text.chars().count());
        assert_eq!(start, 0);
        assert_eq!(end, 4);

        let (slice_mid, _, start_mid, end_mid) = slice_text_bounds(text, 2, 3);
        assert_eq!(slice_mid, "若經初");
        assert_eq!(start_mid, 2);
        assert_eq!(end_mid, 5);
    }

    #[test]
    fn slice_text_bounds_clamps_to_text_length() {
        let text = "般若心経";
        let (slice, total, start, end) = slice_text_bounds(text, 10, 5);
        assert!(slice.is_empty());
        assert_eq!(total, text.chars().count());
        assert_eq!(start, total);
        assert_eq!(end, total);

        let (slice_full, total_full, start_full, end_full) = slice_text_bounds(text, 1, 100);
        assert_eq!(slice_full, "若心経");
        assert_eq!(total_full, text.chars().count());
        assert_eq!(start_full, 1);
        assert_eq!(end_full, total_full);
    }

    #[test]
    fn sat_pick_best_doc_prefers_body_contains() {
        let docs = vec![
            json!({"fascnm":"須摩提女經", "startid":"x", "body":"...須..."}),
            json!({"fascnm":"佛説長阿含經卷第二十", "startid":"y", "body":"...須彌山..."}),
        ];
        let (i, reason, _sc) = sat_pick_best_doc(&docs, "須弥山");
        assert_eq!(i, 1);
        assert_eq!(reason, "bodyContains");
    }

    #[test]
    fn sat_pick_best_doc_keeps_wrap7_order_within_body_contains() {
        let docs = vec![
            json!({"fascnm":"佛説長阿含經卷第十八", "startid":"a", "body":"...須彌山..."}),
            json!({"fascnm":"須摩提女經", "startid":"b", "body":"...須彌山..."}),
        ];
        let (i, reason, _sc) = sat_pick_best_doc(&docs, "須彌山");
        assert_eq!(i, 0);
        assert_eq!(reason, "bodyContains");
    }

    #[test]
    fn jozen_parse_search_html_parses_counts_and_results() {
        let html = r#"
<!doctype html><html><body>
<p class="rlt1">検索結果：全731件（
50件を表示）</p>
<form name="lastpage"><input type="hidden" name="page" value="15"></form>
<table class="result_table"><tbody>
<tr><th>巻_頁段行</th><th>画像</th><th>書名</th><th>著者・巻数</th><th>本文</th></tr>
<tr>
  <td nowrap><a href="detail.php?lineno=J01_0200B19&okikae=薬師">J01_0200B19</a></td>
  <td nowrap><a href="image.php?lineno=J01_0200B19">画像</a></td>
  <td nowrap>傍説浄土教経論集</td>
  <td nowrap>〓</td>
  <td><span style="color:red;">藥師</span>如來本願經<lb 01_0200B20_ />觀佛三昧海經</td>
</tr>
</tbody></table>
</body></html>
"#;
        let parsed = jozen_parse_search_html(html, "薬師", 10, 200);
        assert_eq!(parsed.total_count, Some(731));
        assert_eq!(parsed.total_pages, Some(15));
        assert_eq!(parsed.displayed_count, Some(50));
        assert_eq!(parsed.results.len(), 1);
        let r0 = &parsed.results[0];
        assert_eq!(r0.lineno, "J01_0200B19");
        assert_eq!(r0.title, "傍説浄土教経論集");
        assert_eq!(r0.author, "〓");
        assert!(r0.snippet.contains("藥師如來本願經"));
        assert!(r0.snippet.contains("觀佛三昧海經"));
        assert!(r0.detail_url.contains("detail.php?lineno=J01_0200B19"));
        assert!(r0.image_url.contains("image.php?lineno=J01_0200B19"));
    }

    #[test]
    fn jozen_extract_detail_parses_header_and_lines() {
        let html = r#"
<!doctype html><html><body>
<div id="topnavi">
  <a href="detail.php?lineno=J01_0199" class="tnbn_prev">(J01_0199)</a>
  <a href="detail.php?lineno=J01_0201" class="tnbn_next">(J01_0201)</a>
</div>
<p class="sdt01">J0100　傍説浄土教経論集　〓　<a href="image.php?lineno=J01_0200A01">画像</a></p>
<table class="sd_table"><tbody>
<tr class="sd_pc"><th>巻＿頁段行</th><th>本文</th></tr>
<tr><td class="sd_td1">J01_0200A01:</td><td class="sd_td2">般舟三昧經</td></tr>
<tr><td class="sd_td1">J01_0200B19:</td><td class="sd_td2"><span style="color:red;">藥師</span>如來本願經</td></tr>
</tbody></table>
</body></html>
"#;
        let src = "https://example.test/detail.php?lineno=J01_0200B19";
        let d = jozen_extract_detail(html, src);
        assert_eq!(d.source_url, src);
        assert!(d.work_header.starts_with("J0100"));
        assert!(!d.work_header.contains("画像"));
        assert_eq!(d.textno.as_deref(), Some("J0100"));
        assert_eq!(d.page_prev.as_deref(), Some("J01_0199"));
        assert_eq!(d.page_next.as_deref(), Some("J01_0201"));
        assert_eq!(d.line_ids, vec!["J01_0200A01".to_string(), "J01_0200B19".to_string()]);
        assert!(d.content.contains("[J01_0200A01] 般舟三昧經"));
        assert!(d.content.contains("[J01_0200B19] 藥師如來本願經"));
    }
}

fn main() -> Result<()> {
    // Initialize optional repo policy from env (rate limits / future robots compliance)
    daizo_core::repo::init_policy_from_env();
    let stdin = std::io::stdin();
    let mut stdin = BufReader::new(stdin.lock());
    let mut stdout = std::io::stdout();
    loop {
        let Some(msg) = read_message(&mut stdin)? else {
            break;
        };
        if let Ok(req) = serde_json::from_value::<Request>(msg.clone()) {
            dbg_log(&format!("[recv] method={} id={}", req.method, req.id));
            let resp = match req.method.as_str() {
                "initialize" => handle_initialize(req.id),
                "tools/list" => handle_tools_list(req.id),
                "tools/call" => handle_call(req.id, &req.params),
                _ => {
                    json!({"jsonrpc":"2.0","id":req.id,"error":{"code": -32601, "message":"Method not found"}})
                }
            };
            write_message(&mut stdout, &resp)?;
        } else {
            // ignore non-request messages
            dbg_log("[recv] non-request/ignored");
        }
    }
    Ok(())
}
