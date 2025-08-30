use quick_xml::events::{BytesStart, Event};
use quick_xml::Reader;
use rayon::prelude::*;
use serde::Serialize;
use std::collections::BTreeMap;
use std::borrow::Cow;
use std::collections::HashMap;
use unicode_normalization::UnicodeNormalization;
use std::fs::File;
use std::io::BufReader;
use std::path::{Path, PathBuf};
use walkdir::WalkDir;

use serde::Deserialize;
use encoding_rs;

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct IndexEntry {
    pub id: String,
    pub title: String,
    pub path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub meta: Option<BTreeMap<String, String>>, // optional metadata (e.g., for Tipitaka)
}

fn stem_from(path: &Path) -> String {
    path.file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_string()
}

fn local_name<'a>(name: &'a [u8]) -> &'a [u8] {
    match name.rsplit(|b| *b == b':').next() {
        Some(n) => n,
        None => name,
    }
}

fn attr_val<'a>(e: &'a BytesStart<'a>, key: &[u8]) -> Option<Cow<'a, str>> {
    for a in e.attributes().with_checks(false) {
        if let Ok(a) = a {
            if a.key.as_ref() == key {
                return String::from_utf8(a.value.into_owned()).ok().map(Cow::Owned);
            }
        }
    }
    None
}

pub fn build_index(root: &Path, glob_hint: Option<&str>) -> Vec<IndexEntry> {
    let mut paths: Vec<PathBuf> = Vec::new();
    for e in WalkDir::new(root).into_iter().filter_map(|e| e.ok()) {
        if e.file_type().is_file() {
            if let Some(name) = e.path().file_name().and_then(|s| s.to_str()) {
                if name.ends_with(".xml") {
                    if let Some(h) = glob_hint {
                        if !e.path().to_string_lossy().contains(h) {
                            continue;
                        }
                    }
                    paths.push(e.into_path());
                }
            }
        }
    }

    paths
        .par_iter()
        .filter_map(|p| {
            let f = File::open(p).ok()?;
            let mut reader = Reader::from_reader(BufReader::new(f));
            reader.config_mut().trim_text_start = true;
            reader.config_mut().trim_text_end = true;
            let mut buf = Vec::new();
            let mut id: Option<String> = None;
            let mut title: Option<String> = None; // from teiHeader/title
            let mut in_header = false;
            let mut in_title = false;

            // fallback: first <head> or <jhead><title>
            let mut path_stack: Vec<Vec<u8>> = Vec::new();
            let mut in_head = false;
            let mut head_buf = String::new();
            let mut in_jhead_title = false;
            let mut jhead_buf = String::new();
            let mut fallback_title: Option<String> = None;
            loop {
                match reader.read_event_into(&mut buf) {
                    Ok(Event::Start(e)) | Ok(Event::Empty(e)) => {
                        let name_owned = e.name().as_ref().to_owned();
                        let name = local_name(&name_owned);
                        if id.is_none() {
                            if let Some(v) = attr_val(&e, b"xml:id") { id = Some(v.to_string()); }
                        }
                        // stack push for fallback scanning
                        path_stack.push(name.to_vec());

                        if name == b"teiHeader" { in_header = true; }
                        if in_header && name == b"title" { in_title = true; }

                        // fallback: head or jhead/title
                        if name == b"head" { in_head = true; head_buf.clear(); }
                        if name == b"title" {
                            if path_stack.iter().any(|n| n.as_slice() == b"jhead") {
                                in_jhead_title = true; jhead_buf.clear();
                            }
                        }
                    }
                    Ok(Event::End(e)) => {
                        let name_owned = e.name().as_ref().to_owned();
                        let name = local_name(&name_owned);
                        if name == b"title" { in_title = false; in_jhead_title = false; }
                        if name == b"head" && in_head {
                            if fallback_title.is_none() {
                                let t = head_buf.split_whitespace().collect::<Vec<_>>().join(" ");
                                if !t.is_empty() { fallback_title = Some(t); }
                            }
                            in_head = false; head_buf.clear();
                        }
                        if name == b"teiHeader" {
                            // do not break; continue to allow fallback scanning in body if no title yet
                            // only early-stop if we already have a header title
                            if title.is_some() { break; }
                        }
                        path_stack.pop();
                    }
                    Ok(Event::Text(t)) => {
                        if in_title {
                            let t = t.decode().unwrap_or_default().into_owned();
                            if !t.trim().is_empty() { title = Some(t); }
                        }
                        // fallback buffers
                        let tx = t.decode().unwrap_or_default();
                        if in_head { head_buf.push_str(&tx); }
                        if in_jhead_title { jhead_buf.push_str(&tx); }
                    }
                    Ok(Event::Eof) => break,
                    Err(_) => break,
                    _ => {}
                }
                buf.clear();
                if title.is_some() && id.is_some() { break; }
                // consider jhead/title as candidate if not set yet
                if fallback_title.is_none() && !jhead_buf.trim().is_empty() {
                    let t = jhead_buf.split_whitespace().collect::<Vec<_>>().join(" ");
                    if !t.is_empty() { fallback_title = Some(t); }
                }
            }
            let id = id.unwrap_or_else(|| stem_from(p));
            let title = title
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .or(fallback_title)
                .unwrap_or_else(|| stem_from(p));
            let abs = std::fs::canonicalize(p).unwrap_or_else(|_| p.clone());
            Some(IndexEntry { id, title, path: abs.to_string_lossy().to_string(), meta: None })
        })
        .collect()
}

