use clap::{Parser, Subcommand};
use daizo_core::{
    build_index,
    build_tipitaka_index,
    build_cbeta_index,
    extract_text,
    extract_text_opts,
    extract_cbeta_juan,
    list_heads_generic,
    list_heads_cbeta,
    cbeta_grep,
    tipitaka_grep,
};
use serde::Serialize;
use std::fs;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Parser, Debug)]
#[command(name = "daizo-rs", about = "High-performance helpers for daizo-mcp")] 
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand, Debug)]
enum Commands {
    /// Initialize data: clone xml-p5 and tipitaka-xml and build indices
    Init {
        /// Override HOME/.daizo base
        #[arg(long)]
        base: Option<PathBuf>,
    },
    /// Print CLI version
    Version {},
    /// Diagnose install and data directories
    Doctor {
        /// Verbose output
        #[arg(long, default_value_t = false)]
        verbose: bool,
    },
    /// Uninstall binaries from $DAIZO_DIR/bin (and optionally data/cache)
    Uninstall {
        /// Also remove data (xml-p5, tipitaka-xml) and cache under $DAIZO_DIR
        #[arg(long, default_value_t = false)]
        purge: bool,
    },
    /// Update this CLI via cargo-install
    Update {
        /// Install from a git repo (e.g. https://github.com/owner/daizo-mcp)
        #[arg(long)]
        git: Option<String>,
        /// Execute the install instead of just printing the command
        #[arg(long, default_value_t = false)]
        yes: bool,
    },
    /// Search CBETA titles (index-based)
    CbetaTitleSearch {
        /// Query string
        #[arg(long)]
        query: String,
        /// Max results
        #[arg(long, default_value_t = 10)]
        limit: usize,
        /// Output JSON
        #[arg(long, default_value_t = false)]
        json: bool,
    },
    /// Fetch CBETA text by id or query
    CbetaFetch {
        /// Canonical id (e.g., T0002)
        #[arg(long)]
        id: Option<String>,
        /// Alternative: search query to pick best match
        #[arg(long)]
        query: Option<String>,
        /// Extract a juan/part (e.g., 1 or 001)
        #[arg(long)]
        part: Option<String>,
        /// Include <note> text
        #[arg(long, default_value_t = false)]
        include_notes: bool,
        /// Headings preview count
        #[arg(long, default_value_t = 10)]
        headings_limit: usize,
        /// Pagination: start char
        #[arg(long)]
        start_char: Option<usize>,
        /// Pagination: end char
        #[arg(long)]
        end_char: Option<usize>,
        /// Pagination: max chars
        #[arg(long)]
        max_chars: Option<usize>,
        /// Pagination: page index
        #[arg(long)]
        page: Option<usize>,
        /// Pagination: page size
        #[arg(long)]
        page_size: Option<usize>,
        /// Target line number for context extraction
        #[arg(long)]
        line_number: Option<usize>,
        /// Number of lines before target line (default: 10)
        #[arg(long, default_value_t = 10)]
        context_before: usize,
        /// Number of lines after target line (default: 100)
        #[arg(long, default_value_t = 100)]
        context_after: usize,
        /// Number of lines before/after target line (deprecated, use context_before/context_after)
        #[arg(long)]
        context_lines: Option<usize>,
        /// Output JSON
        #[arg(long, default_value_t = false)]
        json: bool,
    },
    /// Search SAT wrap7.php
    SatSearch {
        /// Query string
        #[arg(long)]
        query: String,
        /// Rows
        #[arg(long, default_value_t = 100)]
        rows: usize,
        /// Offset
        #[arg(long, default_value_t = 0)]
        offs: usize,
        /// Exact mode
        #[arg(long, default_value_t = true)]
        exact: bool,
        /// Titles only filter (client-side)
        #[arg(long, default_value_t = false)]
        titles_only: bool,
        /// Fields to return (wrap7 `fl`), comma-separated. Default excludes body.
        #[arg(long, default_value = "id,fascnm,fascnum,startid,endid")]
        fields: String,
        /// Filter queries (wrap7 `fq`). Repeatable.
        #[arg(long)]
        fq: Vec<String>,
        /// Auto run pipeline (pick best title and fetch detail)
        #[arg(long, default_value_t = false)]
        autofetch: bool,
        /// Slice start for autofetch
        #[arg(long)]
        start_char: Option<usize>,
        /// Slice max chars for autofetch
        #[arg(long)]
        max_chars: Option<usize>,
        /// Output JSON
        #[arg(long, default_value_t = false)]
        json: bool,
    },
    /// Fetch SAT detail by URL
    SatFetch {
        #[arg(long)]
        url: Option<String>,
        /// Prefer useid (startid from search). If provided, URL is ignored.
        #[arg(long)]
        useid: Option<String>,
        #[arg(long)]
        start_char: Option<usize>,
        #[arg(long)]
        max_chars: Option<usize>,
        /// Output JSON
        #[arg(long, default_value_t = false)]
        json: bool,
    },
    /// Fetch SAT detail by useid/key
    SatDetail {
        #[arg(long)]
        useid: String,
        #[arg(long, default_value = "")]
        key: String,
        #[arg(long)]
        start_char: Option<usize>,
        #[arg(long)]
        max_chars: Option<usize>,
        /// Output JSON
        #[arg(long, default_value_t = false)]
        json: bool,
    },
    /// Search SAT (wrap7), select best title, then fetch by useid
    SatPipeline {
        /// Query string
        #[arg(long)]
        query: String,
        /// Rows from search
        #[arg(long, default_value_t = 100)]
        rows: usize,
        /// Offset
        #[arg(long, default_value_t = 0)]
        offs: usize,
        /// Fields (wrap7 `fl`), must include `fascnm,startid`
        #[arg(long, default_value = "id,fascnm,startid,endid")] 
        fields: String,
        /// Filter queries (wrap7 `fq`), repeatable
        #[arg(long)]
        fq: Vec<String>,
        /// Slice start char for fetched detail
        #[arg(long)]
        start_char: Option<usize>,
        /// Slice max chars for fetched detail
        #[arg(long)]
        max_chars: Option<usize>,
        /// Output JSON (MCP envelope)
        #[arg(long, default_value_t = true)]
        json: bool,
    },
    /// Search Tipitaka (romn) titles (index-based)
    TipitakaTitleSearch {
        /// Query string
        #[arg(long)]
        query: String,
        /// Max results
        #[arg(long, default_value_t = 10)]
        limit: usize,
        /// Output JSON
        #[arg(long, default_value_t = false)]
        json: bool,
    },
    /// Fetch Tipitaka (romn) text by id or query
    TipitakaFetch {
        /// File stem id (e.g. abh01m.mul)
        #[arg(long)]
        id: Option<String>,
        /// Alternative: search query to pick best match
        #[arg(long)]
        query: Option<String>,
        /// Section by head index
        #[arg(long)]
        head_index: Option<usize>,
        /// Section by head text match
        #[arg(long)]
        head_query: Option<String>,
        /// Headings preview count
        #[arg(long, default_value_t = 10)]
        headings_limit: usize,
        /// Pagination: start char
        #[arg(long)]
        start_char: Option<usize>,
        /// Pagination: end char
        #[arg(long)]
        end_char: Option<usize>,
        /// Pagination: max chars
        #[arg(long)]
        max_chars: Option<usize>,
        /// Pagination: page index
        #[arg(long)]
        page: Option<usize>,
        /// Pagination: page size
        #[arg(long)]
        page_size: Option<usize>,
        /// Target line number for context extraction
        #[arg(long)]
        line_number: Option<usize>,
        /// Number of lines before target line (default: 10)
        #[arg(long, default_value_t = 10)]
        context_before: usize,
        /// Number of lines after target line (default: 100)
        #[arg(long, default_value_t = 100)]
        context_after: usize,
        /// Number of lines before/after target line (deprecated, use context_before/context_after)
        #[arg(long)]
        context_lines: Option<usize>,
        /// Output JSON
        #[arg(long, default_value_t = false)]
        json: bool,
    },
    /// Build CBETA index under ~/.daizo/cache/cbeta-index.json
    CbetaIndex {
        /// Root directory of xml-p5 (default ~/.daizo/xml-p5)
        #[arg(long)]
        root: Option<PathBuf>,
        /// Output path (default ~/.daizo/cache/cbeta-index.json)
        #[arg(long)]
        out: Option<PathBuf>,
    },
    /// Build Tipitaka (romn) index under ~/.daizo/cache/tipitaka-index.json
    TipitakaIndex {
        /// Root directory of tipitaka-xml (default ~/.daizo/tipitaka-xml)
        #[arg(long)]
        root: Option<PathBuf>,
        /// Output path (default ~/.daizo/cache/tipitaka-index.json)
        #[arg(long)]
        out: Option<PathBuf>,
    },
    /// Rebuild search indexes (deletes cache JSON first)
    IndexRebuild {
        /// Source to rebuild: cbeta | tipitaka | all
        #[arg(long, default_value = "all")]
        source: String,
    },
    /// Extract plain text from an XML file path (reads from stdin XML if --path omitted)
    ExtractText {
        #[arg(long)]
        path: Option<PathBuf>,
    },
    /// Search CBETA corpus (content-based)
    CbetaSearch {
        /// Query string (regular expression)
        #[arg(long)]
        query: String,
        /// Maximum number of files to return
        #[arg(long, default_value_t = 20)]
        max_results: usize,
        /// Maximum matches per file
        #[arg(long, default_value_t = 5)]
        max_matches_per_file: usize,
        /// Output JSON
        #[arg(long, default_value_t = false)]
        json: bool,
    },
    /// Search Tipitaka corpus (content-based)
    TipitakaSearch {
        /// Query string (regular expression)
        #[arg(long)]
        query: String,
        /// Maximum number of files to return
        #[arg(long, default_value_t = 20)]
        max_results: usize,
        /// Maximum matches per file
        #[arg(long, default_value_t = 5)]
        max_matches_per_file: usize,
        /// Output JSON
        #[arg(long, default_value_t = false)]
        json: bool,
    },
}

