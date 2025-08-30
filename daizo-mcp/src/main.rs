use anyhow::Result;
use daizo_core::{build_index, build_tipitaka_index, build_cbeta_index, extract_text, extract_text_opts, extract_cbeta_juan, list_heads_cbeta, list_heads_generic, IndexEntry};
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
use walkdir::WalkDir;
use encoding_rs::Encoding;

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

#[derive(Deserialize)]
struct Request {
    id: serde_json::Value,
    method: String,
    #[serde(default)]
    params: serde_json::Value,
}

// ============ Paths & cache ============

fn daizo_home() -> PathBuf {
    if let Ok(p) = std::env::var("DAIZO_DIR") { return PathBuf::from(p); }
    std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from(".")).join(".daizo")
}
fn cbeta_root() -> PathBuf { daizo_home().join("xml-p5") }
fn tipitaka_root() -> PathBuf { daizo_home().join("tipitaka-xml").join("romn") }
fn cache_dir() -> PathBuf { daizo_home().join("cache") }

fn ensure_dir(p: &Path) { let _ = fs::create_dir_all(p); }

fn clone_tipitaka_sparse_mcp(target_dir: &Path) -> bool {
    eprintln!("[daizo-mcp] Cloning Tipitaka (romn only) to: {}", target_dir.display());
    
    // Initialize empty repo
    if !run_cmd("git", &["init"], Some(&target_dir.parent().unwrap_or(Path::new(".")))) {
        eprintln!("[daizo-mcp] Failed to initialize git repository");
        return false;
    }
    
    let target_str = target_dir.to_string_lossy();
    
    // Set up sparse checkout
    let steps = [
        ("git", vec!["-C", &target_str, "remote", "add", "origin", "https://github.com/VipassanaTech/tipitaka-xml"]),
        ("git", vec!["-C", &target_str, "config", "core.sparseCheckout", "true"]),
    ];
    
    for (cmd, args) in &steps {
        if !run_cmd(cmd, &args, None) {
            eprintln!("[daizo-mcp] Failed to configure sparse checkout");
            return false;
        }
    }
    
    // Create sparse-checkout file
    let sparse_file = target_dir.join(".git").join("info").join("sparse-checkout");
    if let Some(parent) = sparse_file.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if std::fs::write(&sparse_file, "romn/\n").is_err() {
        eprintln!("[daizo-mcp] Failed to write sparse-checkout file");
        return false;
    }
    
    // Pull only the romn directory
    if !run_cmd("git", &["-C", &target_str, "pull", "--depth", "1", "origin", "master"], None) {
        eprintln!("[daizo-mcp] Failed to pull romn directory");
        return false;
    }
    
    eprintln!("[daizo-mcp] Tipitaka romn directory cloned successfully");
    true
}

fn run_cmd(cmd: &str, args: &[&str], cwd: Option<&Path>) -> bool {
    use std::process::Command;
    eprintln!("[daizo-mcp] {} {}", cmd, args.join(" "));
    let mut c = Command::new(cmd);
    c.args(args);
    if let Some(d) = cwd { c.current_dir(d); }
    // Enable progress output for git commands
    if cmd == "git" {
        c.stdout(std::process::Stdio::inherit());
        c.stderr(std::process::Stdio::inherit());
    }
    let result = c.status().map(|s| s.success()).unwrap_or(false);
    if result {
        eprintln!("[daizo-mcp] {} completed successfully", cmd);
    } else {
        eprintln!("[daizo-mcp] {} failed", cmd);
    }
    result
}

fn ensure_cbeta_data() {
    let root = cbeta_root();
    if root.exists() { 
        eprintln!("[daizo-mcp] CBETA data exists: {}", root.display());
        return; 
    }
    eprintln!("[daizo-mcp] Downloading CBETA data to: {}", root.display());
    ensure_dir(root.parent().unwrap_or(Path::new(".")));
    let _ = run_cmd("git", &["clone", "--depth", "1", "https://github.com/cbeta-org/xml-p5", root.to_string_lossy().as_ref()], None);
}