// CBETA 用: TEI ヘッダや本文の構造からメタ情報を抽出してインデックスを高精度化
pub fn build_cbeta_index(root: &Path) -> Vec<IndexEntry> {
    let mut paths: Vec<PathBuf> = Vec::new();
    for e in WalkDir::new(root).into_iter().filter_map(|e| e.ok()) {
        if e.file_type().is_file() {
            if let Some(name) = e.path().file_name().and_then(|s| s.to_str()) {
                if name.ends_with(".xml") { paths.push(e.into_path()); }
            }
        }
    }

    paths
        .par_iter()
        .filter_map(|p| {
            let f = File::open(p).ok()?;
            let mut reader = Reader::from_reader(BufReader::new(f));
            reader.config_mut().trim_text_start = true;
            reader.config_mut().trim_text_end = true;
            let mut buf = Vec::new();

            let mut id: Option<String> = None;
            let mut title_header: Option<String> = None;
            let mut author: Option<String> = None;
            let mut editor: Option<String> = None;
            // respStmt aggregation (role + names)
            let mut in_resp_stmt = false;
            let mut in_resp_role = false;
            let mut in_resp_name = false;
            let mut cur_resp_role: String = String::new();
            let mut cur_resp_names: Vec<String> = Vec::new();
            let mut resp_entries: Vec<String> = Vec::new();
            let mut publisher: Option<String> = None;
            let mut pubdate: Option<String> = None;
            let mut idno: Option<String> = None;
            let mut heads: Vec<String> = Vec::new();
            let mut juan_count: usize = 0;

            let mut path_stack: Vec<Vec<u8>> = Vec::new();
            let mut in_title_header = false;
            let mut in_author = false;
            let mut in_editor = false;
            let mut in_publisher = false;
            let mut in_date = false;
            let mut in_idno = false;
            let mut in_head = false;
            let mut head_buf = String::new();

            let mut events = 0usize;
            let max_events = 50_000usize;

            loop {
                match reader.read_event_into(&mut buf) {
                    Ok(Event::Start(e)) | Ok(Event::Empty(e)) => {
                        let name_owned = e.name().as_ref().to_owned();
                        let lname = local_name(&name_owned).to_vec();
                        if id.is_none() {
                            if let Some(v) = attr_val(&e, b"xml:id") { id = Some(v.to_string()); }
                        }
                        path_stack.push(lname.clone());

                        if lname.as_slice() == b"title" {
                            if path_stack.iter().any(|n| n.as_slice() == b"titleStmt") && path_stack.iter().any(|n| n.as_slice() == b"teiHeader") {
                                in_title_header = true;
                            }
                        }
                        if lname.as_slice() == b"author" && path_stack.iter().any(|n| n.as_slice() == b"titleStmt") { in_author = true; }
                        if lname.as_slice() == b"editor" && path_stack.iter().any(|n| n.as_slice() == b"titleStmt") { in_editor = true; }
                        if lname.as_slice() == b"respStmt" { in_resp_stmt = true; cur_resp_role.clear(); cur_resp_names.clear(); }
                        if in_resp_stmt && lname.as_slice() == b"resp" { in_resp_role = true; }
                        if in_resp_stmt && (lname.as_slice() == b"name" || lname.as_slice() == b"persName") { in_resp_name = true; }
                        if lname.as_slice() == b"publisher" { in_publisher = true; }
                        if lname.as_slice() == b"date" { in_date = true; }
                        if lname.as_slice() == b"idno" { in_idno = true; }
                        if lname.as_slice() == b"head" { in_head = true; head_buf.clear(); }
                        if lname.as_slice() == b"juan" {
                            let fun = attr_val(&e, b"fun").map(|v| v.to_ascii_lowercase());
                            if fun.as_deref() == Some("open") || fun.is_none() { juan_count += 1; }
                        }
                    }
                    Ok(Event::End(e)) => {
                        let name_owned = e.name().as_ref().to_owned();
                        let lname = local_name(&name_owned);
                        if lname == b"title" && in_title_header { in_title_header = false; }
                        if lname == b"author" && in_author { in_author = false; }
                        if lname == b"editor" && in_editor { in_editor = false; }
                        if lname == b"resp" && in_resp_role { in_resp_role = false; }
                        if (lname == b"name" || lname == b"persName") && in_resp_name { in_resp_name = false; }
                        if lname == b"respStmt" && in_resp_stmt {
                            // finalize current respStmt entry
                            let names_join = cur_resp_names.join("・");
                            let entry = if !cur_resp_role.trim().is_empty() {
                                format!("{}: {}", cur_resp_role.trim(), names_join)
                            } else { names_join };
                            if !entry.trim().is_empty() { resp_entries.push(entry); }
                            in_resp_stmt = false;
                            cur_resp_role.clear(); cur_resp_names.clear();
                        }
                        if lname == b"publisher" && in_publisher { in_publisher = false; }
                        if lname == b"date" && in_date { in_date = false; }
                        if lname == b"idno" && in_idno { in_idno = false; }
                        if lname == b"head" && in_head {
                            let t = head_buf.split_whitespace().collect::<Vec<_>>().join(" ");
                            if !t.is_empty() && heads.len() < 12 { heads.push(t); }
                            in_head = false; head_buf.clear();
                        }
                        path_stack.pop();
                    }
                    Ok(Event::Text(t)) => {
                        let tx = t.decode().unwrap_or_default();
                        let s = tx.to_string();
                        if in_title_header && title_header.is_none() && !s.trim().is_empty() { title_header = Some(s.clone()); }
                        if in_author && author.is_none() && !s.trim().is_empty() { author = Some(s.clone()); }
                        if in_editor && editor.is_none() && !s.trim().is_empty() { editor = Some(s.clone()); }
                        if in_resp_role && !s.trim().is_empty() { cur_resp_role.push_str(&s); }
                        if in_resp_name && !s.trim().is_empty() { cur_resp_names.push(s.clone()); }
                        if in_publisher && publisher.is_none() && !s.trim().is_empty() { publisher = Some(s.clone()); }
                        if in_date && pubdate.is_none() && !s.trim().is_empty() { pubdate = Some(s.clone()); }
                        if in_idno && idno.is_none() && !s.trim().is_empty() { idno = Some(s.clone()); }
                        if in_head { head_buf.push_str(&s); }
                    }
                    Ok(Event::Eof) => break,
                    Err(_) => break,
                    _ => {}
                }
                buf.clear();
                events += 1; if events > max_events { break; }
            }

            let id = id.unwrap_or_else(|| stem_from(p));
            let title = title_header
                .or_else(|| heads.get(0).cloned())
                .unwrap_or_else(|| stem_from(p));
            let abs = std::fs::canonicalize(p).unwrap_or_else(|_| p.clone());

            let canon = abs.parent()
                .and_then(|pp| pp.strip_prefix(root).ok())
                .and_then(|rel| rel.components().next())
                .map(|c| c.as_os_str().to_string_lossy().to_string())
                .unwrap_or_default();

            let fname = abs.file_name().and_then(|s| s.to_str()).unwrap_or("").to_string();
            let mut nnum: Option<String> = None;
            if let Some(pos) = fname.to_lowercase().find('n') {
                let digits: String = fname[pos+1..].chars().take_while(|c| c.is_ascii_digit()).collect();
                if !digits.is_empty() { nnum = Some(digits); }
            }

            let mut meta = BTreeMap::new();
            if !canon.is_empty() { meta.insert("canon".to_string(), canon); }
            if let Some(a) = author { meta.insert("author".to_string(), a); }
            if let Some(ed) = editor { meta.insert("editor".to_string(), ed); }
            if !resp_entries.is_empty() { meta.insert("respAll".to_string(), resp_entries.join(" | ")); }
            // try to extract translators from resp entries
            if !resp_entries.is_empty() {
                let mut translators: Vec<String> = Vec::new();
                for e in resp_entries.iter() {
                    let low = e.to_lowercase();
                    if low.contains("譯") || low.contains("译") || low.contains("translat") || low.contains("tr.") {
                        // extract part after ':' if any
                        if let Some(pos) = e.find(':') { translators.push(e[pos+1..].trim().to_string()); }
                        else { translators.push(e.clone()); }
                    }
                }
                if !translators.is_empty() { meta.insert("translator".to_string(), translators.join("・")); }
            }
            if let Some(pu) = publisher { meta.insert("publisher".to_string(), pu); }
            if let Some(pd) = pubdate { meta.insert("date".to_string(), pd); }
            if let Some(i) = idno { meta.insert("idno".to_string(), i); }
            if let Some(nn) = nnum { meta.insert("nnum".to_string(), nn); }
            if juan_count > 0 { meta.insert("juanCount".to_string(), juan_count.to_string()); }
            if !heads.is_empty() { meta.insert("headsPreview".to_string(), heads.iter().take(10).cloned().collect::<Vec<_>>().join(" | ")); }

            Some(IndexEntry { id, title, path: abs.to_string_lossy().to_string(), meta: if meta.is_empty() { None } else { Some(meta) } })
        })
        .collect()
}

