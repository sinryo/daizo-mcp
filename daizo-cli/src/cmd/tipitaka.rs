use crate::regex_utils::ws_fuzzy_regex;
use crate::{
    decode_xml_bytes, load_or_build_tipitaka_index_cli, resolve_tipitaka_path, slice_text_cli,
    SliceArgs,
};
use daizo_core::path_resolver::tipitaka_root;
use daizo_core::text_utils::highlight_text;
use daizo_core::{extract_text, list_heads_generic, tipitaka_grep};
use std::path::Path;

pub fn tipitaka_title_search(query: &str, limit: usize, json: bool) -> anyhow::Result<()> {
    let idx = load_or_build_tipitaka_index_cli();
    let hits = super::super::best_match(&idx, query, limit);
    if json {
        let items: Vec<_> = hits
            .iter()
            .map(|h| {
                serde_json::json!({
                    "id": Path::new(&h.entry.path).file_stem().unwrap().to_string_lossy(),
                    "title": h.entry.title,
                    "path": h.entry.path,
                    "score": h.score,
                })
            })
            .collect();
        println!(
            "{}",
            serde_json::to_string_pretty(
                &serde_json::json!({"count": items.len(), "results": items})
            )?
        );
    } else {
        for (i, h) in hits.iter().enumerate() {
            let id = Path::new(&h.entry.path)
                .file_stem()
                .unwrap()
                .to_string_lossy();
            println!("{}. {}  {}", i + 1, id, h.entry.title);
        }
    }
    Ok(())
}