fn ensure_tipitaka_data() {
    let base = daizo_home().join("tipitaka-xml");
    if !base.exists() {
        ensure_dir(base.parent().unwrap_or(Path::new(".")));
        let _ = clone_tipitaka_sparse_mcp(&base);
    } else {
        eprintln!("[daizo-mcp] Tipitaka data exists: {}", base.display());
    }
    let root = tipitaka_root();
    ensure_dir(root.parent().unwrap_or(Path::new(".")));
}

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
                "prompts": {},
                "logging": {}
            },
            "serverInfo": { "name": "daizo-mcp", "version": "0.1.0" }
        }
    })
}

fn tools_list() -> Vec<serde_json::Value> {
    vec![
        tool("cbeta_search", "Search CBETA titles", json!({"type":"object","properties":{"query":{"type":"string"},"limit":{"type":"number"}},"required":["query"]})),
        tool("cbeta_fetch", "Fetch CBETA text", json!({"type":"object","properties":{
            "id":{"type":"string"},"query":{"type":"string"},"part":{"type":"string"},"includeNotes":{"type":"boolean"},"headingsLimit":{"type":"number"},"startChar":{"type":"number"},"endChar":{"type":"number"},"maxChars":{"type":"number"},"page":{"type":"number"},"pageSize":{"type":"number"}
        }})),
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
        tool("sat_fetch", "Fetch SAT page (prefer useid → detail URL)", json!({"type":"object","properties":{
            "url":{"type":"string"},
            "useid":{"type":"string"},
            "startChar":{"type":"number"},
            "maxChars":{"type":"number"}
        }})),
        tool("sat_detail", "Fetch SAT detail by useid", json!({"type":"object","properties":{"useid":{"type":"string"},"key":{"type":"string"},"startChar":{"type":"number"},"maxChars":{"type":"number"}},"required":["useid"]})),
        tool("sat_pipeline", "Search wrap7, pick best title, then fetch detail", json!({"type":"object","properties":{
            "query":{"type":"string"},
            "rows":{"type":"number"},
            "offs":{"type":"number"},
            "fields":{"type":"string"},
            "fq":{"type":"array","items":{"type":"string"}},
            "startChar":{"type":"number"},
            "maxChars":{"type":"number"}
        },"required":["query"]})),
        tool("tipitaka_search", "Search Tipitaka (romn) titles", json!({"type":"object","properties":{"query":{"type":"string"},"limit":{"type":"number"}},"required":["query"]})),
        tool("tipitaka_fetch", "Fetch Tipitaka (romn) text", json!({"type":"object","properties":{
            "id":{"type":"string"},"query":{"type":"string"},"headIndex":{"type":"number"},"headQuery":{"type":"string"},"headingsLimit":{"type":"number"},"startChar":{"type":"number"},"endChar":{"type":"number"},"maxChars":{"type":"number"},"page":{"type":"number"},"pageSize":{"type":"number"}
        }})),
        tool("index_rebuild", "Rebuild search indexes", json!({"type":"object","properties":{"source":{"type":"string","enum":["cbeta","tipitaka","all"]}},"required":["source"]})),
    ]
}

fn tool(name: &str, description: &str, input_schema: serde_json::Value) -> serde_json::Value {
    json!({"name": name, "description": description, "inputSchema": input_schema })
}

fn handle_tools_list(id: serde_json::Value) -> serde_json::Value {
    json!({"jsonrpc":"2.0","id":id,"result": {"tools": tools_list()}})
}

fn normalized(s: &str) -> String {
    use unicode_normalization::UnicodeNormalization;
    let mut t: String = s.nfkd().collect::<String>().to_lowercase();
    let map: [(&str,&str); 12] = [("経","經"),("经","經"),("观","觀"),("圣","聖"),("会","會"),("后","後"),("国","國"),("灵","靈"),("广","廣"),("龙","龍"),("台","臺"),("体","體")];
    for (a,b) in map.iter() { t = t.replace(a, b); }
    t.chars().filter(|c| c.is_alphanumeric()).collect()
}

fn normalized_with_spaces(s: &str) -> String {
    use unicode_normalization::UnicodeNormalization;
    let t: String = s.nfkd().collect::<String>().to_lowercase();
    t.chars().map(|c| if c.is_alphanumeric() { c } else { ' ' }).collect::<String>()
        .split_whitespace().collect::<Vec<_>>().join(" ")
}