// Tipitaka 用: teiHeader が空な場合が多いため、<p rend="..."> 系から書誌情報を抽出してタイトルを構築
pub fn build_tipitaka_index(root: &Path) -> Vec<IndexEntry> {
    // 走査: root 配下の .xml で .toc.xml は除外 (rootは既にromnディレクトリを指している)
    let mut paths: Vec<PathBuf> = Vec::new();
    for e in WalkDir::new(root).into_iter().filter_map(|e| e.ok()) {
        if e.file_type().is_file() {
            if let Some(name) = e.path().file_name().and_then(|s| s.to_str()) {
                if name.ends_with(".xml")
                    && !name.contains("toc")
                    && !name.contains("sitemap")
                    && !name.contains("tree")
                    && !name.ends_with(".xsl")
                    && !name.ends_with(".css")
                {
                    paths.push(e.into_path());
                }
            }
        }
    }

    paths
        .par_iter()
        .filter_map(|p| {
            // UTF-16 TipitakaファイルをUTF-8で読み込み
            let content = match std::fs::read(p) {
                Ok(bytes) => {
                    // UTF-16 BOMを検出して適切にデコード
                    if bytes.starts_with(&[0xFF, 0xFE]) {
                        // UTF-16 LE
                        match encoding_rs::UTF_16LE.decode(&bytes) {
                            (decoded, _, false) => decoded.into_owned(),
                            _ => return None,
                        }
                    } else if bytes.starts_with(&[0xFE, 0xFF]) {
                        // UTF-16 BE  
                        match encoding_rs::UTF_16BE.decode(&bytes) {
                            (decoded, _, false) => decoded.into_owned(),
                            _ => return None,
                        }
                    } else {
                        // UTF-8として読み込み
                        match String::from_utf8(bytes) {
                            Ok(s) => s,
                            Err(_) => return None,
                        }
                    }
                },
                Err(_) => return None,
            };
            
            let mut reader = Reader::from_str(&content);
            reader.config_mut().trim_text_start = true;
            reader.config_mut().trim_text_end = true;
            let mut buf = Vec::new();
            
            // より正確な構造解析のための変数
            let mut in_p = false;
            let mut in_head = false;
            let _in_div = false;
            let mut current_rend: Option<String> = None;
            let mut current_buf = String::new();
            let mut head_buf = String::new();
            
            // 構造化データの収集
            let mut fields: std::collections::HashMap<String, Vec<String>> = std::collections::HashMap::new();
            let mut heads: Vec<String> = Vec::new();
            let mut div_info: Vec<(String, String)> = Vec::new(); // (n, type) pairs
            
            // 対象キー（拡張） - gathalastも含める
            let wanted_p = ["nikaya", "title", "subhead", "subsubhead", "gathalast"];
            let wanted_head = ["book", "chapter"];
            let mut events_read = 0usize;
            let max_events = 50_000usize; // ファイル全体を読むために大きく設定

            loop {
                match reader.read_event_into(&mut buf) {
                    Ok(Event::Start(e)) | Ok(Event::Empty(e)) => {
                        let name_owned = e.name().as_ref().to_owned();
                        let name = local_name(&name_owned);
                        
                        if name == b"p" {
                            in_p = true;
                            current_buf.clear();
                            current_rend = attr_val(&e, b"rend").map(|v| v.to_ascii_lowercase());
                        } else if name == b"head" {
                            in_head = true;
                            head_buf.clear();
                            current_rend = attr_val(&e, b"rend").map(|v| v.to_ascii_lowercase());
                        } else if name == b"div" {
                            // div要素の属性を記録
                            if let (Some(n), Some(type_val)) = (attr_val(&e, b"n"), attr_val(&e, b"type")) {
                                div_info.push((n.to_string(), type_val.to_string()));
                            }
                        }
                    }
                    Ok(Event::End(e)) => {
                        let name_owned = e.name().as_ref().to_owned();
                        let name = local_name(&name_owned);
                        
                        if name == b"p" && in_p {
                            if let Some(rend) = current_rend.take() {
                                let key = rend.trim().to_string();
                                let val = current_buf.trim().to_string();
                                if !val.is_empty() && wanted_p.contains(&key.as_str()) {
                                    let values = fields.entry(key).or_insert_with(Vec::new);
                                    // 重複チェック: 同じ文字列が既に存在しない場合のみ追加
                                    if !values.contains(&val) {
                                        values.push(val);
                                    }
                                }
                            }
                            in_p = false;
                            current_buf.clear();
                        }
                        
                        if name == b"head" && in_head {
                            if let Some(rend) = current_rend.take() {
                                let key = rend.trim().to_string();
                                let val = head_buf.trim().to_string();
                                if !val.is_empty() {
                                    if wanted_head.contains(&key.as_str()) {
                                        let values = fields.entry(key).or_insert_with(Vec::new);
                                        // 重複チェック: 同じ文字列が既に存在しない場合のみ追加
                                        if !values.contains(&val) {
                                            values.push(val.clone());
                                        }
                                    }
                                    // 全てのheadを一般コレクションにも追加（重複チェック）
                                    if heads.len() < 15 && !heads.contains(&val) { 
                                        heads.push(val); 
                                    }
                                }
                            } else {
                                // rendなしのheadも収集（重複チェック）
                                let t = head_buf.split_whitespace().collect::<Vec<_>>().join(" ");
                                if !t.is_empty() && heads.len() < 15 && !heads.contains(&t) { 
                                    heads.push(t); 
                                }
                            }
                            in_head = false; 
                            head_buf.clear();
                        }
                    }
                    Ok(Event::Text(t)) => {
                        let tx = t.decode().unwrap_or_default();
                        if in_p {
                            current_buf.push_str(&tx);
                        }
                        if in_head {
                            head_buf.push_str(&tx);
                        }
                    }
                    Ok(Event::Eof) => break,
                    Err(_) => break,
                    _ => {}
                }
                buf.clear();
                events_read += 1;
                if events_read > max_events { break; }
            }

            // タイトル組み立て: 階層順序に従って構築
            let parts_order = ["nikaya", "book", "title", "subhead", "subsubhead", "chapter", "gathalast"]; 
            let mut parts: Vec<String> = Vec::new();
            for k in parts_order.iter() {
                if let Some(values) = fields.get(*k) {
                    // 複数値がある場合は最初の値を使用（タイトル構築用）
                    if let Some(first_val) = values.first() {
                        if !first_val.trim().is_empty() { 
                            parts.push(first_val.trim().to_string()); 
                        }
                    }
                }
            }
            let title = if parts.is_empty() { stem_from(p) } else { parts.join(" · ") };
            let id = stem_from(p);
            let abs = std::fs::canonicalize(p).unwrap_or_else(|_| p.clone());
            
            // メタデータの構築
            let mut meta_map: BTreeMap<String, String> = BTreeMap::new();
            
            // フィールド値をメタデータに格納
            for (key, values) in fields.iter() {
                if values.len() == 1 {
                    // 単一値の場合
                    meta_map.insert(key.clone(), values[0].clone());
                } else if values.len() > 1 {
                    // 複数値の場合
                    meta_map.insert(key.clone(), values.join(" | "));
                    // 個別のエントリも作成
                    for (i, value) in values.iter().enumerate() {
                        meta_map.insert(format!("{}_{}", key, i + 1), value.clone());
                    }
                }
            }
            
            // div情報をメタデータに追加
            if !div_info.is_empty() {
                let mut div_sections: Vec<String> = Vec::new();
                let mut div_types: Vec<String> = Vec::new();
                for (n, type_val) in div_info.iter() {
                    div_sections.push(format!("{}({})", n, type_val));
                    div_types.push(type_val.clone());
                }
                meta_map.insert("sections".to_string(), div_sections.join(" | "));
                meta_map.insert("section_types".to_string(), div_types.join(" | "));
            }
            // Infer alias for Nikaya codes (DN/MN/SN/AN/KN) with number if discoverable
            let nikaya_fold = meta_map.get("nikaya").map(|s| fold_ascii(s));
            let mut alias_prefix: Option<&'static str> = None;
            if let Some(nf) = &nikaya_fold {
                if nf.contains("digha") { alias_prefix = Some("DN"); }
                else if nf.contains("majjhima") { alias_prefix = Some("MN"); }
                else if nf.contains("samyutta") || nf.contains("saṃyutta") || nf.contains("samyutta") { alias_prefix = Some("SN"); }
                else if nf.contains("anguttara") || nf.contains("agguttara") || nf.contains("aṅguttara") { alias_prefix = Some("AN"); }
                else if nf.contains("khuddaka") { alias_prefix = Some("KN"); }
            }
            if let Some(prefix) = alias_prefix {
                // try to find a number from book/title/subhead/chapter
                let mut num_str: Option<String> = None;
                for k in ["book","title","subhead","subsubhead","chapter"].iter() {
                    if let Some(v) = meta_map.get(*k) {
                        if let Some(ns) = first_number(v) { num_str = Some(ns); break; }
                    }
                }
                let mut aliases: Vec<String> = Vec::new();
                // always include the prefix itself
                aliases.push(prefix.to_string());
                aliases.push(prefix.to_lowercase());
                if let Some(ns) = num_str.clone() {
                    let n_trim = ns.trim_start_matches('0');
                    let n_trim = if n_trim.is_empty() { "0" } else { n_trim };
                    // numeric variants
                    let forms = vec![
                        format!("{}{}", prefix, n_trim),
                        format!("{}{:0>2}", prefix, ns),
                        format!("{}{:0>3}", prefix, ns),
                        format!("{} {}", prefix, n_trim),
                        format!("{} {:0>2}", prefix, ns),
                        format!("{} {:0>3}", prefix, ns),
                    ];
                    for f in forms { aliases.push(f.clone()); aliases.push(f.to_lowercase()); }
                    // roman numeral variant
                    if let Ok(n) = n_trim.parse::<usize>() {
                        let r = roman_upper(n);
                        let forms_r = vec![
                            format!("{} {}", prefix, r),
                            format!("{}{}", prefix, r),
                        ];
                        for f in forms_r { aliases.push(f.clone()); aliases.push(f.to_lowercase()); }
                    }
                }
                // Composite alias for SN/AN like "SN 12.2"
                if prefix == "SN" || prefix == "AN" {
                    if let Some((a, b)) = first_two_numbers_from_meta(&meta_map) {
                        let a_trim = a.trim_start_matches('0');
                        let a_trim = if a_trim.is_empty() { "0" } else { a_trim };
                        let b_trim = b.trim_start_matches('0');
                        let b_trim = if b_trim.is_empty() { "0" } else { b_trim };
                        let forms = vec![
                            format!("{} {}.{}", prefix, a_trim, b_trim),
                            format!("{}{}.{}", prefix, a_trim, b_trim),
                            format!("{} {:0>2}.{:0>2}", prefix, a, b),
                            format!("{}{:0>2}.{:0>2}", prefix, a, b),
                        ];
                        for f in forms { aliases.push(f.clone()); aliases.push(f.to_lowercase()); }
                        // Roman for first component optionally
                        if let Ok(na) = a_trim.parse::<usize>() {
                            let ra = roman_upper(na);
                            let forms_r = vec![
                                format!("{} {}.{}", prefix, ra, b_trim),
                                format!("{}{}.{}", prefix, ra, b_trim),
                            ];
                            for f in forms_r { aliases.push(f.clone()); aliases.push(f.to_lowercase()); }
                        }
                    }
                }
                meta_map.insert("alias".to_string(), aliases.join(" "));
                meta_map.insert("alias_prefix".to_string(), prefix.to_string());
            }
            if !heads.is_empty() {
                meta_map.insert("headsPreview".to_string(), heads.iter().take(10).cloned().collect::<Vec<_>>().join(" | "));
                // add alias variants from heads and title
                let mut alias_ext: Vec<String> = Vec::new();
                for s in heads.iter().take(6) {
                    alias_ext.extend(pali_title_variants(s));
                }
                alias_ext.extend(pali_title_variants(&title));
                if !alias_ext.is_empty() {
                    let prev = meta_map.remove("alias").unwrap_or_default();
                    let combined = if prev.is_empty() { alias_ext.join(" ") } else { format!("{} {}", prev, alias_ext.join(" ")) };
                    meta_map.insert("alias".to_string(), combined);
                }
            }
            let meta = if meta_map.is_empty() { None } else { Some(meta_map) };
            Some(IndexEntry { id, title, path: abs.to_string_lossy().to_string(), meta })
        })
        .collect()
}

