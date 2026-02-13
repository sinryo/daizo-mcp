use crate::{slice_text_cli, SliceArgs};
use daizo_core::path_resolver::cache_dir;
use daizo_core::text_utils::{
    find_highlight_positions, is_subsequence, jaccard, normalized, token_jaccard,
    ws_cjk_variant_fuzzy_regex_literal,
};
use std::path::PathBuf;

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
pub(crate) struct SatHit {
    pub title: String,
    pub url: String,
    pub snippet: String,
}

pub(crate) fn http_client() -> &'static reqwest::blocking::Client {
    use std::sync::OnceLock;
    static CLI: OnceLock<reqwest::blocking::Client> = OnceLock::new();
    CLI.get_or_init(|| {
        reqwest::blocking::Client::builder()
            .timeout(std::time::Duration::from_secs(15))
            .build()
            .unwrap()
    })
}

pub(crate) fn cache_path_for(url: &str) -> PathBuf {
    let mut hasher = sha1::Sha1::new();
    use sha1::Digest;
    hasher.update(url.as_bytes());
    let h = hasher.finalize();
    let fname = format!("{:x}.txt", h);
    let dir = cache_dir().join("sat");
    let _ = std::fs::create_dir_all(&dir);
    dir.join(fname)
}

pub(crate) fn sat_fetch_cli(url: &str) -> String {
    let cache = cache_path_for(url);
    if let Ok(t) = std::fs::read_to_string(&cache) {
        return t;
    }
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
        std::thread::sleep(std::time::Duration::from_millis(backoff));
        backoff = (backoff * 2).min(8000);
    }
    String::new()
}

