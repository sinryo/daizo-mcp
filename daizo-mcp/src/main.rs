use anyhow::Result;
use daizo_core::{build_tipitaka_index, build_cbeta_index, build_gretil_index, extract_text, extract_text_opts, extract_cbeta_juan, list_heads_cbeta, list_heads_generic, IndexEntry, cbeta_grep, tipitaka_grep, gretil_grep};
use regex::Regex;
use reqwest::blocking::Client;
use scraper::{Html, Selector};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha1::{Digest, Sha1};
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};
use encoding_rs::Encoding;
use daizo_core::text_utils::{normalized, token_jaccard, jaccard, is_subsequence, compute_match_score_sanskrit};
use daizo_core::path_resolver::{
    resolve_cbeta_path_by_id,
    resolve_tipitaka_by_id,
    find_tipitaka_content_for_base,
    find_exact_file_by_name,
    cbeta_root,
    gretil_root,
    tipitaka_root,
    cache_dir,
    daizo_home,
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

// ============ MCP stdio framing ============

fn dbg_enabled() -> bool { std::env::var("DAIZO_DEBUG").ok().as_deref() == Some("1") }
fn dbg_log(msg: &str) {
    if !dbg_enabled() { return; }
    let path = daizo_home().join("daizo-mcp.log");
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
        let _ = writeln!(f, "{}", msg);
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum FramingMode { Lsp, Lines }

static MODE: OnceLock<FramingMode> = OnceLock::new();

fn set_mode(m: FramingMode) { let _ = MODE.set(m); }
fn get_mode() -> FramingMode { *MODE.get().unwrap_or(&FramingMode::Lsp) }

fn read_message(stdin: &mut impl BufRead) -> Result<Option<serde_json::Value>> {
    // Try to read one logical unit. Support two modes:
    // 1) LSP-style headers with Content-Length and blank line
    // 2) Single-line JSON (newline-delimited JSON)

    let mut line = String::new();
    let n = stdin.read_line(&mut line)?;
    if n == 0 { return Ok(None); }

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
        if line == "\n" || line == "\r\n" || line.trim().is_empty() { break; }
        line.clear();
        let n = stdin.read_line(&mut line)?;
        if n == 0 { break; }
        headers.push_str(&line);
        if line == "\n" || line == "\r\n" || line.trim().is_empty() { break; }
    }
    set_mode(FramingMode::Lsp);
    dbg_log(&format!("[hdr]{}", headers.replace('\r', "\\r").replace('\n', "\\n")));

    // parse Content-Length
    let mut content_length = 0usize;
    for hline in headers.lines() {
        let h = hline.trim();
        if h.to_lowercase().starts_with("content-length:") {
            if let Some(v) = h.split(':').nth(1) { content_length = v.trim().parse().unwrap_or(0); }
        }
    }
    if content_length == 0 { dbg_log("[body] skip len=0"); return Ok(Some(serde_json::Value::Null)); }
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
    std::env::var(key).ok().and_then(|s| s.parse::<usize>().ok()).unwrap_or(default_v)
}

fn default_max_chars() -> usize { env_usize("DAIZO_MCP_MAX_CHARS", 6000) }
fn default_snippet_len() -> usize { env_usize("DAIZO_MCP_SNIPPET_LEN", 120) }
fn default_auto_files() -> usize { env_usize("DAIZO_MCP_AUTO_FILES", 1) }
fn default_auto_matches() -> usize { env_usize("DAIZO_MCP_AUTO_MATCHES", 1) }

#[derive(Deserialize)]
struct Request {
    id: serde_json::Value,
    method: String,
    #[serde(default)]
    params: serde_json::Value,
}

// ============ Paths & cache (via daizo-core::path_resolver) ============
fn ensure_dir(p: &Path) { let _ = fs::create_dir_all(p); }

fn ensure_cbeta_data() { let _ = daizo_core::repo::ensure_cbeta_data_at(&cbeta_root()); }

fn ensure_tipitaka_data() { let _ = daizo_core::repo::ensure_tipitaka_data_at(&daizo_home().join("tipitaka-xml")); }

fn load_index(path: &Path) -> Option<Vec<IndexEntry>> {
    fs::read(path).ok().and_then(|b| serde_json::from_slice(&b).ok())
}

fn save_index(path: &Path, entries: &Vec<IndexEntry>) -> Result<()> { ensure_dir(path.parent().unwrap()); fs::write(path, serde_json::to_vec(entries)?)?; Ok(()) }

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
                            {"role": "system", "content": "Prefer low-token flow: 1) *_search to locate files, 2) read _meta.fetchSuggestions, 3) call *_fetch with {id, lineNumber, contextBefore:1, contextAfter:3}. Use *_pipeline only when you need a cross-file summary; set autoFetch=false by default."}
                        ]
                    }
                },
                "logging": {}
            },
            .3.5" }
        }
    })
}