fn fold_ascii(s: &str) -> String {
    let t: String = s.nfkd().collect::<String>().to_lowercase();
    t.chars().filter(|c| c.is_alphanumeric() || c.is_whitespace()).collect()
}

fn first_number(s: &str) -> Option<String> {
    let mut out = String::new();
    for ch in s.chars() {
        if ch.is_ascii_digit() { out.push(ch); }
        else if !out.is_empty() { break; }
    }
    if out.is_empty() { None } else { Some(out) }
}

fn roman_upper(mut n: usize) -> String {
    // simple roman numeral converter (1..3999)
    if n == 0 || n > 3999 { return n.to_string(); }
    let mut out = String::new();
    let vals = [(1000,"M"),(900,"CM"),(500,"D"),(400,"CD"),(100,"C"),(90,"XC"),(50,"L"),(40,"XL"),(10,"X"),(9,"IX"),(5,"V"),(4,"IV"),(1,"I")];
    for (v, s) in vals.iter() {
        while n >= *v { out.push_str(s); n -= *v; }
    }
    out
}

fn first_two_numbers_from_meta(meta: &BTreeMap<String, String>) -> Option<(String, String)> {
    let mut nums: Vec<String> = Vec::new();
    for k in ["book","title","subhead","subsubhead","chapter"].iter() {
        if let Some(v) = meta.get(*k) {
            let mut cur = String::new();
            for ch in v.chars() {
                if ch.is_ascii_digit() { cur.push(ch); }
                else {
                    if !cur.is_empty() { nums.push(cur.clone()); cur.clear(); }
                }
            }
            if !cur.is_empty() { nums.push(cur); }
            if nums.len() >= 2 { break; }
        }
    }
    if nums.len() >= 2 { Some((nums[0].clone(), nums[1].clone())) } else { None }
}

fn pali_title_variants(s: &str) -> Vec<String> {
    // generate normalized and ascii-vowel-doubling variants to help search recall
    let mut out: Vec<String> = Vec::new();
    let base = s.trim();
    if base.is_empty() { return out; }
    // plain
    out.push(base.to_string());
    // folded ascii (remove diacritics)
    out.push(fold_ascii(base));
    // double vowel transliteration
    let mut dbl = String::new();
    for ch in base.chars() {
        let repl = match ch {
            'ā' | 'Ā' => Some("aa"),
            'ī' | 'Ī' => Some("ii"),
            'ū' | 'Ū' => Some("uu"),
            'ṅ' | 'Ṅ' => Some("ng"),
            'ñ' | 'Ñ' => Some("ny"),
            'ṭ' | 'Ṭ' => Some("t"),
            'ḍ' | 'Ḍ' => Some("d"),
            'ṇ' | 'Ṇ' => Some("n"),
            'ḷ' | 'Ḷ' => Some("l"),
            'ṃ' | 'Ṃ' | 'ṁ' | 'Ṁ' => Some("m"),
            _ => None,
        };
        if let Some(r) = repl { dbl.push_str(r); } else { dbl.push(ch); }
    }
    out.push(dbl.to_lowercase());
    // also common keyword expansions
    out.push(base.replace("sutta", "suttanta"));
    out.push(fold_ascii(&out.last().cloned().unwrap_or_default()));
    out
}

pub fn extract_text(xml: &str) -> String {
    extract_text_opts(xml, false)
}

