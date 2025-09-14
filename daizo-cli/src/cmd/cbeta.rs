use crate::{
    load_or_build_cbeta_index_cli,
    resolve_cbeta_path_cli,
    SliceArgs,
    slice_text_cli,
    decode_xml_bytes,
};
use daizo_core::{extract_text, extract_text_opts, extract_cbeta_juan, list_heads_cbeta, cbeta_grep};
use daizo_core::text_utils::highlight_text;
use crate::regex_utils::ws_fuzzy_regex;
use daizo_core::path_resolver::cbeta_root;

pub fn cbeta_title_search(query: &str, limit: usize, json: bool) -> anyhow::Result<()> {
    let idx = load_or_build_cbeta_index_cli();
    let hits = super::super::best_match(&idx, query, limit);
    if json {
        let items: Vec<_> = hits.iter().map(|h| serde_json::json!({
            "id": h.entry.id,
            "title": h.entry.title,
            "path": h.entry.path,
            "score": h.score,
        })).collect();
        println!("{}", serde_json::to_string_pretty(&serde_json::json!({"count": items.len(), "results": items}))?);
    } else {
        for (i, h) in hits.iter().enumerate() { println!("{}. {}  {}", i+1, h.entry.id, h.entry.title); }
    }
    Ok(())
}

pub fn cbeta_fetch(args: &crate::Commands) -> anyhow::Result<()> {
    if let crate::Commands::CbetaFetch { id, query, part, include_notes, full, highlight, highlight_regex, highlight_prefix, highlight_suffix, headings_limit, start_char, end_char, max_chars, page, page_size, line_number, context_before, context_after, context_lines, json } = args {
        let path = resolve_cbeta_path_cli(id.as_deref(), query.as_deref());
        if path.as_os_str().is_empty() || !path.exists() { return Ok(()); }
        let xml = std::fs::read(&path).map(|b| decode_xml_bytes(&b)).unwrap_or_default();
        let (text, extraction_method, part_matched) = if let Some(line_num) = line_number {
            let before = context_lines.unwrap_or(*context_before);
            let after = context_lines.unwrap_or(*context_after);
            let context_text = daizo_core::extract_xml_around_line_asymmetric(&xml, *line_num, before, after);
            (context_text, format!("line-context-{}-{}-{}", line_num, before, after), false)
        } else if let Some(p) = part.as_ref() {
            if let Some(sec) = extract_cbeta_juan(&xml, p) { (sec, "cbeta-juan".to_string(), true) } else { (extract_text_opts(&xml, *include_notes), "full".to_string(), false) }
        } else { (extract_text_opts(&xml, *include_notes), "full".to_string(), false) };
        let slice = SliceArgs { page: *page, page_size: *page_size, start_char: *start_char, end_char: *end_char, max_chars: *max_chars };
        let mut sliced = if *full { text.clone() } else { slice_text_cli(&text, &slice) };
        let mut highlighted = 0usize; let mut hl_positions: Vec<serde_json::Value> = Vec::new();
        if let Some(hpat0) = highlight.as_deref() {
            let looks_like_regex = hpat0.chars().any(|c| ".+*?[](){}|\\".contains(c));
            let mut hl_is_regex = *highlight_regex;
            let hpat = if hpat0.chars().any(|c| c.is_whitespace()) && !looks_like_regex && !hl_is_regex { hl_is_regex = true; ws_fuzzy_regex(hpat0) } else { hpat0.to_string() };
            let hpre = highlight_prefix.as_deref().unwrap_or(">>> ");
            let hsuf = highlight_suffix.as_deref().unwrap_or(" <<<");
            let (decorated, count, positions) = highlight_text(&sliced, &hpat, hl_is_regex, hpre, hsuf);
            sliced = decorated; highlighted = count;
            hl_positions = positions.into_iter().map(|p| serde_json::json!({"startChar": p.start_char, "endChar": p.end_char})).collect();
        }
        let heads = list_heads_cbeta(&xml);
        if *json {
            let idx = load_or_build_cbeta_index_cli();
            let (matched_id, matched_title, matched_score) = if let Some(q) = query.as_deref() {
                if let Some(hit) = super::super::best_match(&idx, q, 1).into_iter().next() { (Some(hit.entry.id.clone()), Some(hit.entry.title.clone()), Some(hit.score)) } else { (id.clone(), None, None) }
            } else { (id.clone(), None, None) };
            let meta = serde_json::json!({
                "totalLength": text.len(),
                "returnedStart": slice.start().unwrap_or(0),
                "returnedEnd": slice.end_bound(text.len(), sliced.len()),
                "truncated": (sliced.len() as u64) < (text.len() as u64),
                "sourcePath": path.to_string_lossy(),
                "extractionMethod": extraction_method,
                "partMatched": part_matched,
                "headingsTotal": heads.len(),
                "headingsPreview": heads.into_iter().take(*headings_limit).collect::<Vec<_>>(),
                "matchedId": matched_id,
                "matchedTitle": matched_title,
                "matchedScore": matched_score,
                "highlighted": if highlighted > 0 { Some(highlighted) } else { None::<usize> },
                "highlightPositions": if !hl_positions.is_empty() { Some(hl_positions) } else { None::<Vec<serde_json::Value>> },
            });
            let envelope = serde_json::json!({
                "jsonrpc":"2.0","id": serde_json::Value::Null,
                "result": { "content": [{"type":"text","text": sliced}], "_meta": meta }
            });
            println!("{}", serde_json::to_string_pretty(&envelope)?);
        } else {
            println!("{}", sliced);
        }
    }
    Ok(())
}