#[derive(Serialize)]
struct IndexResult<'a> {
    count: usize,
    out: &'a str,
}

fn default_daizo() -> PathBuf {
    if let Ok(p) = std::env::var("DAIZO_DIR") { return PathBuf::from(p); }
    std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from(".")).join(".daizo")
}

fn ensure_dir(p: &PathBuf) -> anyhow::Result<()> { fs::create_dir_all(p)?; Ok(()) }

fn clone_tipitaka_sparse(target_dir: &Path) -> bool {
    eprintln!("[clone] Cloning Tipitaka (romn only) to: {}", target_dir.display());
    
    // Remove target directory if it exists but is empty
    if target_dir.exists() {
        let _ = fs::remove_dir_all(target_dir);
    }
    
    // Use git clone with sparse-checkout directly
    let temp_dir = target_dir.parent().unwrap_or(Path::new("."));
    let target_name = target_dir.file_name().unwrap_or_else(|| std::ffi::OsStr::new("tipitaka-xml"));
    
    // Clone the repository with no checkout
    if !run("git", &["clone", "--no-checkout", "--depth", "1", 
                    "https://github.com/VipassanaTech/tipitaka-xml", 
                    target_name.to_string_lossy().as_ref()], 
           Some(&temp_dir.to_path_buf())) {
        eprintln!("[error] Failed to clone repository");
        return false;
    }
    
    let target_str = target_dir.to_string_lossy();
    
    // Configure sparse checkout
    if !run("git", &["-C", &target_str, "config", "core.sparseCheckout", "true"], None) {
        eprintln!("[error] Failed to configure sparse checkout");
        return false;
    }
    
    // Create sparse-checkout file
    let sparse_file = target_dir.join(".git").join("info").join("sparse-checkout");
    if let Some(parent) = sparse_file.parent() {
        let _ = fs::create_dir_all(parent);
    }
    if fs::write(&sparse_file, "romn/\n").is_err() {
        eprintln!("[error] Failed to write sparse-checkout file");
        return false;
    }
    
    // Checkout only the romn directory
    if !run("git", &["-C", &target_str, "checkout"], None) {
        eprintln!("[error] Failed to checkout romn directory");
        return false;
    }
    
    eprintln!("[clone] Tipitaka romn directory cloned successfully");
    true
}

