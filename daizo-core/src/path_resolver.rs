use crate::IndexEntry;
use glob::glob;
use ignore::WalkBuilder;
use regex::Regex;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

pub fn daizo_home() -> PathBuf {
    if let Ok(p) = std::env::var("DAIZO_DIR") {
        return PathBuf::from(p);
    }
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".daizo")
}

pub fn cbeta_root() -> PathBuf {
    daizo_home().join("xml-p5")
}
pub fn tipitaka_root() -> PathBuf {
    daizo_home().join("tipitaka-xml").join("romn")
}
pub fn gretil_root() -> PathBuf {
    daizo_home().join("GRETIL").join("1_sanskr").join("tei")
}
pub fn sarit_root() -> PathBuf {
    daizo_home().join("SARIT-corpus")
}
pub fn muktabodha_root() -> PathBuf {
    daizo_home().join("MUKTABODHA")
}
pub fn cache_dir() -> PathBuf {
    daizo_home().join("cache")
}

pub fn find_in_dir(root: &Path, stem_hint: &str) -> Option<PathBuf> {
    let hint = stem_hint.to_lowercase();
    let result: Mutex<Option<PathBuf>> = Mutex::new(None);

    WalkBuilder::new(root)
        .hidden(false)
        .git_ignore(false)
        .git_global(false)
        .git_exclude(false)
        .build_parallel()
        .run(|| {
            let hint = hint.clone();
            let result = &result;
            Box::new(move |entry| {
                if result.lock().unwrap().is_some() {
                    return ignore::WalkState::Quit;
                }
                if let Ok(e) = entry {
                    if e.file_type().is_some_and(|ft| ft.is_file()) {
                        if let Some(stem) = e.path().file_stem().and_then(|s| s.to_str()) {
                            if stem.to_lowercase().contains(&hint)
                                && e.path().extension().and_then(|s| s.to_str()) == Some("xml")
                            {
                                *result.lock().unwrap() = Some(e.path().to_path_buf());
                                return ignore::WalkState::Quit;
                            }
                        }
                    }
                }
                ignore::WalkState::Continue
            })
        });

    result.into_inner().unwrap()
}

pub fn find_exact_file_by_name(root: &Path, filename: &str) -> Option<PathBuf> {
    let target = filename.to_lowercase();
    let result: Mutex<Option<PathBuf>> = Mutex::new(None);

    WalkBuilder::new(root)
        .hidden(false)
        .git_ignore(false)
        .git_global(false)
        .git_exclude(false)
        .build_parallel()
        .run(|| {
            let target = target.clone();
            let result = &result;
            Box::new(move |entry| {
                if result.lock().unwrap().is_some() {
                    return ignore::WalkState::Quit;
                }
                if let Ok(e) = entry {
                    if e.file_type().is_some_and(|ft| ft.is_file()) {
                        if let Some(name) = e.path().file_name().and_then(|s| s.to_str()) {
                            if name.to_lowercase() == target {
                                *result.lock().unwrap() = Some(e.path().to_path_buf());
                                return ignore::WalkState::Quit;
                            }
                        }
                    }
                }
                ignore::WalkState::Continue
            })
        });

    result.into_inner().unwrap()
}

/// Fast direct path resolution for CBETA using glob patterns.
/// This avoids expensive directory traversal when the ID format is known.
pub fn resolve_cbeta_path_direct(id: &str) -> Option<PathBuf> {
    let re = Regex::new(r"^([A-Za-z]+)(\d+)$").ok()?;
    let root = cbeta_root();

    if let Some(c) = re.captures(id) {
        let canon = c[1].to_uppercase();
        let num = &c[2];

        // Pattern: xml-p5/{CANON}/{CANON}*/T*n{NUM}.xml
        // e.g., T0001 -> xml-p5/T/T*/T*n0001.xml
        let pattern = format!(
            "{}/{}/{}*/{}*n{}.xml",
            root.display(),
            canon,
            canon,
            canon,
            num
        );

        if let Ok(paths) = glob(&pattern) {
            for entry in paths.filter_map(|e| e.ok()) {
                return Some(entry);
            }
        }

        // Fallback: try with lowercase canon
        let pattern_lower = format!(
            "{}/{}/{}*/{}*n{}.xml",
            root.display(),
            canon.to_lowercase(),
            canon.to_lowercase(),
            canon.to_lowercase(),
            num
        );
        if let Ok(paths) = glob(&pattern_lower) {
            for entry in paths.filter_map(|e| e.ok()) {
                return Some(entry);
            }
        }
    }

    None
}

