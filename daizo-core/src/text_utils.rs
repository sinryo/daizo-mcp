use crate::IndexEntry;
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use unicode_normalization::UnicodeNormalization;

/// Normalize string for general matching: NFKD + lower + CJK harmonization + alnum only
pub fn normalized(s: &str) -> String {
    // Hot path: avoid repeated `String::replace` passes (O(n * variants)).
    // We normalize by: NFKD -> lowercase -> map common CJK variants -> keep alnum only.
    let mut out = String::with_capacity(s.len());
    for ch in s.nfkd() {
        for lc in ch.to_lowercase() {
            // Map common simplified/variant forms to traditional used frequently in corpora.
            let mapped = match lc {
                '経' | '经' => '經',
                '観' | '观' => '觀',
                '仏' => '佛',
                '圣' => '聖',
                '会' => '會',
                '后' => '後',
                '国' => '國',
                '灵' => '靈',
                '广' => '廣',
                '龙' => '龍',
                '台' => '臺',
                '体' => '體',
                '訳' | '译' => '譯',
                '蔵' => '藏',
                '禅' => '禪',
                '浄' | '净' => '淨',
                '証' | '证' => '證',
                '覚' | '觉' => '覺',
                // Common Buddhist variants
                '弥' => '彌',
                '倶' => '俱',
                '舎' => '舍',
                _ => lc,
            };
            if mapped.is_alphanumeric() {
                out.push(mapped);
            }
        }
    }
    out
}

fn cjk_variant_group(ch: char) -> Option<&'static str> {
    // Minimal CJK variant groups useful for Buddhist corpora.
    // Use in regex character classes to match both forms.
    match ch {
        '経' | '經' | '经' => Some("經経经"),
        '観' | '觀' | '观' => Some("觀観观"),
        '仏' | '佛' => Some("佛仏"),
        '訳' | '譯' | '译' => Some("譯訳译"),
        '蔵' | '藏' => Some("藏蔵"),
        '禅' | '禪' => Some("禪禅"),
        '浄' | '淨' | '净' => Some("淨浄净"),
        '証' | '證' | '证' => Some("證証证"),
        '覚' | '覺' | '觉' => Some("覺覚觉"),
        '圣' | '聖' => Some("聖圣"),
        '会' | '會' => Some("會会"),
        '后' | '後' => Some("後后"),
        '国' | '國' => Some("國国"),
        '灵' | '靈' => Some("靈灵"),
        '广' | '廣' => Some("廣广"),
        '龙' | '龍' => Some("龍龙"),
        '台' | '臺' => Some("臺台"),
        '体' | '體' => Some("體体"),
        // Common Buddhist variants
        '弥' | '彌' => Some("彌弥"),
        '倶' | '俱' => Some("俱倶"),
        '舎' | '舍' => Some("舍舎"),
        _ => None,
    }
}

/// Collapse consecutive whitespace into `\\s*` and expand known CJK variants into
/// regex character classes. Intended for building safe regex patterns from a
/// literal user query (non-regex input).
pub fn ws_cjk_variant_fuzzy_regex_literal(s: &str) -> String {
    let mut out = String::new();
    let mut in_ws = false;
    for ch in s.chars() {
        if ch.is_whitespace() {
            if !in_ws {
                out.push_str("\\s*");
                in_ws = true;
            }
            continue;
        }
        in_ws = false;
        if let Some(vs) = cjk_variant_group(ch) {
            out.push('[');
            for vch in vs.chars() {
                if vch == '\\' || vch == ']' || vch == '-' {
                    out.push('\\');
                }
                out.push(vch);
            }
            out.push(']');
        } else {
            out.push_str(&regex::escape(&ch.to_string()));
        }
    }
    out
}

/// Normalize while preserving token boundaries (non-alnum -> space, then squash)
pub fn normalized_with_spaces(s: &str) -> String {
    // Hot path: avoid intermediate Vec allocations from split/join.
    let mut out = String::with_capacity(s.len());
    let mut in_ws = true; // suppress leading spaces
    for ch in s.nfkd() {
        for lc in ch.to_lowercase() {
            if lc.is_alphanumeric() {
                out.push(lc);
                in_ws = false;
            } else if !in_ws {
                out.push(' ');
                in_ws = true;
            }
        }
    }
    if out.ends_with(' ') {
        out.pop();
    }
    out
}

