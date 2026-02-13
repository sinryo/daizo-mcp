use clap::{Parser, Subcommand};
use daizo_core::path_resolver::{
    cache_dir, cbeta_root, find_exact_file_by_name, find_tipitaka_content_for_base, gretil_root,
    resolve_cbeta_path_by_id, resolve_gretil_by_id, resolve_gretil_path_direct,
    resolve_tipitaka_by_id, tipitaka_root,
};
use daizo_core::text_utils::compute_match_score_sanskrit;
use daizo_core::text_utils::{compute_match_score, normalized};
use daizo_core::{
    build_cbeta_index, build_gretil_index, build_index, build_tipitaka_index, extract_text,
};
use serde::Serialize;
use std::env;
use std::fs;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::process::Command;
mod regex_utils;
//

/// バージョン情報を生成
fn long_version() -> &'static str {
    concat!(
        env!("DAIZO_VERSION"),
        "\nBuilt: ",
        env!("BUILD_DATE"),
        "\nCommit: ",
        env!("GIT_HASH")
    )
}

#[derive(Parser, Debug)]
#[command(
    name = "daizo-cli",
    about = "High-performance Buddhist text search and retrieval CLI",
    version = env!("DAIZO_VERSION"),
    long_version = long_version()
)]
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
    /// Search GRETIL and optionally auto-fetch contexts or full text (pipeline)
    GretilPipeline {
        /// Query string (regex)
        #[arg(long)]
        query: String,
        /// Maximum files to return
        #[arg(long, default_value_t = 10)]
        max_results: usize,
        /// Maximum matches per file (search)
        #[arg(long, default_value_t = 3)]
        max_matches_per_file: usize,
        /// Context lines before
        #[arg(long, default_value_t = 10)]
        context_before: usize,
        /// Context lines after
        #[arg(long, default_value_t = 100)]
        context_after: usize,
        /// Auto-fetch top files
        #[arg(long, default_value_t = false)]
        autofetch: bool,
        /// Number of files to auto-fetch (default 1 when autofetch)
        #[arg(long)]
        auto_fetch_files: Option<usize>,
        /// Per-file context count (override)
        #[arg(long)]
        auto_fetch_matches: Option<usize>,
        /// Include matched line in context
        #[arg(long, default_value_t = true)]
        include_match_line: bool,
        /// Include short highlight snippet
        #[arg(long, default_value_t = true)]
        include_highlight_snippet: bool,
        /// Minimum snippet length to include
        #[arg(long, default_value_t = 0)]
        min_snippet_len: usize,
        /// Highlight pattern for contexts
        #[arg(long)]
        highlight: Option<String>,
        /// Interpret highlight as regex
        #[arg(long, default_value_t = false)]
        highlight_regex: bool,
        /// Highlight prefix (fallback: $DAIZO_HL_PREFIX or ">>> ")
        #[arg(long)]
        highlight_prefix: Option<String>,
        /// Highlight suffix (fallback: $DAIZO_HL_SUFFIX or " <<<")
        #[arg(long)]
        highlight_suffix: Option<String>,
        /// Snippet prefix (fallback: $DAIZO_SNIPPET_PREFIX or ">>> ")
        #[arg(long)]
        snippet_prefix: Option<String>,
        /// Snippet suffix (fallback: $DAIZO_SNIPPET_SUFFIX or "")
        #[arg(long)]
        snippet_suffix: Option<String>,
        /// Fetch full text instead of contexts
        #[arg(long, default_value_t = false)]
        full: bool,
        /// Include <note> in full text
        #[arg(long, default_value_t = false)]
        include_notes: bool,
        /// Output JSON envelope
        #[arg(long, default_value_t = true)]
        json: bool,
    },
    /// Search GRETIL titles (index-based)
    GretilTitleSearch {
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
    /// Fetch GRETIL text by id or query
    GretilFetch {
        /// File stem id (e.g., Bhagavadgita)
        #[arg(long)]
        id: Option<String>,
        /// Alternative: search query to pick best match
        #[arg(long)]
        query: Option<String>,
        /// Include <note> text
        #[arg(long, default_value_t = false)]
        include_notes: bool,
        /// Return full text (no slicing)
        #[arg(long, default_value_t = false)]
        full: bool,
        /// Highlight string or regex (with --line-number)
        #[arg(long)]
        highlight: Option<String>,
        /// Interpret highlight as regex
        #[arg(long, default_value_t = false)]
        highlight_regex: bool,
        /// Highlight prefix (with --highlight)
        #[arg(long)]
        highlight_prefix: Option<String>,
        /// Highlight suffix (with --highlight)
        #[arg(long)]
        highlight_suffix: Option<String>,
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
    /// Search GRETIL corpus (content-based)
    GretilSearch {
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
    /// Search CBETA and optionally auto-fetch contexts or full text (pipeline)
    CbetaPipeline {
        /// Query string (regex)
        #[arg(long)]
        query: String,
        /// Maximum files to return
        #[arg(long, default_value_t = 10)]
        max_results: usize,
        /// Maximum matches per file (search)
        #[arg(long, default_value_t = 3)]
        max_matches_per_file: usize,
        /// Context lines before
        #[arg(long, default_value_t = 10)]
        context_before: usize,
        /// Context lines after
        #[arg(long, default_value_t = 100)]
        context_after: usize,
        /// Auto-fetch top files
        #[arg(long, default_value_t = false)]
        autofetch: bool,
        /// Number of files to auto-fetch (default 1 when autofetch)
        #[arg(long)]
        auto_fetch_files: Option<usize>,
        /// Per-file context count (override)
        #[arg(long)]
        auto_fetch_matches: Option<usize>,
        /// Include matched line in context
        #[arg(long, default_value_t = true)]
        include_match_line: bool,
        /// Include short highlight snippet
        #[arg(long, default_value_t = true)]
        include_highlight_snippet: bool,
        /// Minimum snippet length to include
        #[arg(long, default_value_t = 0)]
        min_snippet_len: usize,
        /// Highlight pattern for contexts
        #[arg(long)]
        highlight: Option<String>,
        /// Interpret highlight as regex
        #[arg(long, default_value_t = false)]
        highlight_regex: bool,
        /// Highlight prefix (fallback: $DAIZO_HL_PREFIX or ">>> ")
        #[arg(long)]
        highlight_prefix: Option<String>,
        /// Highlight suffix (fallback: $DAIZO_HL_SUFFIX or " <<<")
        #[arg(long)]
        highlight_suffix: Option<String>,
        /// Snippet prefix (fallback: $DAIZO_SNIPPET_PREFIX or ">>> ")
        #[arg(long)]
        snippet_prefix: Option<String>,
        /// Snippet suffix (fallback: $DAIZO_SNIPPET_SUFFIX or "")
        #[arg(long)]
        snippet_suffix: Option<String>,
        /// Fetch full text instead of contexts
        #[arg(long, default_value_t = false)]
        full: bool,
        /// Include <note> in full text
        #[arg(long, default_value_t = false)]
        include_notes: bool,
        /// Output JSON envelope
        #[arg(long, default_value_t = true)]
        json: bool,
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
        /// Return full text (no slicing)
        #[arg(long, default_value_t = false)]
        full: bool,
        /// Highlight string or regex (with --line-number)
        #[arg(long)]
        highlight: Option<String>,
        /// Interpret highlight as regex
        #[arg(long, default_value_t = false)]
        highlight_regex: bool,
        /// Highlight prefix (with --highlight)
        #[arg(long)]
        highlight_prefix: Option<String>,
        /// Highlight suffix (with --highlight)
        #[arg(long)]
        highlight_suffix: Option<String>,
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
        #[arg(long, default_value = "id,fascnm,startid,endid,body")]
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
        /// Highlight string or regex (with --line-number)
        #[arg(long)]
        highlight: Option<String>,
        /// Interpret highlight as regex
        #[arg(long, default_value_t = false)]
        highlight_regex: bool,
        /// Highlight prefix (with --highlight)
        #[arg(long)]
        highlight_prefix: Option<String>,
        /// Highlight suffix (with --highlight)
        #[arg(long)]
        highlight_suffix: Option<String>,
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
    if let Ok(p) = std::env::var("DAIZO_DIR") {
        return PathBuf::from(p);
    }
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".daizo")
}

fn ensure_dir(p: &PathBuf) -> anyhow::Result<()> {
    fs::create_dir_all(p)?;
    Ok(())
}

fn clone_tipitaka_sparse(target_dir: &Path) -> bool {
    eprintln!(
        "[clone] Cloning Tipitaka (romn only) to: {}",
        target_dir.display()
    );

    // Remove target directory if it exists but is empty
    if target_dir.exists() {
        let _ = fs::remove_dir_all(target_dir);
    }

    // Use git clone with sparse-checkout directly
    let temp_dir = target_dir.parent().unwrap_or(Path::new("."));
    let target_name = target_dir
        .file_name()
        .unwrap_or_else(|| std::ffi::OsStr::new("tipitaka-xml"));

    // Clone the repository with no checkout
    if !run(
        "git",
        &[
            "clone",
            "--no-checkout",
            "--depth",
            "1",
            "https://github.com/VipassanaTech/tipitaka-xml",
            target_name.to_string_lossy().as_ref(),
        ],
        Some(&temp_dir.to_path_buf()),
    ) {
        eprintln!("[error] Failed to clone repository");
        return false;
    }

    let target_str = target_dir.to_string_lossy();

    // Configure sparse checkout
    if !run(
        "git",
        &["-C", &target_str, "config", "core.sparseCheckout", "true"],
        None,
    ) {
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
    if let Some(d) = cwd {
        c.current_dir(d);
    }
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
    // Initialize optional repo policy from env (rate limits / future robots compliance)
    daizo_core::repo::init_policy_from_env();
    let cli = Cli::parse();
    match cli.command {
        Commands::Init { base } => {
            // Display startup message with colored output
            eprintln!("\x1b[33m📥 First-time setup requires downloading Buddhist texts. This may take several minutes... / 初回起動時はお経のダウンロードに時間がかかります。しばらくお待ちください... / 首次啟動需要下載佛經文本，可能需要幾分鐘時間...\x1b[0m");

            let base_dir = base.unwrap_or(default_daizo());
            ensure_dir(&base_dir)?;
            // ensure data via shared helpers
            let cbeta_dir = base_dir.join("xml-p5");
            if !daizo_core::repo::ensure_cbeta_data_at(&cbeta_dir) {
                anyhow::bail!("failed to ensure CBETA data");
            }
            let tipitaka_dir = base_dir.join("tipitaka-xml");
            if !daizo_core::repo::ensure_tipitaka_data_at(&tipitaka_dir) {
                anyhow::bail!("failed to ensure Tipitaka data");
            }
            // build indices
            eprintln!("[init] Building CBETA index...");
            let cbeta_entries = build_cbeta_index(&cbeta_dir);
            eprintln!("[init] Found {} CBETA entries", cbeta_entries.len());

            eprintln!("[init] Building Tipitaka index...");
            let tipitaka_entries = build_index(&tipitaka_dir.join("romn"), Some("romn"));
            eprintln!("[init] Found {} Tipitaka entries", tipitaka_entries.len());

            let cache_dir = base_dir.join("cache");
            fs::create_dir_all(&cache_dir)?;
            let cbeta_out = cache_dir.join("cbeta-index.json");
            let tipitaka_out = cache_dir.join("tipitaka-index.json");
            fs::write(&cbeta_out, serde_json::to_vec(&cbeta_entries)?)?;
            fs::write(&tipitaka_out, serde_json::to_vec(&tipitaka_entries)?)?;
            println!(
                "[init] cbeta-index: {} ({} entries)",
                cbeta_out.to_string_lossy(),
                cbeta_entries.len()
            );
            println!(
                "[init] tipitaka-index: {} ({} entries)",
                tipitaka_out.to_string_lossy(),
                tipitaka_entries.len()
            );
        }
        Commands::CbetaTitleSearch { query, limit, json } => {
            cmd_cbeta::cbeta_title_search(&query, limit, json)?;
        }
        Commands::CbetaFetch {
            id,
            query,
            part,
            include_notes,
            full,
            highlight,
            highlight_regex,
            highlight_prefix,
            highlight_suffix,
            headings_limit,
            start_char,
            end_char,
            max_chars,
            page,
            page_size,
            line_number,
            context_before,
            context_after,
            context_lines,
            json,
        } => {
            let tmp = Commands::CbetaFetch {
                id,
                query,
                part,
                include_notes,
                full,
                highlight,
                highlight_regex,
                highlight_prefix,
                highlight_suffix,
                headings_limit,
                start_char,
                end_char,
                max_chars,
                page,
                page_size,
                line_number,
                context_before,
                context_after,
                context_lines,
                json,
            };
            cmd_cbeta::cbeta_fetch(&tmp)?;
        }
        Commands::CbetaPipeline {
            query,
            max_results,
            max_matches_per_file,
            context_before,
            context_after,
            autofetch,
            auto_fetch_files,
            auto_fetch_matches,
            include_match_line,
            include_highlight_snippet,
            min_snippet_len,
            highlight,
            highlight_regex,
            highlight_prefix,
            highlight_suffix,
            snippet_prefix,
            snippet_suffix,
            full,
            include_notes,
            json,
        } => {
            let tmp = Commands::CbetaPipeline {
                query,
                max_results,
                max_matches_per_file,
                context_before,
                context_after,
                autofetch,
                auto_fetch_files,
                auto_fetch_matches,
                include_match_line,
                include_highlight_snippet,
                min_snippet_len,
                highlight,
                highlight_regex,
                highlight_prefix,
                highlight_suffix,
                snippet_prefix,
                snippet_suffix,
                full,
                include_notes,
                json,
            };
            cmd_cbeta::cbeta_pipeline(&tmp)?;
        }
        Commands::GretilTitleSearch { query, limit, json } => {
            cmd_gretil::gretil_title_search(&query, limit, json)?;
        }
        Commands::GretilFetch {
            id,
            query,
            include_notes,
            full,
            highlight,
            highlight_regex,
            highlight_prefix,
            highlight_suffix,
            headings_limit,
            start_char,
            end_char,
            max_chars,
            page,
            page_size,
            line_number,
            context_before,
            context_after,
            context_lines,
            json,
        } => {
            let tmp = Commands::GretilFetch {
                id,
                query,
                include_notes,
                full,
                highlight,
                highlight_regex,
                highlight_prefix,
                highlight_suffix,
                headings_limit,
                start_char,
                end_char,
                max_chars,
                page,
                page_size,
                line_number,
                context_before,
                context_after,
                context_lines,
                json,
            };
            cmd_gretil::gretil_fetch(&tmp)?;
        }
        Commands::GretilPipeline {
            query,
            max_results,
            max_matches_per_file,
            context_before,
            context_after,
            autofetch,
            auto_fetch_files,
            auto_fetch_matches,
            include_match_line,
            include_highlight_snippet,
            min_snippet_len,
            highlight,
            highlight_regex,
            highlight_prefix,
            highlight_suffix,
            snippet_prefix,
            snippet_suffix,
            full,
            include_notes,
            json,
        } => {
            let tmp = Commands::GretilPipeline {
                query,
                max_results,
                max_matches_per_file,
                context_before,
                context_after,
                autofetch,
                auto_fetch_files,
                auto_fetch_matches,
                include_match_line,
                include_highlight_snippet,
                min_snippet_len,
                highlight,
                highlight_regex,
                highlight_prefix,
                highlight_suffix,
                snippet_prefix,
                snippet_suffix,
                full,
                include_notes,
                json,
            };
            cmd_gretil::gretil_pipeline(&tmp)?;
        }
        Commands::GretilSearch {
            query,
            max_results,
            max_matches_per_file,
            json,
        } => {
            cmd_gretil::gretil_search(&query, max_results, max_matches_per_file, json)?;
        }

        Commands::SatSearch {
            query,
            rows,
            offs,
            exact,
            titles_only,
            fields,
            fq,
            autofetch,
            start_char,
            max_chars,
            json,
        } => {
            cmd::sat::sat_search(
                &query,
                rows,
                offs,
                exact,
                titles_only,
                &fields,
                &fq,
                autofetch,
                start_char,
                max_chars,
                json,
            )?;
            return Ok(());
        }
        Commands::SatFetch {
            url,
            useid,
            start_char,
            max_chars,
            json,
        } => {
            cmd::sat::sat_fetch(url.as_ref(), useid.as_ref(), start_char, max_chars, json)?;
            return Ok(());
        }
        Commands::SatDetail {
            useid,
            key: _,
            start_char,
            max_chars,
            json,
        } => {
            cmd::sat::sat_detail(&useid, start_char, max_chars, json)?;
            return Ok(());
        }
        Commands::SatPipeline {
            query,
            rows,
            offs,
            fields,
            fq,
            start_char,
            max_chars,
            json,
        } => {
            cmd::sat::sat_pipeline(
                &query, rows, offs, &fields, &fq, start_char, max_chars, json,
            )?;
        }
        Commands::CbetaIndex { root, out } => {
            let default_base = default_daizo().join("xml-p5");
            let base = root.unwrap_or(default_base.clone());

            // Ensure CBETA data exists
            if !default_base.exists() {
                eprintln!("[cbeta-index] CBETA data not found, downloading...");
                let ok = run(
                    "git",
                    &[
                        "clone",
                        "--depth",
                        "1",
                        "https://github.com/cbeta-org/xml-p5",
                        default_base.to_string_lossy().as_ref(),
                    ],
                    None,
                );
                if !ok {
                    anyhow::bail!("Failed to clone CBETA repository");
                }
            }

            let entries = build_cbeta_index(&base);
            let outp = out.unwrap_or(default_daizo().join("cache").join("cbeta-index.json"));
            if let Some(parent) = outp.parent() {
                fs::create_dir_all(parent)?;
            }
            fs::write(&outp, serde_json::to_vec(&entries)?)?;
            println!(
                "{}",
                serde_json::to_string(&IndexResult {
                    count: entries.len(),
                    out: outp.to_string_lossy().as_ref()
                })?
            );
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
            if let Some(parent) = outp.parent() {
                fs::create_dir_all(parent)?;
            }
            fs::write(&outp, serde_json::to_vec(&entries)?)?;
            println!(
                "{}",
                serde_json::to_string(&IndexResult {
                    count: entries.len(),
                    out: outp.to_string_lossy().as_ref()
                })?
            );
        }
        Commands::TipitakaTitleSearch { query, limit, json } => {
            cmd_tipitaka::tipitaka_title_search(&query, limit, json)?;
        }
        Commands::TipitakaFetch {
            id,
            query,
            head_index,
            head_query,
            headings_limit,
            highlight,
            highlight_regex,
            highlight_prefix,
            highlight_suffix,
            start_char,
            end_char,
            max_chars,
            page,
            page_size,
            line_number,
            context_before,
            context_after,
            context_lines,
            json,
        } => {
            let tmp = Commands::TipitakaFetch {
                id,
                query,
                head_index,
                head_query,
                headings_limit,
                highlight,
                highlight_regex,
                highlight_prefix,
                highlight_suffix,
                start_char,
                end_char,
                max_chars,
                page,
                page_size,
                line_number,
                context_before,
                context_after,
                context_lines,
                json,
            };
            cmd_tipitaka::tipitaka_fetch(&tmp)?;
            return Ok(());
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
                let ok = run(
                    cli_path.to_string_lossy().as_ref(),
                    &["tipitaka-index"],
                    None,
                );
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
            let xml = if let Some(p) = path {
                fs::read_to_string(p)?
            } else {
                let mut s = String::new();
                io::stdin().read_to_string(&mut s)?;
                s
            };
            let t = extract_text(&xml);
            println!("{}", t);
        }
        Commands::CbetaSearch {
            query,
            max_results,
            max_matches_per_file,
            json,
        } => {
            cmd_cbeta::cbeta_search(&query, max_results, max_matches_per_file, json)?;
        }
        Commands::TipitakaSearch {
            query,
            max_results,
            max_matches_per_file,
            json,
        } => {
            cmd_tipitaka::tipitaka_search(&query, max_results, max_matches_per_file, json)?;
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
                if !ok {
                    anyhow::bail!("update failed: {}", preview);
                }
                // Post-install: rebuild indexes using the installed binary
                let ok2 = run("daizo-cli", &["index-rebuild", "--source", "all"], None);
                if !ok2 {
                    eprintln!("[warn] index rebuild failed after update; run: daizo-cli index-rebuild --source all");
                }
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
            println!(
                " - daizo-cli: {}",
                if cli.exists() { "OK" } else { "MISSING" }
            );
            println!(
                " - daizo-mcp: {}",
                if mcp.exists() { "OK" } else { "MISSING" }
            );
            println!("data:");
            println!(
                " - xml-p5: {}",
                if cbeta.exists() {
                    "OK"
                } else {
                    "MISSING (will clone on demand)"
                }
            );
            println!(
                " - tipitaka-xml: {}",
                if tipi.exists() {
                    "OK"
                } else {
                    "MISSING (will clone on demand)"
                }
            );
            println!(
                "cache: {}",
                if cache.exists() {
                    cache.display().to_string()
                } else {
                    format!("{} (will create)", cache.display())
                }
            );
            if verbose {
                if cli.exists() {
                    println!(
                        "   size: {} bytes",
                        std::fs::metadata(&cli).map(|m| m.len()).unwrap_or(0)
                    );
                }
                if mcp.exists() {
                    println!(
                        "   size: {} bytes",
                        std::fs::metadata(&mcp).map(|m| m.len()).unwrap_or(0)
                    );
                }
            }
        }
        Commands::Uninstall { purge } => {
            let base = default_daizo();
            let bin = base.join("bin");
            let cli = bin.join("daizo-cli");
            let mcp = bin.join("daizo-mcp");
            let mut removed: Vec<String> = Vec::new();
            if cli.exists() {
                let _ = std::fs::remove_file(&cli);
                removed.push(cli.display().to_string());
            }
            if mcp.exists() {
                let _ = std::fs::remove_file(&mcp);
                removed.push(mcp.display().to_string());
            }
            if purge {
                let cbeta = base.join("xml-p5");
                let tipi = base.join("tipitaka-xml");
                let cache = base.join("cache");
                let _ = std::fs::remove_dir_all(&cbeta);
                let _ = std::fs::remove_dir_all(&tipi);
                let _ = std::fs::remove_dir_all(&cache);
                println!("[purge] removed data/cache under {}", base.display());
            }
            if removed.is_empty() {
                println!(
                    "no binaries removed (nothing found under {})",
                    bin.display()
                );
            } else {
                println!("removed: {}", removed.join(", "));
            }
        }
    }
    Ok(())
}

// ===== helpers (shared in daizo-core::text_utils) =====

#[derive(Clone, Debug, serde::Serialize)]
struct ScoredHit<'a> {
    #[serde(skip_serializing)]
    entry: &'a daizo_core::IndexEntry,
    score: f32,
}

pub(crate) fn best_match<'a>(
    entries: &'a [daizo_core::IndexEntry],
    q: &str,
    limit: usize,
) -> Vec<ScoredHit<'a>> {
    let nq = normalized(q);
    let mut scored: Vec<(f32, &daizo_core::IndexEntry)> = entries
        .iter()
        .map(|e| {
            let mut s = compute_match_score(e, q, false);
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
    scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap());
    scored
        .into_iter()
        .take(limit)
        .map(|(s, e)| ScoredHit { entry: e, score: s })
        .collect()
}

// paths & cache provided by daizo_core::path_resolver

pub(crate) fn load_or_build_tipitaka_index_cli() -> Vec<daizo_core::IndexEntry> {
    let out = cache_dir().join("tipitaka-index.json");
    if let Ok(b) = std::fs::read(&out) {
        if let Ok(mut v) = serde_json::from_slice::<Vec<daizo_core::IndexEntry>>(&b) {
            v.retain(|e| !e.path.ends_with(".toc.xml"));
            let missing = v
                .iter()
                .take(10)
                .filter(|e| !std::path::Path::new(&e.path).exists())
                .count();
            let lacks_meta = v.iter().take(10).any(|e| e.meta.is_none());
            let lacks_heads = v.iter().take(20).any(|e| {
                e.meta
                    .as_ref()
                    .map(|m| !m.contains_key("headsPreview"))
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
            if !v.is_empty() && missing == 0 && !lacks_meta && !lacks_heads && !lacks_composite {
                return v;
            }
        }
    }
    let mut entries = build_tipitaka_index(&tipitaka_root());
    entries.retain(|e| !e.path.ends_with(".toc.xml"));
    let _ = std::fs::create_dir_all(cache_dir());
    let _ = std::fs::write(&out, serde_json::to_vec(&entries).unwrap_or_default());
    entries
}

pub(crate) fn load_or_build_cbeta_index_cli() -> Vec<daizo_core::IndexEntry> {
    let out = cache_dir().join("cbeta-index.json");
    if let Ok(b) = std::fs::read(&out) {
        if let Ok(v) = serde_json::from_slice::<Vec<daizo_core::IndexEntry>>(&b) {
            let missing = v
                .iter()
                .take(10)
                .filter(|e| !std::path::Path::new(&e.path).exists())
                .count();
            if !v.is_empty() && missing == 0 {
                return v;
            }
        }
    }
    let entries = build_index(&cbeta_root(), None);
    let _ = std::fs::create_dir_all(cache_dir());
    let _ = std::fs::write(&out, serde_json::to_vec(&entries).unwrap_or_default());
    entries
}

pub(crate) fn load_or_build_gretil_index_cli() -> Vec<daizo_core::IndexEntry> {
    let out = cache_dir().join("gretil-index.json");
    if let Ok(b) = std::fs::read(&out) {
        if let Ok(v) = serde_json::from_slice::<Vec<daizo_core::IndexEntry>>(&b) {
            let missing = v
                .iter()
                .take(10)
                .filter(|e| !std::path::Path::new(&e.path).exists())
                .count();
            if !v.is_empty() && missing == 0 {
                return v;
            }
        }
    }
    let entries = build_gretil_index(&gretil_root());
    let _ = std::fs::create_dir_all(cache_dir());
    let _ = std::fs::write(&out, serde_json::to_vec(&entries).unwrap_or_default());
    entries
}

// directory scans are provided by daizo_core::path_resolver

pub(crate) fn resolve_tipitaka_path(id: Option<&str>, query: Option<&str>) -> PathBuf {
    // ID指定時は直接パス解決を最初に試みる（高速）
    if let Some(id) = id {
        // 直接パス解決（インデックス不要）
        if let Some(p) = daizo_core::path_resolver::resolve_tipitaka_path_direct(id) {
            return p;
        }
        // フォールバック: インデックスから検索
        let idx = load_or_build_tipitaka_index_cli();
        if let Some(p) = resolve_tipitaka_by_id(&idx, id) {
            return p;
        }
        // strict filename fallback
        if let Some(p) = find_exact_file_by_name(&tipitaka_root(), &format!("{}.xml", id)) {
            return p;
        }
    } else if let Some(q) = query {
        let idx = load_or_build_tipitaka_index_cli();
        if let Some(hit) = best_match(&idx, q, 1).into_iter().next() {
            return PathBuf::from(&hit.entry.path);
        }
    }
    PathBuf::new()
}

pub(crate) fn resolve_cbeta_path_cli(id: Option<&str>, query: Option<&str>) -> PathBuf {
    // 修正: IDがある場合は優先してqueryを無視
    if let Some(id) = id {
        if let Some(p) = resolve_cbeta_path_by_id(id) {
            return p;
        }
    } else if let Some(q) = query {
        let idx = load_or_build_cbeta_index_cli();
        if let Some(hit) = best_match(&idx, q, 1).into_iter().next() {
            return PathBuf::from(&hit.entry.path);
        }
    }
    PathBuf::new()
}

pub(crate) fn resolve_gretil_path_cli(id: Option<&str>, query: Option<&str>) -> PathBuf {
    if let Some(id_str) = id {
        // 直接パス解決を最初に試行（インデックスロード不要で最速）
        if let Some(p) = resolve_gretil_path_direct(id_str) {
            return p;
        }
        // フォールバック: インデックスベースの解決
        let idx = load_or_build_gretil_index_cli();
        if let Some(p) = resolve_gretil_by_id(&idx, id_str) {
            return p;
        }
        if let Some(p) = find_exact_file_by_name(&gretil_root(), &format!("{}.xml", id_str)) {
            return p;
        }
    } else if let Some(q) = query {
        let idx = load_or_build_gretil_index_cli();
        if let Some(hit) = best_match_gretil(&idx, q, 1).into_iter().next() {
            return PathBuf::from(&hit.entry.path);
        }
    }
    PathBuf::new()
}

fn best_match_gretil<'a>(
    entries: &'a [daizo_core::IndexEntry],
    q: &str,
    limit: usize,
) -> Vec<ScoredHit<'a>> {
    let nq = normalized(q);
    let mut scored: Vec<(f32, &daizo_core::IndexEntry)> = entries
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
    scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap());
    scored
        .into_iter()
        .take(limit)
        .map(|(s, e)| ScoredHit { entry: e, score: s })
        .collect()
}