/// Resolve CBETA path by canonical id, trying fast glob first, then fallback scan.
pub fn resolve_cbeta_path_by_id(id: &str) -> Option<PathBuf> {
    // Try fast direct resolution first
    if let Some(path) = resolve_cbeta_path_direct(id) {
        return Some(path);
    }

    // Fallback: slower scan for non-standard IDs using ignore crate
    let m = Regex::new(r"^([A-Za-z]+)(\d+)$").ok()?;
    let root = cbeta_root();
    if let Some(c) = m.captures(id) {
        let canon = c[1].to_string();
        let num = c[2].to_string();
        let search_pattern = format!("n{}", num);
        let result: Mutex<Option<PathBuf>> = Mutex::new(None);

        WalkBuilder::new(root.join(&canon))
            .hidden(false)
            .git_ignore(false)
            .build_parallel()
            .run(|| {
                let search_pattern = search_pattern.clone();
                let result = &result;
                Box::new(move |entry| {
                    if result.lock().unwrap().is_some() {
                        return ignore::WalkState::Quit;
                    }
                    if let Ok(e) = entry {
                        if e.file_type().is_some_and(|ft| ft.is_file()) {
                            let name = e.file_name().to_string_lossy().to_lowercase();
                            if name.contains(&search_pattern) && name.ends_with(".xml") {
                                *result.lock().unwrap() = Some(e.path().to_path_buf());
                                return ignore::WalkState::Quit;
                            }
                        }
                    }
                    ignore::WalkState::Continue
                })
            });

        if let Some(path) = result.into_inner().unwrap() {
            return Some(path);
        }
    }

    // fallback: anywhere *id*.xml
    let id_lower = id.to_lowercase();
    let result: Mutex<Option<PathBuf>> = Mutex::new(None);

    WalkBuilder::new(&root)
        .hidden(false)
        .git_ignore(false)
        .build_parallel()
        .run(|| {
            let id_lower = id_lower.clone();
            let result = &result;
            Box::new(move |entry| {
                if result.lock().unwrap().is_some() {
                    return ignore::WalkState::Quit;
                }
                if let Ok(e) = entry {
                    if e.file_type().is_some_and(|ft| ft.is_file()) {
                        let name = e.file_name().to_string_lossy().to_lowercase();
                        if name.contains(&id_lower) && name.ends_with(".xml") {
                            *result.lock().unwrap() = Some(e.path().to_path_buf());
                            return ignore::WalkState::Quit;
                        }
                    }
                }
                ignore::WalkState::Continue
            })
        });

    result.into_inner().unwrap()
}