fn tokenset(s: &str) -> std::collections::HashSet<String> {
    normalized_with_spaces(s).split_whitespace().map(|w| w.to_string()).collect()
}

fn token_jaccard(a: &str, b: &str) -> f32 {
    let sa: std::collections::HashSet<_> = tokenset(a);
    let sb: std::collections::HashSet<_> = tokenset(b);
    if sa.is_empty() || sb.is_empty() { return 0.0; }
    let inter = sa.intersection(&sb).count() as f32;
    let uni = (sa.len() + sb.len()).saturating_sub(inter as usize) as f32;
    if uni == 0.0 { 0.0 } else { inter / uni }
}

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
    eprintln!("[cbeta] building index…");
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
    eprintln!("[tipitaka] building index…");
    let mut entries = build_tipitaka_index(&tipitaka_root());
    entries.retain(|e| !e.path.ends_with(".toc.xml"));
    let _ = save_index(&out, &entries);
    entries
}

#[derive(Clone, Debug, Serialize)]
struct ScoredHit<'a> { #[serde(skip_serializing)] entry: &'a IndexEntry, score: f32 }

fn best_match<'a>(entries: &'a [IndexEntry], q: &str, limit: usize) -> Vec<ScoredHit<'a>> {
    let nq = normalized(q);
    let mut scored: Vec<(f32, &IndexEntry)> = entries.iter().map(|e| {
        let meta_str = e.meta.as_ref().map(|m| m.values().cloned().collect::<Vec<_>>().join(" ")).unwrap_or_default();
        let alias = e.meta.as_ref().and_then(|m| m.get("alias")).cloned().unwrap_or_default();
        let hay_all = format!("{} {} {}", e.title, e.id, meta_str);
        let hay = normalized(&hay_all);
        let mut score = if hay.contains(&nq) { 1.0f32 } else {
            let s_char = jaccard(&hay, &nq);
            let s_tok = token_jaccard(&hay_all, q);
            s_char.max(s_tok)
        };
        // subsequence boost
        if score < 0.95 && (is_subsequence(&hay, &nq) || is_subsequence(&nq, &hay)) { score = score.max(0.85); }
        // alias exact/contains boosts
        let nalias = normalized_with_spaces(&alias).replace(' ', "");
        let nq_nospace = normalized_with_spaces(q).replace(' ', "");
        if !nalias.is_empty() {
            if nalias.split_whitespace().any(|a| a == nq_nospace) || nalias.contains(&nq_nospace) {
                score = score.max(0.95);
            }
        }
        // numeric pattern boost (e.g., 12.2)
        if q.chars().any(|c| c.is_ascii_digit()) {
            let hws = normalized_with_spaces(&hay_all);
            if hws.contains(&normalized_with_spaces(q)) { score = (score + 0.05).min(1.0); }
        }
        (score, e)
    }).collect();
    scored.sort_by(|a,b| b.0.partial_cmp(&a.0).unwrap());
    scored.into_iter().take(limit).map(|(s,e)| ScoredHit { entry: e, score: s }).collect()
}

fn jaccard(a: &str, b: &str) -> f32 {
    let sa: std::collections::HashSet<_> = a.as_bytes().windows(2).collect();
    let sb: std::collections::HashSet<_> = b.as_bytes().windows(2).collect();
    let inter = sa.intersection(&sb).count() as f32;
    let uni = (sa.len() + sb.len()).saturating_sub(inter as usize) as f32;
    if uni == 0.0 { 0.0 } else { inter / uni }
}

fn is_subsequence(text: &str, pat: &str) -> bool {
    let mut i = 0usize;
    for ch in text.chars() {
        if i < pat.len() && ch == pat.chars().nth(i).unwrap_or('\0') { i += 1; }
        if i >= pat.len() { return true; }
    }
    i >= pat.len()
}