pub fn cbeta_pipeline(args: &crate::Commands) -> anyhow::Result<()> {
    if let crate::Commands::CbetaPipeline { query, max_results, max_matches_per_file, context_before, context_after, autofetch, auto_fetch_files, auto_fetch_matches, include_match_line, include_highlight_snippet, min_snippet_len, highlight, highlight_regex, highlight_prefix, highlight_suffix, snippet_prefix, snippet_suffix, full, include_notes, json } = args {
        let root = cbeta_root();
        let looks_like_regex = query.chars().any(|c| ".+*?[](){}|\\".contains(c));
        let q = if query.chars().any(|c| c.is_whitespace()) && !looks_like_regex { ws_fuzzy_regex(&query) } else { query.clone() };
        let results = cbeta_grep(&root, &q, *max_results, *max_matches_per_file);
        let mut summary = format!("Found {} files with matches for '{}':\n\n", results.len(), q);
        let mut suggestions: Vec<serde_json::Value> = Vec::new();
        for (i, result) in results.iter().enumerate() {
            summary.push_str(&format!("{}. {} ({})\n", i + 1, result.title, result.file_id));
            summary.push_str(&format!("   {} matches\n", result.total_matches));
            for (j, m) in result.matches.iter().enumerate().take(2) {
                summary.push_str(&format!("   Match {}: ...{}...\n", j + 1, m.context.chars().take(100).collect::<String>()));
            }
            if let Some(m) = result.matches.first() {
                if let Some(ln) = m.line_number { suggestions.push(serde_json::json!({
                    "cmd": "daizo-cli cbeta-fetch --id <ID> --line-number <LN> --context-before <B> --context-after <A>",
                    "id": result.file_id, "lineNumber": ln, "contextBefore": context_before, "contextAfter": context_after
                })); }
            }
            summary.push('\n');
        }
        let mut contents: Vec<serde_json::Value> = vec![serde_json::json!({"type":"text","text": summary})];
        let mut meta = serde_json::json!({
            "searchPattern": q,
            "totalFiles": results.len(),
            "results": results,
            "fetchSuggestions": suggestions
        });
        if *autofetch {
            let take_files = auto_fetch_files.unwrap_or(1).min(results.len());
            let mut fetched: Vec<serde_json::Value> = Vec::new();
            for r in results.iter().take(take_files) {
                let xml = std::fs::read_to_string(&r.file_path).unwrap_or_default();
                if *full {
                    let text = if *include_notes { extract_text_opts(&xml, true) } else { extract_text(&xml) };
                    contents.push(serde_json::json!({"type":"text","text": text}));
                    fetched.push(serde_json::json!({"id": r.file_id, "full": true}));
                } else {
                    let per_file_limit = auto_fetch_matches.unwrap_or(*max_matches_per_file);
                    let mut combined = String::new();
                    let mut count = 0usize;
                    let mut file_highlights: Vec<Vec<serde_json::Value>> = Vec::new();
                    let hpre = highlight_prefix.as_deref().unwrap_or(">>> ");
                    let hsuf = highlight_suffix.as_deref().unwrap_or(" <<<");
                    let spre = snippet_prefix.as_deref().unwrap_or(">>> ");
                    let ssuf = snippet_suffix.as_deref().unwrap_or("");
                    for m in r.matches.iter().take(per_file_limit) {
                        if let Some(ln) = m.line_number {
                            let mut ctx = daizo_core::extract_xml_around_line_asymmetric(&xml, ln, *context_before, *context_after);
                            if !*include_match_line {
                                let mut lines: Vec<&str> = ctx.lines().collect();
                                if *context_before < lines.len() { lines.remove(*context_before); }
                                ctx = lines.join("\n");
                            }
                            if !ctx.trim().is_empty() {
                                if !combined.is_empty() { combined.push_str("\n\n---\n\n"); }
                                if *include_highlight_snippet && !m.highlight.trim().is_empty() && m.highlight.trim().chars().count() >= *min_snippet_len {
                                    combined.push_str(&format!("{}{}{}\n\n", spre, m.highlight.trim(), ssuf));
                                }
                                let mut chigh: Vec<serde_json::Value> = Vec::new();
                                if let Some(pat0) = highlight.as_deref() {
                                    let looks_like = pat0.chars().any(|c| ".+*?[](){}|\\".contains(c));
                                    let mut hlr = *highlight_regex;
                                    let pat = if pat0.chars().any(|c| c.is_whitespace()) && !looks_like && !hlr { hlr = true; ws_fuzzy_regex(pat0) } else { pat0.to_string() };
                                    if hlr {
                                        if let Ok(re) = regex::Regex::new(&pat) {
                                            for mm in re.find_iter(&ctx) {
                                                let sb = mm.start(); let eb = mm.end();
                                                let sc = ctx[..sb].chars().count(); let ec = sc + ctx[sb..eb].chars().count();
                                                chigh.push(serde_json::json!({"startChar": sc, "endChar": ec}));
                                            }
                                            ctx = re.replace_all(&ctx, |caps: &regex::Captures| format!("{}{}{}", hpre, &caps[0], hsuf)).into_owned();
                                        }
                                    } else if !pat.is_empty() {
                                        let mut i2 = 0usize;
                                        while let Some(pos2) = ctx[i2..].find(&pat) {
                                            let abs2 = i2 + pos2; let sc = ctx[..abs2].chars().count(); let ec = sc + pat.chars().count();
                                            chigh.push(serde_json::json!({"startChar": sc, "endChar": ec}));
                                            i2 = abs2 + pat.len();
                                        }
                                        let mut out2 = String::with_capacity(ctx.len()); let mut j2 = 0usize;
                                        while let Some(pos2) = ctx[j2..].find(&pat) {
                                            let abs2 = j2 + pos2; out2.push_str(&ctx[j2..abs2]); out2.push_str(hpre); out2.push_str(&pat); out2.push_str(hsuf); j2 = abs2 + pat.len();
                                        }
                                        out2.push_str(&ctx[j2..]); ctx = out2;
                                    }
                                }
                                combined.push_str(&format!("# {} (line {})\n\n{}", r.file_id, ln, ctx));
                                file_highlights.push(chigh);
                                count += 1;
                            }
                        }
                    }
                    if !combined.is_empty() {
                        contents.push(serde_json::json!({"type":"text","text": combined}));
                        let mut fobj = serde_json::json!({
                            "id": r.file_id, "full": false, "contexts": count,
                            "contextBefore": context_before, "contextAfter": context_after,
                            "includeMatchLine": include_match_line,
                        });
                        fobj["highlightPositions"] = serde_json::json!(file_highlights);
                        fetched.push(fobj);
                    }
                }
            }
            if !fetched.is_empty() { meta["autoFetched"] = serde_json::json!(fetched); }
            // build highlightPositions aggregate for first auto-fetched files
            let mut hl_aggregate: Vec<serde_json::Value> = Vec::new();
            let tf = auto_fetch_files.unwrap_or(1).min(results.len());
            for r in results.iter().take(tf) {
                let per_file_limit = auto_fetch_matches.unwrap_or(*max_matches_per_file);
                let xml = std::fs::read_to_string(&r.file_path).unwrap_or_default();
                let mut file_ctx_highlights: Vec<Vec<serde_json::Value>> = Vec::new();
                for m in r.matches.iter().take(per_file_limit) {
                    if let Some(ln) = m.line_number {
                        let ctx = daizo_core::extract_xml_around_line_asymmetric(&xml, ln, *context_before, *context_after);
                        let mut chigh: Vec<serde_json::Value> = Vec::new();
                        if let Some(pat0) = highlight.as_deref() {
                            let looks_like = pat0.chars().any(|c| ".+*?[](){}|\\".contains(c));
                            let mut hlr = *highlight_regex;
                            let pat = if pat0.chars().any(|c| c.is_whitespace()) && !looks_like && !hlr { hlr = true; ws_fuzzy_regex(pat0) } else { pat0.to_string() };
                            if hlr {
                                if let Ok(re) = regex::Regex::new(&pat) {
                                    for mm in re.find_iter(&ctx) {
                                        let sb = mm.start(); let eb = mm.end();
                                        let sc = ctx[..sb].chars().count(); let ec = sc + ctx[sb..eb].chars().count();
                                        chigh.push(serde_json::json!({"startChar": sc, "endChar": ec}));
                                    }
                                }
                            } else if !pat.is_empty() {
                                let mut i2 = 0usize;
                                while let Some(pos2) = ctx[i2..].find(&pat) {
                                    let abs2 = i2 + pos2; let sc = ctx[..abs2].chars().count(); let ec = sc + pat.chars().count();
                                    chigh.push(serde_json::json!({"startChar": sc, "endChar": ec}));
                                    i2 = abs2 + pat.len();
                                }
                            }
                        }
                        file_ctx_highlights.push(chigh);
                    }
                }
                hl_aggregate.push(serde_json::json!({"id": r.file_id, "highlightPositions": file_ctx_highlights}));
            }
            if !hl_aggregate.is_empty() { meta["highlightPositions"] = serde_json::json!(hl_aggregate); }
        }
        if *json {
            let envelope = serde_json::json!({
                "jsonrpc":"2.0","id": serde_json::Value::Null,
                "result": { "content": contents, "_meta": meta }
            });
            println!("{}", serde_json::to_string_pretty(&envelope)?);
        } else {
            for c in contents { if let Some(t) = c.get("text").and_then(|v| v.as_str()) { println!("{}", t); } }
        }
    }
    Ok(())
}