fn tools_list() -> Vec<serde_json::Value> {
    vec![
        tool("daizo_usage", "Usage guidance for AI: low-token workflow (search->_meta.fetchSuggestions->fetch). Avoid pipeline by default.", json!({"type":"object","properties":{}})),
        tool("cbeta_fetch", "Retrieve CBETA text by ID/part; supports low-cost slices via id+lineNumber (follow cbeta_search _meta.fetchSuggestions)", json!({"type":"object","properties":{
            "id":{"type":"string"},
            "query":{"type":"string"},
            "part":{"type":"string"},
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
        tool("cbeta_search", "Fast regex search over CBETA; returns _meta.fetchSuggestions (use cbeta_fetch with id+lineNumber) and _meta.pipelineHint for low-cost next steps", json!({"type":"object","properties":{
            "query":{"type":"string","description":"Regular expression pattern to search for"},
            "maxResults":{"type":"number","description":"Maximum number of files to return (default: 20)"},
            "maxMatchesPerFile":{"type":"number","description":"Maximum matches per file (default: 5)"}
        },"required":["query"]})),
        tool("cbeta_title_search", "Title-based search in CBETA corpus", json!({"type":"object","properties":{"query":{"type":"string"},"limit":{"type":"number"}},"required":["query"]})),
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
            "snippetPrefix":{"type":"string","description":"Prefix for highlight snippets in pipeline (default '>>> ')"},
            "snippetSuffix":{"type":"string","description":"Suffix for highlight snippets in pipeline (default '')"},
            "highlight":{"type":"string","description":"Highlight string or regex pattern inside contexts"},
            "highlightRegex":{"type":"boolean","description":"Interpret highlight as regex (default false)"},
            "highlightPrefix":{"type":"string","description":"Prefix marker for highlights (default from env or '>>> ')"},
            "highlightSuffix":{"type":"string","description":"Suffix marker for highlights (default from env or ' <<<')"},
            "full":{"type":"boolean"},
            "includeNotes":{"type":"boolean"}
        },"required":["query"]})),
        tool("sat_detail", "Fetch SAT detail by useid", json!({"type":"object","properties":{"useid":{"type":"string"},"key":{"type":"string"},"startChar":{"type":"number"},"maxChars":{"type":"number"}},"required":["useid"]})),
        tool("sat_fetch", "Fetch SAT page (prefer useid → detail URL)", json!({"type":"object","properties":{
            "url":{"type":"string"},
            "useid":{"type":"string"},
            "startChar":{"type":"number"},
            "maxChars":{"type":"number"}
        }})),
        tool("sat_pipeline", "Search wrap7, pick best title, then fetch detail", json!({"type":"object","properties":{
            "query":{"type":"string"},
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
        tool("tipitaka_fetch", "Retrieve Tipitaka by ID/section; supports low-cost slices via id+lineNumber (follow tipitaka_search _meta.fetchSuggestions)", json!({"type":"object","properties":{
            "id":{"type":"string"},
            "query":{"type":"string"},
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
        tool("tipitaka_search", "Fast regex search over Tipitaka; returns _meta.fetchSuggestions (use tipitaka_fetch with id+lineNumber) for low-cost next steps", json!({"type":"object","properties":{
            "query":{"type":"string","description":"Regular expression pattern to search for"},
            "maxResults":{"type":"number","description":"Maximum number of files to return (default: 20)"},
            "maxMatchesPerFile":{"type":"number","description":"Maximum matches per file (default: 5)"}
        },"required":["query"]})),
        tool("tipitaka_title_search", "Title-based search in Tipitaka corpus", json!({"type":"object","properties":{"query":{"type":"string"},"limit":{"type":"number"}},"required":["query"]})),
        // GRETIL (Sanskrit TEI)
        tool("gretil_title_search", "Title-based search in GRETIL corpus", json!({"type":"object","properties":{"query":{"type":"string"},"limit":{"type":"number"}},"required":["query"]})),
        tool("gretil_search", "Fast regex search over GRETIL; returns _meta.fetchSuggestions (use gretil_fetch with id+lineNumber) and _meta.pipelineHint for low-cost next steps", json!({"type":"object","properties":{
            "query":{"type":"string","description":"Regular expression pattern to search for"},
            "maxResults":{"type":"number","description":"Maximum number of files to return (default: 20)"},
            "maxMatchesPerFile":{"type":"number","description":"Maximum matches per file (default: 5)"}
        },"required":["query"]})),
        tool("gretil_fetch", "Retrieve GRETIL by ID; supports low-cost slices via id+lineNumber (follow gretil_search _meta.fetchSuggestions)", json!({"type":"object","properties":{
            "id":{"type":"string"},
            "query":{"type":"string"},
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
    ]
}

fn tool(name: &str, description: &str, input_schema: serde_json::Value) -> serde_json::Value {
    json!({"name": name, "description": description, "inputSchema": input_schema })
}

fn handle_tools_list(id: serde_json::Value) -> serde_json::Value {
    json!({"jsonrpc":"2.0","id":id,"result": {"tools": tools_list()}})
}

// normalization and token similarity helpers are provided by daizo_core::text_utils

fn load_or_build_cbeta_index() -> Vec<IndexEntry> {
    let out = cache_dir().join("cbeta-index.json");
    if let Some(v) = load_index(&out) {
        // 既存インデックスの健全性を軽くチェック（パスの存在 + メタの有無）
        let missing = v.iter().take(10).filter(|e| !Path::new(&e.path).exists()).count();
        let lacks_meta = v.iter().take(10).any(|e| e.meta.is_none());
        if v.is_empty() || missing > 0 || lacks_meta { /* 再生成へ */ } else { return v; }
    }
    // Ensure data exists (clone if needed)
    ensure_cbeta_data();
    let entries = build_cbeta_index(&cbeta_root());
    let _ = save_index(&out, &entries);
    entries
}
fn load_or_build_tipitaka_index() -> Vec<IndexEntry> {
    let out = cache_dir().join("tipitaka-index.json");
    if let Some(mut v) = load_index(&out) {
        v.retain(|e| !e.path.ends_with(".toc.xml"));
        let missing = v.iter().take(10).filter(|e| !Path::new(&e.path).exists()).count();
        let lacks_meta = v.iter().take(10).any(|e| e.meta.is_none());
        let lacks_heads = v.iter().take(20).any(|e| e.meta.as_ref().map(|m| !m.contains_key("headsPreview")).unwrap_or(true));
        let lacks_composite = v.iter().take(50).any(|e| {
            if let Some(m) = &e.meta {
                let p = m.get("alias_prefix").map(|s| s.as_str()).unwrap_or("");
                if p == "SN" || p == "AN" {
                    return !m.get("alias").map(|a| a.contains('.')).unwrap_or(false);
                }
            }
            false
        });
        if v.is_empty() || missing > 0 || lacks_meta || lacks_heads || lacks_composite { /* 再生成へ */ } else { return v; }
    }
    ensure_tipitaka_data();
    let mut entries = build_tipitaka_index(&tipitaka_root());
    entries.retain(|e| !e.path.ends_with(".toc.xml"));
    let _ = save_index(&out, &entries);
    entries
}
fn load_or_build_gretil_index() -> Vec<IndexEntry> {
    let out = cache_dir().join("gretil-index.json");
    if let Some(v) = load_index(&out) {
        let missing = v.iter().take(10).filter(|e| !Path::new(&e.path).exists()).count();
        if v.is_empty() || missing > 0 { /* rebuild */ } else { return v; }
    }
    let entries = build_gretil_index(&gretil_root());
    let _ = save_index(&out, &entries);
    entries
}

#[derive(Clone, Debug, Serialize)]
struct ScoredHit<'a> { #[serde(skip_serializing)] entry: &'a IndexEntry, score: f32 }

fn best_match<'a>(entries: &'a [IndexEntry], q: &str, limit: usize) -> Vec<ScoredHit<'a>> {
    let nq = normalized(q);
    let mut scored: Vec<(f32, &IndexEntry)> = entries
        .iter()
        .map(|e| {
            let mut s = daizo_core::text_utils::compute_match_score(e, q, false);
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
            (s, e)
        })
        .collect();
    scored.sort_by(|a,b| b.0.partial_cmp(&a.0).unwrap());
    scored.into_iter().take(limit).map(|(s,e)| ScoredHit { entry: e, score: s }).collect()
}

fn best_match_tipitaka<'a>(entries: &'a [IndexEntry], q: &str, limit: usize) -> Vec<ScoredHit<'a>> {
    let mut scored: Vec<(f32, &IndexEntry)> = entries
        .iter()
        .map(|e| (daizo_core::text_utils::compute_match_score(e, q, true), e))
        .collect();
    scored.sort_by(|a,b| b.0.partial_cmp(&a.0).unwrap());
    scored.into_iter().take(limit).map(|(s,e)| ScoredHit { entry: e, score: s }).collect()
}

fn best_match_gretil<'a>(entries: &'a [IndexEntry], q: &str, limit: usize) -> Vec<ScoredHit<'a>> {
    let nq = normalized(q);
    let mut scored: Vec<(f32, &IndexEntry)> = entries
        .iter()
        .map(|e| {
            let mut s = compute_match_score_sanskrit(e, q);
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
            (s, e)
        })
        .collect();
    scored.sort_by(|a,b| b.0.partial_cmp(&a.0).unwrap());
    scored.into_iter().take(limit).map(|(s,e)| ScoredHit { entry: e, score: s }).collect()
}

// jaccard and is_subsequence moved to daizo_core::text_utils

// resolve_cbeta_path moved to daizo_core::path_resolver

// removed: unused helper

fn slice_text(text: &str, args: &serde_json::Value) -> String {
    // Slice by character positions (safe for UTF-8).
    let default_max = 8000usize;
    let start_char = args
        .get("page").and_then(|v| v.as_u64())
        .and_then(|p| args.get("pageSize").and_then(|s| s.as_u64()).map(|ps| (p as usize) * (ps as usize)))
        .or_else(|| args.get("startChar").and_then(|v| v.as_u64().map(|x| x as usize)))
        .unwrap_or(0);
    let total_chars = text.chars().count();
    let start_char = std::cmp::min(start_char, total_chars);
    let end_char = if let (Some(p), Some(ps)) = (args.get("page").and_then(|v| v.as_u64()), args.get("pageSize").and_then(|v| v.as_u64())) {
        Some((p as usize) * (ps as usize) + (ps as usize))
    } else if let Some(ec) = args.get("endChar").and_then(|v| v.as_u64()) { Some(ec as usize) }
    else if let Some(mc) = args.get("maxChars").and_then(|v| v.as_u64()) { Some(start_char + mc as usize) } else { None };
    let end_char = end_char.map(|e| std::cmp::min(e, total_chars)).unwrap_or_else(|| std::cmp::min(start_char + default_max, total_chars));
    if start_char >= end_char { return String::new(); }
    // Convert char indices to byte indices
    let s_byte = text.char_indices().nth(start_char).map(|(b,_)| b).unwrap_or(text.len());
    let e_byte = text.char_indices().nth(end_char).map(|(b,_)| b).unwrap_or(text.len());
    if s_byte > e_byte { return String::new(); }
    text[s_byte..e_byte].to_string()
}

fn handle_call(id: serde_json::Value, params: &serde_json::Value) -> serde_json::Value {
    let name = params.get("name").and_then(|v| v.as_str()).unwrap_or("");
    let args = params.get("arguments").cloned().unwrap_or(json!({}));
    let content_text = match name {
        "daizo_usage" => {
            let guide = "Low-token guide:\n\n- Use search → read _meta.fetchSuggestions\n- Then call *_fetch with {id, lineNumber, contextBefore:1, contextAfter:3}\n- Use *_pipeline only for multi-file summary; set autoFetch=false by default\n- Search tools may also provide _meta.pipelineHint\n- Control number of suggestions via DAIZO_HINT_TOP (default 1)";
            return json!({
                "jsonrpc":"2.0",
                "id": id,
                "result": { "content": [{"type":"text","text": guide}], "_meta": {"source": "daizo_usage"} }
            });
        }
        "cbeta_title_search" => {
            let q_raw = args.get("query").and_then(|v| v.as_str()).unwrap_or("");
            let looks_like_regex = q_raw.chars().any(|c| ".+*?[](){}|\\".contains(c));
            let q = if q_raw.chars().any(|c| c.is_whitespace()) && !looks_like_regex {
                to_whitespace_fuzzy_literal(q_raw)
            } else { q_raw.to_string() };
            let limit = args.get("limit").and_then(|v| v.as_u64()).unwrap_or(10) as usize;
            let idx = load_or_build_cbeta_index();
            let hits = best_match(&idx, &q, limit);
            let summary = hits.iter().enumerate().map(|(i,h)| format!("{}. {}  {}", i+1, h.entry.id, h.entry.title)).collect::<Vec<_>>().join("\n");
            let results: Vec<_> = hits.iter().map(|h| {
                let meta = h.entry.meta.as_ref();
                json!({
                    "id": h.entry.id,
                    "title": h.entry.title,
                    "path": h.entry.path,
                    "score": h.score,
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
            }).collect();
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
            // 修正: IDがある場合は優先してqueryを無視
            if let Some(id) = args.get("id").and_then(|v| v.as_str()) {
                let idx = load_or_build_cbeta_index();
                if let Some(hit) = idx.iter().find(|e| e.id == id) {
                    matched_id = Some(hit.id.clone());
                    matched_title = Some(hit.title.clone());
                    path = PathBuf::from(&hit.path);
                } else {
                    path = resolve_cbeta_path_by_id(id).unwrap_or_else(|| PathBuf::from(""));
                    matched_id = Some(id.to_string());
                }
            } else if let Some(q) = args.get("query").and_then(|v| v.as_str()) {
                let idx = load_or_build_cbeta_index();
                if let Some(hit) = best_match(&idx, q, 1).into_iter().next() {
                    matched_id = Some(hit.entry.id.clone());
                    matched_title = Some(hit.entry.title.clone());
                    matched_score = Some(hit.score);
                    path = PathBuf::from(&hit.entry.path);
                }
            }
            let xml = fs::read_to_string(&path).unwrap_or_default();
            // includeNotes support
            let include_notes = args.get("includeNotes").and_then(|v| v.as_bool()).unwrap_or(false);
            
            // lineNumber指定時の処理
            let (text, extraction_method, part_matched) = if let Some(line_num) = args.get("lineNumber").and_then(|v| v.as_u64()) {
                // 新しいパラメータを優先、fallbackで古いパラメータを使用
                let context_before = args.get("contextBefore").and_then(|v| v.as_u64()).unwrap_or(
                    args.get("contextLines").and_then(|v| v.as_u64()).unwrap_or(10)
                ) as usize;
                let context_after = args.get("contextAfter").and_then(|v| v.as_u64()).unwrap_or(
                    args.get("contextLines").and_then(|v| v.as_u64()).unwrap_or(100)
                ) as usize;
                let context_text = daizo_core::extract_xml_around_line_asymmetric(&xml, line_num as usize, context_before, context_after);
                (context_text, format!("line-context-{}-{}-{}", line_num, context_before, context_after), false)
            } else if let Some(part) = args.get("part").and_then(|v| v.as_str()) {
                if let Some(sec) = extract_cbeta_juan(&xml, part) { (sec, "cbeta-juan".to_string(), true) } else { (extract_text_opts(&xml, include_notes), "full".to_string(), false) }
            } else { (extract_text_opts(&xml, include_notes), "full".to_string(), false) };
            
            let full_flag = args.get("full").and_then(|v| v.as_bool()).unwrap_or(false);
            let mut sliced = if full_flag { text.clone() } else { slice_text(&text, &args) };
            // Optional highlight across sliced text
            let mut highlight_count = 0usize;
            let mut highlight_positions: Vec<serde_json::Value> = Vec::new();
            if let Some(hpat) = args.get("highlight").and_then(|v| v.as_str()) {
                let use_re = args.get("highlightRegex").and_then(|v| v.as_bool()).unwrap_or(false);
                let hpre = args.get("highlightPrefix").and_then(|v| v.as_str()).unwrap_or(">>> ");
                let hsuf = args.get("highlightSuffix").and_then(|v| v.as_str()).unwrap_or(" <<<");
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
            let heads = list_heads_cbeta(&xml);
            let hl = args.get("headingsLimit").and_then(|v| v.as_u64()).unwrap_or(10) as usize;
            // enforce output cap
            let cap = default_max_chars();
            if sliced.chars().count() > cap { sliced = sliced.chars().take(cap).collect(); }
            let meta = json!({
                "totalLength": text.len(),
                "returnedStart": args.get("startChar").and_then(|v| v.as_u64()).unwrap_or( args.get("page").and_then(|v| v.as_u64()).and_then(|p| args.get("pageSize").and_then(|s| s.as_u64()).map(|ps| p*ps)).unwrap_or(0) ) ,
                "returnedEnd": args.get("startChar").and_then(|v| v.as_u64()).unwrap_or( args.get("page").and_then(|v| v.as_u64()).and_then(|p| args.get("pageSize").and_then(|s| s.as_u64()).map(|ps| p*ps)).unwrap_or(0) ) + (sliced.len() as u64),
                "truncated": if full_flag { (sliced.len() as u64) < (text.len() as u64) } else { (sliced.len() as u64) < (text.len() as u64) },
                "sourcePath": path.to_string_lossy(),
                "extractionMethod": extraction_method,
                "partMatched": part_matched,
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
        "tipitaka_title_search" => {
            let q = args.get("query").and_then(|v| v.as_str()).unwrap_or("");
            let limit = args.get("limit").and_then(|v| v.as_u64()).unwrap_or(10) as usize;
            let idx = load_or_build_tipitaka_index();
            let hits = best_match_tipitaka(&idx, q, limit);
            hits.iter().enumerate().map(|(i,h)| format!("{}. {}  {}", i+1, Path::new(&h.entry.path).file_stem().unwrap().to_string_lossy(), h.entry.title)).collect::<Vec<_>>().join("\n")
        }
        "tipitaka_fetch" => {
            ensure_tipitaka_data();
            let mut matched_id: Option<String> = None;
            let mut matched_title: Option<String> = None;
            let mut matched_score: Option<f32> = None;
            // 修正: IDがある場合は優先してqueryを無視
            let mut path: PathBuf = if let Some(id) = args.get("id").and_then(|v| v.as_str()) {
                let idx = load_or_build_tipitaka_index();
                if let Some(p) = resolve_tipitaka_by_id(&idx, id) {
                    // fill matched info from the resolved path
                    matched_id = Path::new(&p).file_stem().map(|s| s.to_string_lossy().into_owned());
                    if let Some(e) = idx.iter().find(|e| e.path == p.to_string_lossy()) {
                        matched_title = Some(e.title.clone());
                    }
                    p
                } else {
                    PathBuf::new()
                }
            } else if let Some(q) = args.get("query").and_then(|v| v.as_str()) {
                let idx = load_or_build_tipitaka_index();
                if let Some(hit) = best_match_tipitaka(&idx, q, 1).into_iter().next() {
                    matched_title = Some(hit.entry.title.clone());
                    matched_score = Some(hit.score);
                    matched_id = Path::new(&hit.entry.path).file_stem().map(|s| s.to_string_lossy().into_owned());
                    PathBuf::from(&hit.entry.path)
                } else { PathBuf::new() }
            } else { PathBuf::new() };

            // まだ見つからない場合は、厳密に `<id>.xml` を探す
            if path.as_os_str().is_empty() || !path.exists() {
                let fname = format!("{}.xml", args.get("id").and_then(|v| v.as_str()).unwrap_or_default());
                if let Some(p) = find_exact_file_by_name(&tipitaka_root(), &fname) { path = p; }
            }
            // 見つからない場合は空に近い応答
            if path.as_os_str().is_empty() { path = PathBuf::from(""); }
            // If we matched a TOC file (e.g., s0404m1.mul.toc.xml), try to open the first content part (e.g., s0404m1.mul0.xml)
            if let Some(name) = path.file_name().and_then(|s| s.to_str()) {
                if name.ends_with(".toc.xml") {
                    if let Some(stem) = Path::new(name).file_stem().and_then(|s| s.to_str()) {
                        let base = stem.trim_end_matches(".toc");
                        // Prefer base0.xml, then any base*.xml (non-TOC)
                        let dir: PathBuf = path.parent().map(|p| p.to_path_buf()).unwrap_or_else(|| tipitaka_root());
                        let mut candidate: Option<PathBuf> = None;
                        let base0 = dir.join(format!("{}0.xml", base));
                        if base0.exists() { candidate = Some(base0); }
                        if candidate.is_none() {
                            // Scan directory for base*.xml excluding *.toc.xml
                            if let Ok(read_dir) = fs::read_dir(&dir) {
                                for entry in read_dir.flatten() {
                                    let p = entry.path();
                                    if p.extension().and_then(|s| s.to_str()) == Some("xml") {
                                        if let Some(stem2) = p.file_stem().and_then(|s| s.to_str()) {
                                            if stem2.starts_with(base) && !stem2.ends_with(".toc") {
                                                candidate = Some(p.clone());
                                                break;
                                            }
                                        }
                                    }
                                }
                            }
                        }
                        if let Some(c) = candidate { path = c; }
                    }
                }
            }
            // 読み取り時にエンコーディング問題で空になるのを避けるため、バイト読み + UTF-8(代替) に変更
            let mut cur_path = path.clone();
            let mut xml = fs::read(&cur_path).map(|b| decode_xml_bytes(&b)).unwrap_or_default();
            let (mut text, mut extraction_method) = if let Some(line_num) = args.get("lineNumber").and_then(|v| v.as_u64()) {
                // 新しいパラメータを優先、fallbackで古いパラメータを使用
                let context_before = args.get("contextBefore").and_then(|v| v.as_u64()).unwrap_or(
                    args.get("contextLines").and_then(|v| v.as_u64()).unwrap_or(10)
                ) as usize;
                let context_after = args.get("contextAfter").and_then(|v| v.as_u64()).unwrap_or(
                    args.get("contextLines").and_then(|v| v.as_u64()).unwrap_or(100)
                ) as usize;
                let context_text = daizo_core::extract_xml_around_line_asymmetric(&xml, line_num as usize, context_before, context_after);
                (context_text, format!("line-context-{}-{}-{}", line_num, context_before, context_after))
            } else if let Some(hq) = args.get("headQuery").and_then(|v| v.as_str()) { 
                (extract_section_by_head(&xml, None, Some(hq)).unwrap_or_else(|| extract_text(&xml)), "head-query".to_string())
            } else if let Some(hi) = args.get("headIndex").and_then(|v| v.as_u64()) { 
                (extract_section_by_head(&xml, Some(hi as usize), None).unwrap_or_else(|| extract_text(&xml)), "head-index".to_string()) 
            } else { 
                (extract_text(&xml), "full".to_string()) 
            };
            // フォールバックA：抽出が空で、同ベースの連番ファイルがある場合は最小番号を開く
            if text.trim().is_empty() {
                if let Some(stem) = cur_path.file_stem().and_then(|s| s.to_str()) {
                    let ends_with_digit = stem.chars().last().map(|c| c.is_ascii_digit()).unwrap_or(false);
                    if !ends_with_digit {
                        if let Some(candidate) = find_tipitaka_content_for_base(stem) {
                            if candidate != cur_path {
                                cur_path = candidate.clone();
                                xml = fs::read(&candidate).map(|b| decode_xml_bytes(&b)).unwrap_or_default();
                                let (t2, m2) = if let Some(hq) = args.get("headQuery").and_then(|v| v.as_str()) { (extract_section_by_head(&xml, None, Some(hq)).unwrap_or_else(|| extract_text(&xml)), "head-query+base-fallback".to_string())
                                            } else if let Some(hi) = args.get("headIndex").and_then(|v| v.as_u64()) { (extract_section_by_head(&xml, Some(hi as usize), None).unwrap_or_else(|| extract_text(&xml)), "head-index+base-fallback".to_string()) } else { (extract_text(&xml), "full+base-fallback".to_string()) };
                                text = t2; extraction_method = m2;
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
                    if !t.trim().is_empty() { text = t; extraction_method = "plain-strip-tags".to_string(); }
                }
            }
            let mut sliced = slice_text(&text, &args);
            // enforce output cap
            let cap = default_max_chars();
            if sliced.chars().count() > cap { sliced = sliced.chars().take(cap).collect(); }
            // Optional highlight for Tipitaka
            let hl_in = args.get("highlight").and_then(|v| v.as_str());
            let mut hl_regex = args.get("highlightRegex").and_then(|v| v.as_bool()).unwrap_or(false);
            let hpre = args.get("highlightPrefix").and_then(|v| v.as_str()).map(|s| s.to_string())
                .or_else(|| std::env::var("DAIZO_HL_PREFIX").ok()).unwrap_or_else(|| ">>> ".to_string());
            let hsuf = args.get("highlightSuffix").and_then(|v| v.as_str()).map(|s| s.to_string())
                .or_else(|| std::env::var("DAIZO_HL_SUFFIX").ok()).unwrap_or_else(|| " <<<".to_string());
            let mut highlight_positions: Vec<serde_json::Value> = Vec::new();
            let mut highlight_count = 0usize;
            if let Some(hpat0) = hl_in {
                let looks_like_regex = hpat0.chars().any(|c| ".+*?[](){}|\\".contains(c));
                let hpat = if hpat0.chars().any(|c| c.is_whitespace()) && !looks_like_regex && !hl_regex { hl_regex = true; to_whitespace_fuzzy_literal(hpat0) } else { hpat0.to_string() };
                if hl_regex {
                    if let Ok(re) = Regex::new(&hpat) {
                        for m in re.find_iter(&sliced) {
                            let sb = m.start(); let eb = m.end();
                            let sc = sliced[..sb].chars().count(); let ec = sc + sliced[sb..eb].chars().count();
                            highlight_positions.push(json!({"startChar": sc, "endChar": ec}));
                        }
                        let mut count = 0usize;
                        let replaced = re.replace_all(&sliced, |caps: &regex::Captures| { count += 1; format!("{}{}{}", hpre, &caps[0], hsuf) });
                        sliced = replaced.into_owned();
                        highlight_count = count;
                    }
                } else if !hpat.is_empty() {
                    let mut i = 0usize;
                    while let Some(pos) = sliced[i..].find(&hpat) {
                        let abs = i + pos; let sc = sliced[..abs].chars().count(); let ec = sc + hpat.chars().count();
                        highlight_positions.push(json!({"startChar": sc, "endChar": ec}));
                        i = abs + hpat.len();
                    }
                    let mut out = String::with_capacity(sliced.len()); let mut j = 0usize;
                    while let Some(pos) = sliced[j..].find(&hpat) {
                        let abs = j + pos; out.push_str(&sliced[j..abs]); out.push_str(&hpre); out.push_str(&hpat); out.push_str(&hsuf); j = abs + hpat.len(); highlight_count += 1; }
                    out.push_str(&sliced[j..]); sliced = out;
                }
            }
            let heads = list_heads_generic(&xml);
            let hl = args.get("headingsLimit").and_then(|v| v.as_u64()).unwrap_or(10) as usize;
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
            let titles_only = args.get("titlesOnly").and_then(|v| v.as_bool()).unwrap_or(false);
            let fields = args.get("fields").and_then(|v| v.as_str()).unwrap_or("id,fascnm,startid,endid");
            let fq: Vec<String> = args.get("fq").and_then(|v| v.as_array()).map(|a| a.iter().filter_map(|x| x.as_str().map(|s| s.to_string())).collect()).unwrap_or_default();
            if let Some(jsonv) = sat_wrap7_search_json(q, rows, offs, fields, &fq) {
                let docs_v = jsonv.get("response").and_then(|r| r.get("docs")).cloned().unwrap_or(json!([]));
                let count = jsonv.get("response").and_then(|r| r.get("numFound")).and_then(|v| v.as_u64()).unwrap_or(0);
                let meta_base = json!({ "count": count, "results": docs_v, "titlesOnly": titles_only, "fl": fields, "fq": fq });
                let auto = args.get("autoFetch").and_then(|v| v.as_bool()).unwrap_or(false);
                if auto {
                    let docs = jsonv.get("response").and_then(|r| r.get("docs")).and_then(|v| v.as_array()).cloned().unwrap_or_default();
                    if docs.is_empty() {
                        let summary = "0 results".to_string();
                        return json!({ "jsonrpc":"2.0","id": id, "result": { "content": [{"type":"text","text": summary }], "_meta": meta_base }});
                    }
                    let mut best_i = 0usize; let mut best_sc = -1f32;
                    for (i, d) in docs.iter().enumerate() {
                        let title = d.get("fascnm").and_then(|v| v.as_str()).unwrap_or("");
                        let sc = title_score(title, q);
                        if sc > best_sc { best_sc = sc; best_i = i; }
                    }
                    let chosen = &docs[best_i];
                    let useid = chosen.get("startid").and_then(|v| v.as_str()).unwrap_or("");
                    let url = sat_detail_build_url(useid);
                    let t = sat_fetch(&url);
                    let start = args.get("startChar").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
                    let maxc = args.get("maxChars").and_then(|v| v.as_u64()).unwrap_or(8000) as usize;
                    let end = std::cmp::min(t.len(), start+maxc);
                    let sliced = t.get(start..end).unwrap_or("").to_string();
                    let mut meta = meta_base;
                    meta["chosen"] = chosen.clone();
                    meta["titleScore"] = json!(best_sc);
                    meta["sourceUrl"] = json!(url);
                    meta["returnedStart"] = json!(start as u64);
                    meta["returnedEnd"] = json!(end as u64);
                    meta["totalLength"] = json!(t.len());
                    meta["truncated"] = json!(end < t.len());
                    meta["extractionMethod"] = json!("sat-detail-extract");
                    return json!({ "jsonrpc":"2.0","id": id, "result": { "content": [{"type":"text","text": sliced }], "_meta": meta }});
                } else {
                    let summary = if titles_only { format!("{} titles; see _meta.results", count) } else { format!("{} results; see _meta.results", count) };
                    return json!({ "jsonrpc":"2.0","id": id, "result": { "content": [{"type":"text","text": summary }], "_meta": meta_base }});
                }
            } else {
                let hits = sat_search_results(q, rows, offs, exact, titles_only);
                let meta = json!({ "count": hits.len(), "results": hits, "titlesOnly": titles_only });
                let summary = if titles_only { format!("{} titles; see _meta.results", meta["count"].as_u64().unwrap_or(0)) } else { format!("{} results; see _meta.results", meta["count"].as_u64().unwrap_or(0)) };
                return json!({ "jsonrpc":"2.0","id": id, "result": { "content": [{"type":"text","text": summary }], "_meta": meta }});
            }
        }
        "sat_fetch" => {
            // Prefer building URL from useid (startid). Fallback to direct url.
            let url = if let Some(uid) = args.get("useid").and_then(|v| v.as_str()) {
                sat_detail_build_url(uid)
            } else { args.get("url").and_then(|v| v.as_str()).unwrap_or("").to_string() };
            let start = args.get("startChar").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
            let maxc = args.get("maxChars").and_then(|v| v.as_u64()).unwrap_or(8000) as usize;
            let t = sat_fetch(&url);
            let end = std::cmp::min(t.len(), start+maxc);
            let sliced = t.get(start..end).unwrap_or("").to_string();
            let meta = json!({
                "totalLength": t.len(),
                "returnedStart": start as u64,
                "returnedEnd": end as u64,
                "truncated": end < t.len(),
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
            let maxc = args.get("maxChars").and_then(|v| v.as_u64()).unwrap_or(8000) as usize;
            let t = sat_fetch(&url);
            let end = std::cmp::min(t.len(), start+maxc);
            let sliced = t.get(start..end).unwrap_or("").to_string();
            let meta = json!({
                "totalLength": t.len(),
                "returnedStart": start as u64,
                "returnedEnd": end as u64,
                "truncated": end < t.len(),
                "sourceUrl": url,
                "extractionMethod": "sat-detail-extract"
            });
            return json!({"jsonrpc":"2.0","id": id, "result": { "content": [{"type":"text","text": sliced}], "_meta": meta }});
        }
        "sat_pipeline" => {
            let q = args.get("query").and_then(|v| v.as_str()).unwrap_or("");
            let rows = args.get("rows").and_then(|v| v.as_u64()).unwrap_or(100) as usize;
            let offs = args.get("offs").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
            let fields = args.get("fields").and_then(|v| v.as_str()).unwrap_or("id,fascnm,startid,endid");
            let fq: Vec<String> = args.get("fq").and_then(|v| v.as_array()).map(|a| a.iter().filter_map(|x| x.as_str().map(|s| s.to_string())).collect()).unwrap_or_default();
            let start = args.get("startChar").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
            let maxc = args.get("maxChars").and_then(|v| v.as_u64()).unwrap_or(8000) as usize;
            if let Some(jsonv) = sat_wrap7_search_json(q, rows, offs, fields, &fq) {
                let docs = jsonv.get("response").and_then(|r| r.get("docs")).and_then(|v| v.as_array()).cloned().unwrap_or_default();
                if docs.is_empty() { return json!({"jsonrpc":"2.0","id": id, "result": { "content": [{"type":"text","text": "no results"}], "_meta": {"count": 0} }}); }
                // pick best by title score
                let mut best_i = 0usize; let mut best_sc = -1f32;
                for (i, d) in docs.iter().enumerate() {
                    let title = d.get("fascnm").and_then(|v| v.as_str()).unwrap_or("");
                    let sc = title_score(title, q);
                    if sc > best_sc { best_sc = sc; best_i = i; }
                }
                let chosen = &docs[best_i];
                let useid = chosen.get("startid").and_then(|v| v.as_str()).unwrap_or("");
                let url = sat_detail_build_url(useid);
                let t = sat_fetch(&url);
                let end = std::cmp::min(t.len(), start+maxc);
                let sliced = t.get(start..end).unwrap_or("").to_string();
                let count = jsonv.get("response").and_then(|r| r.get("numFound")).and_then(|v| v.as_u64()).unwrap_or(0);
                let meta = json!({
                    "totalLength": t.len(),
                    "returnedStart": start as u64,
                    "returnedEnd": end as u64,
                    "truncated": end < t.len(),
                    "sourceUrl": url,
                    "extractionMethod": "sat-detail-extract",
                    "search": {"rows": rows, "offs": offs, "fl": fields, "fq": fq, "count": count},
                    "chosen": chosen,
                    "titleScore": best_sc
                });
                return json!({"jsonrpc":"2.0","id": id, "result": { "content": [{"type":"text","text": sliced}], "_meta": meta }});
            } else {
                return json!({"jsonrpc":"2.0","id": id, "result": { "content": [{"type":"text","text": "no results"}], "_meta": {"count": 0} }});
            }
        }
        "cbeta_search" => {
            let q_raw = args.get("query").and_then(|v| v.as_str()).unwrap_or("");
            let looks_like_regex = q_raw.chars().any(|c| ".+*?[](){}|\\".contains(c));
            let q = if q_raw.chars().any(|c| c.is_whitespace()) && !looks_like_regex {
                to_whitespace_fuzzy_literal(q_raw)
            } else { q_raw.to_string() };
            let max_results = args.get("maxResults").and_then(|v| v.as_u64()).unwrap_or(20) as usize;
            let max_matches_per_file = args.get("maxMatchesPerFile").and_then(|v| v.as_u64()).unwrap_or(5) as usize;
            
            ensure_cbeta_data();
            let results = cbeta_grep(&cbeta_root(), &q, max_results, max_matches_per_file);
            
            let mut summary = format!("Found {} files with matches for '{}':\n\n", results.len(), q);
            for (i, result) in results.iter().enumerate() {
                summary.push_str(&format!("{}. {} ({})\n", i + 1, result.title, result.file_id));
                summary.push_str(&format!("   {} matches, {}\n", result.total_matches, 
                    result.fetch_hints.total_content_size.as_deref().unwrap_or("unknown size")));
                
                for (j, m) in result.matches.iter().enumerate().take(2) {
                    summary.push_str(&format!("   Match {}: ...{}...\n", j + 1, 
                        m.context.chars().take(100).collect::<String>()));
                }
                if result.matches.len() > 2 {
                    summary.push_str(&format!("   ... and {} more matches\n", result.matches.len() - 2));
                }
                
                if !result.fetch_hints.recommended_parts.is_empty() {
                    summary.push_str(&format!("   Recommended parts: {}\n", 
                        result.fetch_hints.recommended_parts.join(", ")));
                }
                summary.push('\n');
            }
            // Lightweight next-call hints for AI clients (low token cost)
            let hint_top = std::env::var("DAIZO_HINT_TOP").ok().and_then(|s| s.parse::<usize>().ok()).unwrap_or(1);
            let mut fetch_suggestions: Vec<serde_json::Value> = Vec::new();
            for r in results.iter().take(hint_top) {
                if let Some(m) = r.matches.first() { if let Some(ln) = m.line_number {
                    fetch_suggestions.push(json!({
                        "tool": "cbeta_fetch",
                        "args": {"id": r.file_id, "lineNumber": ln, "contextBefore": 1, "contextAfter": 3},
                        "mode": "low-cost"
                    }));
                }}
            }
            let mut meta = json!({
                "searchPattern": q,
                "totalFiles": results.len(),
                "results": results,
                "hint": "Use cbeta_fetch (id + lineNumber) for low-cost context; cbeta_pipeline with autoFetch=false to summarize",
                "fetchSuggestions": fetch_suggestions
            });
            // Optional pipeline hint (kept minimal)
            meta["pipelineHint"] = json!({
                "tool": "cbeta_pipeline",
                "args": {"query": q, "autoFetch": false, "maxResults": 5, "maxMatchesPerFile": 1, "includeHighlightSnippet": false, "includeMatchLine": true }
            });
            
            return json!({"jsonrpc":"2.0","id": id, "result": { "content": [{"type":"text","text": summary}], "_meta": meta }});
        }
        "cbeta_pipeline" => {
            let q = args.get("query").and_then(|v| v.as_str()).unwrap_or("");
            let max_results = args.get("maxResults").and_then(|v| v.as_u64()).unwrap_or(10) as usize;
            let max_matches_per_file = args.get("maxMatchesPerFile").and_then(|v| v.as_u64()).unwrap_or(3) as usize;
            let context_before = args.get("contextBefore").and_then(|v| v.as_u64()).unwrap_or(10) as usize;
            let context_after = args.get("contextAfter").and_then(|v| v.as_u64()).unwrap_or(100) as usize;
            let auto_fetch = args.get("autoFetch").and_then(|v| v.as_bool()).unwrap_or(false);
            let auto_fetch_files = args.get("autoFetchFiles").and_then(|v| v.as_u64()).map(|x| x as usize).unwrap_or_else(|| if auto_fetch { default_auto_files() } else { 0 });
            let auto_fetch_matches = args.get("autoFetchMatches").and_then(|v| v.as_u64()).map(|x| x as usize);
            let include_match_line = args.get("includeMatchLine").and_then(|v| v.as_bool()).unwrap_or(true);
            let include_highlight_snippet = args.get("includeHighlightSnippet").and_then(|v| v.as_bool()).unwrap_or(true);
            let min_snippet_len = args.get("minSnippetLen").and_then(|v| v.as_u64()).map(|x| x as usize).unwrap_or(default_snippet_len());
            // highlight/snippet markers with env fallbacks
            let hl_in = args.get("highlight").and_then(|v| v.as_str());
            let mut hl_regex = args.get("highlightRegex").and_then(|v| v.as_bool()).unwrap_or(false);
            let hl_pat: Option<String> = hl_in.map(|p| {
                let looks_like_regex_hl = p.chars().any(|c| ".+*?[](){}|\\".contains(c));
                if p.chars().any(|c| c.is_whitespace()) && !looks_like_regex_hl && !hl_regex {
                    hl_regex = true; // 空白を含む素朴な文字列 → \\s* に畳んだ正規表現として扱う
                    to_whitespace_fuzzy_literal(p)
                } else { p.to_string() }
            });
            let hl_pre = args.get("highlightPrefix").and_then(|v| v.as_str()).map(|s| s.to_string())
                .or_else(|| std::env::var("DAIZO_HL_PREFIX").ok())
                .unwrap_or_else(|| ">>> ".to_string());
            let hl_suf = args.get("highlightSuffix").and_then(|v| v.as_str()).map(|s| s.to_string())
                .or_else(|| std::env::var("DAIZO_HL_SUFFIX").ok())
                .unwrap_or_else(|| " <<<".to_string());
            let snip_pre = args.get("snippetPrefix").and_then(|v| v.as_str()).map(|s| s.to_string())
                .or_else(|| std::env::var("DAIZO_SNIPPET_PREFIX").ok())
                .unwrap_or_else(|| ">>> ".to_string());
            let snip_suf = args.get("snippetSuffix").and_then(|v| v.as_str()).map(|s| s.to_string())
                .or_else(|| std::env::var("DAIZO_SNIPPET_SUFFIX").ok())
                .unwrap_or_else(String::new);
            let full = args.get("full").and_then(|v| v.as_bool()).unwrap_or(false);
            let include_notes = args.get("includeNotes").and_then(|v| v.as_bool()).unwrap_or(false);

            ensure_cbeta_data();
            let results = cbeta_grep(&cbeta_root(), &q, max_results, max_matches_per_file);

            // Build summary and suggestions
            let mut summary = format!("Found {} files with matches for '{}':\n\n", results.len(), q);
            let mut suggestions: Vec<serde_json::Value> = Vec::new();
            for (i, result) in results.iter().enumerate() {
                summary.push_str(&format!("{}. {} ({})\n", i + 1, result.title, result.file_id));
                summary.push_str(&format!("   {} matches\n", result.total_matches));
                for (j, m) in result.matches.iter().enumerate().take(2) {
                    summary.push_str(&format!("   Match {}: ...{}...\n", j + 1, m.context.chars().take(100).collect::<String>()));
                }
                if let Some(m) = result.matches.first() {
                    if let Some(ln) = m.line_number { suggestions.push(json!({
                        "tool": "cbeta_fetch", "args": {"id": result.file_id, "lineNumber": ln, "contextBefore": context_before, "contextAfter": context_after}
                    })); }
                }
                summary.push('\n');
            }

            let mut content_items: Vec<serde_json::Value> = vec![json!({"type":"text","text": summary})];
            let mut meta = json!({
                "searchPattern": q,
                "totalFiles": results.len(),
                "results": results,
                "fetchSuggestions": suggestions
            });

            if auto_fetch && auto_fetch_files > 0 {
                let take_files = std::cmp::min(auto_fetch_files, results.len());
                let mut fetched: Vec<serde_json::Value> = Vec::new();
                for r in results.iter().take(take_files) {
                    let xml = fs::read_to_string(&r.file_path).unwrap_or_default();
                    if full {
                        let text = if include_notes { extract_text_opts(&xml, true) } else { extract_text(&xml) };
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
                        if include_highlight_snippet { per_file_limit = per_file_limit.min(default_auto_matches()); }
                        for m in r.matches.iter().take(per_file_limit) {
                            if let Some(ln) = m.line_number {
                                let mut ctx = daizo_core::extract_xml_around_line_asymmetric(&xml, ln, context_before, context_after);
                                if !include_match_line {
                                    // best-effort: remove central line (position = context_before)
                                    let mut lines: Vec<&str> = ctx.lines().collect();
                                    if context_before < lines.len() { lines.remove(context_before); }
                                    ctx = lines.join("\n");
                                }
                                if !ctx.trim().is_empty() {
                        if !combined.is_empty() { combined.push_str("\n\n---\n\n"); }
                        // Prefer compact snippet; avoid dumping full context unless explicitly requested
                        if include_highlight_snippet && !m.highlight.trim().is_empty() && m.highlight.trim().chars().count() >= min_snippet_len {
                            combined.push_str(&format!("{}{}{}", snip_pre, m.highlight.trim(), snip_suf));
                        }
                        // optional in-context highlighting and positions per context
                        let mut chigh: Vec<serde_json::Value> = Vec::new();
                        if let Some(pats) = hl_pat.as_deref() {
                            if hl_regex {
                                if let Ok(re) = regex::Regex::new(pats) {
                                    for mm in re.find_iter(&ctx) {
                                        let sb = mm.start(); let eb = mm.end();
                                        let sc = ctx[..sb].chars().count(); let ec = sc + ctx[sb..eb].chars().count();
                                        chigh.push(json!({"startChar": sc, "endChar": ec}));
                                    }
                                    ctx = re.replace_all(&ctx, |caps: &regex::Captures| format!("{}{}{}", hl_pre, &caps[0], hl_suf)).into_owned();
                                }
                            } else if !pats.is_empty() {
                                let mut i = 0usize;
                                while let Some(pos) = ctx[i..].find(pats) {
                                    let abs = i + pos; let sc = ctx[..abs].chars().count(); let ec = sc + pats.chars().count();
                                    chigh.push(json!({"startChar": sc, "endChar": ec}));
                                    i = abs + pats.len();
                                }
                                let mut out = String::with_capacity(ctx.len()); let mut j = 0usize;
                                while let Some(pos) = ctx[j..].find(pats) {
                                    let abs = j + pos; out.push_str(&ctx[j..abs]); out.push_str(&hl_pre); out.push_str(pats); out.push_str(&hl_suf); j = abs + pats.len();
                                }
                                out.push_str(&ctx[j..]); ctx = out;
                            }
                        }
                        if !include_highlight_snippet {
                            combined.push_str(&format!("# {} (line {})\n\n{}", r.file_id, ln, ctx));
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
                            if highlight_counts.iter().any(|&c| c > 0) { fobj["highlightCounts"] = json!(highlight_counts); }
                            fobj["highlightPositions"] = json!(file_highlights);
                            fetched.push(fobj);
                        }
                    }
                }
                if !fetched.is_empty() { meta["autoFetched"] = json!(fetched); }
            }

            return json!({"jsonrpc":"2.0","id": id, "result": { "content": content_items, "_meta": meta }});
        }
        "gretil_title_search" => {
            let q_raw = args.get("query").and_then(|v| v.as_str()).unwrap_or("");
            let looks_like_regex = q_raw.chars().any(|c| ".+*?[](){}|\\".contains(c));
            let q = if q_raw.chars().any(|c| c.is_whitespace()) && !looks_like_regex {
                to_whitespace_fuzzy_literal(q_raw)
            } else { q_raw.to_string() };
            let limit = args.get("limit").and_then(|v| v.as_u64()).unwrap_or(10) as usize;
            let idx = load_or_build_gretil_index();
            let hits = best_match_gretil(&idx, &q, limit);
            let summary = hits.iter().enumerate().map(|(i,h)| format!("{}. {}  {}", i+1, h.entry.id, h.entry.title)).collect::<Vec<_>>().join("\n");
            let results: Vec<_> = hits.iter().map(|h| json!({
                "id": h.entry.id,
                "title": h.entry.title,
                "path": h.entry.path,
                "score": h.score
            })).collect();
            let meta = json!({ "count": results.len(), "results": results });
            return json!({"jsonrpc":"2.0","id": id, "result": { "content": [{"type":"text","text": summary }], "_meta": meta }});
        }
        "gretil_fetch" => {
            let mut matched_id: Option<String> = None;
            let mut matched_title: Option<String> = None;
            let mut matched_score: Option<f32> = None;
            let mut path: PathBuf = PathBuf::new();
            if let Some(id) = args.get("id").and_then(|v| v.as_str()) {
                let idx = load_or_build_gretil_index();
                if let Some(p) = daizo_core::path_resolver::resolve_gretil_by_id(&idx, id) {
                    matched_id = Path::new(&p).file_stem().map(|s| s.to_string_lossy().into_owned());
                    if let Some(e) = idx.iter().find(|e| e.path == p.to_string_lossy()) { matched_title = Some(e.title.clone()); }
                    path = p;
                }
            } else if let Some(q) = args.get("query").and_then(|v| v.as_str()) {
                let idx = load_or_build_gretil_index();
                if let Some(hit) = best_match_gretil(&idx, q, 1).into_iter().next() {
                    matched_title = Some(hit.entry.title.clone());
                    matched_score = Some(hit.score);
                    matched_id = Path::new(&hit.entry.path).file_stem().map(|s| s.to_string_lossy().into_owned());
                    path = PathBuf::from(&hit.entry.path);
                }
            }
            if path.as_os_str().is_empty() { return json!({"jsonrpc":"2.0","id": id, "result": { "content": [{"type":"text","text": "not found"}] }}); }
            let xml = fs::read_to_string(&path).unwrap_or_default();
            let include_notes = args.get("includeNotes").and_then(|v| v.as_bool()).unwrap_or(false);
            let (text, extraction_method) = if let Some(line_num) = args.get("lineNumber").and_then(|v| v.as_u64()) {
                let before = args.get("contextBefore").and_then(|v| v.as_u64()).unwrap_or(args.get("contextLines").and_then(|v| v.as_u64()).unwrap_or(10)) as usize;
                let after = args.get("contextAfter").and_then(|v| v.as_u64()).unwrap_or(args.get("contextLines").and_then(|v| v.as_u64()).unwrap_or(100)) as usize;
                let context_text = daizo_core::extract_xml_around_line_asymmetric(&xml, line_num as usize, before, after);
                (context_text, format!("line-context-{}-{}-{}", line_num, before, after))
            } else {
                (extract_text_opts(&xml, include_notes), "full".to_string())
            };
            let full_flag = args.get("full").and_then(|v| v.as_bool()).unwrap_or(false);
            let mut sliced = if full_flag { text.clone() } else { slice_text(&text, &args) };
            let mut highlight_count = 0usize; let mut highlight_positions: Vec<serde_json::Value> = Vec::new();
            if let Some(hpat) = args.get("highlight").and_then(|v| v.as_str()) {
                let use_re = args.get("highlightRegex").and_then(|v| v.as_bool()).unwrap_or(false);
                let hpre = args.get("highlightPrefix").and_then(|v| v.as_str()).unwrap_or(">>> ");
                let hsuf = args.get("highlightSuffix").and_then(|v| v.as_str()).unwrap_or(" <<<");
                let original = sliced.clone();
                if use_re {
                    if let Ok(re) = regex::Regex::new(hpat) {
                        for m in re.find_iter(&original) {
                            let sb = m.start(); let eb = m.end();
                            let sc = original[..sb].chars().count(); let ec = sc + original[sb..eb].chars().count();
                            highlight_positions.push(json!({"startChar": sc, "endChar": ec}));
                        }
                        let mut count = 0usize;
                        let replaced = re.replace_all(&sliced, |caps: &regex::Captures| { count += 1; format!("{}{}{}", hpre, &caps[0], hsuf) });
                        sliced = replaced.into_owned(); highlight_count = count;
                    }
                } else if !hpat.is_empty() {
                    let mut i = 0usize;
                    while let Some(pos) = original[i..].find(hpat) { let abs = i + pos; let sc = original[..abs].chars().count(); let ec = sc + hpat.chars().count(); highlight_positions.push(json!({"startChar": sc, "endChar": ec})); i = abs + hpat.len(); }
                    let mut out = String::with_capacity(sliced.len()); let mut j = 0usize;
                    while let Some(pos) = sliced[j..].find(hpat) { let abs = j + pos; out.push_str(&sliced[j..abs]); out.push_str(hpre); out.push_str(hpat); out.push_str(hsuf); j = abs + hpat.len(); highlight_count += 1; }
                    out.push_str(&sliced[j..]); sliced = out;
                }
            }
            // cap output
            let cap = default_max_chars();
            if sliced.chars().count() > cap { sliced = sliced.chars().take(cap).collect(); }
            let heads = list_heads_generic(&xml);
            let hl = args.get("headingsLimit").and_then(|v| v.as_u64()).unwrap_or(10) as usize;
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
            let q = if q_raw.chars().any(|c| c.is_whitespace()) && !looks_like_regex { to_whitespace_fuzzy_literal(q_raw) } else { q_raw.to_string() };
            let max_results = args.get("maxResults").and_then(|v| v.as_u64()).unwrap_or(20) as usize;
            let max_matches_per_file = args.get("maxMatchesPerFile").and_then(|v| v.as_u64()).unwrap_or(5) as usize;
            let results = gretil_grep(&gretil_root(), &q, max_results, max_matches_per_file);
            let mut summary = format!("Found {} files with matches for '{}':\n\n", results.len(), q);
            for (i, result) in results.iter().enumerate() {
                summary.push_str(&format!("{}. {} ({})\n", i + 1, result.title, result.file_id));
                summary.push_str(&format!("   {} matches, {}\n", result.total_matches, result.fetch_hints.total_content_size.as_deref().unwrap_or("unknown size")));
                for (j, m) in result.matches.iter().enumerate().take(2) { summary.push_str(&format!("   Match {}: ...{}...\n", j + 1, m.context.chars().take(100).collect::<String>())); }
                if result.matches.len() > 2 { summary.push_str(&format!("   ... and {} more matches\n", result.matches.len() - 2)); }
                summary.push('\n');
            }
            // Lightweight next-call hints (low token) for GRETIL
            let hint_top = std::env::var("DAIZO_HINT_TOP").ok().and_then(|s| s.parse::<usize>().ok()).unwrap_or(1);
            let mut fetch_suggestions: Vec<serde_json::Value> = Vec::new();
            for r in results.iter().take(hint_top) {
                if let Some(m) = r.matches.first() { if let Some(ln) = m.line_number {
                    fetch_suggestions.push(json!({
                        "tool": "gretil_fetch",
                        "args": {"id": r.file_id, "lineNumber": ln, "contextBefore": 1, "contextAfter": 3},
                        "mode": "low-cost"
                    }));
                }}
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
            let q = if q_raw.chars().any(|c| c.is_whitespace()) && !looks_like_regex { to_whitespace_fuzzy_literal(q_raw) } else { q_raw.to_string() };
            let context_before = args.get("contextBefore").and_then(|v| v.as_u64()).unwrap_or(10) as usize;
            let context_after = args.get("contextAfter").and_then(|v| v.as_u64()).unwrap_or(100) as usize;
            let max_results = args.get("maxResults").and_then(|v| v.as_u64()).unwrap_or(10) as usize;
            let max_matches_per_file = args.get("maxMatchesPerFile").and_then(|v| v.as_u64()).unwrap_or(3) as usize;
            let include_match_line = args.get("includeMatchLine").and_then(|v| v.as_bool()).unwrap_or(true);
            let results = gretil_grep(&gretil_root(), &q, max_results, max_matches_per_file);
            let mut content_items: Vec<serde_json::Value> = Vec::new();
            let mut meta = json!({ "searchPattern": q, "totalFiles": results.len(), "results": results });
            let summary = format!("Found {} files with matches for '{}'", results.len(), q);
            content_items.push(json!({"type":"text","text": summary}));
            if args.get("autoFetch").and_then(|v| v.as_bool()).unwrap_or(false) {
                let full = args.get("full").and_then(|v| v.as_bool()).unwrap_or(false);
                let include_notes = args.get("includeNotes").and_then(|v| v.as_bool()).unwrap_or(false);
                let tf = args.get("autoFetchFiles").and_then(|v| v.as_u64()).unwrap_or(1) as usize;
                let tf = tf.min(results.len());
                let mut fetched: Vec<serde_json::Value> = Vec::new();
                let hl_pre = args.get("highlightPrefix").and_then(|v| v.as_str()).map(|s| s.to_string())
                    .or_else(|| std::env::var("DAIZO_HL_PREFIX").ok())
                    .unwrap_or_else(|| ">>> ".to_string());
                let hl_suf = args.get("highlightSuffix").and_then(|v| v.as_str()).map(|s| s.to_string())
                    .or_else(|| std::env::var("DAIZO_HL_SUFFIX").ok())
                    .unwrap_or_else(|| " <<<".to_string());
                let sn_pre = args.get("snippetPrefix").and_then(|v| v.as_str()).map(|s| s.to_string())
                    .or_else(|| std::env::var("DAIZO_SNIPPET_PREFIX").ok())
                    .unwrap_or_else(|| ">>> ".to_string());
                let sn_suf = args.get("snippetSuffix").and_then(|v| v.as_str()).map(|s| s.to_string())
                    .or_else(|| std::env::var("DAIZO_SNIPPET_SUFFIX").ok())
                    .unwrap_or_else(|| "".to_string());
                let mut file_highlights_all: Vec<Vec<serde_json::Value>> = Vec::new();
                for r in results.iter().take(tf) {
                    let per_file_limit = args.get("autoFetchMatches").and_then(|v| v.as_u64()).unwrap_or(max_matches_per_file as u64) as usize;
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
                                let mut ctx = daizo_core::extract_xml_around_line_asymmetric(&xml, ln, context_before, context_after);
                                let mut chigh: Vec<serde_json::Value> = Vec::new();
                                if let Some(pat) = args.get("highlight").and_then(|v| v.as_str()) {
                                    let looks_like = pat.chars().any(|c| ".+*?[](){}|\\".contains(c));
                                    let mut hlr = args.get("highlightRegex").and_then(|v| v.as_bool()).unwrap_or(false);
                                    let pat = if pat.chars().any(|c| c.is_whitespace()) && !looks_like && !hlr { hlr = true; to_whitespace_fuzzy_literal(pat) } else { pat.to_string() };
                                    if hlr { if let Ok(re) = regex::Regex::new(&pat) {
                                        for mm in re.find_iter(&ctx) { let sb = mm.start(); let eb = mm.end(); let sc = ctx[..sb].chars().count(); let ec = sc + ctx[sb..eb].chars().count(); chigh.push(json!({"startChar": sc, "endChar": ec})); }
                                        let mut ct = 0usize; let rep = re.replace_all(&ctx, |caps: &regex::Captures| { ct += 1; format!("{}{}{}", hl_pre, &caps[0], hl_suf) }); ctx = rep.into_owned(); highlight_counts.push(ct);
                                    }} else if !pat.is_empty() {
                                        let mut i = 0usize; while let Some(pos) = ctx[i..].find(&pat) { let abs = i + pos; let sc = ctx[..abs].chars().count(); let ec = sc + pat.chars().count(); chigh.push(json!({"startChar": sc, "endChar": ec})); i = abs + pat.len(); }
                                    let mut out = String::with_capacity(ctx.len()); let mut j = 0usize; let mut ct = 0usize; while let Some(pos) = ctx[j..].find(&pat) { let abs = j + pos; out.push_str(&ctx[j..abs]); out.push_str(&hl_pre); out.push_str(&pat); out.push_str(&hl_suf); j = abs + pat.len(); ct += 1; } out.push_str(&ctx[j..]); ctx = out; highlight_counts.push(ct);
                                    }
                                }
                                if args.get("includeHighlightSnippet").and_then(|v| v.as_bool()).unwrap_or(true) {
                                    let min_len = args.get("minSnippetLen").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
                                    let snip = ctx.chars().take(std::cmp::max(min_len, 120)).collect::<String>();
                                    combined.push_str(&format!("{}{}{}\n", &sn_pre, snip, &sn_suf));
                                } else {
                                    combined.push_str(&format!("# {}{}\n\n{}", r.file_id, if include_match_line { format!(" (line {})", ln) } else { String::new() }, ctx));
                                }
                                file_highlights.push(json!(chigh));
                            }
                        }
                        if !combined.is_empty() {
                            content_items.push(json!({"type":"text","text": combined}));
                            let mut fobj = json!({"id": r.file_id, "full": false, "contextBefore": context_before, "contextAfter": context_after, "includeMatchLine": include_match_line});
                            if highlight_counts.iter().any(|&c| c > 0) { fobj["highlightCounts"] = json!(highlight_counts); }
                            fobj["highlightPositions"] = json!(file_highlights);
                            fetched.push(fobj);
                        }
                        file_highlights_all.push(file_highlights);
                    }
                }
                if !fetched.is_empty() { meta["autoFetched"] = json!(fetched); }
            }
            return json!({"jsonrpc":"2.0","id": id, "result": { "content": content_items, "_meta": meta }});
        }
        "tipitaka_search" => {
            let q_raw = args.get("query").and_then(|v| v.as_str()).unwrap_or("");
            let looks_like_regex = q_raw.chars().any(|c| ".+*?[](){}|\\".contains(c));
            let q = if q_raw.chars().any(|c| c.is_whitespace()) && !looks_like_regex {
                to_whitespace_fuzzy_literal(q_raw)
            } else { q_raw.to_string() };
            let max_results = args.get("maxResults").and_then(|v| v.as_u64()).unwrap_or(20) as usize;
            let max_matches_per_file = args.get("maxMatchesPerFile").and_then(|v| v.as_u64()).unwrap_or(5) as usize;
            
            ensure_tipitaka_data();
            let results = tipitaka_grep(&tipitaka_root(), &q, max_results, max_matches_per_file);
            
            let mut summary = format!("Found {} files with matches for '{}':\n\n", results.len(), q);
            for (i, result) in results.iter().enumerate() {
                summary.push_str(&format!("{}. {} ({})\n", i + 1, result.title, result.file_id));
                summary.push_str(&format!("   {} matches, {}\n", result.total_matches, 
                    result.fetch_hints.total_content_size.as_deref().unwrap_or("unknown size")));
                
                for (j, m) in result.matches.iter().enumerate().take(2) {
                    summary.push_str(&format!("   Match {}: ...{}...\n", j + 1, 
                        m.context.chars().take(100).collect::<String>()));
                }
                if result.matches.len() > 2 {
                    summary.push_str(&format!("   ... and {} more matches\n", result.matches.len() - 2));
                }
                
                if !result.fetch_hints.structure_info.is_empty() {
                    summary.push_str(&format!("   Structure: {}\n", 
                        result.fetch_hints.structure_info.join(", ")));
                }
                summary.push('\n');
            }
            // Lightweight next-call hints for Tipitaka (no pipeline tool)
            let hint_top = std::env::var("DAIZO_HINT_TOP").ok().and_then(|s| s.parse::<usize>().ok()).unwrap_or(1);
            let mut fetch_suggestions: Vec<serde_json::Value> = Vec::new();
            for r in results.iter().take(hint_top) {
                if let Some(m) = r.matches.first() { if let Some(ln) = m.line_number {
                    fetch_suggestions.push(json!({
                        "tool": "tipitaka_fetch",
                        "args": {"id": r.file_id, "lineNumber": ln, "contextBefore": 1, "contextAfter": 3},
                        "mode": "low-cost"
                    }));
                }}
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
        let rest = &lower[pos+8..];
        let rest_orig = &head[pos+8..];
        let mut i = 0usize;
        while i < rest.len() && (rest[i] as char).is_ascii_whitespace() { i += 1; }
        if i < rest.len() && rest[i] == b'=' { i += 1; }
        while i < rest.len() && (rest[i] as char).is_ascii_whitespace() { i += 1; }
        if i < rest.len() && (rest[i] == b'"' || rest[i] == b'\'') {
            let quote = rest[i];
            i += 1;
            let mut j = i;
            while j < rest.len() && rest[j] != quote { j += 1; }
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
    CLIENT.get_or_init(|| Client::builder()
        .user_agent("daizo-mcp/0.1 (+https://github.com/sinryo/daizo-mcp)")
        .connect_timeout(Duration::from_secs(5))
        .timeout(Duration::from_secs(12))
        .build()
        .expect("reqwest client"))
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
                    match resp.text() { Ok(t) => return Some(t), Err(_) => {} }
                }
                if status.as_u16() == 429 || status.is_server_error() {
                    // retry
                } else {
                    return None;
                }
            }
            Err(e) => { dbg_log(&format!("[http] error attempt={} err={}", attempt+1, e)); }
        }
        attempt += 1;
        if attempt > max_retries { return None; }
        std::thread::sleep(Duration::from_millis(backoff));
        backoff = (backoff.saturating_mul(2)).min(8000);
    }
}

fn sat_fetch(url: &str) -> String {
    let cpath = cache_path_for(url);
    if let Ok(s) = fs::read_to_string(&cpath) { return s; }
    if let Some(txt) = http_get_with_retry(url, 3) {
        let text = extract_sat_text(&txt);
        let _ = fs::write(&cpath, &text);
        return text;
    }
    "".to_string()
}

fn sat_wrap7_build_url(q: &str, rows: usize, offs: usize, fields: &str, fq: &Vec<String>) -> String {
    let mut base = url::Url::parse("https://21dzk.l.u-tokyo.ac.jp/SAT2018/wrap7.php").unwrap();
    base.query_pairs_mut().append_pair("regex", "off");
    // Send the query as-is to wrap7 (caller may include quotes if needed)
    base.query_pairs_mut().append_pair("q", q);
    base.query_pairs_mut().append_pair("rows", &rows.to_string());
    base.query_pairs_mut().append_pair("offs", &offs.to_string());
    base.query_pairs_mut().append_pair("schop", "AND");
    if !fields.trim().is_empty() { base.query_pairs_mut().append_pair("fl", fields); }
    for f in fq { if !f.trim().is_empty() { base.query_pairs_mut().append_pair("fq", f); } }
    base.to_string()
}

fn sat_wrap7_search_json(q: &str, rows: usize, offs: usize, fields: &str, fq: &Vec<String>) -> Option<serde_json::Value> {
    let url = sat_wrap7_build_url(q, rows, offs, fields, fq);
    let cpath = cache_path_for(&url);
    let body = if let Ok(s) = fs::read_to_string(&cpath) { s } else {
        if let Some(txt) = http_get_with_retry(&url, 3) { let _ = fs::write(&cpath, &txt); txt } else { String::new() }
    };
    if body.is_empty() { return None; }
    serde_json::from_str::<serde_json::Value>(&body).ok()
}

fn sat_detail_build_url(useid: &str) -> String {
    format!("https://21dzk.l.u-tokyo.ac.jp/SAT2018/satdb2018pre.php?mode=detail&ob=1&mode2=2&useid={}", urlencoding::encode(useid))
}

fn title_score(title: &str, query: &str) -> f32 {
    let a = normalized(title);
    let b = normalized(query);
    let s_char = jaccard(&a, &b);
    let s_tok = token_jaccard(title, query);
    let mut sc = s_char.max(s_tok);
    if sc < 0.95 && (is_subsequence(&a, &b) || is_subsequence(&b, &a)) { sc = sc.max(0.85); }
    sc
}

fn extract_sat_text(html: &str) -> String {
    let doc = Html::parse_document(html);
    // Prefer SAT detail structure: lines are in span.tx; skip line numbers (.ln) and anchors
    if let Ok(sel) = Selector::parse("span.tx") {
        let mut lines: Vec<String> = Vec::new();
        for node in doc.select(&sel) {
            let t = node.text().collect::<Vec<_>>().join("");
            let t = t.trim();
            if !t.is_empty() { lines.push(t.to_string()); }
        }
        let joined = lines.join("\n");
        let joined = normalize_ws(&joined);
        if joined.len() > 50 { return joined; }
    }
    // Fallbacks
    let candidates = vec!["#text","#viewer","#content","main",".content",".article","#main","#result","#detail","#sattext","pre#text","pre",".text","body"];
    for sel in candidates {
        if let Ok(selector) = Selector::parse(sel) {
            if let Some(node) = doc.select(&selector).next() {
                let t = node.text().collect::<Vec<_>>().join("\n");
                let t = normalize_ws(&t);
                if t.len() > 50 { return t; }
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

fn sat_search_results(q: &str, rows: usize, offs: usize, exact: bool, titles_only: bool) -> Vec<SatHit> {
    // Build JSON API URL
    let mut base = url::Url::parse("https://21dzk.l.u-tokyo.ac.jp/SAT2018/wrap7.php").unwrap();
    base.query_pairs_mut().append_pair("regex", "off");
    let q_param = if exact { format!("\"{}\"", q) } else { q.to_string() };
    base.query_pairs_mut().append_pair("q", &q_param);
    base.query_pairs_mut().append_pair("ttype", "undefined");
    base.query_pairs_mut().append_pair("near", "");
    base.query_pairs_mut().append_pair("amb", "undefined");
    // If titles_only, overfetch to improve chance of collecting enough unique titles
    let rows_query = if titles_only { std::cmp::max(rows * 5, rows) } else { rows };
    base.query_pairs_mut().append_pair("rows", &rows_query.to_string());
    base.query_pairs_mut().append_pair("offs", &offs.to_string());
    base.query_pairs_mut().append_pair("schop", "AND");
    base.query_pairs_mut().append_pair("fq", "");
    let url = base.to_string();

    // Cache raw JSON text with throttle + retry
    let cpath = cache_path_for(&url);
    let body = if let Ok(s) = fs::read_to_string(&cpath) { s } else {
        if let Some(txt) = http_get_with_retry(&url, 3) {
            let _ = fs::write(&cpath, &txt);
            txt
        } else { String::new() }
    };
    if body.is_empty() { return Vec::new(); }

    // Parse JSON and format simple text output
    let v: serde_json::Value = match serde_json::from_str(&body) { Ok(v) => v, Err(_) => return Vec::new() };
    let docs = v.get("response").and_then(|r| r.get("docs")).and_then(|d| d.as_array()).cloned().unwrap_or_default();
    let mut out: Vec<SatHit> = Vec::new();
    for d in docs.into_iter() {
        let title = d.get("fascnm").and_then(|x| x.as_str()).unwrap_or("").to_string();
        let startid = d.get("startid").and_then(|x| x.as_str()).unwrap_or("").to_string();
        let detail = format!(
            "https://21dzk.l.u-tokyo.ac.jp/SAT2018/satdb2018pre.php?mode=detail&ob=1&mode2=2&mode4=&useid={}&cpos=undefined&regsw=off&key={}",
            urlencoding::encode(&startid), urlencoding::encode(q)
        );
        let snippet = if titles_only { String::new() } else { d.get("body").and_then(|x| x.as_str()).unwrap_or("").to_string() };
        let id = d.get("id").and_then(|x| x.as_str()).map(|s| s.to_string());
        out.push(SatHit { title, url: detail, startid, id, snippet });
    }
    if titles_only {
        // Filter by title match against the query (normalized contains), then unique by title
        let nq = normalized(q);
        let mut filtered: Vec<SatHit> = out.into_iter().filter(|h| {
            let ht = normalized(&h.title);
            ht.contains(&nq)
        }).collect();
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

fn extract_section_by_head(xml: &str, head_index: Option<usize>, head_query: Option<&str>) -> Option<String> {
    let re = Regex::new(r"(?is)<head\b[^>]*>(.*?)</head>").ok()?;
    let mut heads: Vec<(usize, usize, String)> = Vec::new();
    for cap in re.captures_iter(xml) {
        let m = cap.get(0).unwrap();
        let text = daizo_core::strip_tags(&cap[1]);
        heads.push((m.start(), m.end(), text));
    }
    if heads.is_empty() { return None; }
    let idx = if let Some(q) = head_query { let ql = q.to_lowercase(); heads.iter().position(|(_,_,t)| t.to_lowercase().contains(&ql))? } else { head_index? };
    let start = heads[idx].1;
    let end = heads.get(idx+1).map(|(s,_,_)| *s).unwrap_or_else(|| xml.len());
    let sect = &xml[start..end];
    Some(extract_text(sect))
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
    let wanted = ["nikaya","title","book","subhead","subsubhead","chapter"];
    let mut count = 0usize;
    let max = 10_000usize; let mut seen = 0usize;
    loop {
        match reader.read_event_into(&mut buf) {
            Ok(quick_xml::events::Event::Start(e)) | Ok(quick_xml::events::Event::Empty(e)) => {
                let name_owned = e.name().as_ref().to_owned();
                let lname = name_owned.rsplit(|b| *b == b':').next().unwrap_or(&name_owned);
                if lname == b"p" {
                    in_p = true; current_buf.clear();
                    current_rend = e.try_get_attribute("rend").ok().flatten().and_then(|a| String::from_utf8(a.value.into_owned()).ok()).map(|s| s.to_ascii_lowercase());
                }
            }
            Ok(quick_xml::events::Event::End(e)) => {
                let name_owned = e.name().as_ref().to_owned();
                let lname = name_owned.rsplit(|b| *b == b':').next().unwrap_or(&name_owned);
                if lname == b"p" && in_p {
                    if let Some(r) = current_rend.take() {
                        if wanted.contains(&r.as_str()) {
                            let val = current_buf.trim();
                            if !val.is_empty() { out.entry(r).or_insert_with(|| val.to_string()); count = out.len(); }
                        }
                    }
                    in_p = false; current_buf.clear();
                }
            }
            Ok(quick_xml::events::Event::Text(t)) => {
                if in_p { if let Ok(tx) = t.decode() { let s = tx.to_string(); if !s.trim().is_empty() { current_buf.push_str(&s); } } }
            }
            Ok(quick_xml::events::Event::Eof) => break,
            Err(_) => break,
            _ => {}
        }
        buf.clear();
        seen += 1; if seen > max || count >= wanted.len() { break; }
    }
    serde_json::to_value(out).unwrap_or(serde_json::json!({}))
}

fn normalize_ws(s: &str) -> String {
    let mut t = s.replace("\r", "");
    t = t.split('\n').map(|l| l.trim()).collect::<Vec<_>>().join("\n");
    while t.contains("\n\n\n") { t = t.replace("\n\n\n", "\n\n"); }
    t
}

fn main() -> Result<()> {
    // Initialize optional repo policy from env (rate limits / future robots compliance)
    daizo_core::repo::init_policy_from_env();
    let stdin = std::io::stdin();
    let mut stdin = BufReader::new(stdin.lock());
    let mut stdout = std::io::stdout();
    loop {
        let Some(msg) = read_message(&mut stdin)? else { break };
        if let Ok(req) = serde_json::from_value::<Request>(msg.clone()) {
            dbg_log(&format!("[recv] method={} id={}", req.method, req.id));
            let resp = match req.method.as_str() {
                "initialize" => handle_initialize(req.id),
                "tools/list" => handle_tools_list(req.id),
                "tools/call" => handle_call(req.id, &req.params),
                _ => json!({"jsonrpc":"2.0","id":req.id,"error":{"code": -32601, "message":"Method not found"}}),
            };
            write_message(&mut stdout, &resp)?;
        } else {
            // ignore non-request messages
            dbg_log("[recv] non-request/ignored");
        }
    }
    Ok(())
}
