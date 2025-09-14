use crate::IndexEntry;
use regex::Regex;
use std::path::{Path, PathBuf};
use walkdir::WalkDir;

pub fn daizo_home() -> PathBuf {
    if let Ok(p) = std::env::var("DAIZO_DIR") {
        return PathBuf::from(p);
    }
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".daizo")
}

pub fn cbeta_root() -> PathBuf { daizo_home().join("xml-p5") }
pub fn tipitaka_root() -> PathBuf { daizo_home().join("tipitaka-xml").join("romn") }
pub fn cache_dir() -> PathBuf { daizo_home().join("cache") }

pub fn find_in_dir(root: &Path, stem_hint: &str) -> Option<PathBuf> {
    for e in WalkDir::new(root).into_iter().filter_map(|e| e.ok()) {
        if e.file_type().is_file() {
            if let Some(stem) = e.path().file_stem().and_then(|s| s.to_str()) {
                if stem.to_lowercase().contains(&stem_hint.to_lowercase())
                    && e.path().extension().and_then(|s| s.to_str()) == Some("xml")
                {
                    return Some(e.path().to_path_buf());
                }
            }
        }
    }
    None
}

pub fn find_exact_file_by_name(root: &Path, filename: &str) -> Option<PathBuf> {
    for e in WalkDir::new(root).into_iter().filter_map(|e| e.ok()) {
        if e.file_type().is_file() {
            if let Some(name) = e.path().file_name().and_then(|s| s.to_str()) {
                if name.eq_ignore_ascii_case(filename) {
                    return Some(e.path().to_path_buf());
                }
            }
        }
    }
    None
}

/// Resolve CBETA path by canonical id, trying canon-specific scan and fallback anywhere scan.
pub fn resolve_cbeta_path_by_id(id: &str) -> Option<PathBuf> {
    let m = Regex::new(r"^([A-Za-z]+)(\d+)$").ok()?;
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
            if name.contains(&id.to_lowercase()) && name.ends_with(".xml") {
                return Some(e.path().to_path_buf());
            }
        }
    }
    None
}

/// For Tipitaka, find the smallest numeric-sequence file that shares the same base.
pub fn find_tipitaka_content_for_base(base: &str) -> Option<PathBuf> {
    let root = tipitaka_root();
    let mut best: Option<(u32, PathBuf)> = None;
    for e in WalkDir::new(&root).into_iter().filter_map(|e| e.ok()) {
        if e.file_type().is_file() {
            let p = e.path();
            if p.extension().and_then(|s| s.to_str()) != Some("xml") {
                continue;
            }
            let name = p
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or("")
                .to_lowercase();
            if !name.contains(&base.to_lowercase()) {
                continue;
            }
            if name.contains("toc") || name.contains("sitemap") || name.contains("tree") {
                continue;
            }
            let num = name
                .trim_end_matches(".xml")
                .chars()
                .rev()
                .take_while(|c| c.is_ascii_digit())
                .collect::<String>();
            let n = num.chars().rev().collect::<String>();
            let rank = if n.is_empty() {
                u32::MAX
            } else {
                n.parse::<u32>().unwrap_or(u32::MAX)
            };
            if best.as_ref().map(|(bn, _)| rank < *bn).unwrap_or(true) {
                best = Some((rank, p.to_path_buf()));
            }
        }
    }
    best.map(|(_, p)| p)
}

/// Resolve a Tipitaka XML path by id (stem) using index and fallbacks.
pub fn resolve_tipitaka_by_id(index: &[IndexEntry], id: &str) -> Option<PathBuf> {
    // exact stem match
    for e in index.iter() {
        if Path::new(&e.path)
            .file_stem()
            .map(|s| s == id)
            .unwrap_or(false)
        {
            return Some(PathBuf::from(&e.path));
        }
    }
    // sequential variant (id + digits)
    let mut best_seq: Option<(u32, PathBuf)> = None;
    for e in index.iter() {
        if let Some(stem) = Path::new(&e.path).file_stem().and_then(|s| s.to_str()) {
            if let Some(rest) = stem.strip_prefix(id) {
                let digits = rest
                    .chars()
                    .take_while(|c| c.is_ascii_digit())
                    .collect::<String>();
                if !digits.is_empty() && rest.chars().all(|c| c.is_ascii_digit()) {
                    if let Ok(n) = digits.parse::<u32>() {
                        if best_seq.as_ref().map(|(bn, _)| n < *bn).unwrap_or(true) {
                            best_seq = Some((n, PathBuf::from(&e.path)));
                        }
                    }
                }
            }
        }
    }
    if let Some((_, p)) = best_seq { return Some(p); }
    // base-content fallback
    if let Some(p) = find_tipitaka_content_for_base(id) { return Some(p); }
    // directory scan fallback
    if let Some(p) = find_in_dir(&tipitaka_root(), id) { return Some(p); }
    // exact filename fallback
    find_exact_file_by_name(&tipitaka_root(), &format!("{}.xml", id))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::IndexEntry;
    use std::collections::BTreeMap;
    use std::fs;

    #[test]
    fn find_exact_file() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("fileA.xml");
        fs::write(&p, "<xml/>").unwrap();
        let f = find_exact_file_by_name(dir.path(), "fileA.xml");
        assert_eq!(f.unwrap().file_name().unwrap(), "fileA.xml");
    }

    #[test]
    fn resolve_tipitaka_by_id_prefers_exact_then_seq() {
        let dir = tempfile::tempdir().unwrap();
        let a0 = dir.path().join("base0.xml");
        let a1 = dir.path().join("base1.xml");
        fs::write(&a0, "<xml/>").unwrap();
        fs::write(&a1, "<xml/>").unwrap();
        let idx = vec![
            IndexEntry { id: "x".into(), title: "t".into(), path: a1.to_string_lossy().into_owned(), meta: Some(BTreeMap::new()) },
            IndexEntry { id: "x".into(), title: "t".into(), path: a0.to_string_lossy().into_owned(), meta: Some(BTreeMap::new()) },
        ];
        let p = resolve_tipitaka_by_id(&idx, "base").unwrap();
        assert_eq!(p.file_name().unwrap(), "base0.xml");
    }
}