/// Pāli-friendly normalization: fold diacritics to ASCII-ish for fuzzy matching
pub fn normalized_pali(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.nfkd() {
        for lc in ch.to_lowercase() {
            let mapped = match lc {
                // Long vowels -> short vowels
                'ā' => 'a',
                'ī' => 'i',
                'ū' => 'u',
                // Nasals and other marks
                'ṅ' => 'n',
                'ñ' => 'n',
                'ṇ' => 'n',
                'ṃ' => 'm',
                // Dental/retroflex consonants
                'ṭ' => 't',
                'ḍ' => 'd',
                'ḷ' => 'l',
                // Other diacritical marks
                'ṛ' => 'r',
                'ḥ' => 'h',
                'ṁ' => 'm',
                _ => lc,
            };
            if mapped.is_alphanumeric() {
                out.push(mapped);
            }
        }
    }
    out
}

/// Sanskrit-friendly normalization: fold IAST diacritics to ASCII-ish
pub fn normalized_sanskrit(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.nfkd() {
        for lc in ch.to_lowercase() {
            let mapped = match lc {
                // Long vowels
                'ā' => 'a',
                'ī' => 'i',
                'ū' => 'u',
                'ȳ' => 'y',
                // Retroflex/dental/aspirates
                'ṭ' => 't',
                'ḍ' => 'd',
                'ṇ' => 'n',
                'ḷ' => 'l',
                'ḹ' => 'l',
                'ḻ' => 'l',
                // Sibilants and palatals
                'ś' => 's',
                'ṣ' => 's',
                'ç' => 'c',
                // Nasals and anusvāra/visarga
                'ṅ' => 'n',
                'ñ' => 'n',
                'ṃ' => 'm',
                'ṁ' => 'm',
                'ḥ' => 'h',
                // Vocalic r/ḷ
                'ṛ' => 'r',
                'ṝ' => 'r',
                _ => lc,
            };
            if mapped.is_alphanumeric() {
                out.push(mapped);
            }
        }
    }
    out
}

/// Compute match score with Sanskrit diacritic folding
pub fn compute_match_score_sanskrit(entry: &IndexEntry, q: &str) -> f32 {
    let nq = normalized(q);
    let meta_str = entry
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
    let alias = entry
        .meta
        .as_ref()
        .and_then(|m| m.get("alias").map(|s| s.as_str()))
        .unwrap_or("");
    let hay_all = format!("{} {} {}", entry.title, entry.id, meta_str);
    let hay = normalized(&hay_all);

    let mut score = if hay.contains(&nq) {
        1.0
    } else {
        let s_char = jaccard(&hay, &nq);
        let s_tok = token_jaccard(&hay_all, q);
        let hay_fold = normalized_sanskrit(&hay_all);
        let nq_fold = normalized_sanskrit(q);
        s_char.max(s_tok).max(jaccard(&hay_fold, &nq_fold))
    };
    if score < 0.95 {
        let subseq = is_subsequence(&hay, &nq)
            || is_subsequence(&nq, &hay)
            || is_subsequence(&normalized_sanskrit(&hay_all), &normalized_sanskrit(q));
        if subseq {
            score = score.max(0.85);
        }
    }
    let nalias = normalized_with_spaces(alias).replace(' ', "");
    let nalias_fold = normalized_sanskrit(alias);
    let nq_nospace = normalized_with_spaces(q).replace(' ', "");
    if !nalias.is_empty() {
        if nalias.split_whitespace().any(|a| a == nq_nospace)
            || nalias.contains(&nq_nospace)
            || (!nalias_fold.is_empty() && nalias_fold.contains(&normalized_sanskrit(q)))
        {
            score = score.max(0.95);
        }
    }
    if q.chars().any(|c| c.is_ascii_digit()) {
        let hws = normalized_with_spaces(&hay_all);
        if hws.contains(&normalized_with_spaces(q)) {
            score = (score + 0.05).min(1.0);
        }
    }
    score
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
    if sa.is_empty() || sb.is_empty() {
        return 0.0;
    }
    let inter = sa.intersection(&sb).count() as f32;
    let uni = (sa.len() + sb.len()).saturating_sub(inter as usize) as f32;
    if uni == 0.0 {
        0.0
    } else {
        inter / uni
    }
}