fn run(cmd: &str, args: &[&str], cwd: Option<&PathBuf>) -> bool {
    eprintln!("[exec] {} {}", cmd, args.join(" "));
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
        eprintln!("[exec] {} completed successfully", cmd);
    } else {
        eprintln!("[exec] {} failed", cmd);
    }
    result
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Commands::Init { base } => {
            // Display startup message with colored output
            eprintln!("\x1b[33m📥 First-time setup requires downloading Buddhist texts. This may take several minutes... / 初回起動時はお経のダウンロードに時間がかかります。しばらくお待ちください... / 首次啟動需要下載佛經文本，可能需要幾分鐘時間...\x1b[0m");
            
            let base_dir = base.unwrap_or(default_daizo());
            ensure_dir(&base_dir)?;
            // clone xml-p5
            let cbeta_dir = base_dir.join("xml-p5");
            if !cbeta_dir.exists() {
                eprintln!("\x1b[36m🔄 Downloading CBETA corpus... / CBETAコーパスをダウンロード中... / 正在下載CBETA語料庫...\x1b[0m");
                println!("[init] cloning xml-p5 -> {}", cbeta_dir.to_string_lossy());
                let ok = run("git", &["clone", "--depth", "1", "https://github.com/cbeta-org/xml-p5", cbeta_dir.to_string_lossy().as_ref()], None);
                if !ok { anyhow::bail!("git clone xml-p5 failed"); }
            } else { println!("[init] xml-p5 exists: {}", cbeta_dir.to_string_lossy()); }
            // clone tipitaka-xml (romn only using sparse-checkout)
            let tipitaka_dir = base_dir.join("tipitaka-xml");
            if !tipitaka_dir.exists() {
                eprintln!("\x1b[36m🔄 Downloading Tipitaka corpus... / Tipitakaコーパスをダウンロード中... / 正在下載Tipitaka語料庫...\x1b[0m");
                ensure_dir(&tipitaka_dir)?;
                if !clone_tipitaka_sparse(&tipitaka_dir) {
                    anyhow::bail!("Failed to clone Tipitaka repository");
                }
            } else { println!("[init] tipitaka-xml exists: {}", tipitaka_dir.to_string_lossy()); }
            // build indices
            eprintln!("[init] Building CBETA index...");
            let cbeta_entries = build_cbeta_index(&cbeta_dir);
            eprintln!("[init] Found {} CBETA entries", cbeta_entries.len());
            
            eprintln!("[init] Building Tipitaka index...");
            let tipitaka_entries = build_index(&tipitaka_dir.join("romn"), Some("romn"));
            eprintln!("[init] Found {} Tipitaka entries", tipitaka_entries.len());
            
            let cache_dir = base_dir.join("cache"); fs::create_dir_all(&cache_dir)?;
            let cbeta_out = cache_dir.join("cbeta-index.json");
            let tipitaka_out = cache_dir.join("tipitaka-index.json");
            fs::write(&cbeta_out, serde_json::to_vec(&cbeta_entries)?)?;
            fs::write(&tipitaka_out, serde_json::to_vec(&tipitaka_entries)?)?;
            println!("[init] cbeta-index: {} ({} entries)", cbeta_out.to_string_lossy(), cbeta_entries.len());
            println!("[init] tipitaka-index: {} ({} entries)", tipitaka_out.to_string_lossy(), tipitaka_entries.len());
        }
        Commands::CbetaTitleSearch { query, limit, json } => {
            let idx = load_or_build_cbeta_index_cli();
            let hits = best_match(&idx, &query, limit);
            let summary = hits.iter().enumerate().map(|(i,h)| format!("{}. {}  {}", i+1, h.entry.id, h.entry.title)).collect::<Vec<_>>().join("\n");
            let meta = serde_json::json!({
                "count": hits.len(),
                "results": hits.iter().map(|h| serde_json::json!({"id": h.entry.id, "title": h.entry.title, "path": h.entry.path, "score": h.score})).collect::<Vec<_>>()
            });
            if json {
                let envelope = serde_json::json!({
                    "jsonrpc":"2.0","id": serde_json::Value::Null,
                    "result": { "content": [{"type":"text","text": summary }], "_meta": meta }
                });
                println!("{}", serde_json::to_string_pretty(&envelope)?);
            } else {
                for (i, h) in hits.iter().enumerate() { println!("{}. {}  {}", i+1, h.entry.id, h.entry.title); }
            }
        }
        Commands::CbetaFetch { id, query, part, include_notes, headings_limit, start_char, end_char, max_chars, page, page_size, line_number, context_before, context_after, context_lines, json } => {
            let path = resolve_cbeta_path_cli(id.as_deref(), query.as_deref());
            if path.as_os_str().is_empty() || !path.exists() { return Ok(()); }
            let xml = std::fs::read(&path).map(|b| decode_xml_bytes(&b)).unwrap_or_default();
            let (text, extraction_method, part_matched) = if let Some(line_num) = line_number {
                // 新しいパラメータを優先、fallbackで古いパラメータを使用
                let before = context_lines.unwrap_or(context_before);
                let after = context_lines.unwrap_or(context_after);
                let context_text = daizo_core::extract_xml_around_line_asymmetric(&xml, line_num, before, after);
                (context_text, format!("line-context-{}-{}-{}", line_num, before, after), false)
            } else if let Some(p) = part.as_ref() {
                if let Some(sec) = extract_cbeta_juan(&xml, p) { (sec, "cbeta-juan".to_string(), true) } else { (extract_text_opts(&xml, include_notes), "full".to_string(), false) }
            } else { (extract_text_opts(&xml, include_notes), "full".to_string(), false) };
            let args = SliceArgs { page, page_size, start_char, end_char, max_chars };
            let sliced = slice_text_cli(&text, &args);
            let heads = list_heads_cbeta(&xml);
            if json {
                let idx = load_or_build_cbeta_index_cli();
                let (matched_id, matched_title, matched_score) = if let Some(q) = query.as_deref() {
                    if let Some(hit) = best_match(&idx, q, 1).into_iter().next() { (Some(hit.entry.id.clone()), Some(hit.entry.title.clone()), Some(hit.score)) } else { (id.clone(), None, None) }
                } else { (id.clone(), None, None) };
                let meta = serde_json::json!({
                    "totalLength": text.len(),
                    "returnedStart": args.start().unwrap_or(0),
                    "returnedEnd": args.end_bound(text.len(), sliced.len()),
                    "truncated": (sliced.len() as u64) < (text.len() as u64),
                    "sourcePath": path.to_string_lossy(),
                    "extractionMethod": extraction_method,
                    "partMatched": part_matched,
                    "headingsTotal": heads.len(),
                    "headingsPreview": heads.into_iter().take(headings_limit).collect::<Vec<_>>(),
                    "matchedId": matched_id,
                    "matchedTitle": matched_title,
                    "matchedScore": matched_score,
                });
                let envelope = serde_json::json!({
                    "jsonrpc":"2.0","id": serde_json::Value::Null,
                    "result": { "content": [{"type":"text","text": sliced}], "_meta": meta }
                });
                println!("{}", serde_json::to_string_pretty(&envelope)?);
            } else {
                println!("{}", sliced);
                eprintln!("[meta] source={} len={} returned={}..{} headings={} extraction=cli-cbeta part={} includeNotes={}",
                    path.to_string_lossy(), text.len(), args.start().unwrap_or(0), args.end_bound(text.len(), sliced.len()), heads.len(), part.unwrap_or_default(), include_notes);
                if !heads.is_empty() { eprintln!("[meta] heads: {}", heads.into_iter().take(headings_limit).collect::<Vec<_>>().join(" | ")); }
            }
        }
        Commands::SatSearch { query, rows, offs, exact, titles_only, fields, fq, autofetch, start_char, max_chars, json } => {
            let wrap = sat_wrap7_search_json(&query, rows, offs, &fields, &fq);
            if autofetch {
                if let Some(w) = wrap.clone() {
                    let docs = w.get("response").and_then(|r| r.get("docs")).and_then(|v| v.as_array()).cloned().unwrap_or_default();
                    if !docs.is_empty() {
                        let mut best_idx = 0usize; let mut best_sc = -1.0f32;
                        for (i,d) in docs.iter().enumerate() {
                            let title = d.get("fascnm").and_then(|v| v.as_str()).unwrap_or("");
                            let sc = title_score(title, &query);
                            if sc > best_sc { best_sc = sc; best_idx = i; }
                        }
                        let chosen = &docs[best_idx];
                        let useid = chosen.get("startid").and_then(|v| v.as_str()).unwrap_or("");
                        let url = sat_detail_build_url(useid);
                        let t = sat_fetch_cli(&url);
                        let start = start_char.unwrap_or(0);
                        let args = SliceArgs { page: None, page_size: None, start_char: Some(start), end_char: None, max_chars };
                        let sliced = slice_text_cli(&t, &args);
                        if json {
                            let count = w.get("response").and_then(|r| r.get("numFound")).and_then(|v| v.as_u64()).unwrap_or(0);
                            let meta = serde_json::json!({
                                "totalLength": t.len(),
                                "returnedStart": start,
                                "returnedEnd": args.end_bound(t.len(), sliced.len()),
                                "truncated": (sliced.len() as u64) < (t.len() as u64),
                                "sourceUrl": url,
                                "extractionMethod": "sat-detail-extract",
                                "search": {"rows": rows, "offs": offs, "fl": fields, "fq": fq, "count": count},
                                "chosen": chosen,
                                "titleScore": best_sc,
                            });
                            let envelope = serde_json::json!({
                                "jsonrpc":"2.0","id": serde_json::Value::Null,
                                "result": { "content": [{"type":"text","text": sliced}], "_meta": meta }
                            });
                            println!("{}", serde_json::to_string_pretty(&envelope)?);
                        } else {
                            println!("{}", sliced);
                            eprintln!("[meta] url={} chosen_title={} score={}", url, chosen.get("fascnm").and_then(|v| v.as_str()).unwrap_or("") , best_sc);
                        }
                        return Ok(());
                    }
                }
                let text = "no results".to_string();
                if json { println!("{}", serde_json::to_string_pretty(&serde_json::json!({
                    "jsonrpc":"2.0","id":null,
                    "result": {"content":[{"type":"text","text": text}],"_meta": {"count":0}} }))?);
                } else { println!("{}", text); }
                return Ok(());
            }
            let wrap = wrap.unwrap_or_else(|| {
                let hits = sat_search_results_cli(&query, rows, offs, exact, titles_only);
                serde_json::json!({"response": {"numFound": hits.len(), "start": offs, "docs": hits }})
            });
            let docs = wrap.get("response").and_then(|r| r.get("docs")).cloned().unwrap_or(serde_json::json!([]));
            let count = wrap.get("response").and_then(|r| r.get("numFound")).and_then(|v| v.as_u64()).unwrap_or(0);
            let meta = serde_json::json!({ "count": count, "results": docs, "titlesOnly": titles_only, "fl": fields, "fq": fq });
            let summary = if titles_only { format!("{} titles; see _meta.results", meta["count"].as_u64().unwrap_or(0)) } else { format!("{} results; see _meta.results", meta["count"].as_u64().unwrap_or(0)) };
            if json {
                let envelope = serde_json::json!({
                    "jsonrpc":"2.0","id": serde_json::Value::Null,
                    "result": { "content": [{"type":"text","text": summary }], "_meta": meta }
                });
                println!("{}", serde_json::to_string_pretty(&envelope)?);
            } else {
                println!("{}", summary);
                eprintln!("{}", serde_json::to_string_pretty(&meta)?);
            }
        }
        Commands::SatFetch { url, useid, start_char, max_chars, json } => {
            let url_final = if let Some(uid) = useid { sat_detail_build_url(&uid) } else { url.unwrap_or_default() };
            let t = sat_fetch_cli(&url_final);
            let start = start_char.unwrap_or(0);
            let args = SliceArgs { page: None, page_size: None, start_char: Some(start), end_char: None, max_chars };
            let sliced = slice_text_cli(&t, &args);
            if json {
                let meta = serde_json::json!({
                    "totalLength": t.len(),
                    "returnedStart": start,
                    "returnedEnd": args.end_bound(t.len(), sliced.len()),
                    "truncated": (sliced.len() as u64) < (t.len() as u64),
                    "sourceUrl": url_final,
                    "extractionMethod": "sat-detail-extract",
                });
                let envelope = serde_json::json!({
                    "jsonrpc":"2.0","id": serde_json::Value::Null,
                    "result": { "content": [{"type":"text","text": sliced}], "_meta": meta }
                });
                println!("{}", serde_json::to_string_pretty(&envelope)?);
            } else {
                println!("{}", sliced);
                eprintln!("[meta] url={} total={} start={} returned={}", url_final, t.len(), start, sliced.len());
            }
        }
        Commands::SatDetail { useid, key: _, start_char, max_chars, json } => {
            let url = sat_detail_build_url(&useid);
            let t = sat_fetch_cli(&url);
            let start = start_char.unwrap_or(0);
            let args = SliceArgs { page: None, page_size: None, start_char: Some(start), end_char: None, max_chars };
            let sliced = slice_text_cli(&t, &args);
            if json {
                let meta = serde_json::json!({
                    "totalLength": t.len(),
                    "returnedStart": start,
                    "returnedEnd": args.end_bound(t.len(), sliced.len()),
                    "truncated": (sliced.len() as u64) < (t.len() as u64),
                    "sourceUrl": url,
                    "extractionMethod": "sat-detail-extract",
                });
                let envelope = serde_json::json!({
                    "jsonrpc":"2.0","id": serde_json::Value::Null,
                    "result": { "content": [{"type":"text","text": sliced}], "_meta": meta }
                });
                println!("{}", serde_json::to_string_pretty(&envelope)?);
            } else {
                println!("{}", sliced);
                eprintln!("[meta] url={} total={} start={} returned={}", url, t.len(), start, sliced.len());
            }
        }
        Commands::SatPipeline { query, rows, offs, fields, fq, start_char, max_chars, json } => {
            let wrap = sat_wrap7_search_json(&query, rows, offs, &fields, &fq);
            if wrap.is_none() {
                let text = "no results".to_string();
                if json { println!("{}", serde_json::to_string_pretty(&serde_json::json!({
                    "jsonrpc":"2.0","id":null,
                    "result": {"content":[{"type":"text","text": text}],"_meta": {"count":0}} }))?);
                } else { println!("{}", text); }
                return Ok(());
            }
            let wrap = wrap.unwrap();
            let docs = wrap.get("response").and_then(|r| r.get("docs")).and_then(|v| v.as_array()).cloned().unwrap_or_default();
            if docs.is_empty() {
                let text = "no results".to_string();
                if json { println!("{}", serde_json::to_string_pretty(&serde_json::json!({
                    "jsonrpc":"2.0","id":null,
                    "result": {"content":[{"type":"text","text": text}],"_meta": {"count":0}} }))?);
                } else { println!("{}", text); }
                return Ok(());
            }
            let mut best_idx = 0usize; let mut best_sc = -1.0f32;
            for (i, d) in docs.iter().enumerate() {
                let title = d.get("fascnm").and_then(|v| v.as_str()).unwrap_or("");
                let sc = title_score(title, &query);
                if sc > best_sc { best_sc = sc; best_idx = i; }
            }
            let chosen = &docs[best_idx];
            let useid = chosen.get("startid").and_then(|v| v.as_str()).unwrap_or("");
            let url = sat_detail_build_url(useid);
            let t = sat_fetch_cli(&url);
            let start = start_char.unwrap_or(0);
            let args = SliceArgs { page: None, page_size: None, start_char: Some(start), end_char: None, max_chars };
            let sliced = slice_text_cli(&t, &args);
            if json {
                let meta = serde_json::json!({
                    "totalLength": t.len(),
                    "returnedStart": start,
                    "returnedEnd": args.end_bound(t.len(), sliced.len()),
                    "truncated": (sliced.len() as u64) < (t.len() as u64),
                    "sourceUrl": url,
                    "extractionMethod": "sat-detail-extract",
                    "search": {"rows": rows, "offs": offs, "fl": fields, "fq": fq, "count": wrap.get("response").and_then(|r| r.get("numFound")).and_then(|x| x.as_u64()).unwrap_or(0)},
                    "chosen": chosen,
                    "titleScore": best_sc,
                });
                let envelope = serde_json::json!({
                    "jsonrpc":"2.0","id": serde_json::Value::Null,
                    "result": { "content": [{"type":"text","text": sliced}], "_meta": meta }
                });
                println!("{}", serde_json::to_string_pretty(&envelope)?);
            } else {
                println!("{}", sliced);
                eprintln!("[meta] url={} total={} start={} returned={} chosen_title={} score={}", url, t.len(), start, sliced.len(), chosen.get("fascnm").and_then(|v| v.as_str()).unwrap_or("") , best_sc);
            }
        }
        Commands::CbetaIndex { root, out } => {
            let default_base = default_daizo().join("xml-p5");
            let base = root.unwrap_or(default_base.clone());
            
            // Ensure CBETA data exists
            if !default_base.exists() {
                eprintln!("[cbeta-index] CBETA data not found, downloading...");
                let ok = run("git", &["clone", "--depth", "1", "https://github.com/cbeta-org/xml-p5", default_base.to_string_lossy().as_ref()], None);
                if !ok {
                    anyhow::bail!("Failed to clone CBETA repository");
                }
            }
            
            let entries = build_cbeta_index(&base);
            let outp = out.unwrap_or(default_daizo().join("cache").join("cbeta-index.json"));
            if let Some(parent) = outp.parent() { fs::create_dir_all(parent)?; }
            fs::write(&outp, serde_json::to_vec(&entries)?)?;
            println!("{}", serde_json::to_string(&IndexResult { count: entries.len(), out: outp.to_string_lossy().as_ref() })?);
        }
        Commands::TipitakaIndex { root, out } => {
            let default_base = default_daizo().join("tipitaka-xml");
            let base = root.unwrap_or(default_base.clone());
            
            // Ensure Tipitaka data exists
            if !default_base.exists() {
                eprintln!("[tipitaka-index] Tipitaka data not found, downloading...");
                if !clone_tipitaka_sparse(&default_base) {
                    anyhow::bail!("Failed to clone Tipitaka repository");
                }
            }
            
            let entries = build_tipitaka_index(&base);
            let outp = out.unwrap_or(default_daizo().join("cache").join("tipitaka-index.json"));
            if let Some(parent) = outp.parent() { fs::create_dir_all(parent)?; }
            fs::write(&outp, serde_json::to_vec(&entries)?)?;
            println!("{}", serde_json::to_string(&IndexResult { count: entries.len(), out: outp.to_string_lossy().as_ref() })?);
        }
        Commands::TipitakaTitleSearch { query, limit, json } => {
            let idx = load_or_build_tipitaka_index_cli();
            let hits = best_match(&idx, &query, limit);
            if json {
                let items: Vec<_> = hits.iter().map(|h| serde_json::json!({
                    "id": std::path::Path::new(&h.entry.path).file_stem().unwrap().to_string_lossy(),
                    "title": h.entry.title,
                    "path": h.entry.path,
                    "score": h.score,
                })).collect();
                println!("{}", serde_json::to_string_pretty(&serde_json::json!({"count": items.len(), "results": items}))?);
            } else {
                for (i, h) in hits.iter().enumerate() {
                    let id = std::path::Path::new(&h.entry.path).file_stem().unwrap().to_string_lossy();
                    println!("{}. {}  {}", i+1, id, h.entry.title);
                }
            }
        }
        Commands::TipitakaFetch { id, query, head_index, head_query, headings_limit, start_char, end_char, max_chars, page, page_size, line_number, context_before, context_after, context_lines, json } => {
            let path = resolve_tipitaka_path(id.as_deref(), query.as_deref());
            if path.as_os_str().is_empty() || !path.exists() {
                // no output if not found
                return Ok(());
            }
            let xml = std::fs::read(&path).map(|b| decode_xml_bytes(&b)).unwrap_or_default();
            let mut text = if let Some(line_num) = line_number {
                // 新しいパラメータを優先、fallbackで古いパラメータを使用
                let before = context_lines.unwrap_or(context_before);
                let after = context_lines.unwrap_or(context_after);
                daizo_core::extract_xml_around_line_asymmetric(&xml, line_num, before, after)
            } else if let Some(ref hq) = head_query { 
                extract_section_by_head(&xml, None, Some(hq)).unwrap_or_else(|| extract_text(&xml))
            } else if let Some(hi) = head_index { 
                extract_section_by_head(&xml, Some(hi), None).unwrap_or_else(|| extract_text(&xml)) 
            } else { 
                extract_text(&xml) 
            };
            if text.trim().is_empty() {
                // base-id fallback: open smallest sequence for same stem
                if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
                    if let Some(cand) = find_tipitaka_content_for_base(&stem) { if cand != path {
                        let xml2 = std::fs::read(&cand).map(|b| decode_xml_bytes(&b)).unwrap_or_default();
                        text = if let Some(ref hq) = head_query { extract_section_by_head(&xml2, None, Some(hq))
                                } else if let Some(hi) = head_index { extract_section_by_head(&xml2, Some(hi), None) } else { None }
                                .unwrap_or_else(|| extract_text(&xml2));
                    }}
                }
            }
            // final fallback: strip tags
            if text.trim().is_empty() && !xml.trim().is_empty() {
                if let Ok(re) = regex::Regex::new(r"<[^>]+>") {
                    let t = re.replace_all(&xml, " ");
                    text = t.split_whitespace().collect::<Vec<_>>().join(" ");
                }
            }
            let args = SliceArgs { page, page_size, start_char, end_char, max_chars };
            let sliced = slice_text_cli(&text, &args);
            let heads = list_heads_generic(&xml);
            if json {
                let idx = load_or_build_tipitaka_index_cli();
                let (matched_id, matched_title, matched_score) = if let Some(q) = query.as_deref() {
                    if let Some(hit) = best_match(&idx, q, 1).into_iter().next() {
                        (std::path::Path::new(&hit.entry.path).file_stem().map(|s| s.to_string_lossy().into_owned()), Some(hit.entry.title.clone()), Some(hit.score))
                    } else { (path.file_stem().and_then(|s| s.to_str()).map(|s| s.to_string()), None, None) }
                } else { (path.file_stem().and_then(|s| s.to_str()).map(|s| s.to_string()), None, None) };
                let biblio = tipitaka_biblio(&xml);
                let meta = serde_json::json!({
                    "totalLength": text.len(),
                    "returnedStart": args.start().unwrap_or(0),
                    "returnedEnd": args.end_bound(text.len(), sliced.len()),
                    "truncated": (sliced.len() as u64) < (text.len() as u64),
                    "sourcePath": path.to_string_lossy(),
                    "extractionMethod": if head_query.is_some() { "head-query" } else if head_index.is_some() { "head-index" } else { "full" },
                    "headingsTotal": heads.len(),
                    "headingsPreview": heads.clone().into_iter().take(headings_limit).collect::<Vec<_>>(),
                    "matchedId": matched_id,
                    "matchedTitle": matched_title,
                    "matchedScore": matched_score,
                    "biblio": biblio,
                });
                let envelope = serde_json::json!({
                    "jsonrpc":"2.0","id": serde_json::Value::Null,
                    "result": { "content": [{"type":"text","text": sliced}], "_meta": meta }
                });
                println!("{}", serde_json::to_string_pretty(&envelope)?);
            } else {
                println!("{}", sliced);
                eprintln!("[meta] source={} len={} returned={}..{} headings={} extraction=cli",
                    path.to_string_lossy(), text.len(),
                    args.start().unwrap_or(0), args.end_bound(text.len(), sliced.len()), heads.len());
                if !heads.is_empty() { eprintln!("[meta] heads: {}", heads.into_iter().take(headings_limit).collect::<Vec<_>>().join(" | ")); }
            }
        }
        Commands::IndexRebuild { source } => {
            eprintln!("\x1b[33m📥 Rebuilding search indexes... / インデックスを再構築中... / 正在重建搜索索引...\x1b[0m");
            
            let src = source.to_lowercase();
            let base = default_daizo();
            let cache = base.join("cache");
            fs::create_dir_all(&cache)?;
            
            let mut summary = serde_json::Map::new();
            let mut rebuilt: Vec<&str> = Vec::new();
            
            // Delete cache files first
            if src == "cbeta" || src == "all" { 
                let _ = fs::remove_file(cache.join("cbeta-index.json")); 
            }
            if src == "tipitaka" || src == "all" { 
                let _ = fs::remove_file(cache.join("tipitaka-index.json")); 
            }
            
            // Call individual index commands
            if src == "cbeta" || src == "all" {
                eprintln!("[rebuild] Running cbeta-index...");
                let cli_path = std::env::current_exe()?;
                let ok = run(cli_path.to_string_lossy().as_ref(), &["cbeta-index"], None);
                if ok {
                    rebuilt.push("cbeta");
                    summary.insert("cbeta".to_string(), serde_json::json!("completed"));
                } else {
                    eprintln!("[error] CBETA index rebuild failed");
                }
            }
            
            if src == "tipitaka" || src == "all" {
                eprintln!("[rebuild] Running tipitaka-index...");
                let cli_path = std::env::current_exe()?;
                let ok = run(cli_path.to_string_lossy().as_ref(), &["tipitaka-index"], None);
                if ok {
                    rebuilt.push("tipitaka");
                    summary.insert("tipitaka".to_string(), serde_json::json!("completed"));
                } else {
                    eprintln!("[error] Tipitaka index rebuild failed");
                }
            }
            
            summary.insert("rebuilt".to_string(), serde_json::json!(rebuilt));
            println!("{}", serde_json::to_string(&summary)?);
        }
        Commands::ExtractText { path } => {
            let xml = if let Some(p) = path { fs::read_to_string(p)? } else {
                let mut s = String::new(); io::stdin().read_to_string(&mut s)?; s
            };
            let t = extract_text(&xml);
            println!("{}", t);
        }
        Commands::CbetaSearch { query, max_results, max_matches_per_file, json } => {
            let results = cbeta_grep(&cbeta_root(), &query, max_results, max_matches_per_file);
            
            if json {
                let meta = serde_json::json!({
                    "searchPattern": query,
                    "totalFiles": results.len(),
                    "results": results,
                    "hint": "Use cbeta-fetch with the file_id and recommended parts to get full content"
                });
                let summary = format!("Found {} files with matches for '{}'", results.len(), query);
                let envelope = serde_json::json!({
                    "jsonrpc":"2.0","id": serde_json::Value::Null,
                    "result": { "content": [{"type":"text","text": summary}], "_meta": meta }
                });
                println!("{}", serde_json::to_string_pretty(&envelope)?);
            } else {
                println!("Found {} files with matches for '{}':\n", results.len(), query);
                for (i, result) in results.iter().enumerate() {
                    println!("{}. {} ({})", i + 1, result.title, result.file_id);
                    println!("   {} matches, {}", result.total_matches, 
                        result.fetch_hints.total_content_size.as_deref().unwrap_or("unknown size"));
                    
                    for (j, m) in result.matches.iter().enumerate().take(2) {
                        println!("   Match {}: ...{}...", j + 1, 
                            m.context.chars().take(100).collect::<String>());
                    }
                    if result.matches.len() > 2 {
                        println!("   ... and {} more matches", result.matches.len() - 2);
                    }
                    
                    if !result.fetch_hints.recommended_parts.is_empty() {
                        println!("   Recommended parts: {}", 
                            result.fetch_hints.recommended_parts.join(", "));
                    }
                    println!();
                }
            }
        }
        Commands::TipitakaSearch { query, max_results, max_matches_per_file, json } => {
            let results = tipitaka_grep(&tipitaka_root(), &query, max_results, max_matches_per_file);
            
            if json {
                let meta = serde_json::json!({
                    "searchPattern": query,
                    "totalFiles": results.len(),
                    "results": results,
                    "hint": "Use tipitaka-fetch with the file_id to get full content"
                });
                let summary = format!("Found {} files with matches for '{}'", results.len(), query);
                let envelope = serde_json::json!({
                    "jsonrpc":"2.0","id": serde_json::Value::Null,
                    "result": { "content": [{"type":"text","text": summary}], "_meta": meta }
                });
                println!("{}", serde_json::to_string_pretty(&envelope)?);
            } else {
                println!("Found {} files with matches for '{}':\n", results.len(), query);
                for (i, result) in results.iter().enumerate() {
                    println!("{}. {} ({})", i + 1, result.title, result.file_id);
                    println!("   {} matches, {}", result.total_matches, 
                        result.fetch_hints.total_content_size.as_deref().unwrap_or("unknown size"));
                    
                    for (j, m) in result.matches.iter().enumerate().take(2) {
                        println!("   Match {}: ...{}...", j + 1, 
                            m.context.chars().take(100).collect::<String>());
                    }
                    if result.matches.len() > 2 {
                        println!("   ... and {} more matches", result.matches.len() - 2);
                    }
                    
                    if !result.fetch_hints.structure_info.is_empty() {
                        println!("   Structure: {}", 
                            result.fetch_hints.structure_info.join(", "));
                    }
                    println!();
                }
            }
        }
        Commands::Update { git, yes } => {
            // Build the cargo install command (owned strings)
            let mut cmd: Vec<String> = Vec::new();
            cmd.push("cargo".into());
            cmd.push("install".into());
            if let Some(repo) = git {
                cmd.push("--git".into());
                cmd.push(repo);
                cmd.push("daizo-cli".into());
            } else {
                cmd.push("--path".into());
                cmd.push(".".into());
                cmd.push("-p".into());
                cmd.push("daizo-cli".into());
            }
            cmd.push("--locked".into());
            cmd.push("--force".into());
            let preview = cmd.join(" ");
            if yes {
                // Convert to &str for run()
                let argv: Vec<&str> = cmd.iter().skip(1).map(|s| s.as_str()).collect();
                let ok = run(&cmd[0], &argv, None);
                if !ok { anyhow::bail!("update failed: {}", preview); }
                // Post-install: rebuild indexes using the installed binary
                let ok2 = run("daizo-cli", &["index-rebuild", "--source", "all"], None);
                if !ok2 { eprintln!("[warn] index rebuild failed after update; run: daizo-cli index-rebuild --source all"); }
            } else {
                eprintln!("[plan] {}", preview);
                eprintln!("Use --git <repo-url> to install from GitHub; add --yes to execute.");
            }
        }
        Commands::Version {} => {
            println!("daizo-cli {}", env!("CARGO_PKG_VERSION"));
        }
        Commands::Doctor { verbose } => {
            let base = default_daizo();
            let bin = base.join("bin");
            let cli = bin.join("daizo-cli");
            let mcp = bin.join("daizo-mcp");
            let cbeta = base.join("xml-p5");
            let tipi = base.join("tipitaka-xml");
            let cache = base.join("cache");
            println!("DAIZO_DIR: {}", base.display());
            println!("bin: {}", bin.display());
            println!(" - daizo-cli: {}", if cli.exists() { "OK" } else { "MISSING" });
            println!(" - daizo-mcp: {}", if mcp.exists() { "OK" } else { "MISSING" });
            println!("data:");
            println!(" - xml-p5: {}", if cbeta.exists() { "OK" } else { "MISSING (will clone on demand)" });
            println!(" - tipitaka-xml: {}", if tipi.exists() { "OK" } else { "MISSING (will clone on demand)" });
            println!("cache: {}", if cache.exists() { cache.display().to_string() } else { format!("{} (will create)", cache.display()) });
            if verbose {
                if cli.exists() { println!("   size: {} bytes", std::fs::metadata(&cli).map(|m| m.len()).unwrap_or(0)); }
                if mcp.exists() { println!("   size: {} bytes", std::fs::metadata(&mcp).map(|m| m.len()).unwrap_or(0)); }
            }
        }
        Commands::Uninstall { purge } => {
            let base = default_daizo();
            let bin = base.join("bin");
            let cli = bin.join("daizo-cli");
            let mcp = bin.join("daizo-mcp");
            let mut removed: Vec<String> = Vec::new();
            if cli.exists() { let _ = std::fs::remove_file(&cli); removed.push(cli.display().to_string()); }
            if mcp.exists() { let _ = std::fs::remove_file(&mcp); removed.push(mcp.display().to_string()); }
            if purge {
                let cbeta = base.join("xml-p5");
                let tipi = base.join("tipitaka-xml");
                let cache = base.join("cache");
                let _ = std::fs::remove_dir_all(&cbeta);
                let _ = std::fs::remove_dir_all(&tipi);
                let _ = std::fs::remove_dir_all(&cache);
                println!("[purge] removed data/cache under {}", base.display());
            }
            if removed.is_empty() { println!("no binaries removed (nothing found under {})", bin.display()); }
            else { println!("removed: {}", removed.join(", ")); }
        }
    }
    Ok(())
}