fn parse_gaiji_map(xml: &str) -> HashMap<String, String> {
    let mut map = HashMap::new();
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text_start = true;
    reader.config_mut().trim_text_end = true;
    let mut buf = Vec::new();
    let mut in_chardecl = false;
    let mut in_char = false;
    let mut current_id: Option<String> = None;
    let mut current_mapping_type: Option<String> = None;
    let mut current_val: Option<String> = None;
    let mut in_charname = false;
    let mut in_mapping = false;
    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e)) | Ok(Event::Empty(e)) => {
                let name_owned = e.name().as_ref().to_owned();
                let name = local_name(&name_owned);
                if name == b"charDecl" { in_chardecl = true; }
                else if in_chardecl && name == b"char" {
                    in_char = true;
                    current_id = attr_val(&e, b"xml:id").map(|v| v.to_string());
                    current_val = None;
                    current_mapping_type = None;
                } else if in_char && name == b"mapping" {
                    current_mapping_type = attr_val(&e, b"type").map(|v| v.to_string());
                    in_mapping = true;
                } else if in_char && name == b"charName" {
                    in_charname = true;
                }
            }
            Ok(Event::End(e)) => {
                let name_owned = e.name().as_ref().to_owned();
                let name = local_name(&name_owned);
                if name == b"charDecl" { in_chardecl = false; }
                if name == b"mapping" { in_mapping = false; }
                if name == b"charName" { in_charname = false; }
                if name == b"char" && in_char {
                    if let (Some(id), Some(v)) = (current_id.clone(), current_val.clone()) {
                        if !v.is_empty() { map.insert(id, v); }
                    }
                    in_char = false;
                    current_id = None;
                    current_val = None;
                    current_mapping_type = None;
                }
            }
            Ok(Event::Text(t)) => {
                let text = t.decode().unwrap_or_default().into_owned();
                if in_char && in_mapping && current_mapping_type.as_deref() == Some("unicode") {
                    if !text.trim().is_empty() { current_val = Some(text); }
                } else if in_char && in_mapping && current_mapping_type.as_deref() == Some("normal") {
                    if current_val.is_none() && !text.trim().is_empty() { current_val = Some(text); }
                } else if in_char && in_charname {
                    if current_val.is_none() && !text.trim().is_empty() { current_val = Some(text); }
                }
            }
            Ok(Event::Eof) => break,
            Err(_) => break,
            _ => {}
        }
        buf.clear();
    }
    map
}

pub fn extract_text_opts(xml: &str, include_notes: bool) -> String {
    let gaiji = parse_gaiji_map(xml);
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text_start = true;
    reader.config_mut().trim_text_end = true;
    let mut buf = Vec::new();
    let mut out = String::new();
    let mut skip_depth: usize = 0; // for excluding notes
    let mut collect_note: bool = false;
    let mut note_depth: usize = 0;
    let mut note_buf = String::new();
    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e)) => {
                let name_owned = e.name().as_ref().to_owned();
                let name = local_name(&name_owned);
                if name == b"note" {
                    if include_notes {
                        collect_note = true;
                        note_depth = 1;
                        note_buf.clear();
                    } else {
                        skip_depth = 1;
                    }
                } else if name == b"lb" {
                    if skip_depth == 0 && !collect_note { out.push('\n'); }
                } else if name == b"pb" {
                    if skip_depth == 0 && !collect_note { out.push('\n'); out.push('\n'); }
                } else if name == b"g" {
                    if skip_depth == 0 {
                        if let Some(r) = attr_val(&e, b"ref") {
                            let key = r.trim_start_matches('#').to_string();
                            if let Some(v) = gaiji.get(&key) { out.push_str(v); }
                        }
                    }
                }
            }
            Ok(Event::Empty(e)) => {
                let name_owned = e.name().as_ref().to_owned();
                let name = local_name(&name_owned);
                if name == b"lb" {
                    if skip_depth == 0 && !collect_note { out.push('\n'); }
                } else if name == b"pb" {
                    if skip_depth == 0 && !collect_note { out.push('\n'); out.push('\n'); }
                } else if name == b"g" {
                    if skip_depth == 0 {
                        if let Some(r) = attr_val(&e, b"ref") {
                            let key = r.trim_start_matches('#').to_string();
                            if let Some(v) = gaiji.get(&key) { out.push_str(v); }
                        }
                    }
                } else if name == b"note" {
                    // empty note
                    if include_notes && !collect_note && skip_depth == 0 {
                        // nothing
                    }
                }
            }
            Ok(Event::End(e)) => {
                let name_owned = e.name().as_ref().to_owned();
                let name = local_name(&name_owned);
                if name == b"note" {
                    if skip_depth > 0 { if skip_depth > 0 { skip_depth -= 1; } }
                    if collect_note {
                        note_depth = note_depth.saturating_sub(1);
                        if note_depth == 0 {
                            let t = note_buf.split_whitespace().collect::<Vec<_>>().join(" ");
                            if !t.is_empty() { out.push_str(" [注] "); out.push_str(&t); out.push(' '); }
                            collect_note = false;
                            note_buf.clear();
                        }
                    }
                } else if collect_note {
                    if note_depth > 0 { note_depth -= 1; }
                } else if skip_depth > 0 {
                    skip_depth -= 1;
                }
            }
            Ok(Event::Text(t)) => {
                let text = t.decode().unwrap_or_default().into_owned();
                if collect_note {
                    note_buf.push_str(&text);
                } else if skip_depth == 0 {
                    out.push_str(&text);
                }
            }
            Ok(Event::CData(t)) => {
                let text = String::from_utf8_lossy(&t).into_owned();
                if collect_note { note_buf.push_str(&text); } else if skip_depth == 0 { out.push_str(&text); }
            }
            Ok(Event::Eof) => break,
            Err(_) => break,
            _ => {}
        }
        buf.clear();
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

pub fn extract_cbeta_juan(xml: &str, part: &str) -> Option<String> {
    let gaiji = parse_gaiji_map(xml);
    let target_n1 = part.to_string();
    let target_n2 = format!("{:0>3}", part);
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text_start = true;
    reader.config_mut().trim_text_end = true;
    let mut buf = Vec::new();
    let mut capturing = false;
    let mut out = String::new();
    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e)) | Ok(Event::Empty(e)) => {
                let name_owned = e.name().as_ref().to_owned();
                let name = local_name(&name_owned);
                if name == b"juan" {
                    let fun = attr_val(&e, b"fun").map(|v| v.to_ascii_lowercase());
                    let n = attr_val(&e, b"n").unwrap_or(Cow::Borrowed(""));
                    if !capturing {
                        if (n == target_n1 || n == target_n2) && (fun.as_deref() == Some("open") || fun.is_none()) {
                            capturing = true;
                        }
                    } else {
                        if fun.as_deref() == Some("close") {
                            break;
                        }
                    }
                } else if capturing {
                    if name == b"lb" { out.push('\n'); }
                    else if name == b"pb" { out.push('\n'); out.push('\n'); }
                    else if name == b"g" {
                        if let Some(r) = attr_val(&e, b"ref") {
                            let key = r.trim_start_matches('#').to_string();
                            if let Some(v) = gaiji.get(&key) { out.push_str(v); }
                        }
                    }
                }
            }
            Ok(Event::End(_)) => {}
            Ok(Event::Text(t)) => {
                if capturing { out.push_str(&t.decode().unwrap_or_default()); }
            }
            Ok(Event::CData(t)) => {
                if capturing { out.push_str(&String::from_utf8_lossy(&t)); }
            }
            Ok(Event::Eof) => break,
            Err(_) => break,
            _ => {}
        }
        buf.clear();
    }
    if out.is_empty() { None } else { Some(out.split_whitespace().collect::<Vec<_>>().join(" ")) }
}