pub fn cbeta_search(query: &str, max_results: usize, max_matches_per_file: usize, json: bool) -> anyhow::Result<()> {
    let looks_like_regex = query.chars().any(|c| ".+*?[](){}|\\".contains(c));
    let q = if query.chars().any(|c| c.is_whitespace()) && !looks_like_regex { ws_fuzzy_regex(query) } else { query.to_string() };
    let results = cbeta_grep(&cbeta_root(), &q, max_results, max_matches_per_file);
    if json {
        let meta = serde_json::json!({
            "searchPattern": q,
            "totalFiles": results.len(),
            "results": results,
            "hint": "Use cbeta-fetch with the file_id and recommended parts to get full content"
        });
        let summary = format!("Found {} files with matches for '{}'", results.len(), q);
        let envelope = serde_json::json!({
            "jsonrpc":"2.0","id": serde_json::Value::Null,
            "result": { "content": [{"type":"text","text": summary}], "_meta": meta }
        });
        println!("{}", serde_json::to_string_pretty(&envelope)?);
    } else {
        println!("Found {} files with matches for '{}':\n", results.len(), q);
        for (i, result) in results.iter().enumerate() {
            println!("{}. {} ({})", i + 1, result.title, result.file_id);
            println!("   {} matches, {}", result.total_matches, result.fetch_hints.total_content_size.as_deref().unwrap_or("unknown size"));
            for (j, m) in result.matches.iter().enumerate().take(2) {
                println!("   Match {}: ...{}...", j + 1, m.context.chars().take(100).collect::<String>());
            }
            if result.matches.len() > 2 { println!("   ... and {} more matches", result.matches.len() - 2); }
            if !result.fetch_hints.recommended_parts.is_empty() { println!("   Recommended parts: {}", result.fetch_hints.recommended_parts.join(", ")); }
            println!();
        }
    }
    Ok(())
}