// ===== helpers mirrored from MCP =====

#[derive(Clone, Debug, serde::Serialize)]
struct ScoredHit<'a> { #[serde(skip_serializing)] entry: &'a daizo_core::IndexEntry, score: f32 }

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

fn best_match<'a>(entries: &'a [daizo_core::IndexEntry], q: &str, limit: usize) -> Vec<ScoredHit<'a>> {
    let nq = normalized(q);
    let mut scored: Vec<(f32, &daizo_core::IndexEntry)> = entries.iter().map(|e| {
        let meta_str = e.meta.as_ref().map(|m| m.values().cloned().collect::<Vec<_>>().join(" ")).unwrap_or_default();
        let alias = e.meta.as_ref().and_then(|m| m.get("alias")).cloned().unwrap_or_default();
        let hay_all = format!("{} {} {}", e.title, e.id, meta_str);
        let hay = normalized(&hay_all);
        let mut score = if hay.contains(&nq) { 1.0f32 } else {
            let s_char = jaccard(&hay, &nq);
            let s_tok = token_jaccard(&hay_all, q);
            s_char.max(s_tok)
        };
        if score < 0.95 && (is_subsequence(&hay, &nq) || is_subsequence(&nq, &hay)) { score = score.max(0.85); }
        let nalias = normalized_with_spaces(&alias).replace(' ', "");
        let nq_nospace = normalized_with_spaces(q).replace(' ', "");
        if !nalias.is_empty() {
            if nalias.split_whitespace().any(|a| a == nq_nospace) || nalias.contains(&nq_nospace) {
                score = score.max(0.95);
            }
        }
        if q.chars().any(|c| c.is_ascii_digit()) {
            let hws = normalized_with_spaces(&hay_all);
            if hws.contains(&normalized_with_spaces(q)) { score = (score + 0.05).min(1.0); }
        }
        (score, e)
    }).collect();
    scored.sort_by(|a,b| b.0.partial_cmp(&a.0).unwrap());
    scored.into_iter().take(limit).map(|(s,e)| ScoredHit { entry: e, score: s }).collect()
}