pub(crate) fn extract_section_by_head(
    xml: &str,
    head_index: Option<usize>,
    head_query: Option<&str>,
) -> Option<String> {
    let re = regex::Regex::new(r"(?is)<head\\b[^>]*>(.*?)</head>").ok()?;
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
    let sect = &xml[start..end];
    Some(extract_text(sect))
}

pub(crate) struct SliceArgs {
    pub page: Option<usize>,
    pub page_size: Option<usize>,
    pub start_char: Option<usize>,
    pub end_char: Option<usize>,
    pub max_chars: Option<usize>,
}
impl SliceArgs {
    fn start(&self) -> Option<usize> {
        if let (Some(p), Some(ps)) = (self.page, self.page_size) {
            Some(p * ps)
        } else {
            self.start_char
        }
    }
    fn end_bound(&self, total: usize, sliced_len: usize) -> usize {
        if let (Some(p), Some(ps)) = (self.page, self.page_size) {
            std::cmp::min(p * ps + ps, total)
        } else if let Some(e) = self.end_char {
            std::cmp::min(e, total)
        } else if let Some(mc) = self.max_chars {
            std::cmp::min(self.start().unwrap_or(0) + mc, total)
        } else {
            self.start().unwrap_or(0) + sliced_len
        }
    }
}
pub(crate) fn slice_text_cli(text: &str, args: &SliceArgs) -> String {
    let default_max = 8000usize;
    // Treat indices as character positions, not bytes
    let total_chars = text.chars().count();
    let start_char = std::cmp::min(args.start().unwrap_or(0), total_chars);
    let end_char = if let (Some(p), Some(ps)) = (args.page, args.page_size) {
        Some(p * ps + ps)
    } else if let Some(e) = args.end_char {
        Some(e)
    } else if let Some(mc) = args.max_chars {
        Some(start_char + mc)
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

pub(crate) fn decode_xml_bytes(bytes: &[u8]) -> String {
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

pub(crate) fn sniff_xml_encoding(head: &[u8]) -> Option<String> {
    let lower: Vec<u8> = head.iter().map(|b| b.to_ascii_lowercase()).collect();
    if let Some(pos) = lower.windows(8).position(|w| w == b"encoding") {
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

//

//

//

//

//

//

//

//

//

//

//

//
mod cmd;
use cmd::{cbeta as cmd_cbeta, gretil as cmd_gretil, tipitaka as cmd_tipitaka};