pub fn tipitaka_fetch(args: &crate::Commands) -> anyhow::Result<()> {
    if let crate::Commands::TipitakaFetch {
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
    } = args
    {
        let path = resolve_tipitaka_path(id.as_deref(), query.as_deref());
        if path.as_os_str().is_empty() || !path.exists() {
            return Ok(());
        }
        let xml = std::fs::read(&path)
            .map(|b| decode_xml_bytes(&b))
            .unwrap_or_default();
        let mut text = if let Some(line_num) = line_number {
            let before = context_lines.unwrap_or(*context_before);
            let after = context_lines.unwrap_or(*context_after);
            daizo_core::extract_xml_around_line_asymmetric(&xml, *line_num, before, after)
        } else if let Some(ref hq) = head_query {
            super::super::extract_section_by_head(&xml, None, Some(hq))
                .unwrap_or_else(|| extract_text(&xml))
        } else if let Some(hi) = head_index {
            super::super::extract_section_by_head(&xml, Some(*hi), None)
                .unwrap_or_else(|| extract_text(&xml))
        } else {
            extract_text(&xml)
        };
        if text.trim().is_empty() {
            if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
                if let Some(cand) = super::super::find_tipitaka_content_for_base(&stem) {
                    if cand != path {
                        let xml2 = std::fs::read(&cand)
                            .map(|b| decode_xml_bytes(&b))
                            .unwrap_or_default();
                        text = if let Some(ref hq) = head_query {
                            super::super::extract_section_by_head(&xml2, None, Some(hq))
                        } else if let Some(hi) = head_index {
                            super::super::extract_section_by_head(&xml2, Some(*hi), None)
                        } else {
                            None
                        }
                        .unwrap_or_else(|| extract_text(&xml2));
                    }
                }
            }
        }
        let slice = SliceArgs {
            page: *page,
            page_size: *page_size,
            start_char: *start_char,
            end_char: *end_char,
            max_chars: *max_chars,
        };
        let mut sliced = slice_text_cli(&text, &slice);
        // highlight
        let mut highlighted = 0usize;
        let mut hl_positions: Vec<serde_json::Value> = Vec::new();
        if let Some(hpat0) = highlight.as_deref() {
            let looks_like_regex = hpat0.chars().any(|c| ".+*?[](){}|\\".contains(c));
            let mut hl_is_regex = *highlight_regex;
            let hpat =
                if hpat0.chars().any(|c| c.is_whitespace()) && !looks_like_regex && !hl_is_regex {
                    hl_is_regex = true;
                    ws_fuzzy_regex(hpat0)
                } else {
                    hpat0.to_string()
                };
            let hpre = highlight_prefix.as_deref().unwrap_or(">>> ");
            let hsuf = highlight_suffix.as_deref().unwrap_or(" <<<");
            let (decorated, count, positions) =
                highlight_text(&sliced, &hpat, hl_is_regex, hpre, hsuf);
            sliced = decorated;
            highlighted = count;
            hl_positions = positions
                .into_iter()
                .map(|p| serde_json::json!({"startChar": p.start_char, "endChar": p.end_char}))
                .collect();
        }
        let heads = list_heads_generic(&xml);
        if *json {
            let idx = load_or_build_tipitaka_index_cli();
            let (matched_id, matched_title, matched_score) = if let Some(q) = query.as_deref() {
                if let Some(hit) = super::super::best_match(&idx, q, 1).into_iter().next() {
                    (
                        std::path::Path::new(&hit.entry.path)
                            .file_stem()
                            .map(|s| s.to_string_lossy().into_owned()),
                        Some(hit.entry.title.clone()),
                        Some(hit.score),
                    )
                } else {
                    let stem = path
                        .file_stem()
                        .and_then(|s| s.to_str())
                        .map(|s| s.to_string());
                    let title = idx
                        .iter()
                        .find(|e| e.path == path.to_string_lossy())
                        .map(|e| e.title.clone());
                    (stem, title, None)
                }
            } else {
                let stem = path
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .map(|s| s.to_string());
                let title = idx
                    .iter()
                    .find(|e| e.path == path.to_string_lossy())
                    .map(|e| e.title.clone());
                (stem, title, None)
            };
            let meta = serde_json::json!({
                "totalLength": text.len(),
                "returnedStart": slice.start().unwrap_or(0),
                "returnedEnd": slice.end_bound(text.len(), sliced.len()),
                "truncated": (sliced.len() as u64) < (text.len() as u64),
                "sourcePath": path.to_string_lossy(),
                "extractionMethod": if head_query.is_some() { "head-query" } else if head_index.is_some() { "head-index" } else { "full" },
                "headingsTotal": heads.len(),
                "headingsPreview": heads.clone().into_iter().take(*headings_limit).collect::<Vec<_>>(),
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

pub fn tipitaka_search(
    query: &str,
    max_results: usize,
    max_matches_per_file: usize,
    json: bool,
) -> anyhow::Result<()> {
    let looks_like_regex = query.chars().any(|c| ".+*?[](){}|\\".contains(c));
    let q = if query.chars().any(|c| c.is_whitespace()) && !looks_like_regex {
        ws_fuzzy_regex(query)
    } else {
        query.to_string()
    };
    let results = tipitaka_grep(&tipitaka_root(), &q, max_results, max_matches_per_file);
    if json {
        let meta = serde_json::json!({
            "searchPattern": q,
            "totalFiles": results.len(),
            "results": results,
            "hint": "Use tipitaka-fetch with the file_id to get full content"
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
            println!(
                "   {} matches, {}",
                result.total_matches,
                result
                    .fetch_hints
                    .total_content_size
                    .as_deref()
                    .unwrap_or("unknown size")
            );
            for (j, m) in result.matches.iter().enumerate().take(2) {
                println!(
                    "   Match {}: ...{}...",
                    j + 1,
                    m.context.chars().take(100).collect::<String>()
                );
            }
            if result.matches.len() > 2 {
                println!("   ... and {} more matches", result.matches.len() - 2);
            }
            if !result.fetch_hints.structure_info.is_empty() {
                println!(
                    "   Structure: {}",
                    result.fetch_hints.structure_info.join(", ")
                );
            }
            println!();
        }
    }
    Ok(())
}