/// Token Jaccard similarity with a precomputed token set for `b`.
pub fn token_jaccard_with_tokenset(a: &str, b_tokens: &HashSet<String>) -> f32 {
    let sa: HashSet<_> = tokenset(a);
    if sa.is_empty() || b_tokens.is_empty() {
        return 0.0;
    }
    let inter = sa.intersection(b_tokens).count() as f32;
    let uni = (sa.len() + b_tokens.len()).saturating_sub(inter as usize) as f32;
    if uni == 0.0 {
        0.0
    } else {
        inter / uni
    }
}

#[derive(Clone, Debug)]
pub struct PrecomputedQuery {
    nq: String,
    nq_ws: String,
    nq_nospace: String,
    q_tokens: HashSet<String>,
    has_digit: bool,
    use_pali: bool,
    nq_pali: Option<String>,
}

impl PrecomputedQuery {
    pub fn new(q: &str, use_pali: bool) -> Self {
        let nq = normalized(q);
        let nq_ws = normalized_with_spaces(q);
        let nq_nospace = nq_ws.replace(' ', "");
        let q_tokens: HashSet<String> = nq_ws.split_whitespace().map(|w| w.to_string()).collect();
        let has_digit = q.chars().any(|c| c.is_ascii_digit());
        let nq_pali = if use_pali {
            Some(normalized_pali(q))
        } else {
            None
        };
        Self {
            nq,
            nq_ws,
            nq_nospace,
            q_tokens,
            has_digit,
            use_pali,
            nq_pali,
        }
    }

    pub fn normalized(&self) -> &str {
        self.nq.as_str()
    }
}

/// Character bigram Jaccard similarity
pub fn jaccard(a: &str, b: &str) -> f32 {
    let sa: HashSet<_> = a.as_bytes().windows(2).collect();
    let sb: HashSet<_> = b.as_bytes().windows(2).collect();
    let inter = sa.intersection(&sb).count() as f32;
    let uni = (sa.len() + sb.len()).saturating_sub(inter as usize) as f32;
    if uni == 0.0 {
        0.0
    } else {
        inter / uni
    }
}

/// Simple subsequence test (order-preserving containment)
pub fn is_subsequence(text: &str, pat: &str) -> bool {
    if pat.is_empty() {
        return true;
    }
    let mut it = pat.chars();
    let mut want = it.next();
    for ch in text.chars() {
        if let Some(w) = want {
            if ch == w {
                want = it.next();
                if want.is_none() {
                    return true;
                }
            }
        } else {
            return true;
        }
    }
    want.is_none()
}

/// Compute a fuzzy match score for an IndexEntry against a query.
/// When `use_pali` is true, an additional Pāli-normalized similarity is considered.
pub fn compute_match_score(entry: &IndexEntry, q: &str, use_pali: bool) -> f32 {
    let nq = normalized(q);
    let meta_str = entry
        .meta
        .as_ref()
        .map(|m| {
            let mut s = String::new();
            if use_pali {
                // Tipitaka の meta は本文級に肥大化しやすいので、検索に有効なキーだけを参照する。
                // NOTE: index側でのスリム化が前提だが、旧キャッシュを読んでも遅くならないよう防御。
                for k in [
                    "alias",
                    "alias_prefix",
                    "nikaya",
                    "book",
                    "title",
                    "subhead",
                    "subsubhead",
                    "chapter",
                    "headsPreview",
                ]
                .iter()
                {
                    if let Some(v) = m.get(*k) {
                        if !s.is_empty() {
                            s.push(' ');
                        }
                        s.push_str(v);
                    }
                }
            } else {
                for v in m.values() {
                    if !s.is_empty() {
                        s.push(' ');
                    }
                    s.push_str(v);
                }
            }
            s
        })
        .unwrap_or_default();
    let alias = entry
        .meta
        .as_ref()
        .and_then(|m| m.get("alias").map(|s| s.as_str()))
        .unwrap_or("");
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
        if subseq {
            score = score.max(0.85);
        }
    }

    // alias exact/contains boosts
    let nalias = normalized_with_spaces(alias).replace(' ', "");
    let nalias_pali = if use_pali {
        normalized_pali(alias)
    } else {
        String::new()
    };
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