/// Fast direct path resolution for Tipitaka using Nikāya codes or file stems.
/// Supports: DN, MN, SN, AN, KN (e.g., "DN1", "MN1", "SN1.1") or direct file stems (e.g., "s0101m.mul")
pub fn resolve_tipitaka_path_direct(id: &str) -> Option<PathBuf> {
    let root = tipitaka_root();
    let id_lower = id.to_lowercase();
    let id_upper = id.to_uppercase();

    // Nikāya code mapping: DN->s01, MN->s02, SN->s03, AN->s04, KN->s05
    let nikaya_map: &[(&str, &str)] = &[
        ("DN", "s01"),
        ("MN", "s02"),
        ("SN", "s03"),
        ("AN", "s04"),
        ("KN", "s05"),
    ];

    // Try direct file stem match first (e.g., "s0101m.mul" -> "s0101m.mul.xml")
    let direct_path = root.join(format!("{}.xml", id));
    if direct_path.exists() {
        return Some(direct_path);
    }

    // Try with common suffixes (fastest path for known file stems)
    for suffix in &[".mul.xml", ".att.xml", ".tik.xml", ".nrf.xml"] {
        let path = root.join(format!("{}{}", id, suffix));
        if path.exists() {
            return Some(path);
        }
    }

    // Parse Nikāya code (e.g., "DN1", "MN 1", "SN1.1", "AN1", or just "DN")
    let re_nikaya = Regex::new(r"^(DN|MN|SN|AN|KN)\s*(\d+)?(?:\.(\d+))?$").ok()?;
    if let Some(caps) = re_nikaya.captures(&id_upper) {
        let nikaya = &caps[1];
        let num1 = caps.get(2).map(|m| m.as_str());
        let _num2 = caps.get(3).map(|m| m.as_str());

        // Find the file prefix for this Nikāya
        let file_prefix = nikaya_map
            .iter()
            .find(|(n, _)| *n == nikaya)
            .map(|(_, p)| *p)?;

        // FAST PATH: Try direct path construction first (no glob needed)
        // DN -> s0101m.mul.xml (first volume)
        // DN1 -> s0101m.mul.xml (sutta 1 is in volume 1)
        // Most suttas are in the first volume of each Nikāya
        let vol_num = if let Some(n) = num1 {
            // For simplicity, map sutta number to volume 01 (most common case)
            // DN1-DN13 are in volume 1 (Sīlakkhandhavagga)
            // More sophisticated mapping would require a lookup table
            let sutta_num: u32 = n.parse().unwrap_or(1);
            // Simple heuristic: suttas 1-13 -> vol 01, 14-23 -> vol 02, 24+ -> vol 03
            match nikaya {
                "DN" => {
                    if sutta_num <= 13 {
                        "01"
                    } else if sutta_num <= 23 {
                        "02"
                    } else {
                        "03"
                    }
                }
                "MN" => {
                    // MN has 3 volumes: Mūlapaṇṇāsa (1-50), Majjhimapaṇṇāsa (51-100), Uparipaṇṇāsa (101-152)
                    if sutta_num <= 50 {
                        "01"
                    } else if sutta_num <= 100 {
                        "02"
                    } else {
                        "03"
                    }
                }
                _ => "01", // Default to first volume for SN, AN, KN
            }
        } else {
            "01" // Default to first volume when no number specified
        };

        // Try direct path first
        let direct_vol_path = root.join(format!("{}{}m.mul.xml", file_prefix, vol_num));
        if direct_vol_path.exists() {
            return Some(direct_vol_path);
        }

        // Fallback: try glob pattern (slower but more comprehensive)
        let pattern = if num1.is_some() {
            format!("{}/{}{}*m.mul.xml", root.display(), file_prefix, vol_num)
        } else {
            format!("{}/{}*m.mul.xml", root.display(), file_prefix)
        };

        if let Ok(paths) = glob(&pattern) {
            let mut found: Vec<PathBuf> = paths.filter_map(|e| e.ok()).collect();
            found.sort();
            if let Some(first) = found.first() {
                return Some(first.clone());
            }
        }

        // Final fallback: any file with matching prefix
        let fallback_pattern = format!("{}/{}*.xml", root.display(), file_prefix);
        if let Ok(paths) = glob(&fallback_pattern) {
            let mut found: Vec<PathBuf> = paths.filter_map(|e| e.ok()).collect();
            found.sort();
            if let Some(first) = found.first() {
                return Some(first.clone());
            }
        }
    }

    // Try glob for partial file stem match (e.g., "s0101" -> "s0101m.mul.xml")
    let partial_pattern = format!("{}/{}*.xml", root.display(), id_lower);
    if let Ok(paths) = glob(&partial_pattern) {
        let mut found: Vec<PathBuf> = paths
            .filter_map(|e| e.ok())
            .filter(|p| {
                let name = p.file_name().and_then(|s| s.to_str()).unwrap_or("");
                !name.contains("toc") && !name.contains("sitemap") && !name.contains("tree")
            })
            .collect();
        // Prefer mūla (本文) files
        found.sort_by(|a, b| {
            let a_is_mul = a.to_string_lossy().contains(".mul.");
            let b_is_mul = b.to_string_lossy().contains(".mul.");
            match (a_is_mul, b_is_mul) {
                (true, false) => std::cmp::Ordering::Less,
                (false, true) => std::cmp::Ordering::Greater,
                _ => a.cmp(b),
            }
        });
        if let Some(first) = found.first() {
            return Some(first.clone());
        }
    }

    None
}

