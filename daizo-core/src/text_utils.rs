use std::collections::HashSet;
use unicode_normalization::UnicodeNormalization;
use crate::IndexEntry;
use serde::{Deserialize, Serialize};
use regex::Regex;

/// Normalize string for general matching: NFKD + lower + CJK harmonization + alnum only
pub fn normalized(s: &str) -> String {
    let mut t: String = s.nfkd().collect::<String>().to_lowercase();
    // Map common simplified/variant forms to traditional used frequently in corpora
    let map: [(&str, &str); 12] = [
        ("経", "經"), ("经", "經"), ("观", "觀"), ("圣", "聖"), ("会", "會"), ("后", "後"),
        ("国", "國"), ("灵", "靈"), ("广", "廣"), ("龙", "龍"), ("台", "臺"), ("体", "體"),
    ];
    for (a, b) in map.iter() { t = t.replace(a, b); }
    t.chars().filter(|c| c.is_alphanumeric()).collect()
}

/// Normalize while preserving token boundaries (non-alnum -> space, then squash)
pub fn normalized_with_spaces(s: &str) -> String {
    let t: String = s.nfkd().collect::<String>().to_lowercase();
    t.chars()
        .map(|c| if c.is_alphanumeric() { c } else { ' ' })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// Pāli-friendly normalization: fold diacritics to ASCII-ish for fuzzy matching
pub fn normalized_pali(s: &str) -> String {
    let t: String = s.nfkd().collect::<String>().to_lowercase();
    let t = t
        .chars()
        .map(|c| match c {
            // Long vowels -> short vowels
            'ā' => 'a', 'ī' => 'i', 'ū' => 'u',
            // Nasals and other marks
            'ṅ' => 'n', 'ñ' => 'n', 'ṇ' => 'n', 'ṃ' => 'm',
            // Dental/retroflex consonants
            'ṭ' => 't', 'ḍ' => 'd', 'ḷ' => 'l',
            // Other diacritical marks
            'ṛ' => 'r', 'ḥ' => 'h', 'ṁ' => 'm',
            _ => c,
        })
        .collect::<String>();
    t.chars().filter(|c| c.is_alphanumeric()).collect()
}

pub fn tokenset(s: &str) -> HashSet<String> {
    normalized_with_spaces(s)
        .split_whitespace()
        .map(|w| w.to_string())
        .collect()
}

/// Token Jaccard similarity (set-based)
pub fn token_jaccard(a: &str, b: &str) -> f32 {
    let sa: HashSet<_> = tokenset(a);
    let sb: HashSet<_> = tokenset(b);
    if sa.is_empty() || sb.is_empty() { return 0.0; }
    let inter = sa.intersection(&sb).count() as f32;
    let uni = (sa.len() + sb.len()).saturating_sub(inter as usize) as f32;
    if uni == 0.0 { 0.0 } else { inter / uni }
}

/// Character bigram Jaccard similarity
pub fn jaccard(a: &str, b: &str) -> f32 {
    let sa: HashSet<_> = a.as_bytes().windows(2).collect();
    let sb: HashSet<_> = b.as_bytes().windows(2).collect();
    let inter = sa.intersection(&sb).count() as f32;
    let uni = (sa.len() + sb.len()).saturating_sub(inter as usize) as f32;
    if uni == 0.0 { 0.0 } else { inter / uni }
}

/// Simple subsequence test (order-preserving containment)
pub fn is_subsequence(text: &str, pat: &str) -> bool {
    let mut i = 0usize;
    for ch in text.chars() {
        if i < pat.len() && ch == pat.chars().nth(i).unwrap_or('\0') { i += 1; }
        if i >= pat.len() { return true; }
    }
    i >= pat.len()
}

/// Compute a fuzzy match score for an IndexEntry against a query.
/// When `use_pali` is true, an additional Pāli-normalized similarity is considered.
pub fn compute_match_score(entry: &IndexEntry, q: &str, use_pali: bool) -> f32 {
    let nq = normalized(q);
    let meta_str = entry
        .meta
        .as_ref()
        .map(|m| m.values().cloned().collect::<Vec<_>>().join(" "))
        .unwrap_or_default();
    let alias = entry
        .meta
        .as_ref()
        .and_then(|m| m.get("alias"))
        .cloned()
        .unwrap_or_default();
    let hay_all = format!("{} {} {}", entry.title, entry.id, meta_str);
    let hay = normalized(&hay_all);

    // base similarities
    let mut score = if hay.contains(&nq) {
        1.0
    } else {
        let s_char = jaccard(&hay, &nq);
        let s_tok = token_jaccard(&hay_all, q);
        if use_pali {
            let hay_pali = normalized_pali(&hay_all);
            let nq_pali = normalized_pali(q);
            s_char.max(s_tok).max(jaccard(&hay_pali, &nq_pali))
        } else {
            s_char.max(s_tok)
        }
    };

    // subsequence boost
    if score < 0.95 {
        let hay_pali_opt = use_pali.then(|| normalized_pali(&hay_all));
        let nq_pali_opt = use_pali.then(|| normalized_pali(q));
        let subseq = is_subsequence(&hay, &nq)
            || is_subsequence(&nq, &hay)
            || match (hay_pali_opt, nq_pali_opt) {
                (Some(hp), Some(np)) => is_subsequence(&hp, &np),
                _ => false,
            };
        if subseq { score = score.max(0.85); }
    }

    // alias exact/contains boosts
    let nalias = normalized_with_spaces(&alias).replace(' ', "");
    let nalias_pali = if use_pali { normalized_pali(&alias) } else { String::new() };
    let nq_nospace = normalized_with_spaces(q).replace(' ', "");
    if !nalias.is_empty() {
        if nalias.split_whitespace().any(|a| a == nq_nospace)
            || nalias.contains(&nq_nospace)
            || (use_pali && nalias_pali.contains(&normalized_pali(q)))
        {
            score = score.max(0.95);
        }
    }

    // numeric pattern boost (e.g., 12.2)
    if q.chars().any(|c| c.is_ascii_digit()) {
        let hws = normalized_with_spaces(&hay_all);
        if hws.contains(&normalized_with_spaces(q)) {
            score = (score + 0.05).min(1.0);
        }
    }
    score
}

/// ハイライト位置を返す（文字インデックス）。`is_regex=true` の場合は正規表現検索。
pub fn find_highlight_positions(text: &str, pattern: &str, is_regex: bool) -> Vec<HighlightPos> {
    let mut out: Vec<HighlightPos> = Vec::new();
    if pattern.is_empty() { return out; }
    if is_regex {
        if let Ok(re) = Regex::new(pattern) {
            for m in re.find_iter(text) {
                let sb = m.start();
                let eb = m.end();
                let sc = text[..sb].chars().count();
                let ec = sc + text[sb..eb].chars().count();
                out.push(HighlightPos { start_char: sc, end_char: ec });
            }
        }
    } else {
        let mut i = 0usize;
        while let Some(pos) = text[i..].find(pattern) {
            let abs = i + pos;
            let sc = text[..abs].chars().count();
            let ec = sc + pattern.chars().count();
            out.push(HighlightPos { start_char: sc, end_char: ec });
            i = abs + pattern.len();
        }
    }
    out
}

/// テキストへ装飾を適用し、ハイライト数と位置（文字インデックス）を返す
pub fn highlight_text(text: &str, pattern: &str, is_regex: bool, prefix: &str, suffix: &str) -> (String, usize, Vec<HighlightPos>) {
    if pattern.is_empty() { return (text.to_string(), 0, Vec::new()); }
    let positions = find_highlight_positions(text, pattern, is_regex);
    if positions.is_empty() { return (text.to_string(), 0, positions); }
    if is_regex {
        if let Ok(re) = Regex::new(pattern) {
            let mut count = 0usize;
            let replaced = re.replace_all(text, |caps: &regex::Captures| {
                count += 1;
                format!("{}{}{}", prefix, &caps[0], suffix)
            });
            return (replaced.into_owned(), count, positions);
        }
    }
    // literal replace
    let mut out = String::with_capacity(text.len());
    let mut j = 0usize;
    let mut count = 0usize;
    while let Some(pos) = text[j..].find(pattern) {
        let abs = j + pos;
        out.push_str(&text[j..abs]);
        out.push_str(prefix);
        out.push_str(pattern);
        out.push_str(suffix);
        j = abs + pattern.len();
        count += 1;
    }
    out.push_str(&text[j..]);
    (out, count, positions)
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct HighlightPos {
    pub start_char: usize,
    pub end_char: usize,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::IndexEntry;
    use std::collections::BTreeMap;

    #[test]
    fn normalized_cjk_variants() {
        let a = normalized("经"); // simplified
        let b = normalized("經"); // traditional
        assert_eq!(a, b);
    }

    #[test]
    fn token_jaccard_basic() {
        let s = token_jaccard("a b c", "a c d");
        assert!((s - 0.5).abs() < 1e-5);
    }

    #[test]
    fn compute_match_score_alias_boost() {
        let mut meta = BTreeMap::new();
        meta.insert("alias".to_string(), "DN 1".to_string());
        let e = IndexEntry { id: "id1".into(), title: "Digha Nikaya".into(), path: "/tmp/x.xml".into(), meta: Some(meta) };
        let s = compute_match_score(&e, "DN1", true);
        assert!(s >= 0.95, "expected alias boost >= 0.95, got {}", s);
    }
}