/// `compute_match_score` variant that avoids per-entry query normalization/tokenization.
pub fn compute_match_score_precomputed(entry: &IndexEntry, q: &PrecomputedQuery) -> f32 {
    let nq = q.normalized();
    let meta_str = entry
        .meta
        .as_ref()
        .map(|m| {
            let mut s = String::new();
            if q.use_pali {
                for k in [
                    "alias",
                    "alias_prefix",
                    "nikaya",
                    "book",
                    "title",
                    "subhead",
                    "subsubhead",
                    "chapter",
                    "headsPreview",
                ]
                .iter()
                {
                    if let Some(v) = m.get(*k) {
                        if !s.is_empty() {
                            s.push(' ');
                        }
                        s.push_str(v);
                    }
                }
            } else {
                for v in m.values() {
                    if !s.is_empty() {
                        s.push(' ');
                    }
                    s.push_str(v);
                }
            }
            s
        })
        .unwrap_or_default();
    let alias = entry
        .meta
        .as_ref()
        .and_then(|m| m.get("alias").map(|s| s.as_str()))
        .unwrap_or("");
    let hay_all = format!("{} {} {}", entry.title, entry.id, meta_str);
    let hay = normalized(&hay_all);

    // base similarities
    let mut score = if hay.contains(nq) {
        1.0
    } else {
        let s_char = jaccard(&hay, nq);
        let s_tok = token_jaccard_with_tokenset(&hay_all, &q.q_tokens);
        if q.use_pali {
            let hay_pali = normalized_pali(&hay_all);
            let nq_pali = q.nq_pali.as_deref().unwrap_or("");
            s_char.max(s_tok).max(jaccard(&hay_pali, nq_pali))
        } else {
            s_char.max(s_tok)
        }
    };

    // subsequence boost
    if score < 0.95 {
        let subseq = is_subsequence(&hay, nq)
            || is_subsequence(nq, &hay)
            || if q.use_pali {
                let hp = normalized_pali(&hay_all);
                let np = q.nq_pali.as_deref().unwrap_or("");
                !np.is_empty() && is_subsequence(&hp, np)
            } else {
                false
            };
        if subseq {
            score = score.max(0.85);
        }
    }

    // alias exact/contains boosts
    let nalias = normalized_with_spaces(alias).replace(' ', "");
    let nalias_pali = if q.use_pali {
        normalized_pali(alias)
    } else {
        String::new()
    };
    if !nalias.is_empty() {
        if nalias.split_whitespace().any(|a| a == q.nq_nospace)
            || nalias.contains(&q.nq_nospace)
            || (q.use_pali
                && q.nq_pali
                    .as_deref()
                    .map(|p| !p.is_empty() && nalias_pali.contains(p))
                    .unwrap_or(false))
        {
            score = score.max(0.95);
        }
    }

    // numeric pattern boost (e.g., 12.2)
    if q.has_digit {
        let hws = normalized_with_spaces(&hay_all);
        if hws.contains(&q.nq_ws) {
            score = (score + 0.05).min(1.0);
        }
    }
    score
}