pub fn list_heads_cbeta(xml: &str) -> Vec<String> {
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text_start = true;
    reader.config_mut().trim_text_end = true;
    let mut buf = Vec::new();
    let mut heads: Vec<String> = Vec::new();
    let mut in_head = false;
    let mut head_buf = String::new();
    let mut path_stack: Vec<Vec<u8>> = Vec::new();
    let mut in_jhead_title = false;
    let mut jhead_buf = String::new();
    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e)) => {
                let name_owned = e.name().as_ref().to_owned();
                let lname = local_name(&name_owned).to_vec();
                path_stack.push(lname.clone());
                if lname.as_slice() == b"head" {
                    in_head = true; head_buf.clear();
                }
                if lname.as_slice() == b"title" {
                    if path_stack.iter().any(|n| n.as_slice() == b"jhead") {
                        in_jhead_title = true; jhead_buf.clear();
                    }
                }
            }
            Ok(Event::End(e)) => {
                let name_owned = e.name().as_ref().to_owned();
                let lname = local_name(&name_owned);
                if lname == b"head" && in_head {
                    let t = head_buf.split_whitespace().collect::<Vec<_>>().join(" ");
                    if !t.is_empty() { heads.push(t); }
                    in_head = false; head_buf.clear();
                }
                if lname == b"title" && in_jhead_title {
                    let t = jhead_buf.split_whitespace().collect::<Vec<_>>().join(" ");
                    if !t.is_empty() { heads.push(t); }
                    in_jhead_title = false; jhead_buf.clear();
                }
                path_stack.pop();
            }
            Ok(Event::Text(t)) => {
                let tx = t.decode().unwrap_or_default();
                if in_head { head_buf.push_str(&tx); }
                if in_jhead_title { jhead_buf.push_str(&tx); }
            }
            Ok(Event::Eof) => break,
            Err(_) => break,
            _ => {}
        }
        buf.clear();
    }
    heads
}

pub fn list_heads_generic(xml: &str) -> Vec<String> {
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text_start = true;
    reader.config_mut().trim_text_end = true;
    let mut buf = Vec::new();
    let mut heads: Vec<String> = Vec::new();
    let mut in_head = false;
    let mut head_buf = String::new();
    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e)) => {
                let name_owned = e.name().as_ref().to_owned();
                if local_name(&name_owned) == b"head" { in_head = true; head_buf.clear(); }
            }
            Ok(Event::End(e)) => {
                let name_owned = e.name().as_ref().to_owned();
                if local_name(&name_owned) == b"head" && in_head {
                    let t = head_buf.split_whitespace().collect::<Vec<_>>().join(" ");
                    if !t.is_empty() { heads.push(t); }
                    in_head = false; head_buf.clear();
                }
            }
            Ok(Event::Text(t)) => { if in_head { head_buf.push_str(&t.decode().unwrap_or_default()); } }
            Ok(Event::Eof) => break,
            Err(_) => break,
            _ => {}
        }
        buf.clear();
    }
    heads
}