/// For Tipitaka, find the smallest numeric-sequence file that shares the same base.
pub fn find_tipitaka_content_for_base(base: &str) -> Option<PathBuf> {
    let root = tipitaka_root();
    let base_lower = base.to_lowercase();
    let best: Mutex<Option<(u32, PathBuf)>> = Mutex::new(None);

    WalkBuilder::new(&root)
        .hidden(false)
        .git_ignore(false)
        .git_global(false)
        .git_exclude(false)
        .build_parallel()
        .run(|| {
            let base_lower = base_lower.clone();
            let best = &best;
            Box::new(move |entry| {
                if let Ok(e) = entry {
                    if e.file_type().is_some_and(|ft| ft.is_file()) {
                        let p = e.path();
                        if p.extension().and_then(|s| s.to_str()) != Some("xml") {
                            return ignore::WalkState::Continue;
                        }
                        let name = p
                            .file_name()
                            .and_then(|s| s.to_str())
                            .unwrap_or("")
                            .to_lowercase();
                        if !name.contains(&base_lower) {
                            return ignore::WalkState::Continue;
                        }
                        if name.contains("toc") || name.contains("sitemap") || name.contains("tree")
                        {
                            return ignore::WalkState::Continue;
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
                        let mut guard = best.lock().unwrap();
                        if guard.as_ref().map(|(bn, _)| rank < *bn).unwrap_or(true) {
                            *guard = Some((rank, p.to_path_buf()));
                        }
                    }
                }
                ignore::WalkState::Continue
            })
        });

    best.into_inner().unwrap().map(|(_, p)| p)
}

/// Resolve a Tipitaka XML path by id (stem) using fast direct resolution first, then index fallbacks.
pub fn resolve_tipitaka_by_id(index: &[IndexEntry], id: &str) -> Option<PathBuf> {
    // Try fast direct resolution first (no index needed)
    if let Some(path) = resolve_tipitaka_path_direct(id) {
        return Some(path);
    }

    // Fallback: exact stem match in index
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
    if let Some((_, p)) = best_seq {
        return Some(p);
    }
    // base-content fallback
    if let Some(p) = find_tipitaka_content_for_base(id) {
        return Some(p);
    }
    // directory scan fallback
    if let Some(p) = find_in_dir(&tipitaka_root(), id) {
        return Some(p);
    }
    // exact filename fallback
    find_exact_file_by_name(&tipitaka_root(), &format!("{}.xml", id))
}

/// Fast direct path resolution for GRETIL using direct file access or glob patterns.
/// Supports: full file stem (e.g., "sa_saddharmapuNDarIka") or partial name (e.g., "saddharmapuNDarIka")
pub fn resolve_gretil_path_direct(id: &str) -> Option<PathBuf> {
    let root = gretil_root();

    // Try exact file stem match first (fastest path)
    let exact_path = root.join(format!("{}.xml", id));
    if exact_path.exists() {
        return Some(exact_path);
    }

    // Try with "sa_" prefix if not present
    if !id.starts_with("sa_") {
        let prefixed_path = root.join(format!("sa_{}.xml", id));
        if prefixed_path.exists() {
            return Some(prefixed_path);
        }
    }

    // Try glob pattern for partial matches (e.g., "saddharmapuNDarIka" -> "sa_saddharmapuNDarIka*.xml")
    let pattern = if id.starts_with("sa_") {
        format!("{}/{}*.xml", root.display(), id)
    } else {
        format!("{}/sa_{}*.xml", root.display(), id)
    };

    if let Ok(paths) = glob(&pattern) {
        let mut found: Vec<PathBuf> = paths
            .filter_map(|e| e.ok())
            .filter(|p| {
                // Filter out commentary and alternative versions if main text available
                let name = p.file_name().and_then(|s| s.to_str()).unwrap_or("");
                !name.contains("-comm") && !name.contains("-alt")
            })
            .collect();
        // Sort to get consistent results
        found.sort();
        if let Some(first) = found.first() {
            return Some(first.clone());
        }

        // If no non-commentary found, try again including all matches
        let pattern_all = if id.starts_with("sa_") {
            format!("{}/{}*.xml", root.display(), id)
        } else {
            format!("{}/sa_{}*.xml", root.display(), id)
        };
        if let Ok(paths) = glob(&pattern_all) {
            let mut found_all: Vec<PathBuf> = paths.filter_map(|e| e.ok()).collect();
            found_all.sort();
            if let Some(first) = found_all.first() {
                return Some(first.clone());
            }
        }
    }

    // Case-insensitive glob pattern fallback
    let pattern_ci = format!("{}/sa_*{}*.xml", root.display(), id.to_lowercase());
    if let Ok(paths) = glob(&pattern_ci) {
        let mut found: Vec<PathBuf> = paths
            .filter_map(|e| e.ok())
            .filter(|p| {
                let name = p
                    .file_name()
                    .and_then(|s| s.to_str())
                    .unwrap_or("")
                    .to_lowercase();
                name.contains(&id.to_lowercase())
            })
            .collect();
        found.sort();
        if let Some(first) = found.first() {
            return Some(first.clone());
        }
    }

    None
}

/// Fast direct path resolution for SARIT using direct file access.
/// Supports: file stem (e.g., "asvaghosa-buddhacarita") and tries both repository root
/// and "transliterated/" subdir.
pub fn resolve_sarit_path_direct(id: &str) -> Option<PathBuf> {
    // Avoid accidental path traversal; SARIT IDs should be file stems.
    if id.contains('/') || id.contains('\\') {
        return None;
    }
    let root = sarit_root();

    let fname = if id.ends_with(".xml") {
        id.to_string()
    } else {
        format!("{}.xml", id)
    };

    let p0 = root.join(&fname);
    if p0.exists() {
        return Some(p0);
    }
    let p1 = root.join("transliterated").join(&fname);
    if p1.exists() {
        return Some(p1);
    }
    None
}

/// Resolve a SARIT TEI path by id (file stem) using fast direct resolution first, then index fallbacks.
pub fn resolve_sarit_by_id(index: &[IndexEntry], id: &str) -> Option<PathBuf> {
    if let Some(path) = resolve_sarit_path_direct(id) {
        return Some(path);
    }
    // exact stem match via index first
    for e in index.iter() {
        if Path::new(&e.path)
            .file_stem()
            .map(|s| s == id)
            .unwrap_or(false)
        {
            return Some(PathBuf::from(&e.path));
        }
    }
    // directory scan fallback (can be expensive)
    if let Some(p) = find_in_dir(&sarit_root(), id) {
        return Some(p);
    }
    // exact filename fallback
    find_exact_file_by_name(&sarit_root(), &format!("{}.xml", id)).or_else(|| {
        find_exact_file_by_name(&sarit_root().join("transliterated"), &format!("{}.xml", id))
    })
}

/// Fast direct path resolution for MUKTABODHA.
/// Supports: file stem (id) and tries both .xml and .txt under DAIZO_DIR/MUKTABODHA.
pub fn resolve_muktabodha_path_direct(id: &str) -> Option<PathBuf> {
    if id.contains('/') || id.contains('\\') {
        return None;
    }
    let root = muktabodha_root();

    // If the user passed a filename, try it directly.
    let direct = root.join(id);
    if direct.exists() {
        return Some(direct);
    }

    // Otherwise try common extensions.
    for ext in ["xml", "txt"].iter() {
        let p = root.join(format!("{}.{}", id, ext));
        if p.exists() {
            return Some(p);
        }
    }
    None
}

/// Resolve a MUKTABODHA path by id (file stem) using fast direct resolution first, then index fallbacks.
pub fn resolve_muktabodha_by_id(index: &[IndexEntry], id: &str) -> Option<PathBuf> {
    if let Some(path) = resolve_muktabodha_path_direct(id) {
        return Some(path);
    }
    for e in index.iter() {
        if Path::new(&e.path)
            .file_stem()
            .map(|s| s == id)
            .unwrap_or(false)
        {
            return Some(PathBuf::from(&e.path));
        }
    }

    // Exact filename fallback.
    if let Some(p) = find_exact_file_by_name(&muktabodha_root(), &format!("{}.xml", id)) {
        return Some(p);
    }
    if let Some(p) = find_exact_file_by_name(&muktabodha_root(), &format!("{}.txt", id)) {
        return Some(p);
    }

    // Last resort: scan the directory for a matching stem across xml/txt.
    let id_lower = id.to_lowercase();
    let result: Mutex<Option<PathBuf>> = Mutex::new(None);
    WalkBuilder::new(&muktabodha_root())
        .hidden(false)
        .git_ignore(false)
        .git_global(false)
        .git_exclude(false)
        .build_parallel()
        .run(|| {
            let id_lower = id_lower.clone();
            let result = &result;
            Box::new(move |entry| {
                if result.lock().unwrap().is_some() {
                    return ignore::WalkState::Quit;
                }
                if let Ok(e) = entry {
                    if e.file_type().is_some_and(|ft| ft.is_file()) {
                        let p = e.path();
                        let ext = p.extension().and_then(|s| s.to_str()).unwrap_or("");
                        if ext != "xml" && ext != "txt" {
                            return ignore::WalkState::Continue;
                        }
                        if let Some(stem) = p.file_stem().and_then(|s| s.to_str()) {
                            if stem.to_lowercase() == id_lower {
                                *result.lock().unwrap() = Some(p.to_path_buf());
                                return ignore::WalkState::Quit;
                            }
                        }
                    }
                }
                ignore::WalkState::Continue
            })
        });
    result.into_inner().unwrap()
}

/// Resolve a GRETIL TEI path by id (file stem) using fast direct resolution first, then index fallbacks.
pub fn resolve_gretil_by_id(index: &[IndexEntry], id: &str) -> Option<PathBuf> {
    // Try fast direct resolution first (no index needed)
    if let Some(path) = resolve_gretil_path_direct(id) {
        return Some(path);
    }

    // exact stem match via index first
    for e in index.iter() {
        if Path::new(&e.path)
            .file_stem()
            .map(|s| s == id)
            .unwrap_or(false)
        {
            return Some(PathBuf::from(&e.path));
        }
    }
    // contains match via index
    for e in index.iter() {
        if Path::new(&e.path)
            .file_stem()
            .and_then(|s| s.to_str())
            .map(|s| s.to_lowercase().contains(&id.to_lowercase()))
            .unwrap_or(false)
        {
            return Some(PathBuf::from(&e.path));
        }
    }
    // direct directory scan
    if let Some(p) = find_in_dir(&gretil_root(), id) {
        return Some(p);
    }
    // strict filename fallback
    find_exact_file_by_name(&gretil_root(), &format!("{}.xml", id))
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
    fn resolve_cbeta_path_direct_returns_path_for_valid_id() {
        // This test only runs if CBETA data exists
        let root = cbeta_root();
        if !root.exists() {
            return; // Skip if data not available
        }
        // Test with known CBETA ID format
        let result = resolve_cbeta_path_direct("T0001");
        if let Some(path) = result {
            assert!(path.exists());
            assert!(path.to_string_lossy().contains("T01n0001.xml"));
        }
    }

    #[test]
    fn resolve_cbeta_path_direct_returns_none_for_invalid_id() {
        // Invalid ID format should return None quickly
        let result = resolve_cbeta_path_direct("invalid_id_format");
        assert!(result.is_none());
    }

    #[test]
    fn resolve_tipitaka_path_direct_with_nikaya_code() {
        // This test only runs if Tipitaka data exists
        let root = tipitaka_root();
        if !root.exists() {
            return; // Skip if data not available
        }
        // Test DN (Dīghanikāya)
        let result = resolve_tipitaka_path_direct("DN1");
        if let Some(path) = result {
            assert!(path.exists());
            assert!(path.to_string_lossy().contains("s01"));
            assert!(path.to_string_lossy().contains(".mul."));
        }
        // Test MN (Majjhimanikāya)
        let result_mn = resolve_tipitaka_path_direct("MN1");
        if let Some(path) = result_mn {
            assert!(path.exists());
            assert!(path.to_string_lossy().contains("s02"));
        }
    }

    #[test]
    fn resolve_tipitaka_path_direct_with_file_stem() {
        let root = tipitaka_root();
        if !root.exists() {
            return;
        }
        // Test direct file stem
        let result = resolve_tipitaka_path_direct("s0101m.mul");
        if let Some(path) = result {
            assert!(path.exists());
            assert!(path.to_string_lossy().ends_with("s0101m.mul.xml"));
        }
    }

    #[test]
    fn resolve_tipitaka_path_direct_returns_none_for_invalid_id() {
        let result = resolve_tipitaka_path_direct("INVALID123");
        assert!(result.is_none());
    }

    #[test]
    fn resolve_gretil_path_direct_with_file_stem() {
        let root = gretil_root();
        if !root.exists() {
            return;
        }
        // Test with full file stem
        let result = resolve_gretil_path_direct("sa_saddharmapuNDarIka");
        if let Some(path) = result {
            assert!(path.exists());
            assert!(path.to_string_lossy().contains("saddharmapuNDarIka"));
        }
        // Test without sa_ prefix
        let result_short = resolve_gretil_path_direct("saddharmapuNDarIka");
        if let Some(path) = result_short {
            assert!(path.exists());
            assert!(path.to_string_lossy().contains("saddharmapuNDarIka"));
        }
    }

    #[test]
    fn resolve_gretil_path_direct_returns_none_for_invalid_id() {
        let result = resolve_gretil_path_direct("nonexistent_text_12345");
        assert!(result.is_none());
    }

    #[test]
    fn resolve_tipitaka_by_id_prefers_exact_then_seq() {
        let dir = tempfile::tempdir().unwrap();
        let a0 = dir.path().join("base0.xml");
        let a1 = dir.path().join("base1.xml");
        fs::write(&a0, "<xml/>").unwrap();
        fs::write(&a1, "<xml/>").unwrap();
        let idx = vec![
            IndexEntry {
                id: "x".into(),
                title: "t".into(),
                path: a1.to_string_lossy().into_owned(),
                meta: Some(BTreeMap::new()),
            },
            IndexEntry {
                id: "x".into(),
                title: "t".into(),
                path: a0.to_string_lossy().into_owned(),
                meta: Some(BTreeMap::new()),
            },
        ];
        let p = resolve_tipitaka_by_id(&idx, "base").unwrap();
        assert_eq!(p.file_name().unwrap(), "base0.xml");
    }
}