fn resolve_cbeta_path(id: &str) -> Option<PathBuf> {
    // T0002 -> search for *n0002.xml under T/
    let m = Regex::new(r"^([A-Za-z]+)(\d+)$").unwrap();
    let root = cbeta_root();
    if let Some(c) = m.captures(id) {
        let canon = &c[1];
        let num = &c[2];
        for e in WalkDir::new(root.join(canon)).into_iter().filter_map(|e| e.ok()) {
            if e.file_type().is_file() {
                let name = e.file_name().to_string_lossy().to_lowercase();
                if name.contains(&format!("n{}", num)) && name.ends_with(".xml") {
                    return Some(e.path().to_path_buf());
                }
            }
        }
    }
    // fallback: anywhere *id*.xml
    for e in WalkDir::new(&root).into_iter().filter_map(|e| e.ok()) {
        if e.file_type().is_file() {
            let name = e.file_name().to_string_lossy().to_lowercase();
            if name.contains(&id.to_lowercase()) && name.ends_with(".xml") { return Some(e.path().to_path_buf()); }
        }
    }
    None
}

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
        "cbeta_search" => {
            let q = args.get("query").and_then(|v| v.as_str()).unwrap_or("");
            let limit = args.get("limit").and_then(|v| v.as_u64()).unwrap_or(10) as usize;
            let idx = load_or_build_cbeta_index();
            let hits = best_match(&idx, q, limit);
            hits.iter().enumerate().map(|(i,h)| format!("{}. {}  {}", i+1, h.entry.id, h.entry.title)).collect::<Vec<_>>().join("\n")
        }
        "cbeta_fetch" => {
            ensure_cbeta_data();
            let mut matched_id: Option<String> = None;
            let mut matched_title: Option<String> = None;
            let mut matched_score: Option<f32> = None;
            let mut path: PathBuf = PathBuf::new();
            if let Some(q) = args.get("query").and_then(|v| v.as_str()) {
                let idx = load_or_build_cbeta_index();
                if let Some(hit) = best_match(&idx, q, 1).into_iter().next() {
                    matched_id = Some(hit.entry.id.clone());
                    matched_title = Some(hit.entry.title.clone());
                    matched_score = Some(hit.score);
                    path = PathBuf::from(&hit.entry.path);
                }
            }
            if path.as_os_str().is_empty() {
                if let Some(id) = args.get("id").and_then(|v| v.as_str()) {
                    let idx = load_or_build_cbeta_index();
                    if let Some(hit) = idx.iter().find(|e| e.id == id) {
                        matched_id = Some(hit.id.clone());
                        matched_title = Some(hit.title.clone());
                        path = PathBuf::from(&hit.path);
                    } else {
                        path = resolve_cbeta_path(id).unwrap_or_else(|| PathBuf::from(""));
                        matched_id = Some(id.to_string());
                    }
                }
            }
            let xml = fs::read_to_string(&path).unwrap_or_default();
            // includeNotes support
            let include_notes = args.get("includeNotes").and_then(|v| v.as_bool()).unwrap_or(false);
            let (text, extraction_method, part_matched) = if let Some(part) = args.get("part").and_then(|v| v.as_str()) {
                if let Some(sec) = extract_cbeta_juan(&xml, part) { (sec, "cbeta-juan".to_string(), true) } else { (extract_text_opts(&xml, include_notes), "full".to_string(), false) }
            } else { (extract_text_opts(&xml, include_notes), "full".to_string(), false) };
            let sliced = slice_text(&text, &args);
            let heads = list_heads_cbeta(&xml);
            let hl = args.get("headingsLimit").and_then(|v| v.as_u64()).unwrap_or(10) as usize;
            let meta = json!({
                "totalLength": text.len(),
                "returnedStart": args.get("startChar").and_then(|v| v.as_u64()).unwrap_or( args.get("page").and_then(|v| v.as_u64()).and_then(|p| args.get("pageSize").and_then(|s| s.as_u64()).map(|ps| p*ps)).unwrap_or(0) ) ,
                "returnedEnd": args.get("startChar").and_then(|v| v.as_u64()).unwrap_or( args.get("page").and_then(|v| v.as_u64()).and_then(|p| args.get("pageSize").and_then(|s| s.as_u64()).map(|ps| p*ps)).unwrap_or(0) ) + (sliced.len() as u64),
                "truncated": (sliced.len() as u64) < (text.len() as u64),
                "sourcePath": path.to_string_lossy(),
                "extractionMethod": extraction_method,
                "partMatched": part_matched,
                "headingsTotal": heads.len(),
                "headingsPreview": heads.into_iter().take(hl).collect::<Vec<_>>(),
                "matchedId": matched_id,
                "matchedTitle": matched_title,
                "matchedScore": matched_score,
            });
            return json!({"jsonrpc":"2.0","id": id, "result": { "content": [{"type":"text","text": sliced}], "_meta": meta }});
        }
        "tipitaka_search" => {
            let q = args.get("query").and_then(|v| v.as_str()).unwrap_or("");
            let limit = args.get("limit").and_then(|v| v.as_u64()).unwrap_or(10) as usize;
            let idx = load_or_build_tipitaka_index();
            let hits = best_match(&idx, q, limit);
            hits.iter().enumerate().map(|(i,h)| format!("{}. {}  {}", i+1, Path::new(&h.entry.path).file_stem().unwrap().to_string_lossy(), h.entry.title)).collect::<Vec<_>>().join("\n")
        }
        "tipitaka_fetch" => {
            ensure_tipitaka_data();
            let mut matched_id: Option<String> = None;
            let mut matched_title: Option<String> = None;
            let mut matched_score: Option<f32> = None;
            // 優先: インデックスから直接パスを得る（絶対パス）。
            // 1) query 指定 → best_match の path
            // 2) id 指定 → index 内で stem が一致する path
            let mut path: PathBuf = if let Some(q) = args.get("query").and_then(|v| v.as_str()) {
                let idx = load_or_build_tipitaka_index();
                if let Some(hit) = best_match(&idx, q, 1).into_iter().next() {
                    matched_title = Some(hit.entry.title.clone());
                    matched_score = Some(hit.score);
                    matched_id = Path::new(&hit.entry.path).file_stem().map(|s| s.to_string_lossy().into_owned());
                    PathBuf::from(&hit.entry.path)
                } else { PathBuf::new() }
            } else if let Some(id) = args.get("id").and_then(|v| v.as_str()) {
                let idx = load_or_build_tipitaka_index();
                // まずは完全一致（stem == id）
                let mut exact: Option<PathBuf> = None;
                for e in idx.iter() {
                    if Path::new(&e.path).file_stem().map(|s| s == id).unwrap_or(false) {
                        exact = Some(PathBuf::from(&e.path));
                        matched_title = Some(e.title.clone());
                        matched_id = Some(id.to_string());
                        break;
                    }
                }
                // 次に接頭一致（stem が id + 数字 で始まるもののうち最小番号）
                let mut best_seq: Option<(u32, PathBuf, &str)> = None;
                if exact.is_none() {
                    for e in idx.iter() {
                        if let Some(stem) = Path::new(&e.path).file_stem().and_then(|s| s.to_str()) {
                            if let Some(rest) = stem.strip_prefix(id) {
                                // 記号（例: ".toc"）は除外し、末尾の数字のみを対象
                                let digits = rest.chars().take_while(|c| c.is_ascii_digit()).collect::<String>();
                                if !digits.is_empty() && rest.chars().all(|c| c.is_ascii_digit()) {
                                    if let Ok(n) = digits.parse::<u32>() {
                                        let take = match &best_seq { Some((bn,_,_)) => n < *bn, None => true };
                                        if take {
                                            best_seq = Some((n, PathBuf::from(&e.path), e.title.as_str()));
                                        }
                                    }
                                }
                            }
                        }
                    }
                    if let Some((_, _, t)) = &best_seq { matched_title = Some((*t).to_string()); matched_id = Some(id.to_string()); }
                }
                exact
                    .or(best_seq.map(|(_,p,_)| p))
                    .or_else(|| find_tipitaka_content_for_base(id))
                    .or_else(|| find_in_dir(&tipitaka_root(), id))
                    .unwrap_or_else(|| PathBuf::new())
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
            let (mut text, mut extraction_method) = if let Some(hq) = args.get("headQuery").and_then(|v| v.as_str()) { (extract_section_by_head(&xml, None, Some(hq)).unwrap_or_else(|| extract_text(&xml)), "head-query".to_string())
                        } else if let Some(hi) = args.get("headIndex").and_then(|v| v.as_u64()) { (extract_section_by_head(&xml, Some(hi as usize), None).unwrap_or_else(|| extract_text(&xml)), "head-index".to_string()) } else { (extract_text(&xml), "full".to_string()) };
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
            let sliced = slice_text(&text, &args);
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
        "index_rebuild" => {
            let src = args.get("source").and_then(|v| v.as_str()).unwrap_or("all");
            let mut results = serde_json::Map::new();
            if src == "cbeta" || src == "all" {
                ensure_cbeta_data();
                let entries = build_index(&cbeta_root(), None);
                let out = cache_dir().join("cbeta-index.json");
                let _ = save_index(&out, &entries);
                results.insert("cbeta".to_string(), json!({"count": entries.len(), "cache": out }));
            }
            if src == "tipitaka" || src == "all" {
                ensure_tipitaka_data();
                let mut entries = build_tipitaka_index(&tipitaka_root());
                entries.retain(|e| !e.path.ends_with(".toc.xml"));
                let out = cache_dir().join("tipitaka-index.json");
                let _ = save_index(&out, &entries);
                results.insert("tipitaka".to_string(), json!({"count": entries.len(), "cache": out }));
            }
            let summary = format!("rebuilt: {}", results.keys().cloned().collect::<Vec<_>>().join(", "));
            return json!({"jsonrpc":"2.0","id": id, "result": { "content": [{"type":"text","text": summary}], "_meta": results }});
        }
        _ => format!("unknown tool: {}", name),
    };
    json!({
        "jsonrpc":"2.0",
        "id": id,
        "result": { "content": [{"type":"text","text": content_text }] }
    })
}

