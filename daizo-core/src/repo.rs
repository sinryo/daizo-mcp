use std::path::Path;
use std::process::Command;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

fn log(msg: &str) { eprintln!("[daizo-repo] {}", msg); }

#[derive(Clone, Debug)]
pub struct RepoPolicy {
    pub min_delay_ms: u64,
    pub robots_txt: bool,
    pub user_agent: Option<String>,
}

impl Default for RepoPolicy {
    fn default() -> Self {
        Self { min_delay_ms: 0, robots_txt: false, user_agent: None }
    }
}

static POLICY: OnceLock<RepoPolicy> = OnceLock::new();
static LAST_RUN: OnceLock<Mutex<Option<Instant>>> = OnceLock::new();

fn policy() -> RepoPolicy { POLICY.get().cloned().unwrap_or_default() }

pub fn set_repo_policy(p: RepoPolicy) { let _ = POLICY.set(p); }

pub fn init_policy_from_env() {
    if POLICY.get().is_some() { return; }
    let mut p = RepoPolicy::default();
    if let Ok(ms) = std::env::var("DAIZO_REPO_MIN_DELAY_MS") { if let Ok(v) = ms.parse::<u64>() { p.min_delay_ms = v; } }
    if let Ok(s) = std::env::var("DAIZO_REPO_USER_AGENT") { if !s.is_empty() { p.user_agent = Some(s); } }
    if let Ok(v) = std::env::var("DAIZO_REPO_RESPECT_ROBOTS") { p.robots_txt = matches!(v.as_str(), "1" | "true" | "yes"); }
    set_repo_policy(p);
}

fn maybe_throttle() {
    let p = policy();
    if p.min_delay_ms == 0 { return; }
    let last_lock = LAST_RUN.get_or_init(|| Mutex::new(None));
    let mut last = last_lock.lock().unwrap();
    if let Some(prev) = *last {
        let elapsed = prev.elapsed();
        let min = Duration::from_millis(p.min_delay_ms);
        if elapsed < min { std::thread::sleep(min - elapsed); }
    }
    *last = Some(Instant::now());
}

pub fn run(cmd: &str, args: &[&str], cwd: Option<&Path>) -> bool {
    maybe_throttle();
    log(&format!("{} {}", cmd, args.join(" ")));
    let mut c = Command::new(cmd);
    c.args(args);
    if let Some(d) = cwd { c.current_dir(d); }
    if cmd == "git" {
        c.stdout(std::process::Stdio::inherit());
        c.stderr(std::process::Stdio::inherit());
    }
    c.status().map(|s| s.success()).unwrap_or(false)
}

pub fn ensure_cbeta_data_at(root: &Path) -> bool {
    if root.exists() { return true; }
    if let Some(parent) = root.parent() { let _ = std::fs::create_dir_all(parent); }
    log(&format!("cloning CBETA xml-p5 -> {}", root.display()));
    run(
        "git",
        &["clone", "--depth", "1", "https://github.com/cbeta-org/xml-p5", &root.to_string_lossy()],
        None,
    )
}

pub fn clone_tipitaka_sparse(target_dir: &Path) -> bool {
    log(&format!("cloning Tipitaka (romn only) -> {}", target_dir.display()));
    if let Some(parent) = target_dir.parent() { let _ = std::fs::create_dir_all(parent); }
    // Clone the repository with no checkout
    let temp_dir = target_dir.parent().unwrap_or(Path::new("."));
    let target_name = target_dir
        .file_name()
        .unwrap_or_else(|| std::ffi::OsStr::new("tipitaka-xml"))
        .to_string_lossy()
        .to_string();
    if !run(
        "git",
        &[
            "clone",
            "--no-checkout",
            "--depth",
            "1",
            "https://github.com/VipassanaTech/tipitaka-xml",
            &target_name,
        ],
        Some(temp_dir),
    ) {
        return false;
    }
    let target_str = target_dir.to_string_lossy();
    if !run("git", &["-C", &target_str, "config", "core.sparseCheckout", "true"], None) {
        return false;
    }
    let sparse_file = target_dir.join(".git").join("info").join("sparse-checkout");
    if let Some(parent) = sparse_file.parent() { let _ = std::fs::create_dir_all(parent); }
    if std::fs::write(&sparse_file, "romn/\n").is_err() { return false; }
    if !run("git", &["-C", &target_str, "checkout"], None) { return false; }
    true
}

pub fn ensure_tipitaka_data_at(target_dir: &Path) -> bool {
    if target_dir.join("romn").exists() { return true; }
    clone_tipitaka_sparse(target_dir)
}

pub fn ensure_dir(p: &Path) { let _ = std::fs::create_dir_all(p); }