fn daizo_home() -> PathBuf { std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from(".")).join(".daizo") }
fn tipitaka_root() -> PathBuf { daizo_home().join("tipitaka-xml").join("romn") }
fn cbeta_root() -> PathBuf { daizo_home().join("xml-p5") }
fn cache_dir() -> PathBuf { daizo_home().join("cache") }

fn load_or_build_tipitaka_index_cli() -> Vec<daizo_core::IndexEntry> {
    let out = cache_dir().join("tipitaka-index.json");
    if let Ok(b) = std::fs::read(&out) {
        if let Ok(mut v) = serde_json::from_slice::<Vec<daizo_core::IndexEntry>>(&b) {
            v.retain(|e| !e.path.ends_with(".toc.xml"));
            let missing = v.iter().take(10).filter(|e| !std::path::Path::new(&e.path).exists()).count();
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
            if !v.is_empty() && missing == 0 && !lacks_meta && !lacks_heads && !lacks_composite { return v; }
        }
    }
    let mut entries = build_tipitaka_index(&tipitaka_root());
    entries.retain(|e| !e.path.ends_with(".toc.xml"));
    let _ = std::fs::create_dir_all(cache_dir());
    let _ = std::fs::write(&out, serde_json::to_vec(&entries).unwrap_or_default());
    entries
}