fn find_in_dir(root: &Path, stem_hint: &str) -> Option<PathBuf> {
    for e in WalkDir::new(root).into_iter().filter_map(|e| e.ok()) {
        if e.file_type().is_file() {
            if let Some(stem) = e.path().file_stem().and_then(|s| s.to_str()) {
                if stem.to_lowercase().contains(&stem_hint.to_lowercase()) && e.path().extension().and_then(|s| s.to_str()) == Some("xml") {
                    return Some(e.path().to_path_buf());
                }
            }
        }
    }
    None
}

fn find_tipitaka_content_for_base(base: &str) -> Option<PathBuf> {
    // Prefer ...base0.xml, then lowest-numbered ...base{n}.xml, excluding TOC-like files
    let root = tipitaka_root();
    let mut best: Option<(u32, PathBuf)> = None;
    for e in WalkDir::new(&root).into_iter().filter_map(|e| e.ok()) {
        if e.file_type().is_file() {
            let p = e.path();
            if p.extension().and_then(|s| s.to_str()) != Some("xml") { continue; }
            let name = p.file_name().and_then(|s| s.to_str()).unwrap_or("").to_lowercase();
            if !name.contains(&base.to_lowercase()) { continue; }
            if name.contains("toc") || name.contains("sitemap") || name.contains("tree") { continue; }
            // try to parse trailing number before .xml
            let num = name.trim_end_matches(".xml").chars().rev().take_while(|c| c.is_ascii_digit()).collect::<String>();
            let n = num.chars().rev().collect::<String>();
            let rank = if n.is_empty() { u32::MAX } else { n.parse::<u32>().unwrap_or(u32::MAX) };
            if best.as_ref().map(|(bn,_)| rank < *bn).unwrap_or(true) {
                best = Some((rank, p.to_path_buf()));
            }
        }
    }
    best.map(|(_,p)| p)
}

fn find_exact_file_by_name(root: &Path, filename: &str) -> Option<PathBuf> {
    for e in WalkDir::new(root).into_iter().filter_map(|e| e.ok()) {
        if e.file_type().is_file() {
            if let Some(name) = e.path().file_name().and_then(|s| s.to_str()) {
                if name.eq_ignore_ascii_case(filename) { return Some(e.path().to_path_buf()); }
            }
        }
    }
    None
}

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
    reader.trim_text(true);
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
                if in_p { if let Ok(tx) = t.unescape() { let s = tx.to_string(); if !s.trim().is_empty() { current_buf.push_str(&s); } } }
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
