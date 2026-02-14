use crate::regex_utils::ws_fuzzy_regex;
use crate::{
    decode_xml_bytes, load_or_build_muktabodha_index_cli, resolve_muktabodha_path_cli,
    slice_text_cli, SliceArgs,
};
use daizo_core::path_resolver::muktabodha_root;
use daizo_core::text_utils::highlight_text;
use daizo_core::{extract_text_opts, list_heads_generic, muktabodha_grep};

pub fn muktabodha_title_search(query: &str, limit: usize, json: bool) -> anyhow::Result<()> {
    let idx = load_or_build_muktabodha_index_cli();
    // サンスクリット向けスコアリングを再利用（CLI 側の best_match_gretil）。
    let hits = super::super::best_match_gretil(&idx, query, limit);
    if json {
        let items: Vec<_> = hits
            .iter()
            .map(|h| {
                serde_json::json!({
                    "id": h.entry.id,
                    "title": h.entry.title,
                    "path": h.entry.path,
                    "score": h.score,
                    "meta": h.entry.meta,
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
            println!("{}. {}  {}", i + 1, h.entry.id, h.entry.title);
        }
    }
    Ok(())
}

pub fn muktabodha_fetch(args: &crate::Commands) -> anyhow::Result<()> {
    if let crate::Commands::MuktabodhaFetch {
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
    } = args
    {
        let path = resolve_muktabodha_path_cli(id.as_deref(), query.as_deref());
        if path.as_os_str().is_empty() || !path.exists() {
            return Ok(());
        }
        let bytes = std::fs::read(&path).unwrap_or_default();
        let content = decode_xml_bytes(&bytes);

        let is_xml = path
            .extension()
            .and_then(|s| s.to_str())
            .map(|s| s.eq_ignore_ascii_case("xml"))
            .unwrap_or(false);

        let (text, extraction_method) = if let Some(line_num) = line_number {
            let before = context_lines.unwrap_or(*context_before);
            let after = context_lines.unwrap_or(*context_after);
            let ctx =
                daizo_core::extract_xml_around_line_asymmetric(&content, *line_num, before, after);
            (
                ctx,
                format!("line-context-{}-{}-{}", line_num, before, after),
            )
        } else if is_xml {
            (
                extract_text_opts(&content, *include_notes),
                "full-xml".to_string(),
            )
        } else {
            (content.clone(), "full-txt".to_string())
        };

        let slice = SliceArgs {
            page: *page,
            page_size: *page_size,
            start_char: *start_char,
            end_char: *end_char,
            max_chars: *max_chars,
        };
        let mut sliced = if *full {
            text.clone()
        } else {
            slice_text_cli(&text, &slice)
        };

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

        let heads = if is_xml {
            list_heads_generic(&content)
        } else {
            Vec::new()
        };

        if *json {
            let idx = load_or_build_muktabodha_index_cli();
            let (matched_id, matched_title, matched_score) = if let Some(q) = query.as_deref() {
                if let Some(hit) = super::super::best_match_gretil(&idx, q, 1)
                    .into_iter()
                    .next()
                {
                    (
                        Some(hit.entry.id.clone()),
                        Some(hit.entry.title.clone()),
                        Some(hit.score),
                    )
                } else {
                    (id.clone(), None, None)
                }
            } else {
                (id.clone(), None, None)
            };
            let meta = serde_json::json!({
                "totalLength": text.len(),
                "returnedStart": slice.start().unwrap_or(0),
                "returnedEnd": slice.end_bound(text.len(), sliced.len()),
                "truncated": (sliced.len() as u64) < (text.len() as u64),
                "sourcePath": path.to_string_lossy(),
                "extractionMethod": extraction_method,
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

pub fn muktabodha_search(
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
    let results = muktabodha_grep(&muktabodha_root(), &q, max_results, max_matches_per_file);
    if json {
        let meta = serde_json::json!({
            "searchPattern": q,
            "totalFiles": results.len(),
            "results": results,
            "hint": "Use muktabodha-fetch with the file_id to get full content"
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
            println!();
        }
    }
    Ok(())
}