pub(crate) fn sat_extract_text(html: &str) -> String {
    use scraper::{Html, Selector};
    let dom = Html::parse_document(html);
    let sel = Selector::parse("body").unwrap();
    let mut out = String::new();
    for n in dom.select(&sel) {
        out.push_str(&n.text().collect::<Vec<_>>().join(" "));
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

pub(crate) fn sat_wrap7_build_url(
    q: &str,
    rows: usize,
    offs: usize,
    fields: &str,
    fq: &Vec<String>,
) -> String {
    let base = "https://21dzk.l.u-tokyo.ac.jp/SAT2018/wrap7.php";
    let mut url = format!(
        "{}?regex=off&q={}&rows={}&offs={}&schop=AND",
        base,
        urlencoding::encode(q),
        rows,
        offs
    );
    if !fields.trim().is_empty() {
        url.push_str(&format!("&fl={}", urlencoding::encode(fields)));
    }
    for f in fq {
        if !f.trim().is_empty() {
            url.push_str(&format!("&fq={}", urlencoding::encode(f)));
        }
    }
    url
}

pub(crate) fn sat_wrap7_search_json(
    q: &str,
    rows: usize,
    offs: usize,
    fields: &str,
    fq: &Vec<String>,
) -> Option<serde_json::Value> {
    let url = sat_wrap7_build_url(q, rows, offs, fields, fq);
    for _ in 0..2 {
        if let Ok(r) = http_client().get(&url).send() {
            if r.status().is_success() {
                if let Ok(txt) = r.text() {
                    if let Ok(json) = serde_json::from_str::<serde_json::Value>(&txt) {
                        return Some(json);
                    }
                }
            }
        }
    }
    None
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

pub(crate) fn sat_detail_build_url(useid: &str) -> String {
    format!(
        "https://21dzk.l.u-tokyo.ac.jp/SAT2018/satdb2018pre.php?mode=detail&ob=1&mode2=2&useid={}",
        urlencoding::encode(useid)
    )
}

pub(crate) fn title_score(title: &str, query: &str) -> f32 {
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

fn sat_candidate_order(docs: &[serde_json::Value], query: &str) -> Vec<(usize, &'static str, f32)> {
    let nq = normalized(query);
    let mut any: Vec<(usize, f32)> = Vec::new();
    let mut body: Vec<(usize, f32)> = Vec::new();
    let mut title_contains: Vec<(usize, f32)> = Vec::new();
    for (i, d) in docs.iter().enumerate() {
        let title = d.get("fascnm").and_then(|v| v.as_str()).unwrap_or("");
        let sc = title_score(title, query);
        any.push((i, sc));
        if nq.is_empty() {
            continue;
        }
        if normalized(title).contains(&nq) {
            title_contains.push((i, sc));
        }
        let b = d.get("body").and_then(|v| v.as_str()).unwrap_or("");
        if !b.is_empty() && normalized(b).contains(&nq) {
            body.push((i, sc));
        }
    }
    let sort_sc = |a: &(usize, f32), b: &(usize, f32)| {
        b.1.partial_cmp(&a.1)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.0.cmp(&b.0))
    };
    any.sort_by(sort_sc);

    let mut out: Vec<(usize, &'static str, f32)> = Vec::new();
    let mut seen: std::collections::HashSet<usize> = std::collections::HashSet::new();
    for (i, sc) in body {
        if seen.insert(i) {
            out.push((i, "bodyContains", sc));
        }
    }
    for (i, sc) in title_contains {
        if seen.insert(i) {
            out.push((i, "titleContains", sc));
        }
    }
    for (i, sc) in any {
        if seen.insert(i) {
            out.push((i, "titleScore", sc));
        }
    }
    out
}

pub fn sat_search(
    query: &str,
    rows: usize,
    offs: usize,
    exact: bool,
    titles_only: bool,
    fields: &str,
    fq: &Vec<String>,
    autofetch: bool,
    start_char: Option<usize>,
    max_chars: Option<usize>,
    json: bool,
) -> anyhow::Result<()> {
    let wrap = sat_wrap7_search_json(query, rows, offs, fields, fq);
    if autofetch {
        if let Some(w) = wrap.clone() {
            let docs = w
                .get("response")
                .and_then(|r| r.get("docs"))
                .and_then(|v| v.as_array())
                .cloned()
                .unwrap_or_default();
            if !docs.is_empty() {
                let mut best_idx = 0usize;
                let mut best_sc = -1.0f32;
                for (i, d) in docs.iter().enumerate() {
                    let title = d.get("fascnm").and_then(|v| v.as_str()).unwrap_or("");
                    let sc = title_score(title, query);
                    if sc > best_sc {
                        best_sc = sc;
                        best_idx = i;
                    }
                }
                let chosen = &docs[best_idx];
                let useid = chosen.get("startid").and_then(|v| v.as_str()).unwrap_or("");
                let url = sat_detail_build_url(useid);
                let t = sat_fetch_cli(&url);
                let start = start_char.unwrap_or(0);
                let args = SliceArgs {
                    page: None,
                    page_size: None,
                    start_char: Some(start),
                    end_char: None,
                    max_chars,
                };
                let sliced = slice_text_cli(&t, &args);
                if json {
                    let count = w
                        .get("response")
                        .and_then(|r| r.get("numFound"))
                        .and_then(|v| v.as_u64())
                        .unwrap_or(0);
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
                    eprintln!(
                        "[meta] url={} chosen_title={} score={}",
                        url,
                        chosen.get("fascnm").and_then(|v| v.as_str()).unwrap_or(""),
                        best_sc
                    );
                }
                return Ok(());
            }
        }
        let text = "no results".to_string();
        if json {
            println!(
                "{}",
                serde_json::to_string_pretty(&serde_json::json!({
            "jsonrpc":"2.0","id":null,
            "result": {"content":[{"type":"text","text": text}],"_meta": {"count":0}} }))?
            );
        } else {
            println!("{}", text);
        }
        return Ok(());
    }
    let wrap = wrap.unwrap_or_else(|| {
        let hits = sat_search_results_cli(query, rows, offs, exact, titles_only);
        serde_json::json!({"response": {"numFound": hits.len(), "start": offs, "docs": hits }})
    });
    let docs = wrap
        .get("response")
        .and_then(|r| r.get("docs"))
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    if docs.is_empty() {
        let text = "no results".to_string();
        if json {
            println!(
                "{}",
                serde_json::to_string_pretty(&serde_json::json!({
            "jsonrpc":"2.0","id":null,
            "result": {"content":[{"type":"text","text": text}],"_meta": {"count":0}} }))?
            );
        } else {
            println!("{}", text);
        }
        return Ok(());
    }
    let mut best_idx = 0usize;
    let mut best_sc = -1.0f32;
    for (i, d) in docs.iter().enumerate() {
        let title = d.get("fascnm").and_then(|v| v.as_str()).unwrap_or("");
        let sc = title_score(title, query);
        if sc > best_sc {
            best_sc = sc;
            best_idx = i;
        }
    }
    let chosen = &docs[best_idx];
    let useid = chosen.get("startid").and_then(|v| v.as_str()).unwrap_or("");
    let url = sat_detail_build_url(useid);
    let t = sat_fetch_cli(&url);
    let start = start_char.unwrap_or(0);
    let args = SliceArgs {
        page: None,
        page_size: None,
        start_char: Some(start),
        end_char: None,
        max_chars,
    };
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
        eprintln!(
            "[meta] url={} total={} start={} returned={} chosen_title={} score={}",
            url,
            t.len(),
            start,
            sliced.len(),
            chosen.get("fascnm").and_then(|v| v.as_str()).unwrap_or(""),
            best_sc
        );
    }
    Ok(())
}

pub fn sat_fetch(
    url: Option<&String>,
    useid: Option<&String>,
    start_char: Option<usize>,
    max_chars: Option<usize>,
    json: bool,
) -> anyhow::Result<()> {
    let url_final = if let Some(uid) = useid {
        sat_detail_build_url(uid)
    } else {
        url.cloned().unwrap_or_default()
    };
    let t = sat_fetch_cli(&url_final);
    let start = start_char.unwrap_or(0);
    let args = SliceArgs {
        page: None,
        page_size: None,
        start_char: Some(start),
        end_char: None,
        max_chars,
    };
    let sliced = slice_text_cli(&t, &args);
    if json {
        let meta = serde_json::json!({
            "totalLength": t.len(),
            "returnedStart": start,
            "returnedEnd": args.end_bound(t.len(), sliced.len()),
            "truncated": (sliced.len() as u64) < (t.len() as u64),
            "sourceUrl": url_final,
            "extractionMethod": "sat-detail-extract"
        });
        let envelope = serde_json::json!({
            "jsonrpc":"2.0","id": serde_json::Value::Null,
            "result": { "content": [{"type":"text","text": sliced}], "_meta": meta }
        });
        println!("{}", serde_json::to_string_pretty(&envelope)?);
    } else {
        println!("{}", sliced);
    }
    Ok(())
}

pub fn sat_detail(
    useid: &str,
    start_char: Option<usize>,
    max_chars: Option<usize>,
    _json: bool,
) -> anyhow::Result<()> {
    let url = sat_detail_build_url(useid);
    let t = sat_fetch_cli(&url);
    let start = start_char.unwrap_or(0);
    let args = SliceArgs {
        page: None,
        page_size: None,
        start_char: Some(start),
        end_char: None,
        max_chars,
    };
    let sliced = slice_text_cli(&t, &args);
    let meta = serde_json::json!({
        "totalLength": t.len(),
        "returnedStart": start,
        "returnedEnd": args.end_bound(t.len(), sliced.len()),
        "truncated": (sliced.len() as u64) < (t.len() as u64),
        "sourceUrl": url,
        "extractionMethod": "sat-detail-extract"
    });
    let envelope = serde_json::json!({
        "jsonrpc":"2.0","id": serde_json::Value::Null,
        "result": { "content": [{"type":"text","text": sliced}], "_meta": meta }
    });
    println!("{}", serde_json::to_string_pretty(&envelope)?);
    Ok(())
}

pub fn sat_pipeline(
    query: &str,
    rows: usize,
    offs: usize,
    fields: &str,
    fq: &Vec<String>,
    start_char: Option<usize>,
    max_chars: Option<usize>,
    json: bool,
) -> anyhow::Result<()> {
    let fields_used =
        sat_wrap7_ensure_fields(fields, &["id", "fascnm", "startid", "endid", "body"]);
    let wrap = sat_wrap7_search_json(query, rows, offs, &fields_used, fq);
    if let Some(jsonv) = wrap.clone() {
        let docs = jsonv
            .get("response")
            .and_then(|r| r.get("docs"))
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        if !docs.is_empty() {
            let nq = normalized(query);
            let candidates = sat_candidate_order(&docs, query);
            let mut chosen_i = candidates.first().map(|(i, _, _)| *i).unwrap_or(0);
            let mut chosen_by = candidates
                .first()
                .map(|(_, by, _)| *by)
                .unwrap_or("titleScore");
            let mut chosen_sc = candidates.first().map(|(_, _, sc)| *sc).unwrap_or(-1.0);
            let mut t = String::new();
            let mut url = String::new();

            // Validate against fetched detail when possible; avoid obvious false positives.
            for (i, by, sc) in candidates.into_iter().take(5) {
                let cand = &docs[i];
                let useid = cand.get("startid").and_then(|v| v.as_str()).unwrap_or("");
                if useid.is_empty() {
                    continue;
                }
                let cand_url = sat_detail_build_url(useid);
                let cand_t = sat_fetch_cli(&cand_url);
                if nq.is_empty() || normalized(&cand_t).contains(&nq) {
                    chosen_i = i;
                    chosen_by = by;
                    chosen_sc = sc;
                    url = cand_url;
                    t = cand_t;
                    break;
                }
                // Keep first candidate as fallback
                if t.is_empty() {
                    url = cand_url;
                    t = cand_t;
                }
            }

            let chosen = &docs[chosen_i];
            let start_requested = start_char.unwrap_or(0);
            let mut start = start_requested;
            let q_focus = query
                .trim()
                .strip_prefix('"')
                .and_then(|s| s.strip_suffix('"'))
                .unwrap_or(query)
                .trim();
            let mut focus = serde_json::json!({"enabled": false});
            if start_char.is_none() && !q_focus.is_empty() {
                let pat = ws_cjk_variant_fuzzy_regex_literal(q_focus);
                if let Some(p0) = find_highlight_positions(&t, &pat, true).into_iter().next() {
                    start = p0.start_char.saturating_sub(50);
                    focus = serde_json::json!({
                        "enabled": true,
                        "query": q_focus,
                        "pattern": pat,
                        "matchStartChar": p0.start_char,
                        "matchEndChar": p0.end_char,
                        "startChar": start
                    });
                }
            }
            let args = SliceArgs {
                page: None,
                page_size: None,
                start_char: Some(start),
                end_char: None,
                max_chars,
            };
            let sliced = slice_text_cli(&t, &args);
            if json {
                let count = jsonv
                    .get("response")
                    .and_then(|r| r.get("numFound"))
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0);
                let meta = serde_json::json!({
                    "totalLength": t.len(),
                    "returnedStart": start,
                    "returnedEnd": args.end_bound(t.len(), sliced.len()),
                    "truncated": (sliced.len() as u64) < (t.len() as u64),
                    "sourceUrl": url,
                    "extractionMethod": "sat-detail-extract",
                    "search": {"rows": rows, "offs": offs, "flRequested": fields, "flUsed": fields_used, "fq": fq, "count": count},
                    "chosen": chosen,
                    "chosenBy": chosen_by,
                    "titleScore": chosen_sc,
                    "focus": focus,
                    "startCharRequested": start_requested
                });
                let envelope = serde_json::json!({
                    "jsonrpc":"2.0","id": serde_json::Value::Null,
                    "result": { "content": [{"type":"text","text": sliced}], "_meta": meta }
                });
                println!("{}", serde_json::to_string_pretty(&envelope)?);
            } else {
                println!("{}", sliced);
                eprintln!(
                    "[meta] url={} chosen_title={} score={}",
                    url,
                    chosen.get("fascnm").and_then(|v| v.as_str()).unwrap_or(""),
                    chosen_sc
                );
            }
        } else {
            let text = "no results".to_string();
            if json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&serde_json::json!({
                "jsonrpc":"2.0","id":null,
                "result": {"content":[{"type":"text","text": text}],"_meta": {"count":0}} }))?
                );
            } else {
                println!("{}", text);
            }
        }
    } else {
        let text = "no results".to_string();
        if json {
            println!(
                "{}",
                serde_json::to_string_pretty(&serde_json::json!({
            "jsonrpc":"2.0","id":null,
            "result": {"content":[{"type":"text","text": text}],"_meta": {"count":0}} }))?
            );
        } else {
            println!("{}", text);
        }
    }
    Ok(())
}

pub(crate) fn parse_sat_search_html(
    html: &str,
    q: &str,
    rows: usize,
    offs: usize,
    _exact: bool,
    titles_only: bool,
) -> Vec<SatHit> {
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
        let title = td2
            .text()
            .collect::<Vec<_>>()
            .join(" ")
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        let url = if let Some(a) = td3.select(&a_sel).next() {
            a.value().attr("href").unwrap_or("").to_string()
        } else {
            String::new()
        };
        if !url.contains("satdb2018pre.php") {
            continue;
        }
        out.push(SatHit {
            title,
            url,
            snippet: String::new(),
        });
    }
    if titles_only {
        let nq = normalized(&q);
        let mut filtered: Vec<SatHit> = out
            .into_iter()
            .filter(|h| normalized(&h.title).contains(&nq))
            .collect();
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

pub(crate) fn sat_search_results_cli(
    q: &str,
    rows: usize,
    offs: usize,
    exact: bool,
    titles_only: bool,
) -> Vec<SatHit> {
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
        std::thread::sleep(std::time::Duration::from_millis(backoff));
        backoff = (backoff * 2).min(8000);
    }
    Vec::new()
}