/// Cached-hay variant for non-Pāli corpora (CBETA/GRETIL title indexes).
///
/// `hay_norm` must be `normalized(hay_all)` and `hay_ws` must be `normalized_with_spaces(hay_all)`
/// where `hay_all = format!("{} {} {}", entry.title, entry.id, meta_str)` with the same meta_str
/// selection as `compute_match_score_precomputed` for `use_pali=false`.
pub fn compute_match_score_precomputed_with_hay(
    entry: &IndexEntry,
    hay_norm: &str,
    hay_ws: &str,
    q: &PrecomputedQuery,
) -> f32 {
    if q.use_pali {
        // Tipitaka needs Pāli-normalized similarity; fall back.
        return compute_match_score_precomputed(entry, q);
    }
    let nq = q.normalized();

    let mut score = if hay_norm.contains(nq) {
        1.0
    } else {
        let s_char = jaccard(hay_norm, nq);
        // Compute token-Jaccard directly from pre-normalized whitespace form without allocating Strings.
        let mut uniq: Vec<&str> = Vec::new();
        let mut inter = 0usize;
        for tok in hay_ws.split_whitespace() {
            if uniq.iter().any(|t| *t == tok) {
                continue;
            }
            if q.q_tokens.contains(tok) {
                inter += 1;
            }
            uniq.push(tok);
        }
        let sa_len = uniq.len();
        let sb_len = q.q_tokens.len();
        let s_tok = if sa_len == 0 || sb_len == 0 {
            0.0
        } else {
            let uni = (sa_len + sb_len).saturating_sub(inter);
            if uni == 0 {
                0.0
            } else {
                inter as f32 / uni as f32
            }
        };
        s_char.max(s_tok)
    };

    if score < 0.95 {
        let subseq = is_subsequence(hay_norm, nq) || is_subsequence(nq, hay_norm);
        if subseq {
            score = score.max(0.85);
        }
    }

    let alias = entry
        .meta
        .as_ref()
        .and_then(|m| m.get("alias").map(|s| s.as_str()))
        .unwrap_or("");
    let nalias = normalized_with_spaces(alias).replace(' ', "");
    if !nalias.is_empty() {
        if nalias.split_whitespace().any(|a| a == q.nq_nospace) || nalias.contains(&q.nq_nospace) {
            score = score.max(0.95);
        }
    }

    if q.has_digit && !q.nq_ws.is_empty() && hay_ws.contains(&q.nq_ws) {
        score = (score + 0.05).min(1.0);
    }
    score
}

/// ハイライト位置を返す（文字インデックス）。`is_regex=true` の場合は正規表現検索。
pub fn find_highlight_positions(text: &str, pattern: &str, is_regex: bool) -> Vec<HighlightPos> {
    let mut out: Vec<HighlightPos> = Vec::new();
    if pattern.is_empty() {
        return out;
    }
    if is_regex {
        if let Ok(re) = Regex::new(pattern) {
            for m in re.find_iter(text) {
                let sb = m.start();
                let eb = m.end();
                let sc = text[..sb].chars().count();
                let ec = sc + text[sb..eb].chars().count();
                out.push(HighlightPos {
                    start_char: sc,
                    end_char: ec,
                });
            }
        }
    } else {
        let mut i = 0usize;
        while let Some(pos) = text[i..].find(pattern) {
            let abs = i + pos;
            let sc = text[..abs].chars().count();
            let ec = sc + pattern.chars().count();
            out.push(HighlightPos {
                start_char: sc,
                end_char: ec,
            });
            i = abs + pattern.len();
        }
    }
    out
}

/// テキストへ装飾を適用し、ハイライト数と位置（文字インデックス）を返す
pub fn highlight_text(
    text: &str,
    pattern: &str,
    is_regex: bool,
    prefix: &str,
    suffix: &str,
) -> (String, usize, Vec<HighlightPos>) {
    if pattern.is_empty() {
        return (text.to_string(), 0, Vec::new());
    }
    let positions = find_highlight_positions(text, pattern, is_regex);
    if positions.is_empty() {
        return (text.to_string(), 0, positions);
    }
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
        assert_eq!(normalized("観"), normalized("觀"));
        assert_eq!(normalized("弥"), normalized("彌"));
        assert_eq!(normalized("仏"), normalized("佛"));
        assert_eq!(normalized("訳"), normalized("譯"));
        assert_eq!(normalized("蔵"), normalized("藏"));
        assert_eq!(normalized("禅"), normalized("禪"));
        assert_eq!(normalized("浄"), normalized("淨"));
        assert_eq!(normalized("証"), normalized("證"));
        assert_eq!(normalized("覚"), normalized("覺"));
    }

    #[test]
    fn ws_cjk_variant_fuzzy_regex_literal_matches() {
        let pat = ws_cjk_variant_fuzzy_regex_literal("須弥山");
        let re = regex::Regex::new(&pat).unwrap();
        assert!(re.is_match("須彌山"));
        assert!(re.is_match("須弥山"));
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
        let e = IndexEntry {
            id: "id1".into(),
            title: "Digha Nikaya".into(),
            path: "/tmp/x.xml".into(),
            meta: Some(meta),
        };
        let s = compute_match_score(&e, "DN1", true);
        assert!(s >= 0.95, "expected alias boost >= 0.95, got {}", s);
    }
}