fn load_or_build_cbeta_index_cli() -> Vec<daizo_core::IndexEntry> {
    let out = cache_dir().join("cbeta-index.json");
    if let Ok(b) = std::fs::read(&out) {
        if let Ok(v) = serde_json::from_slice::<Vec<daizo_core::IndexEntry>>(&b) {
            let missing = v.iter().take(10).filter(|e| !std::path::Path::new(&e.path).exists()).count();
            if !v.is_empty() && missing == 0 { return v; }
        }
    }
    let entries = build_index(&cbeta_root(), None);
    let _ = std::fs::create_dir_all(cache_dir());
    let _ = std::fs::write(&out, serde_json::to_vec(&entries).unwrap_or_default());
    entries
}

fn find_in_dir(root: &std::path::Path, stem_hint: &str) -> Option<PathBuf> {
    for e in walkdir::WalkDir::new(root).into_iter().filter_map(|e| e.ok()) {
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
    let root = tipitaka_root();
    let mut best: Option<(u32, PathBuf)> = None;
    for e in walkdir::WalkDir::new(&root).into_iter().filter_map(|e| e.ok()) {
        if e.file_type().is_file() {
            let p = e.path();
            if p.extension().and_then(|s| s.to_str()) != Some("xml") { continue; }
            let name = p.file_name().and_then(|s| s.to_str()).unwrap_or("").to_lowercase();
            if !name.contains(&base.to_lowercase()) { continue; }
            if name.contains("toc") || name.contains("sitemap") || name.contains("tree") { continue; }
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

fn resolve_tipitaka_path(id: Option<&str>, query: Option<&str>) -> PathBuf {
    let mut path: PathBuf = if let Some(q) = query {
        let idx = load_or_build_tipitaka_index_cli();
        if let Some(hit) = best_match(&idx, q, 1).into_iter().next() {
            PathBuf::from(&hit.entry.path)
        } else { PathBuf::new() }
    } else if let Some(id) = id {
        let idx = load_or_build_tipitaka_index_cli();
        let mut exact: Option<PathBuf> = None;
        for e in idx.iter() {
            if std::path::Path::new(&e.path).file_stem().map(|s| s == id).unwrap_or(false) {
                exact = Some(PathBuf::from(&e.path)); break;
            }
        }
        let mut best_seq: Option<(u32, PathBuf)> = None;
        if exact.is_none() {
            for e in idx.iter() {
                if let Some(stem) = std::path::Path::new(&e.path).file_stem().and_then(|s| s.to_str()) {
                    if let Some(rest) = stem.strip_prefix(id) {
                        let digits = rest.chars().take_while(|c| c.is_ascii_digit()).collect::<String>();
                        if !digits.is_empty() && rest.chars().all(|c| c.is_ascii_digit()) {
                            if let Ok(n) = digits.parse::<u32>() {
                                if best_seq.as_ref().map(|(bn,_)| n < *bn).unwrap_or(true) {
                                    best_seq = Some((n, PathBuf::from(&e.path)));
                                }
                            }
                        }
                    }
                }
            }
        }
        exact.or(best_seq.map(|(_,p)| p)).or_else(|| find_tipitaka_content_for_base(id)).or_else(|| find_in_dir(&tipitaka_root(), id)).unwrap_or_else(|| PathBuf::new())
    } else { PathBuf::new() };
    if path.as_os_str().is_empty() || !path.exists() {
        if let Some(id) = id { let fname = format!("{}.xml", id); if let Some(p) = find_exact_file_by_name(&tipitaka_root(), &fname) { path = p; } }
    }
    path
}

fn resolve_cbeta_path_cli(id: Option<&str>, query: Option<&str>) -> PathBuf {
    // 修正: IDがある場合は優先してqueryを無視
    if let Some(id) = id {
        if let Some(p) = resolve_cbeta_by_id_scan(id) { return p; }
        // fallback: anywhere *id*.xml
        for e in walkdir::WalkDir::new(cbeta_root()).into_iter().filter_map(|e| e.ok()) {
            if e.file_type().is_file() {
                let name = e.file_name().to_string_lossy().to_lowercase();
                if name.contains(&id.to_lowercase()) && name.ends_with(".xml") { return e.path().to_path_buf(); }
            }
        }
    } else if let Some(q) = query {
        let idx = load_or_build_cbeta_index_cli();
        if let Some(hit) = best_match(&idx, q, 1).into_iter().next() { return PathBuf::from(&hit.entry.path); }
    }
    PathBuf::new()
}

fn resolve_cbeta_by_id_scan(id: &str) -> Option<PathBuf> {
    let m = regex::Regex::new(r"^([A-Za-z]+)(\d+)$").ok()?;
    let root = cbeta_root();
    if let Some(c) = m.captures(id) {
        let canon = &c[1];
        let num = &c[2];
        for e in walkdir::WalkDir::new(root.join(canon)).into_iter().filter_map(|e| e.ok()) {
            if e.file_type().is_file() {
                let name = e.file_name().to_string_lossy().to_lowercase();
                if name.contains(&format!("n{}", num)) && name.ends_with(".xml") { return Some(e.path().to_path_buf()); }
            }
        }
    }
    None
}

fn find_exact_file_by_name(root: &std::path::Path, filename: &str) -> Option<PathBuf> {
    for e in walkdir::WalkDir::new(root).into_iter().filter_map(|e| e.ok()) {
        if e.file_type().is_file() {
            if let Some(name) = e.path().file_name().and_then(|s| s.to_str()) {
                if name.eq_ignore_ascii_case(filename) { return Some(e.path().to_path_buf()); }
            }
        }
    }
    None
}

fn extract_section_by_head(xml: &str, head_index: Option<usize>, head_query: Option<&str>) -> Option<String> {
    let re = regex::Regex::new(r"(?is)<head\\b[^>]*>(.*?)</head>").ok()?;
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

struct SliceArgs { page: Option<usize>, page_size: Option<usize>, start_char: Option<usize>, end_char: Option<usize>, max_chars: Option<usize> }
impl SliceArgs {
    fn start(&self) -> Option<usize> {
        if let (Some(p), Some(ps)) = (self.page, self.page_size) { Some(p*ps) }
        else { self.start_char }
    }
    fn end_bound(&self, total: usize, sliced_len: usize) -> usize {
        if let (Some(p), Some(ps)) = (self.page, self.page_size) { std::cmp::min(p*ps + ps, total) }
        else if let Some(e) = self.end_char { std::cmp::min(e, total) }
        else if let Some(mc) = self.max_chars { std::cmp::min(self.start().unwrap_or(0) + mc, total) }
        else { self.start().unwrap_or(0) + sliced_len }
    }
}
fn slice_text_cli(text: &str, args: &SliceArgs) -> String {
    let default_max = 8000usize;
    // Treat indices as character positions, not bytes
    let total_chars = text.chars().count();
    let start_char = std::cmp::min(args.start().unwrap_or(0), total_chars);
    let end_char = if let (Some(p), Some(ps)) = (args.page, args.page_size) { Some(p*ps + ps) }
                   else if let Some(e) = args.end_char { Some(e) }
                   else if let Some(mc) = args.max_chars { Some(start_char + mc) } else { None };
    let end_char = end_char.map(|e| std::cmp::min(e, total_chars)).unwrap_or_else(|| std::cmp::min(start_char + default_max, total_chars));
    if start_char >= end_char { return String::new(); }
    // Convert char indices to byte indices
    let s_byte = text.char_indices().nth(start_char).map(|(b,_)| b).unwrap_or(text.len());
    let e_byte = text.char_indices().nth(end_char).map(|(b,_)| b).unwrap_or(text.len());
    if s_byte > e_byte { return String::new(); }
    text[s_byte..e_byte].to_string()
}

fn decode_xml_bytes(bytes: &[u8]) -> String {
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
    // UTF-32 BOM cases omitted; extremely rare for XML and not directly supported here.
    let sniff_len = std::cmp::min(512, bytes.len());
    let head = &bytes[..sniff_len];
    if let Some(enc) = sniff_xml_encoding(head) {
        if let Some(encod) = encoding_rs::Encoding::for_label(enc.as_bytes()) {
            let (cow, _, _) = encod.decode(bytes);
            return cow.into_owned();
        }
    }
    match String::from_utf8(bytes.to_vec()) {
        Ok(s) => s,
        Err(_) => {
            let (cow, _, _) = encoding_rs::WINDOWS_1252.decode(bytes);
            cow.into_owned()
        }
    }
}

fn sniff_xml_encoding(head: &[u8]) -> Option<String> {
    let lower: Vec<u8> = head.iter().map(|b| b.to_ascii_lowercase()).collect();
    if let Some(pos) = lower.windows(8).position(|w| w == b"encoding") {
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

// ========== SAT helpers (CLI) ==========

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
struct SatHit { title: String, url: String, snippet: String }

fn http_client() -> &'static reqwest::blocking::Client {
    use std::sync::OnceLock;
    static CLI: OnceLock<reqwest::blocking::Client> = OnceLock::new();
    CLI.get_or_init(|| {
        reqwest::blocking::Client::builder()
            .timeout(std::time::Duration::from_secs(15))
            .build()
            .unwrap()
    })
}

// Tipitaka biblio (same logic as MCP)
fn tipitaka_biblio(xml: &str) -> serde_json::Value {
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

fn cache_path_for(url: &str) -> PathBuf {
    let mut hasher = sha1::Sha1::new();
    use sha1::Digest;
    hasher.update(url.as_bytes());
    let h = hasher.finalize();
    let fname = format!("{:x}.txt", h);
    let dir = cache_dir().join("sat");
    let _ = std::fs::create_dir_all(&dir);
    dir.join(fname)
}

fn sat_fetch_cli(url: &str) -> String {
    let cache = cache_path_for(url);
    if let Ok(t) = std::fs::read_to_string(&cache) { return t; }
    let mut backoff = 500u64;
    for _ in 0..3 {
        let res = http_client().get(url).send();
        if let Ok(r) = res {
            if r.status().is_success() {
                if let Ok(html) = r.text() {
                    let t = sat_extract_text(&html);
                    let _ = std::fs::write(&cache, &t);
                    return t;
                }
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(backoff)); backoff = (backoff*2).min(8000);
    }
    String::new()
}

fn sat_extract_text(html: &str) -> String {
    use scraper::{Html, Selector};
    let dom = Html::parse_document(html);
    let sel = Selector::parse("body").unwrap();
    let mut out = String::new();
    for n in dom.select(&sel) { out.push_str(&n.text().collect::<Vec<_>>().join(" ")); }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn sat_search_results_cli(q: &str, rows: usize, offs: usize, exact: bool, titles_only: bool) -> Vec<SatHit> {
    let base = "https://21dzk.l.u-tokyo.ac.jp/SAT2018/sat/satdb2018.php";
    let url = format!("{}?use=func&ui_lang=ja&form=0&smode=1&dpnum=10&db_num=100&tbl=SAT&jtype=AND&wk=&line=0&part=0&eps=&keyword={}&o8=1&l8=&o9=1&l9=&o4=2&l4=rb&spage={}&perpage={}",
        base, urlencoding::encode(q), offs, rows);
    let mut backoff = 500u64;
    for _ in 0..3 {
        if let Ok(r) = http_client().get(&url).send() {
            if let Ok(html) = r.text() {
                return parse_sat_search_html(&html, q, rows, offs, exact, titles_only);
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(backoff)); backoff = (backoff*2).min(8000);
    }
    Vec::new()
}

fn parse_sat_search_html(html: &str, q: &str, rows: usize, offs: usize, _exact: bool, titles_only: bool) -> Vec<SatHit> {
    use scraper::{Html, Selector};
    let doc = Html::parse_document(html);
    let row_sel = Selector::parse("tr").unwrap();
    let a_sel = Selector::parse("a").unwrap();
    let td_sel = Selector::parse("td").unwrap();
    let mut out: Vec<SatHit> = Vec::new();
    for tr in doc.select(&row_sel) {
        let mut tds = tr.select(&td_sel);
        let Some(_td1) = tds.next() else { continue };
        let Some(td2) = tds.next() else { continue };
        let Some(td3) = tds.next() else { continue };
        let title = td2.text().collect::<Vec<_>>().join(" ").split_whitespace().collect::<Vec<_>>().join(" ");
        let url = if let Some(a) = td3.select(&a_sel).next() { a.value().attr("href").unwrap_or("").to_string() } else { String::new() };
        if !url.contains("satdb2018pre.php") { continue; }
        out.push(SatHit { title, url, snippet: String::new() });
    }
    if titles_only {
        let nq = normalized(&q);
        let mut filtered: Vec<SatHit> = out.into_iter().filter(|h| normalized(&h.title).contains(&nq)).collect();
        let mut seen = std::collections::HashSet::new();
        filtered.retain(|h| seen.insert(h.title.clone()));
        let start = std::cmp::min(offs, filtered.len());
        let end = std::cmp::min(start + rows, filtered.len());
        filtered[start..end].to_vec()
    } else {
        let start = std::cmp::min(offs, out.len());
        let end = std::cmp::min(start + rows, out.len());
        out[start..end].to_vec()
    }
}

fn sat_wrap7_search_json(q: &str, rows: usize, offs: usize, fields: &str, fq: &Vec<String>) -> Option<serde_json::Value> {
    let url = sat_wrap7_build_url(q, rows, offs, fields, fq);
    // Fetch
    for _ in 0..2 {
        if let Ok(r) = http_client().get(&url).send() {
            if r.status().is_success() {
                if let Ok(txt) = r.text() {
                    if let Ok(json) = serde_json::from_str::<serde_json::Value>(&txt) { return Some(json); }
                }
            }
        }
    }
    None
}

fn sat_wrap7_build_url(q: &str, rows: usize, offs: usize, fields: &str, fq: &Vec<String>) -> String {
    let base = "https://21dzk.l.u-tokyo.ac.jp/SAT2018/wrap7.php";
    let mut url = format!(
        "{}?regex=off&q={}&rows={}&offs={}&schop=AND",
        base,
        urlencoding::encode(q),
        rows,
        offs
    );
    if !fields.trim().is_empty() { url.push_str(&format!("&fl={}", urlencoding::encode(fields))); }
    for f in fq { if !f.trim().is_empty() { url.push_str(&format!("&fq={}", urlencoding::encode(f))); } }
    url
}

fn sat_detail_build_url(useid: &str) -> String {
    // Required fixed params per observation: mode=detail, ob=1, mode2=2
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
    if sc < 0.95 && (is_subsequence(&a, &b) || is_subsequence(&b, &a)) { sc = sc.max(0.85); }
    sc
}

#[cfg(test)]
mod tests_cli_sat_wrap7_json {
    use super::*;

    #[test]
    fn builds_wrap7_url_with_fields_and_fq() {
        let url = sat_wrap7_build_url("大日", 5, 10, "id,fascnm", &vec!["tr:法賢".into(), "series:T".into()]);
        assert!(url.contains("regex=off"));
        assert!(url.contains("rows=5"));
        assert!(url.contains("offs=10"));
        assert!(url.contains("fl=id%2Cfascnm"));
        assert!(url.contains("fq=tr%3A%E6%B3%95%E8%B3%A2"));
        assert!(url.contains("fq=series%3AT"));
    }

    #[test]
    fn parse_minimal_wrap7_json() {
        let txt = r#"{
            "responseHeader": {"status":0, "QTime":1, "params":{"rows":"2","start":"0"}},
            "response": {"numFound":123, "start":0, "docs":[
                {"id":"1001","fascnm":"Title A","fascnum":"1","startid":"0001_,01,0001a01","endid":"0001_,01,0002b01"},
                {"id":"1002","fascnm":"Title B","tr":["譯者X"],"startid":"0002_,01,0001a01","endid":"0002_,01,0003b01"}
            ]}
        }"#;
        let v: serde_json::Value = serde_json::from_str(txt).unwrap();
        assert_eq!(v["response"]["numFound"].as_u64().unwrap(), 123);
        assert_eq!(v["response"]["docs"].as_array().unwrap().len(), 2);
        assert!(v["response"]["docs"][0]["fascnm"].is_string());
    }
}

#[cfg(test)]
mod tests_cli_sat_detail_url {
    use super::*;

    #[test]
    fn builds_detail_url_minimal() {
        let url = sat_detail_build_url("0015_,01,0246b01");
        assert!(url.contains("mode=detail"));
        assert!(url.contains("ob=1"));
        assert!(url.contains("mode2=2"));
        assert!(url.contains("useid=0015_%2C01%2C0246b01"));
        // Should not include unused params like key, cpos, regsw, mode4
        assert!(!url.contains("key="));
        assert!(!url.contains("cpos="));
        assert!(!url.contains("mode4"));
    }
}

#[cfg(test)]
mod tests_cli_sat {
    use super::*;
    use std::fs;

    #[test]
    fn parse_search_rows_basic() {
        let html = r#"
        <table>
          <tr><td>1</td><td>法華經</td><td><a href="https://21dzk.l.u-tokyo.ac.jp/SAT2018/satdb2018pre.php?id=1">detail</a></td></tr>
          <tr><td>2</td><td>阿含經</td><td><a href="https://21dzk.l.u-tokyo.ac.jp/SAT2018/satdb2018pre.php?id=2">detail</a></td></tr>
        </table>
        "#;
        let hits = parse_sat_search_html(html, "法華", 10, 0, true, true);
        assert_eq!(hits.len(), 1);
        assert!(hits[0].url.contains("satdb2018pre.php?id=1"));
        assert!(hits[0].title.contains("法華"));
    }

    #[test]
    fn extract_text_basic() {
        let html = "<html><body><h1>A</h1><p>B C</p></body></html>";
        let t = sat_extract_text(html);
        assert!(t.contains("A"));
        assert!(t.contains("B"));
        assert!(t.contains("C"));
    }

    #[test]
    fn fetch_uses_cache_without_network() {
        // Point HOME to a writable temp under target/
        let cwd = std::env::current_dir().unwrap();
        let home = cwd.join("target").join("test-home");
        let _ = fs::create_dir_all(&home);
        std::env::set_var("HOME", &home);
        let url = "https://21dzk.l.u-tokyo.ac.jp/SAT2018/satdb2018pre.php?mode=detail&useid=XYZ";
        let cache = super::cache_path_for(url);
        let _ = fs::create_dir_all(cache.parent().unwrap());
        fs::write(&cache, "Hello SAT Cache").unwrap();
        let t = super::sat_fetch_cli(url);
        assert_eq!(t, "Hello SAT Cache");
    }
}