pub fn strip_tags(s: &str) -> String {
    // For external callers that still use it, provide a simple whitespace normalize
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[derive(Serialize, Debug, Clone)]
pub struct GrepResult {
    pub file_path: String,
    pub file_id: String,
    pub title: String,
    pub matches: Vec<GrepMatch>,
    pub total_matches: usize,
    pub fetch_hints: FetchHints,
}

#[derive(Serialize, Debug, Clone)]
pub struct GrepMatch {
    pub context: String,
    pub highlight: String,
    pub juan_number: Option<String>,  // CBETA用
    pub section: Option<String>,      // 構造情報
    pub line_number: Option<usize>,   // マッチした行番号
}

#[derive(Serialize, Debug, Clone)]
pub struct FetchHints {
    pub recommended_parts: Vec<String>,
    pub total_content_size: Option<String>,
    pub structure_info: Vec<String>,
}

fn search_index(entries: &[IndexEntry], q: &str, limit: usize) -> Vec<IndexEntry> {
    // best_match関数を使って検索し、IndexEntryのベクトルとして返す
    use unicode_normalization::UnicodeNormalization;
    
    let normalized = |s: &str| -> String {
        s.nfc().collect::<String>().to_lowercase().chars()
            .filter(|c| !c.is_whitespace() && !c.is_ascii_punctuation())
            .collect()
    };
    
    let jaccard = |a: &str, b: &str| -> f32 {
        let sa: std::collections::HashSet<_> = a.chars().collect();
        let sb: std::collections::HashSet<_> = b.chars().collect();
        if sa.is_empty() || sb.is_empty() { return 0.0; }
        let inter = sa.intersection(&sb).count() as f32;
        let uni = (sa.len() + sb.len()).saturating_sub(inter as usize) as f32;
        if uni == 0.0 { 0.0 } else { inter / uni }
    };
    
    let tokenset = |s: &str| -> std::collections::HashSet<String> {
        s.split_whitespace().map(|w| normalized(w)).filter(|w| !w.is_empty()).collect()
    };
    
    let token_jaccard = |a: &str, b: &str| -> f32 {
        let sa: std::collections::HashSet<_> = tokenset(a);
        let sb: std::collections::HashSet<_> = tokenset(b);
        if sa.is_empty() || sb.is_empty() { return 0.0; }
        let inter = sa.intersection(&sb).count() as f32;
        let uni = (sa.len() + sb.len()).saturating_sub(inter as usize) as f32;
        if uni == 0.0 { 0.0 } else { inter / uni }
    };
    
    let nq = normalized(q);
    let mut scored: Vec<(f32, &IndexEntry)> = entries.iter().map(|e| {
        let meta_str = e.meta.as_ref().map(|m| m.values().cloned().collect::<Vec<_>>().join(" ")).unwrap_or_default();
        let hay_all = format!("{} {} {}", e.title, e.id, meta_str);
        let hay = normalized(&hay_all);
        let mut score = if hay.contains(&nq) { 1.0f32 } else {
            let s_char = jaccard(&hay, &nq);
            let s_tok = token_jaccard(&hay_all, q);
            s_char.max(s_tok)
        };
        
        // ID完全一致ボーナス
        if e.id.to_lowercase() == q.to_lowercase() { score = 1.1; }
        
        (score, e)
    }).collect();
    
    scored.sort_by(|a,b| b.0.partial_cmp(&a.0).unwrap());
    scored.into_iter()
        .take(limit)
        .filter(|(s, _)| *s > 0.1) // 最低スコア閾値
        .map(|(_, e)| e.clone())
        .collect()
}

pub fn cbeta_grep(root: &Path, query: &str, max_results: usize, max_matches_per_file: usize) -> Vec<GrepResult> {
    // 1. まずTフォルダから優先的に検索
    let t_folder = root.join("T");
    let mut all_results = Vec::new();
    
    if t_folder.exists() {
        let t_results = cbeta_grep_internal(&t_folder, query, max_results, max_matches_per_file);
        all_results.extend(t_results);
    }
    
    // 2. まだ結果が不足している場合は、他のフォルダも検索
    if all_results.len() < max_results {
        let remaining_limit = max_results - all_results.len();
        let other_results = cbeta_grep_internal_exclude_t(root, query, remaining_limit, max_matches_per_file);
        all_results.extend(other_results);
    }
    
    // 3. タイトル検索を実行して、マッチしたものがあれば上位に移動
    let index = build_cbeta_index(root);
    let title_results = search_index(&index, query, max_results);
    
    if !title_results.is_empty() {
        // タイトル検索結果をIDの集合に変換
        let title_ids: std::collections::HashSet<_> = title_results.iter().map(|t| &t.id).collect();
        
        // grep結果をタイトルマッチ優先でソート
        all_results.sort_by(|a, b| {
            let a_in_title = title_ids.contains(&a.file_id);
            let b_in_title = title_ids.contains(&b.file_id);
            
            match (a_in_title, b_in_title) {
                (true, false) => std::cmp::Ordering::Less,   // aがタイトルマッチ → 先に
                (false, true) => std::cmp::Ordering::Greater, // bがタイトルマッチ → 先に
                _ => {
                    // 両方タイトルマッチまたは両方非マッチの場合、T系列優先
                    let a_is_t = a.file_id.starts_with('T');
                    let b_is_t = b.file_id.starts_with('T');
                    
                    match (a_is_t, b_is_t) {
                        (true, false) => std::cmp::Ordering::Less,
                        (false, true) => std::cmp::Ordering::Greater,
                        _ => a.file_id.cmp(&b.file_id),
                    }
                }
            }
        });
    }
    
    all_results.truncate(max_results);
    all_results
}

fn cbeta_grep_internal(root: &Path, query: &str, max_results: usize, max_matches_per_file: usize) -> Vec<GrepResult> {
    use regex::RegexBuilder;
    
    let re = match RegexBuilder::new(query)
        .case_insensitive(true)
        .multi_line(true)
        .build() 
    {
        Ok(r) => r,
        Err(_) => return Vec::new(),
    };
    
    let mut paths: Vec<PathBuf> = Vec::new();
    for e in WalkDir::new(root).into_iter().filter_map(|e| e.ok()) {
        if e.file_type().is_file() {
            if let Some(name) = e.path().file_name().and_then(|s| s.to_str()) {
                if name.ends_with(".xml") { 
                    paths.push(e.into_path()); 
                }
            }
        }
    }

    paths
        .par_iter()
        .filter_map(|p| {
            let content = std::fs::read_to_string(p).ok()?;
            let matches: Vec<_> = re.find_iter(&content).collect();
            
            if matches.is_empty() {
                return None;
            }

            let mut grep_matches = Vec::new();
            let mut juan_info = Vec::new();
            
            // Juan情報の抽出（高速化のため制限付き）
            let mut reader = Reader::from_str(&content);
            reader.config_mut().trim_text_start = true;
            reader.config_mut().trim_text_end = true;
            let mut buf = Vec::new();
            let mut events = 0;
            
            loop {
                if events > 5000 { break; }
                match reader.read_event_into(&mut buf) {
                    Ok(Event::Start(e)) | Ok(Event::Empty(e)) => {
                        let name_owned = e.name().as_ref().to_owned();
                        let name = local_name(&name_owned);
                        if name == b"juan" {
                            if let Some(n) = attr_val(&e, b"n") {
                                juan_info.push(n.to_string());
                            }
                        }
                    }
                    Ok(Event::Eof) => break,
                    Err(_) => break,
                    _ => {}
                }
                buf.clear();
                events += 1;
            }
            
            // マッチ箇所の文脈抽出
            for mat in matches.iter().take(max_matches_per_file) {
                let start = mat.start();
                let end = mat.end();
                
                // 行数を計算
                let line_number = Some(content[..start].lines().count());
                
                // 文字境界を考慮した安全なスライシング
                let context_start = start.saturating_sub(100);
                let context_end = std::cmp::min(end + 100, content.len());
                
                // 文字境界を見つける
                let safe_start = content.char_indices()
                    .find(|(i, _)| *i >= context_start)
                    .map(|(i, _)| i)
                    .unwrap_or(context_start);
                let safe_end = content.char_indices()
                    .find(|(i, _)| *i >= context_end)
                    .map(|(i, _)| i)
                    .unwrap_or(content.len());
                
                let context = if safe_start < safe_end {
                    content[safe_start..safe_end]
                        .split_whitespace()
                        .collect::<Vec<_>>()
                        .join(" ")
                } else {
                    String::new()
                };
                
                // ハイライト部分も文字境界を考慮
                let highlight_start = content.char_indices()
                    .find(|(i, _)| *i >= start)
                    .map(|(i, _)| i)
                    .unwrap_or(start);
                let highlight_end = content.char_indices()
                    .find(|(i, _)| *i >= end)
                    .map(|(i, _)| i)
                    .unwrap_or(content.len());
                
                let highlight = if highlight_start < highlight_end {
                    content[highlight_start..highlight_end].to_string()
                } else {
                    String::new()
                };
                
                grep_matches.push(GrepMatch {
                    context,
                    highlight,
                    juan_number: juan_info.first().cloned(),
                    section: None,
                    line_number,
                });
            }
            
            let file_id = stem_from(p);
            let title = file_id.clone(); // 簡易タイトル
            
            // Fetch用ヒント
            let fetch_hints = FetchHints {
                recommended_parts: juan_info.clone(),
                total_content_size: Some(format!("{}KB", content.len() / 1024)),
                structure_info: vec![format!("{}個のjuan", juan_info.len())],
            };
            
            Some(GrepResult {
                file_path: p.to_string_lossy().to_string(),
                file_id,
                title,
                matches: grep_matches,
                total_matches: matches.len(),
                fetch_hints,
            })
        })
        .collect::<Vec<_>>()
        .into_iter()
        .take(max_results)
        .collect()
}

fn cbeta_grep_internal_exclude_t(root: &Path, query: &str, max_results: usize, max_matches_per_file: usize) -> Vec<GrepResult> {
    use regex::RegexBuilder;
    
    let re = match RegexBuilder::new(query)
        .case_insensitive(true)
        .multi_line(true)
        .build() 
    {
        Ok(r) => r,
        Err(_) => return Vec::new(),
    };
    
    let mut paths: Vec<PathBuf> = Vec::new();
    for e in WalkDir::new(root).into_iter().filter_map(|e| e.ok()) {
        if e.file_type().is_file() {
            if let Some(name) = e.path().file_name().and_then(|s| s.to_str()) {
                // Tフォルダを除外し、XMLファイルのみ対象
                if name.ends_with(".xml") && !e.path().to_string_lossy().contains("/T/") { 
                    paths.push(e.into_path()); 
                }
            }
        }
    }

    paths
        .par_iter()
        .filter_map(|p| {
            let content = std::fs::read_to_string(p).ok()?;
            let matches: Vec<_> = re.find_iter(&content).collect();
            
            if matches.is_empty() {
                return None;
            }

            let mut grep_matches = Vec::new();
            let mut juan_info = Vec::new();
            
            // Juan情報の抽出（高速化のため制限付き）
            let mut reader = Reader::from_str(&content);
            reader.config_mut().trim_text_start = true;
            reader.config_mut().trim_text_end = true;
            let mut buf = Vec::new();
            let mut events = 0;
            
            loop {
                if events > 5000 { break; }
                match reader.read_event_into(&mut buf) {
                    Ok(Event::Start(e)) | Ok(Event::Empty(e)) => {
                        let name_owned = e.name().as_ref().to_owned();
                        let name = local_name(&name_owned);
                        if name == b"juan" {
                            if let Some(n) = attr_val(&e, b"n") {
                                juan_info.push(n.to_string());
                            }
                        }
                    }
                    Ok(Event::Eof) => break,
                    Err(_) => break,
                    _ => {}
                }
                buf.clear();
                events += 1;
            }
            
            // マッチ箇所の文脈抽出
            for mat in matches.iter().take(max_matches_per_file) {
                let start = mat.start();
                let end = mat.end();
                
                // 行数を計算
                let line_number = Some(content[..start].lines().count());
                
                // 文字境界を考慮した安全なスライシング
                let context_start = start.saturating_sub(100);
                let context_end = std::cmp::min(end + 100, content.len());
                
                // 文字境界を見つける
                let safe_start = content.char_indices()
                    .find(|(i, _)| *i >= context_start)
                    .map(|(i, _)| i)
                    .unwrap_or(context_start);
                let safe_end = content.char_indices()
                    .find(|(i, _)| *i >= context_end)
                    .map(|(i, _)| i)
                    .unwrap_or(content.len());
                
                let context = if safe_start < safe_end {
                    content[safe_start..safe_end]
                        .split_whitespace()
                        .collect::<Vec<_>>()
                        .join(" ")
                } else {
                    String::new()
                };
                
                // ハイライト部分も文字境界を考慮
                let highlight_start = content.char_indices()
                    .find(|(i, _)| *i >= start)
                    .map(|(i, _)| i)
                    .unwrap_or(start);
                let highlight_end = content.char_indices()
                    .find(|(i, _)| *i >= end)
                    .map(|(i, _)| i)
                    .unwrap_or(content.len());
                
                let highlight = if highlight_start < highlight_end {
                    content[highlight_start..highlight_end].to_string()
                } else {
                    String::new()
                };
                
                grep_matches.push(GrepMatch {
                    context,
                    highlight,
                    juan_number: juan_info.first().cloned(),
                    section: None,
                    line_number,
                });
            }
            
            let file_id = stem_from(p);
            let title = file_id.clone(); // 簡易タイトル
            
            // Fetch用ヒント
            let fetch_hints = FetchHints {
                recommended_parts: juan_info.clone(),
                total_content_size: Some(format!("{}KB", content.len() / 1024)),
                structure_info: vec![format!("{}個のjuan", juan_info.len())],
            };
            
            Some(GrepResult {
                file_path: p.to_string_lossy().to_string(),
                file_id,
                title,
                matches: grep_matches,
                total_matches: matches.len(),
                fetch_hints,
            })
        })
        .collect::<Vec<_>>()
        .into_iter()
        .take(max_results)
        .collect()
}

pub fn tipitaka_grep(root: &Path, query: &str, max_results: usize, max_matches_per_file: usize) -> Vec<GrepResult> {
    use regex::RegexBuilder;
    
    let re = match RegexBuilder::new(query)
        .case_insensitive(true)
        .multi_line(true)
        .build() 
    {
        Ok(r) => r,
        Err(_) => return Vec::new(),
    };
    
    let mut paths: Vec<PathBuf> = Vec::new();
    for e in WalkDir::new(root).into_iter().filter_map(|e| e.ok()) {
        if e.file_type().is_file() {
            if let Some(name) = e.path().file_name().and_then(|s| s.to_str()) {
                if name.ends_with(".xml") 
                    && !name.contains("toc") 
                    && !name.contains("sitemap") 
                {
                    paths.push(e.into_path());
                }
            }
        }
    }

    paths
        .par_iter()
        .filter_map(|p| {
            // UTF-16対応の読み込み
            let content = match std::fs::read(p) {
                Ok(bytes) => {
                    if bytes.starts_with(&[0xFF, 0xFE]) {
                        match encoding_rs::UTF_16LE.decode(&bytes) {
                            (decoded, _, false) => decoded.into_owned(),
                            _ => return None,
                        }
                    } else if bytes.starts_with(&[0xFE, 0xFF]) {
                        match encoding_rs::UTF_16BE.decode(&bytes) {
                            (decoded, _, false) => decoded.into_owned(),
                            _ => return None,
                        }
                    } else {
                        match String::from_utf8(bytes) {
                            Ok(s) => s,
                            Err(_) => return None,
                        }
                    }
                },
                Err(_) => return None,
            };
            
            let matches: Vec<_> = re.find_iter(&content).collect();
            
            if matches.is_empty() {
                return None;
            }

            let mut grep_matches = Vec::new();
            let mut structure_info = Vec::new();
            
            // 構造情報の高速抽出
            let mut reader = Reader::from_str(&content);
            reader.config_mut().trim_text_start = true;
            reader.config_mut().trim_text_end = true;
            let mut buf = Vec::new();
            let mut events = 0;
            let mut nikaya = None;
            let mut book = None;
            
            loop {
                if events > 5000 { break; }
                match reader.read_event_into(&mut buf) {
                    Ok(Event::Start(e)) | Ok(Event::Empty(e)) => {
                        let name_owned = e.name().as_ref().to_owned();
                        let name = local_name(&name_owned);
                        if name == b"p" {
                            if let Some(rend) = attr_val(&e, b"rend") {
                                let rend_str = rend.to_ascii_lowercase();
                                if rend_str == "nikaya" && nikaya.is_none() {
                                    // 次のテキストを取得
                                }
                            }
                        } else if name == b"head" {
                            if let Some(rend) = attr_val(&e, b"rend") {
                                let rend_str = rend.to_ascii_lowercase();
                                if rend_str == "book" && book.is_none() {
                                    // 次のテキストを取得
                                }
                            }
                        } else if name == b"div" {
                            if let (Some(n), Some(type_val)) = (attr_val(&e, b"n"), attr_val(&e, b"type")) {
                                structure_info.push(format!("{}({})", n, type_val));
                            }
                        }
                    }
                    Ok(Event::Text(t)) => {
                        let text = t.decode().unwrap_or_default().into_owned();
                        if nikaya.is_none() && text.trim().len() > 5 {
                            nikaya = Some(text.trim().to_string());
                        } else if book.is_none() && text.trim().len() > 3 {
                            book = Some(text.trim().to_string());
                        }
                    }
                    Ok(Event::Eof) => break,
                    Err(_) => break,
                    _ => {}
                }
                buf.clear();
                events += 1;
            }
            
            // マッチ箇所の文脈抽出
            for mat in matches.iter().take(max_matches_per_file) {
                let start = mat.start();
                let end = mat.end();
                
                // 行数を計算
                let line_number = Some(content[..start].lines().count());
                
                // 文字境界を考慮した安全なスライシング
                let context_start = start.saturating_sub(150);
                let context_end = std::cmp::min(end + 150, content.len());
                
                // 文字境界を見つける
                let safe_start = content.char_indices()
                    .find(|(i, _)| *i >= context_start)
                    .map(|(i, _)| i)
                    .unwrap_or(context_start);
                let safe_end = content.char_indices()
                    .find(|(i, _)| *i >= context_end)
                    .map(|(i, _)| i)
                    .unwrap_or(content.len());
                
                let context = if safe_start < safe_end {
                    content[safe_start..safe_end]
                        .split_whitespace()
                        .collect::<Vec<_>>()
                        .join(" ")
                } else {
                    String::new()
                };
                
                // ハイライト部分も文字境界を考慮
                let highlight_start = content.char_indices()
                    .find(|(i, _)| *i >= start)
                    .map(|(i, _)| i)
                    .unwrap_or(start);
                let highlight_end = content.char_indices()
                    .find(|(i, _)| *i >= end)
                    .map(|(i, _)| i)
                    .unwrap_or(content.len());
                
                let highlight = if highlight_start < highlight_end {
                    content[highlight_start..highlight_end].to_string()
                } else {
                    String::new()
                };
                
                grep_matches.push(GrepMatch {
                    context,
                    highlight,
                    juan_number: None,
                    section: structure_info.first().cloned(),
                    line_number,
                });
            }
            
            let file_id = stem_from(p);
            let title = [nikaya.as_deref(), book.as_deref()]
                .iter()
                .filter_map(|&s| s)
                .collect::<Vec<_>>()
                .join(" · ");
            let title = if title.is_empty() { file_id.clone() } else { title };
            
            // Fetch用ヒント
            let fetch_hints = FetchHints {
                recommended_parts: vec!["full".to_string()], // Tipitakaは通常全体を取得
                total_content_size: Some(format!("{}KB", content.len() / 1024)),
                structure_info,
            };
            
            Some(GrepResult {
                file_path: p.to_string_lossy().to_string(),
                file_id,
                title,
                matches: grep_matches,
                total_matches: matches.len(),
                fetch_hints,
            })
        })
        .collect::<Vec<_>>()
        .into_iter()
        .take(max_results)
        .collect()
}

mod lib_line_extraction;
pub use lib_line_extraction::*;
