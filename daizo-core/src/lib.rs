use quick_xml::events::{BytesStart, Event};
use quick_xml::Reader;
use rayon::prelude::*;
use serde::Serialize;
use std::borrow::Cow;
use std::collections::BTreeMap;
use std::collections::HashMap;
use std::fs::File;
use std::io::BufReader;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use unicode_normalization::UnicodeNormalization;

use encoding_rs;
use grep_matcher::Matcher;
use grep_regex::RegexMatcherBuilder;
use grep_searcher::sinks::UTF8;
use grep_searcher::{BinaryDetection, SearcherBuilder};
use ignore::WalkBuilder;
use serde::Deserialize;

pub mod path_resolver;
pub mod repo;
pub mod text_utils;

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

/// Collect XML file paths using ignore crate (fd-style fast walker)
fn collect_xml_paths(root: &Path, filter: impl Fn(&Path, &str) -> bool + Sync) -> Vec<PathBuf> {
    let paths = Mutex::new(Vec::new());
    WalkBuilder::new(root)
        .hidden(false) // Include hidden files (XML repos may have them)
        .git_ignore(false) // Don't use .gitignore for data files
        .git_global(false)
        .git_exclude(false)
        .build_parallel()
        .run(|| {
            Box::new(|entry| {
                if let Ok(e) = entry {
                    if e.file_type().is_some_and(|ft| ft.is_file()) {
                        if let Some(name) = e.path().file_name().and_then(|s| s.to_str()) {
                            if filter(e.path(), name) {
                                paths.lock().unwrap().push(e.into_path());
                            }
                        }
                    }
                }
                ignore::WalkState::Continue
            })
        });
    let mut out = paths.into_inner().unwrap();
    // Deterministic ordering; avoid run-to-run variation when callers later apply limits.
    out.sort();
    out
}

fn collect_xml_paths_cached(
    cache: &'static OnceLock<Mutex<HashMap<PathBuf, Arc<Vec<PathBuf>>>>>,
    root: &Path,
    filter: impl Fn(&Path, &str) -> bool + Sync,
) -> Arc<Vec<PathBuf>> {
    let key = root.to_path_buf();
    let m = cache.get_or_init(|| Mutex::new(HashMap::new()));
    if let Some(v) = m.lock().unwrap().get(&key).cloned() {
        return v;
    }
    // Build outside the lock; walking the tree can be expensive.
    let paths = collect_xml_paths(root, filter);
    let arc = Arc::new(paths);
    m.lock().unwrap().insert(key, arc.clone());
    arc
}

static XML_PATHS_ALL_CACHE: OnceLock<Mutex<HashMap<PathBuf, Arc<Vec<PathBuf>>>>> = OnceLock::new();
static CBETA_XML_PATHS_EXCLUDE_T_CACHE: OnceLock<Mutex<HashMap<PathBuf, Arc<Vec<PathBuf>>>>> =
    OnceLock::new();
static TIPITAKA_XML_PATHS_CACHE: OnceLock<Mutex<HashMap<PathBuf, Arc<Vec<PathBuf>>>>> =
    OnceLock::new();
static SARIT_XML_PATHS_CACHE: OnceLock<Mutex<HashMap<PathBuf, Arc<Vec<PathBuf>>>>> =
    OnceLock::new();

fn is_sarit_xml(path: &Path, name: &str) -> bool {
    if !name.ends_with(".xml") {
        return false;
    }
    // Exclude non-text artifacts / schemas / tool files in the SARIT repo.
    let p = path.to_string_lossy();
    for bad in ["/.git/", "/out/", "/schemas/", "/tools/"].iter() {
        if p.contains(bad) {
            return false;
        }
    }
    // Exclude the header template file(s) if present.
    if name.contains("tei-header-template") || name.starts_with("00-sarit-tei-header") {
        return false;
    }
    true
}

pub fn build_index(root: &Path, glob_hint: Option<&str>) -> Vec<IndexEntry> {
    let hint = glob_hint.map(|s| s.to_string());
    let paths = collect_xml_paths(root, |path, name| {
        if !name.ends_with(".xml") {
            return false;
        }
        if let Some(ref h) = hint {
            if !path.to_string_lossy().contains(h) {
                return false;
            }
        }
        true
    });

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
                            if let Some(v) = attr_val(&e, b"xml:id") {
                                id = Some(v.to_string());
                            }
                        }
                        // stack push for fallback scanning
                        path_stack.push(name.to_vec());

                        if name == b"teiHeader" {
                            in_header = true;
                        }
                        if in_header && name == b"title" {
                            in_title = true;
                        }

                        // fallback: head or jhead/title
                        if name == b"head" {
                            in_head = true;
                            head_buf.clear();
                        }
                        if name == b"title" {
                            if path_stack.iter().any(|n| n.as_slice() == b"jhead") {
                                in_jhead_title = true;
                                jhead_buf.clear();
                            }
                        }
                    }
                    Ok(Event::End(e)) => {
                        let name_owned = e.name().as_ref().to_owned();
                        let name = local_name(&name_owned);
                        if name == b"title" {
                            in_title = false;
                            in_jhead_title = false;
                        }
                        if name == b"head" && in_head {
                            if fallback_title.is_none() {
                                let t = head_buf.split_whitespace().collect::<Vec<_>>().join(" ");
                                if !t.is_empty() {
                                    fallback_title = Some(t);
                                }
                            }
                            in_head = false;
                            head_buf.clear();
                        }
                        if name == b"teiHeader" {
                            // do not break; continue to allow fallback scanning in body if no title yet
                            // only early-stop if we already have a header title
                            if title.is_some() {
                                break;
                            }
                        }
                        path_stack.pop();
                    }
                    Ok(Event::Text(t)) => {
                        if in_title {
                            let t = t.decode().unwrap_or_default().into_owned();
                            if !t.trim().is_empty() {
                                title = Some(t);
                            }
                        }
                        // fallback buffers
                        let tx = t.decode().unwrap_or_default();
                        if in_head {
                            head_buf.push_str(&tx);
                        }
                        if in_jhead_title {
                            jhead_buf.push_str(&tx);
                        }
                    }
                    Ok(Event::Eof) => break,
                    Err(_) => break,
                    _ => {}
                }
                buf.clear();
                if title.is_some() && id.is_some() {
                    break;
                }
                // consider jhead/title as candidate if not set yet
                if fallback_title.is_none() && !jhead_buf.trim().is_empty() {
                    let t = jhead_buf.split_whitespace().collect::<Vec<_>>().join(" ");
                    if !t.is_empty() {
                        fallback_title = Some(t);
                    }
                }
            }
            let id = id.unwrap_or_else(|| stem_from(p));
            let title = title
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .or(fallback_title)
                .unwrap_or_else(|| stem_from(p));
            let abs = std::fs::canonicalize(p).unwrap_or_else(|_| p.clone());
            Some(IndexEntry {
                id,
                title,
                path: abs.to_string_lossy().to_string(),
                meta: None,
            })
        })
        .collect()
}

/// SARIT 用: リポジトリ内の TEI P5 テキストをインデックス化（スキーマ/生成物を除外）。
/// ID はファイル stem を採用（xml:id は揺れがあるため）。
pub fn build_sarit_index(root: &Path) -> Vec<IndexEntry> {
    let paths = collect_xml_paths(root, |path, name| is_sarit_xml(path, name));

    paths
        .par_iter()
        .filter_map(|p| {
            let f = File::open(p).ok()?;
            let mut reader = Reader::from_reader(BufReader::new(f));
            reader.config_mut().trim_text_start = true;
            reader.config_mut().trim_text_end = true;
            let mut buf = Vec::new();

            let mut xml_id: Option<String> = None;
            let mut titles: Vec<(bool, String)> = Vec::new(); // (is_main, text)
            let mut author: Option<String> = None;
            let mut editor: Option<String> = None;

            let mut path_stack: Vec<Vec<u8>> = Vec::new();
            let mut in_title = false;
            let mut cur_title_is_main = false;
            let mut title_buf = String::new();
            let mut in_author = false;
            let mut author_buf = String::new();
            let mut in_editor = false;
            let mut editor_buf = String::new();

            let mut events = 0usize;
            let max_events = 80_000usize;

            loop {
                if events >= max_events {
                    break;
                }
                match reader.read_event_into(&mut buf) {
                    Ok(Event::Start(e)) | Ok(Event::Empty(e)) => {
                        let name_owned = e.name().as_ref().to_owned();
                        let lname = local_name(&name_owned).to_vec();
                        if xml_id.is_none() {
                            if let Some(v) = attr_val(&e, b"xml:id") {
                                xml_id = Some(v.to_string());
                            }
                        }
                        path_stack.push(lname.clone());

                        if lname.as_slice() == b"title"
                            && path_stack.iter().any(|n| n.as_slice() == b"titleStmt")
                            && path_stack.iter().any(|n| n.as_slice() == b"teiHeader")
                        {
                            in_title = true;
                            title_buf.clear();
                            cur_title_is_main = attr_val(&e, b"type")
                                .map(|v| v.to_ascii_lowercase().contains("main"))
                                .unwrap_or(false);
                        }
                        if lname.as_slice() == b"author"
                            && path_stack.iter().any(|n| n.as_slice() == b"titleStmt")
                            && author.is_none()
                        {
                            in_author = true;
                            author_buf.clear();
                        }
                        if lname.as_slice() == b"editor"
                            && path_stack.iter().any(|n| n.as_slice() == b"titleStmt")
                            && editor.is_none()
                        {
                            in_editor = true;
                            editor_buf.clear();
                        }
                    }
                    Ok(Event::End(e)) => {
                        let name_owned = e.name().as_ref().to_owned();
                        let lname = local_name(&name_owned);
                        if lname == b"title" && in_title {
                            let t = title_buf.split_whitespace().collect::<Vec<_>>().join(" ");
                            if !t.is_empty() {
                                titles.push((cur_title_is_main, t));
                            }
                            in_title = false;
                            cur_title_is_main = false;
                            title_buf.clear();
                        }
                        if lname == b"author" && in_author {
                            let t = author_buf.split_whitespace().collect::<Vec<_>>().join(" ");
                            if !t.is_empty() {
                                author = Some(t);
                            }
                            in_author = false;
                            author_buf.clear();
                        }
                        if lname == b"editor" && in_editor {
                            let t = editor_buf.split_whitespace().collect::<Vec<_>>().join(" ");
                            if !t.is_empty() {
                                editor = Some(t);
                            }
                            in_editor = false;
                            editor_buf.clear();
                        }
                        if lname == b"teiHeader" && !titles.is_empty() {
                            break;
                        }
                        path_stack.pop();
                    }
                    Ok(Event::Text(t)) => {
                        let tx = t.decode().unwrap_or_default();
                        if in_title {
                            title_buf.push_str(&tx);
                        }
                        if in_author {
                            author_buf.push_str(&tx);
                        }
                        if in_editor {
                            editor_buf.push_str(&tx);
                        }
                    }
                    Ok(Event::Eof) => break,
                    Err(_) => break,
                    _ => {}
                }
                buf.clear();
                events += 1;
            }

            let id = stem_from(p);
            let title = titles
                .iter()
                .find(|(is_main, _)| *is_main)
                .or_else(|| titles.first())
                .map(|(_, s)| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| id.clone());
            let abs = std::fs::canonicalize(p).unwrap_or_else(|_| p.clone());

            let mut meta: BTreeMap<String, String> = BTreeMap::new();
            if let Some(x) = xml_id {
                meta.insert("xmlId".to_string(), x);
            }
            if let Some(a) = author {
                meta.insert("author".to_string(), a);
            }
            if let Some(ed) = editor {
                meta.insert("editor".to_string(), ed);
            }
            meta.insert("indexVersion".to_string(), "sarit_index_v1".to_string());

            Some(IndexEntry {
                id,
                title,
                path: abs.to_string_lossy().to_string(),
                meta: Some(meta),
            })
        })
        .collect()
}

// GRETIL 用: TEI ヘッダ（titleStmt/author/editor/respStmt/publisher/date）と本文<head>からメタ情報を抽出
pub fn build_gretil_index(root: &Path) -> Vec<IndexEntry> {
    let paths = collect_xml_paths(root, |_, name| name.ends_with(".xml"));

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
            let mut title_main: Option<String> = None;
            let mut author: Option<String> = None;
            let mut editor: Option<String> = None;
            let mut translator: Option<String> = None;
            let mut publisher: Option<String> = None;
            let mut date: Option<String> = None;
            let mut idno: Option<String> = None;
            let mut heads: Vec<String> = Vec::new();

            let mut path_stack: Vec<Vec<u8>> = Vec::new();
            let mut in_title_header = false;
            let mut in_title_main = false;
            let mut in_author = false;
            let mut in_editor = false;
            let mut in_publisher = false;
            let mut in_date = false;
            let mut in_idno = false;
            let mut in_head = false;
            let mut head_buf = String::new();

            // respStmt（役割＋名前）
            let mut in_resp_stmt = false;
            let mut in_resp_role = false;
            let mut in_resp_name = false;
            let mut cur_resp_role: String = String::new();
            let mut cur_resp_names: Vec<String> = Vec::new();
            let mut resp_entries: Vec<String> = Vec::new();

            let mut events = 0usize;
            let max_events = 50_000usize;

            loop {
                match reader.read_event_into(&mut buf) {
                    Ok(Event::Start(e)) | Ok(Event::Empty(e)) => {
                        let name_owned = e.name().as_ref().to_owned();
                        let lname = local_name(&name_owned).to_vec();
                        if id.is_none() {
                            if let Some(v) = attr_val(&e, b"xml:id") {
                                id = Some(v.to_string());
                            }
                        }
                        path_stack.push(lname.clone());

                        // titleStmt/title
                        if lname.as_slice() == b"title" {
                            if path_stack.iter().any(|n| n.as_slice() == b"titleStmt")
                                && path_stack.iter().any(|n| n.as_slice() == b"teiHeader")
                            {
                                in_title_header = true;
                                // prefer type="main"
                                if let Some(t) =
                                    attr_val(&e, b"type").map(|v| v.to_ascii_lowercase())
                                {
                                    if t.contains("main") {
                                        in_title_main = true;
                                    }
                                }
                            }
                        }
                        if lname.as_slice() == b"author"
                            && path_stack.iter().any(|n| n.as_slice() == b"titleStmt")
                        {
                            in_author = true;
                        }
                        if lname.as_slice() == b"editor"
                            && path_stack.iter().any(|n| n.as_slice() == b"titleStmt")
                        {
                            in_editor = true;
                        }
                        if lname.as_slice() == b"publisher" {
                            in_publisher = true;
                        }
                        if lname.as_slice() == b"date" {
                            in_date = true;
                        }
                        if lname.as_slice() == b"idno" {
                            in_idno = true;
                        }
                        if lname.as_slice() == b"head" {
                            in_head = true;
                            head_buf.clear();
                        }

                        if lname.as_slice() == b"respStmt" {
                            in_resp_stmt = true;
                            cur_resp_role.clear();
                            cur_resp_names.clear();
                        }
                        if in_resp_stmt && lname.as_slice() == b"resp" {
                            in_resp_role = true;
                        }
                        if in_resp_stmt
                            && (lname.as_slice() == b"name" || lname.as_slice() == b"persName")
                        {
                            in_resp_name = true;
                        }
                    }
                    Ok(Event::End(e)) => {
                        let name_owned = e.name().as_ref().to_owned();
                        let lname = local_name(&name_owned);
                        if lname == b"title" && in_title_header {
                            in_title_header = false;
                            in_title_main = false;
                        }
                        if lname == b"author" && in_author {
                            in_author = false;
                        }
                        if lname == b"editor" && in_editor {
                            in_editor = false;
                        }
                        if lname == b"publisher" && in_publisher {
                            in_publisher = false;
                        }
                        if lname == b"date" && in_date {
                            in_date = false;
                        }
                        if lname == b"idno" && in_idno {
                            in_idno = false;
                        }
                        if lname == b"head" && in_head {
                            let t = head_buf.split_whitespace().collect::<Vec<_>>().join(" ");
                            if !t.is_empty() && heads.len() < 12 {
                                heads.push(t);
                            }
                            in_head = false;
                            head_buf.clear();
                        }
                        if lname == b"resp" && in_resp_role {
                            in_resp_role = false;
                        }
                        if (lname == b"name" || lname == b"persName") && in_resp_name {
                            in_resp_name = false;
                        }
                        if lname == b"respStmt" && in_resp_stmt {
                            // 役割ベースで翻訳者を推測
                            let role = cur_resp_role.to_lowercase();
                            let names = cur_resp_names.join("・");
                            if translator.is_none() && !names.is_empty() {
                                if role.contains("transl")
                                    || role.contains("translator")
                                    || role.contains("translation")
                                    || role.contains("trl")
                                {
                                    translator = Some(names.clone());
                                }
                            }
                            // respAll 収集
                            if !names.is_empty() {
                                if role.is_empty() {
                                    resp_entries.push(names.clone());
                                } else {
                                    resp_entries.push(format!("{}: {}", role, names));
                                }
                            }
                            in_resp_stmt = false;
                            cur_resp_role.clear();
                            cur_resp_names.clear();
                        }
                        path_stack.pop();
                    }
                    Ok(Event::Text(t)) => {
                        let tx = t.decode().unwrap_or_default();
                        let s = tx.trim();
                        if in_title_main && title_main.is_none() && !s.is_empty() {
                            title_main = Some(s.to_string());
                        }
                        if in_title_header && title_header.is_none() && !s.is_empty() {
                            title_header = Some(s.to_string());
                        }
                        if in_author && author.is_none() && !s.is_empty() {
                            author = Some(s.to_string());
                        }
                        if in_editor && editor.is_none() && !s.is_empty() {
                            editor = Some(s.to_string());
                        }
                        if in_publisher && publisher.is_none() && !s.is_empty() {
                            publisher = Some(s.to_string());
                        }
                        if in_date && date.is_none() && !s.is_empty() {
                            date = Some(s.to_string());
                        }
                        if in_idno && idno.is_none() && !s.is_empty() {
                            idno = Some(s.to_string());
                        }
                        if in_head {
                            head_buf.push_str(&tx);
                        }
                        if in_resp_role {
                            cur_resp_role.push_str(&tx);
                        }
                        if in_resp_name {
                            cur_resp_names.push(s.to_string());
                        }
                    }
                    Ok(Event::Eof) => break,
                    Err(_) => break,
                    _ => {}
                }
                buf.clear();
                events += 1;
                if events > max_events {
                    break;
                }
            }

            let id = id.unwrap_or_else(|| stem_from(p));
            let mut title = title_main
                .or(title_header)
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| stem_from(p));
            // タイトルが過度に長い場合は先頭100字にトリム
            if title.chars().count() > 120 {
                title = title.chars().take(120).collect::<String>();
            }
            let abs = std::fs::canonicalize(p).unwrap_or_else(|_| p.clone());

            let mut meta_map: BTreeMap<String, String> = BTreeMap::new();
            if let Some(v) = author {
                meta_map.insert("author".to_string(), v);
            }
            if let Some(v) = editor {
                meta_map.insert("editor".to_string(), v);
            }
            if let Some(v) = translator {
                meta_map.insert("translator".to_string(), v);
            }
            if let Some(v) = publisher {
                meta_map.insert("publisher".to_string(), v);
            }
            if let Some(v) = date {
                meta_map.insert("date".to_string(), v);
            }
            if let Some(v) = idno {
                meta_map.insert("idno".to_string(), v);
            }
            if !heads.is_empty() {
                meta_map.insert(
                    "headsPreview".to_string(),
                    heads
                        .iter()
                        .take(10)
                        .cloned()
                        .collect::<Vec<_>>()
                        .join(" | "),
                );
            }

            // keywords, classCode, catRef 抽出
            // 再パースはコストが高いので上の走査で拾うのが理想だが、簡潔に二段回で対応
            let content = std::fs::read_to_string(p).unwrap_or_default();
            let mut reader2 = Reader::from_str(&content);
            reader2.config_mut().trim_text_start = true;
            reader2.config_mut().trim_text_end = true;
            let mut buf2 = Vec::new();
            let mut in_keywords = false;
            let mut in_term = false;
            let mut terms: Vec<String> = Vec::new();
            let mut in_classcode = false;
            let mut class_codes: Vec<String> = Vec::new();
            let mut cat_refs: Vec<String> = Vec::new();
            loop {
                match reader2.read_event_into(&mut buf2) {
                    Ok(Event::Start(e)) | Ok(Event::Empty(e)) => {
                        let name_owned = e.name().as_ref().to_owned();
                        let lname = local_name(&name_owned);
                        if lname == b"keywords" {
                            in_keywords = true;
                        }
                        if lname == b"term" && in_keywords {
                            in_term = true;
                        }
                        if lname == b"classCode" {
                            in_classcode = true;
                        }
                        if lname == b"catRef" {
                            if let Some(t) = attr_val(&e, b"target") {
                                let v = t.trim().to_string();
                                if !v.is_empty() {
                                    cat_refs.push(v);
                                }
                            }
                        }
                    }
                    Ok(Event::End(e)) => {
                        let name_owned = e.name().as_ref().to_owned();
                        let lname = local_name(&name_owned);
                        if lname == b"keywords" {
                            in_keywords = false;
                        }
                        if lname == b"term" && in_keywords {
                            in_term = false;
                        }
                        if lname == b"classCode" {
                            in_classcode = false;
                        }
                    }
                    Ok(Event::Text(t)) => {
                        let s = t.decode().unwrap_or_default();
                        if in_term {
                            let v = s.trim();
                            if !v.is_empty() {
                                terms.push(v.to_string());
                            }
                        }
                        if in_classcode {
                            let v = s.trim();
                            if !v.is_empty() {
                                class_codes.push(v.to_string());
                            }
                        }
                    }
                    Ok(Event::Eof) => break,
                    Err(_) => break,
                    _ => {}
                }
                buf2.clear();
            }
            if !terms.is_empty() {
                meta_map.insert("keywords".to_string(), terms.join(" | "));
            }
            if !class_codes.is_empty() {
                meta_map.insert("classCode".to_string(), class_codes.join(" | "));
            }
            if !cat_refs.is_empty() {
                meta_map.insert("catRef".to_string(), cat_refs.join(" | "));
            }
            if !resp_entries.is_empty() {
                meta_map.insert("respAll".to_string(), resp_entries.join(" | "));
            }

            Some(IndexEntry {
                id,
                title,
                path: abs.to_string_lossy().to_string(),
                meta: if meta_map.is_empty() {
                    None
                } else {
                    Some(meta_map)
                },
            })
        })
        .collect()
}

#[derive(Clone, Debug)]
struct HeaderTitleCandidate {
    text: String,
    lang: Option<String>,
}

fn cbeta_cjk_ratio(s: &str) -> f32 {
    let mut total = 0usize;
    let mut cjk = 0usize;
    for ch in s.chars() {
        if ch.is_whitespace() {
            continue;
        }
        total += 1;
        // CJK Unified Ideographs + Extension A (good enough for titles)
        if ('\u{4E00}'..='\u{9FFF}').contains(&ch) || ('\u{3400}'..='\u{4DBF}').contains(&ch) {
            cjk += 1;
        }
    }
    if total == 0 {
        0.0
    } else {
        (cjk as f32) / (total as f32)
    }
}

fn cbeta_title_contains_collection_keywords(s: &str) -> bool {
    // Use normalized() to fold diacritics (e.g., Tripiṭaka -> tripitaka).
    let hay = crate::text_utils::normalized(s);
    hay.contains("tripitaka") || hay.contains("taisho") || hay.contains("canon")
}

fn pick_best_cbeta_header_title(cands: &[HeaderTitleCandidate]) -> Option<String> {
    let mut best: Option<(&HeaderTitleCandidate, f32)> = None;
    for c in cands {
        if c.text.trim().is_empty() {
            continue;
        }
        let mut sc = 0.0f32;
        if let Some(lang) = &c.lang {
            let l = lang.to_lowercase();
            if l.starts_with("zh") {
                sc += 3.0;
            } else if l.starts_with("ja") {
                sc += 2.0;
            } else {
                sc += 0.5;
            }
        }
        sc += cbeta_cjk_ratio(&c.text) * 2.0;
        if cbeta_title_contains_collection_keywords(&c.text) {
            sc -= 2.0;
        }
        // Small length bonus to prefer informative (non-empty) titles.
        sc += (c.text.chars().count().min(30) as f32) / 100.0;
        match best {
            None => best = Some((c, sc)),
            Some((_, bsc)) if sc > bsc => best = Some((c, sc)),
            _ => {}
        }
    }
    best.map(|(c, _)| c.text.clone())
}

// CBETA 用: TEI ヘッダや本文の構造からメタ情報を抽出してインデックスを高精度化
pub fn build_cbeta_index(root: &Path) -> Vec<IndexEntry> {
    let paths = collect_xml_paths(root, |_, name| name.ends_with(".xml"));

    paths
        .par_iter()
        .filter_map(|p| {
            let f = File::open(p).ok()?;
            let mut reader = Reader::from_reader(BufReader::new(f));
            reader.config_mut().trim_text_start = true;
            reader.config_mut().trim_text_end = true;
            let mut buf = Vec::new();

            let mut id: Option<String> = None;
            let mut titles: Vec<HeaderTitleCandidate> = Vec::new();
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
            let mut title_lang: Option<String> = None;
            let mut title_buf = String::new();
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
                            if let Some(v) = attr_val(&e, b"xml:id") {
                                id = Some(v.to_string());
                            }
                        }
                        path_stack.push(lname.clone());

                        if lname.as_slice() == b"title" {
                            if path_stack.iter().any(|n| n.as_slice() == b"titleStmt")
                                && path_stack.iter().any(|n| n.as_slice() == b"teiHeader")
                            {
                                in_title_header = true;
                                title_buf.clear();
                                title_lang = attr_val(&e, b"xml:lang").map(|v| v.to_string());
                            }
                        }
                        if lname.as_slice() == b"author"
                            && path_stack.iter().any(|n| n.as_slice() == b"titleStmt")
                        {
                            in_author = true;
                        }
                        if lname.as_slice() == b"editor"
                            && path_stack.iter().any(|n| n.as_slice() == b"titleStmt")
                        {
                            in_editor = true;
                        }
                        if lname.as_slice() == b"respStmt" {
                            in_resp_stmt = true;
                            cur_resp_role.clear();
                            cur_resp_names.clear();
                        }
                        if in_resp_stmt && lname.as_slice() == b"resp" {
                            in_resp_role = true;
                        }
                        if in_resp_stmt
                            && (lname.as_slice() == b"name" || lname.as_slice() == b"persName")
                        {
                            in_resp_name = true;
                        }
                        if lname.as_slice() == b"publisher" {
                            in_publisher = true;
                        }
                        if lname.as_slice() == b"date" {
                            in_date = true;
                        }
                        if lname.as_slice() == b"idno" {
                            in_idno = true;
                        }
                        if lname.as_slice() == b"head" {
                            in_head = true;
                            head_buf.clear();
                        }
                        if lname.as_slice() == b"juan" {
                            let fun = attr_val(&e, b"fun").map(|v| v.to_ascii_lowercase());
                            if fun.as_deref() == Some("open") || fun.is_none() {
                                juan_count += 1;
                            }
                        }
                    }
                    Ok(Event::End(e)) => {
                        let name_owned = e.name().as_ref().to_owned();
                        let lname = local_name(&name_owned);
                        if lname == b"title" && in_title_header {
                            let t = title_buf.split_whitespace().collect::<Vec<_>>().join(" ");
                            if !t.is_empty() {
                                titles.push(HeaderTitleCandidate {
                                    text: t,
                                    lang: title_lang.take(),
                                });
                            }
                            in_title_header = false;
                            title_buf.clear();
                            title_lang = None;
                        }
                        if lname == b"author" && in_author {
                            in_author = false;
                        }
                        if lname == b"editor" && in_editor {
                            in_editor = false;
                        }
                        if lname == b"resp" && in_resp_role {
                            in_resp_role = false;
                        }
                        if (lname == b"name" || lname == b"persName") && in_resp_name {
                            in_resp_name = false;
                        }
                        if lname == b"respStmt" && in_resp_stmt {
                            // finalize current respStmt entry
                            let names_join = cur_resp_names.join("・");
                            let entry = if !cur_resp_role.trim().is_empty() {
                                format!("{}: {}", cur_resp_role.trim(), names_join)
                            } else {
                                names_join
                            };
                            if !entry.trim().is_empty() {
                                resp_entries.push(entry);
                            }
                            in_resp_stmt = false;
                            cur_resp_role.clear();
                            cur_resp_names.clear();
                        }
                        if lname == b"publisher" && in_publisher {
                            in_publisher = false;
                        }
                        if lname == b"date" && in_date {
                            in_date = false;
                        }
                        if lname == b"idno" && in_idno {
                            in_idno = false;
                        }
                        if lname == b"head" && in_head {
                            let t = head_buf.split_whitespace().collect::<Vec<_>>().join(" ");
                            if !t.is_empty() && heads.len() < 12 {
                                heads.push(t);
                            }
                            in_head = false;
                            head_buf.clear();
                        }
                        path_stack.pop();
                    }
                    Ok(Event::Text(t)) => {
                        let tx = t.decode().unwrap_or_default();
                        let s = tx.to_string();
                        if in_title_header {
                            title_buf.push_str(&s);
                        }
                        if in_author && author.is_none() && !s.trim().is_empty() {
                            author = Some(s.clone());
                        }
                        if in_editor && editor.is_none() && !s.trim().is_empty() {
                            editor = Some(s.clone());
                        }
                        if in_resp_role && !s.trim().is_empty() {
                            cur_resp_role.push_str(&s);
                        }
                        if in_resp_name && !s.trim().is_empty() {
                            cur_resp_names.push(s.clone());
                        }
                        if in_publisher && publisher.is_none() && !s.trim().is_empty() {
                            publisher = Some(s.clone());
                        }
                        if in_date && pubdate.is_none() && !s.trim().is_empty() {
                            pubdate = Some(s.clone());
                        }
                        if in_idno && idno.is_none() && !s.trim().is_empty() {
                            idno = Some(s.clone());
                        }
                        if in_head {
                            head_buf.push_str(&s);
                        }
                    }
                    Ok(Event::Eof) => break,
                    Err(_) => break,
                    _ => {}
                }
                buf.clear();
                events += 1;
                if events > max_events {
                    break;
                }
            }

            let id = id.unwrap_or_else(|| stem_from(p));
            let title = pick_best_cbeta_header_title(&titles)
                .or_else(|| heads.get(0).cloned())
                .unwrap_or_else(|| stem_from(p));
            let abs = std::fs::canonicalize(p).unwrap_or_else(|_| p.clone());

            let canon = abs
                .parent()
                .and_then(|pp| pp.strip_prefix(root).ok())
                .and_then(|rel| rel.components().next())
                .map(|c| c.as_os_str().to_string_lossy().to_string())
                .unwrap_or_default();

            let fname = abs
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or("")
                .to_string();
            let mut nnum: Option<String> = None;
            if let Some(pos) = fname.to_lowercase().find('n') {
                let digits: String = fname[pos + 1..]
                    .chars()
                    .take_while(|c| c.is_ascii_digit())
                    .collect();
                if !digits.is_empty() {
                    nnum = Some(digits);
                }
            }

            let mut meta = BTreeMap::new();
            meta.insert("indexVersion".to_string(), "cbeta_index_v2".to_string());
            if !canon.is_empty() {
                meta.insert("canon".to_string(), canon);
            }
            if let Some(a) = author {
                meta.insert("author".to_string(), a);
            }
            if let Some(ed) = editor {
                meta.insert("editor".to_string(), ed);
            }
            if !resp_entries.is_empty() {
                meta.insert("respAll".to_string(), resp_entries.join(" | "));
            }
            // try to extract translators from resp entries
            if !resp_entries.is_empty() {
                let mut translators: Vec<String> = Vec::new();
                for e in resp_entries.iter() {
                    let low = e.to_lowercase();
                    if low.contains("譯")
                        || low.contains("译")
                        || low.contains("translat")
                        || low.contains("tr.")
                    {
                        // extract part after ':' if any
                        if let Some(pos) = e.find(':') {
                            translators.push(e[pos + 1..].trim().to_string());
                        } else {
                            translators.push(e.clone());
                        }
                    }
                }
                if !translators.is_empty() {
                    meta.insert("translator".to_string(), translators.join("・"));
                }
            }
            if let Some(pu) = publisher {
                meta.insert("publisher".to_string(), pu);
            }
            if let Some(pd) = pubdate {
                meta.insert("date".to_string(), pd);
            }
            if let Some(i) = idno {
                meta.insert("idno".to_string(), i);
            }
            if let Some(nn) = nnum {
                meta.insert("nnum".to_string(), nn);
            }
            if juan_count > 0 {
                meta.insert("juanCount".to_string(), juan_count.to_string());
            }
            if !heads.is_empty() {
                meta.insert(
                    "headsPreview".to_string(),
                    heads
                        .iter()
                        .take(10)
                        .cloned()
                        .collect::<Vec<_>>()
                        .join(" | "),
                );
            }

            Some(IndexEntry {
                id,
                title,
                path: abs.to_string_lossy().to_string(),
                meta: if meta.is_empty() { None } else { Some(meta) },
            })
        })
        .collect()
}

// Tipitaka 用: teiHeader が空な場合が多いため、<p rend="..."> 系から書誌情報を抽出してタイトルを構築
pub fn build_tipitaka_index(root: &Path) -> Vec<IndexEntry> {
    // 走査: root 配下の .xml で .toc.xml は除外 (rootは既にromnディレクトリを指している)
    let paths = collect_xml_paths_cached(&TIPITAKA_XML_PATHS_CACHE, root, |_, name| {
        name.ends_with(".xml")
            && !name.contains("toc")
            && !name.contains("sitemap")
            && !name.contains("tree")
            && !name.ends_with(".xsl")
            && !name.ends_with(".css")
    });

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
                }
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
            let mut fields: std::collections::HashMap<String, Vec<String>> =
                std::collections::HashMap::new();
            let mut heads: Vec<String> = Vec::new();
            let mut div_info: Vec<(String, String)> = Vec::new(); // (n, type) pairs

            // 対象キー（軽量）: タイトル/別名生成に必要な最小限のみ
            // NOTE: gathalast は本文級に膨らみうるためインデックスには含めない（速度最優先）。
            const MAX_VALUES_PER_KEY: usize = 8;
            const MAX_DIV_INFO: usize = 64;
            let wanted_p = ["nikaya", "title", "subhead", "subsubhead"];
            let wanted_head = ["book", "chapter"];
            let mut events_read = 0usize;
            let max_events = 12_000usize; // 先頭付近の書誌情報だけで十分なケースが大半

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
                            if let (Some(n), Some(type_val)) =
                                (attr_val(&e, b"n"), attr_val(&e, b"type"))
                            {
                                if div_info.len() < MAX_DIV_INFO {
                                    div_info.push((n.to_string(), type_val.to_string()));
                                }
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
                                    if values.len() < MAX_VALUES_PER_KEY && !values.contains(&val) {
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
                                        if values.len() < MAX_VALUES_PER_KEY
                                            && !values.contains(&val)
                                        {
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
                if events_read > max_events {
                    break;
                }
            }

            // タイトル組み立て: 階層順序に従って構築
            let parts_order = [
                "nikaya",
                "book",
                "title",
                "subhead",
                "subsubhead",
                "chapter",
            ];
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
            let title = if parts.is_empty() {
                stem_from(p)
            } else {
                parts.join(" · ")
            };
            let id = stem_from(p);
            let abs = std::fs::canonicalize(p).unwrap_or_else(|_| p.clone());

            // メタデータの構築
            let mut meta_map: BTreeMap<String, String> = BTreeMap::new();

            // フィールド値をメタデータに格納
            for (key, values) in fields.iter() {
                if values.is_empty() {
                    continue;
                }
                // 大量の本文級フィールドをインデックスに溜めない（速度/サイズ優先）。
                // 必要十分な範囲だけを join する（values 自体も上限あり）。
                meta_map.insert(key.clone(), values.join(" | "));
            }

            // index versioning (invalidate old heavy caches)
            meta_map.insert("indexVersion".to_string(), "tipitaka_index_v2".to_string());

            // headsPreview は常にキーを持たせ、MCP側のキャッシュ妥当性チェックを安定させる
            meta_map.insert(
                "headsPreview".to_string(),
                if heads.is_empty() {
                    String::new()
                } else {
                    heads
                        .iter()
                        .take(10)
                        .cloned()
                        .collect::<Vec<_>>()
                        .join(" | ")
                },
            );

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
                if nf.contains("digha") {
                    alias_prefix = Some("DN");
                } else if nf.contains("majjhima") {
                    alias_prefix = Some("MN");
                } else if nf.contains("samyutta")
                    || nf.contains("saṃyutta")
                    || nf.contains("samyutta")
                {
                    alias_prefix = Some("SN");
                } else if nf.contains("anguttara")
                    || nf.contains("agguttara")
                    || nf.contains("aṅguttara")
                {
                    alias_prefix = Some("AN");
                } else if nf.contains("khuddaka") {
                    alias_prefix = Some("KN");
                }
            }
            if let Some(prefix) = alias_prefix {
                // try to find a number from book/title/subhead/chapter
                let mut num_str: Option<String> = None;
                for k in ["book", "title", "subhead", "subsubhead", "chapter"].iter() {
                    if let Some(v) = meta_map.get(*k) {
                        if let Some(ns) = first_number(v) {
                            num_str = Some(ns);
                            break;
                        }
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
                    for f in forms {
                        aliases.push(f.clone());
                        aliases.push(f.to_lowercase());
                    }
                    // roman numeral variant
                    if let Ok(n) = n_trim.parse::<usize>() {
                        let r = roman_upper(n);
                        let forms_r = vec![format!("{} {}", prefix, r), format!("{}{}", prefix, r)];
                        for f in forms_r {
                            aliases.push(f.clone());
                            aliases.push(f.to_lowercase());
                        }
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
                        for f in forms {
                            aliases.push(f.clone());
                            aliases.push(f.to_lowercase());
                        }
                        // Roman for first component optionally
                        if let Ok(na) = a_trim.parse::<usize>() {
                            let ra = roman_upper(na);
                            let forms_r = vec![
                                format!("{} {}.{}", prefix, ra, b_trim),
                                format!("{}{}.{}", prefix, ra, b_trim),
                            ];
                            for f in forms_r {
                                aliases.push(f.clone());
                                aliases.push(f.to_lowercase());
                            }
                        }
                    }
                }
                meta_map.insert("alias".to_string(), aliases.join(" "));
                meta_map.insert("alias_prefix".to_string(), prefix.to_string());
            }
            if !heads.is_empty() {
                // add alias variants from heads and title
                let mut alias_ext: Vec<String> = Vec::new();
                for s in heads.iter().take(6) {
                    alias_ext.extend(pali_title_variants(s));
                }
                alias_ext.extend(pali_title_variants(&title));
                if !alias_ext.is_empty() {
                    let prev = meta_map.remove("alias").unwrap_or_default();
                    let combined = if prev.is_empty() {
                        alias_ext.join(" ")
                    } else {
                        format!("{} {}", prev, alias_ext.join(" "))
                    };
                    meta_map.insert("alias".to_string(), combined);
                }
            }
            let meta = if meta_map.is_empty() {
                None
            } else {
                Some(meta_map)
            };
            Some(IndexEntry {
                id,
                title,
                path: abs.to_string_lossy().to_string(),
                meta,
            })
        })
        .collect()
}

fn fold_ascii(s: &str) -> String {
    let t: String = s.nfkd().collect::<String>().to_lowercase();
    t.chars()
        .filter(|c| c.is_alphanumeric() || c.is_whitespace())
        .collect()
}

fn first_number(s: &str) -> Option<String> {
    let mut out = String::new();
    for ch in s.chars() {
        if ch.is_ascii_digit() {
            out.push(ch);
        } else if !out.is_empty() {
            break;
        }
    }
    if out.is_empty() {
        None
    } else {
        Some(out)
    }
}

fn roman_upper(mut n: usize) -> String {
    // simple roman numeral converter (1..3999)
    if n == 0 || n > 3999 {
        return n.to_string();
    }
    let mut out = String::new();
    let vals = [
        (1000, "M"),
        (900, "CM"),
        (500, "D"),
        (400, "CD"),
        (100, "C"),
        (90, "XC"),
        (50, "L"),
        (40, "XL"),
        (10, "X"),
        (9, "IX"),
        (5, "V"),
        (4, "IV"),
        (1, "I"),
    ];
    for (v, s) in vals.iter() {
        while n >= *v {
            out.push_str(s);
            n -= *v;
        }
    }
    out
}

fn first_two_numbers_from_meta(meta: &BTreeMap<String, String>) -> Option<(String, String)> {
    let mut nums: Vec<String> = Vec::new();
    for k in ["book", "title", "subhead", "subsubhead", "chapter"].iter() {
        if let Some(v) = meta.get(*k) {
            let mut cur = String::new();
            for ch in v.chars() {
                if ch.is_ascii_digit() {
                    cur.push(ch);
                } else {
                    if !cur.is_empty() {
                        nums.push(cur.clone());
                        cur.clear();
                    }
                }
            }
            if !cur.is_empty() {
                nums.push(cur);
            }
            if nums.len() >= 2 {
                break;
            }
        }
    }
    if nums.len() >= 2 {
        Some((nums[0].clone(), nums[1].clone()))
    } else {
        None
    }
}

fn pali_title_variants(s: &str) -> Vec<String> {
    // generate normalized and ascii-vowel-doubling variants to help search recall
    let mut out: Vec<String> = Vec::new();
    let base = s.trim();
    if base.is_empty() {
        return out;
    }
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
        if let Some(r) = repl {
            dbl.push_str(r);
        } else {
            dbl.push(ch);
        }
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
                if name == b"charDecl" {
                    in_chardecl = true;
                } else if in_chardecl && name == b"char" {
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
                if name == b"charDecl" {
                    in_chardecl = false;
                }
                if name == b"mapping" {
                    in_mapping = false;
                }
                if name == b"charName" {
                    in_charname = false;
                }
                if name == b"char" && in_char {
                    if let (Some(id), Some(v)) = (current_id.clone(), current_val.clone()) {
                        if !v.is_empty() {
                            map.insert(id, v);
                        }
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
                    if !text.trim().is_empty() {
                        current_val = Some(text);
                    }
                } else if in_char && in_mapping && current_mapping_type.as_deref() == Some("normal")
                {
                    if current_val.is_none() && !text.trim().is_empty() {
                        current_val = Some(text);
                    }
                } else if in_char && in_charname {
                    if current_val.is_none() && !text.trim().is_empty() {
                        current_val = Some(text);
                    }
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

/// Fast CBETA gaiji map extraction: parse only `<charDecl>` and exit early.
///
/// This is important for `format=plain` snippet extraction, where we want to
/// resolve `<g ref="#CBxxxx">` even when the snippet itself doesn't include
/// `<charDecl>`.
pub fn cbeta_gaiji_map_fast(xml: &str) -> HashMap<String, String> {
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
            Ok(Event::Start(e)) => {
                let name_owned = e.name().as_ref().to_owned();
                let name = local_name(&name_owned);
                if name == b"charDecl" {
                    in_chardecl = true;
                } else if in_chardecl && name == b"char" {
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
            Ok(Event::Empty(e)) => {
                let name_owned = e.name().as_ref().to_owned();
                let name = local_name(&name_owned);
                if name == b"charDecl" {
                    // unlikely, but treat as done
                    break;
                } else if in_chardecl && name == b"char" {
                    // empty char: ignore
                } else if in_char && name == b"mapping" {
                    // empty mapping: ignore
                }
            }
            Ok(Event::End(e)) => {
                let name_owned = e.name().as_ref().to_owned();
                let name = local_name(&name_owned);
                if name == b"charDecl" {
                    break;
                }
                if name == b"mapping" {
                    in_mapping = false;
                }
                if name == b"charName" {
                    in_charname = false;
                }
                if name == b"char" && in_char {
                    if let (Some(id), Some(v)) = (current_id.clone(), current_val.clone()) {
                        if !v.is_empty() {
                            map.insert(id, v);
                        }
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
                    if !text.trim().is_empty() {
                        current_val = Some(text);
                    }
                } else if in_char && in_mapping && current_mapping_type.as_deref() == Some("normal")
                {
                    if current_val.is_none() && !text.trim().is_empty() {
                        current_val = Some(text);
                    }
                } else if in_char && in_charname {
                    if current_val.is_none() && !text.trim().is_empty() {
                        current_val = Some(text);
                    }
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

fn squash_newlines_max(s: &str, max_consecutive: usize) -> String {
    let mut out = String::with_capacity(s.len());
    let mut run = 0usize;
    for ch in s.chars() {
        if ch == '\n' {
            run += 1;
            if run <= max_consecutive {
                out.push('\n');
            }
        } else {
            run = 0;
            out.push(ch);
        }
    }
    out
}

fn normalize_plain_lines(s: &str) -> String {
    let s = s.replace("\r", "");
    let mut lines: Vec<String> = Vec::new();
    for line in s.lines() {
        let t = line.trim();
        if t.is_empty() {
            lines.push(String::new());
            continue;
        }
        let mut collapsed = String::with_capacity(t.len());
        let mut in_ws = false;
        for ch in t.chars() {
            if ch.is_whitespace() {
                if !in_ws {
                    collapsed.push(' ');
                    in_ws = true;
                }
            } else {
                in_ws = false;
                collapsed.push(ch);
            }
        }
        lines.push(collapsed);
    }
    let joined = lines.join("\n");
    let squashed = squash_newlines_max(&joined, 2);
    squashed.trim_matches('\n').to_string()
}

fn extract_cbeta_plain_impl(
    xml: &str,
    gaiji: Option<&HashMap<String, String>>,
    include_notes: bool,
    skip_tei_header: bool,
) -> String {
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text_start = true;
    reader.config_mut().trim_text_end = true;
    let mut buf = Vec::new();
    let mut out = String::new();

    // Exclude <teiHeader> content (metadata) by tracking XML element depth.
    let mut skip_header_depth: usize = 0;
    // Exclude notes when include_notes=false (depth-based subtree skip).
    let mut skip_note_depth: usize = 0;
    // Collect note text when include_notes=true.
    let mut collect_note: bool = false;
    let mut note_depth: usize = 0;
    let mut note_buf = String::new();

    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e)) => {
                let name_owned = e.name().as_ref().to_owned();
                let name = local_name(&name_owned);

                if skip_header_depth > 0 {
                    // Inside teiHeader; just track depth.
                    skip_header_depth += 1;
                    continue;
                }
                if skip_tei_header && name == b"teiHeader" {
                    skip_header_depth = 1;
                    continue;
                }

                if collect_note {
                    note_depth += 1;
                    if name == b"g" {
                        if let Some(r) = attr_val(&e, b"ref") {
                            let key = r.trim_start_matches('#').to_string();
                            if let Some(v) = gaiji.and_then(|m| m.get(&key)) {
                                note_buf.push_str(v);
                            }
                        }
                    }
                    continue;
                }

                if skip_note_depth > 0 {
                    skip_note_depth += 1;
                    continue;
                }

                if name == b"note" {
                    if include_notes {
                        collect_note = true;
                        note_depth = 1;
                        note_buf.clear();
                    } else {
                        skip_note_depth = 1;
                    }
                    continue;
                }

                if name == b"lb" {
                    out.push('\n');
                } else if name == b"pb" {
                    out.push('\n');
                    out.push('\n');
                } else if name == b"g" {
                    if let Some(r) = attr_val(&e, b"ref") {
                        let key = r.trim_start_matches('#').to_string();
                        if let Some(v) = gaiji.and_then(|m| m.get(&key)) {
                            out.push_str(v);
                        }
                    }
                }
            }
            Ok(Event::Empty(e)) => {
                let name_owned = e.name().as_ref().to_owned();
                let name = local_name(&name_owned);

                if skip_header_depth > 0 {
                    continue;
                }
                if skip_tei_header && name == b"teiHeader" {
                    continue;
                }

                if collect_note {
                    if name == b"g" {
                        if let Some(r) = attr_val(&e, b"ref") {
                            let key = r.trim_start_matches('#').to_string();
                            if let Some(v) = gaiji.and_then(|m| m.get(&key)) {
                                note_buf.push_str(v);
                            }
                        }
                    }
                    continue;
                }

                if skip_note_depth > 0 {
                    continue;
                }

                if name == b"lb" {
                    out.push('\n');
                } else if name == b"pb" {
                    out.push('\n');
                    out.push('\n');
                } else if name == b"g" {
                    if let Some(r) = attr_val(&e, b"ref") {
                        let key = r.trim_start_matches('#').to_string();
                        if let Some(v) = gaiji.and_then(|m| m.get(&key)) {
                            out.push_str(v);
                        }
                    }
                } else if name == b"note" {
                    // empty note: ignore
                }
            }
            Ok(Event::End(e)) => {
                let name_owned = e.name().as_ref().to_owned();
                let name = local_name(&name_owned);

                if skip_header_depth > 0 {
                    skip_header_depth = skip_header_depth.saturating_sub(1);
                    continue;
                }

                if collect_note {
                    note_depth = note_depth.saturating_sub(1);
                    if name == b"note" && note_depth == 0 {
                        let t = note_buf.split_whitespace().collect::<Vec<_>>().join(" ");
                        if !t.is_empty() {
                            out.push_str(" [注] ");
                            out.push_str(&t);
                        }
                        collect_note = false;
                        note_buf.clear();
                    }
                    continue;
                }

                if skip_note_depth > 0 {
                    skip_note_depth = skip_note_depth.saturating_sub(1);
                    continue;
                }
            }
            Ok(Event::Text(t)) => {
                if skip_header_depth > 0 {
                    // ignore
                } else if collect_note {
                    note_buf.push_str(&t.decode().unwrap_or_default());
                } else if skip_note_depth == 0 {
                    out.push_str(&t.decode().unwrap_or_default());
                }
            }
            Ok(Event::CData(t)) => {
                if skip_header_depth > 0 {
                    // ignore
                } else if collect_note {
                    note_buf.push_str(&String::from_utf8_lossy(&t));
                } else if skip_note_depth == 0 {
                    out.push_str(&String::from_utf8_lossy(&t));
                }
            }
            Ok(Event::Eof) => break,
            Err(_) => break,
            _ => {}
        }
        buf.clear();
    }

    normalize_plain_lines(&out)
}

/// Extract CBETA plain text from a full TEI XML document.
/// - resolves gaiji using `<charDecl>`
/// - excludes `<teiHeader>`
/// - preserves line breaks (`lb` -> `\n`, `pb` -> blank line)
pub fn extract_cbeta_plain_from_xml(xml: &str, include_notes: bool) -> String {
    let gaiji = cbeta_gaiji_map_fast(xml);
    extract_cbeta_plain_impl(xml, Some(&gaiji), include_notes, true)
}

/// Extract CBETA plain text from an XML snippet, using a gaiji map precomputed
/// from the full document.
pub fn extract_cbeta_plain_from_snippet(
    snippet_xml: &str,
    gaiji: &HashMap<String, String>,
    include_notes: bool,
) -> String {
    extract_cbeta_plain_impl(snippet_xml, Some(gaiji), include_notes, true)
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
                    if skip_depth == 0 && !collect_note {
                        out.push('\n');
                    }
                } else if name == b"pb" {
                    if skip_depth == 0 && !collect_note {
                        out.push('\n');
                        out.push('\n');
                    }
                } else if name == b"g" {
                    if skip_depth == 0 {
                        if let Some(r) = attr_val(&e, b"ref") {
                            let key = r.trim_start_matches('#').to_string();
                            if let Some(v) = gaiji.get(&key) {
                                out.push_str(v);
                            }
                        }
                    }
                }
            }
            Ok(Event::Empty(e)) => {
                let name_owned = e.name().as_ref().to_owned();
                let name = local_name(&name_owned);
                if name == b"lb" {
                    if skip_depth == 0 && !collect_note {
                        out.push('\n');
                    }
                } else if name == b"pb" {
                    if skip_depth == 0 && !collect_note {
                        out.push('\n');
                        out.push('\n');
                    }
                } else if name == b"g" {
                    if skip_depth == 0 {
                        if let Some(r) = attr_val(&e, b"ref") {
                            let key = r.trim_start_matches('#').to_string();
                            if let Some(v) = gaiji.get(&key) {
                                out.push_str(v);
                            }
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
                    if skip_depth > 0 {
                        if skip_depth > 0 {
                            skip_depth -= 1;
                        }
                    }
                    if collect_note {
                        note_depth = note_depth.saturating_sub(1);
                        if note_depth == 0 {
                            let t = note_buf.split_whitespace().collect::<Vec<_>>().join(" ");
                            if !t.is_empty() {
                                out.push_str(" [注] ");
                                out.push_str(&t);
                                out.push(' ');
                            }
                            collect_note = false;
                            note_buf.clear();
                        }
                    }
                } else if collect_note {
                    if note_depth > 0 {
                        note_depth -= 1;
                    }
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
                if collect_note {
                    note_buf.push_str(&text);
                } else if skip_depth == 0 {
                    out.push_str(&text);
                }
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
                        if (n == target_n1 || n == target_n2)
                            && (fun.as_deref() == Some("open") || fun.is_none())
                        {
                            capturing = true;
                        }
                    } else {
                        if fun.as_deref() == Some("close") {
                            break;
                        }
                    }
                } else if capturing {
                    if name == b"lb" {
                        out.push('\n');
                    } else if name == b"pb" {
                        out.push('\n');
                        out.push('\n');
                    } else if name == b"g" {
                        if let Some(r) = attr_val(&e, b"ref") {
                            let key = r.trim_start_matches('#').to_string();
                            if let Some(v) = gaiji.get(&key) {
                                out.push_str(v);
                            }
                        }
                    }
                }
            }
            Ok(Event::End(_)) => {}
            Ok(Event::Text(t)) => {
                if capturing {
                    out.push_str(&t.decode().unwrap_or_default());
                }
            }
            Ok(Event::CData(t)) => {
                if capturing {
                    out.push_str(&String::from_utf8_lossy(&t));
                }
            }
            Ok(Event::Eof) => break,
            Err(_) => break,
            _ => {}
        }
        buf.clear();
    }
    if out.is_empty() {
        None
    } else {
        Some(out.split_whitespace().collect::<Vec<_>>().join(" "))
    }
}

/// Extract a single CBETA juan as plain text with line breaks preserved.
/// This respects `include_notes` and resolves gaiji using `<charDecl>` from the full document.
pub fn extract_cbeta_juan_plain(xml: &str, part: &str, include_notes: bool) -> Option<String> {
    let gaiji = cbeta_gaiji_map_fast(xml);
    let target_n1 = part.to_string();
    let target_n2 = format!("{:0>3}", part);
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text_start = true;
    reader.config_mut().trim_text_end = true;
    let mut buf = Vec::new();
    let mut capturing = false;
    let mut out = String::new();

    let mut skip_note_depth: usize = 0;
    let mut collect_note: bool = false;
    let mut note_depth: usize = 0;
    let mut note_buf = String::new();

    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e)) => {
                let name_owned = e.name().as_ref().to_owned();
                let name = local_name(&name_owned);
                if name == b"juan" {
                    let fun = attr_val(&e, b"fun").map(|v| v.to_ascii_lowercase());
                    let n = attr_val(&e, b"n").unwrap_or(Cow::Borrowed(""));
                    if !capturing {
                        if (n == target_n1 || n == target_n2)
                            && (fun.as_deref() == Some("open") || fun.is_none())
                        {
                            capturing = true;
                        }
                    } else if fun.as_deref() == Some("close") {
                        break;
                    }
                } else if capturing {
                    if collect_note {
                        note_depth += 1;
                        if name == b"g" {
                            if let Some(r) = attr_val(&e, b"ref") {
                                let key = r.trim_start_matches('#').to_string();
                                if let Some(v) = gaiji.get(&key) {
                                    note_buf.push_str(v);
                                }
                            }
                        }
                    } else if skip_note_depth > 0 {
                        skip_note_depth += 1;
                    } else if name == b"note" {
                        if include_notes {
                            collect_note = true;
                            note_depth = 1;
                            note_buf.clear();
                        } else {
                            skip_note_depth = 1;
                        }
                    } else if name == b"lb" {
                        out.push('\n');
                    } else if name == b"pb" {
                        out.push('\n');
                        out.push('\n');
                    } else if name == b"g" {
                        if let Some(r) = attr_val(&e, b"ref") {
                            let key = r.trim_start_matches('#').to_string();
                            if let Some(v) = gaiji.get(&key) {
                                out.push_str(v);
                            }
                        }
                    }
                }
            }
            Ok(Event::Empty(e)) => {
                if !capturing {
                    // ignore
                } else {
                    let name_owned = e.name().as_ref().to_owned();
                    let name = local_name(&name_owned);
                    if collect_note {
                        if name == b"g" {
                            if let Some(r) = attr_val(&e, b"ref") {
                                let key = r.trim_start_matches('#').to_string();
                                if let Some(v) = gaiji.get(&key) {
                                    note_buf.push_str(v);
                                }
                            }
                        }
                    } else if skip_note_depth == 0 {
                        if name == b"lb" {
                            out.push('\n');
                        } else if name == b"pb" {
                            out.push('\n');
                            out.push('\n');
                        } else if name == b"g" {
                            if let Some(r) = attr_val(&e, b"ref") {
                                let key = r.trim_start_matches('#').to_string();
                                if let Some(v) = gaiji.get(&key) {
                                    out.push_str(v);
                                }
                            }
                        }
                    }
                }
            }
            Ok(Event::End(e)) => {
                if capturing {
                    let name_owned = e.name().as_ref().to_owned();
                    let name = local_name(&name_owned);
                    if collect_note {
                        note_depth = note_depth.saturating_sub(1);
                        if name == b"note" && note_depth == 0 {
                            let t = note_buf.split_whitespace().collect::<Vec<_>>().join(" ");
                            if !t.is_empty() {
                                out.push_str(" [注] ");
                                out.push_str(&t);
                            }
                            collect_note = false;
                            note_buf.clear();
                        }
                    } else if skip_note_depth > 0 {
                        skip_note_depth = skip_note_depth.saturating_sub(1);
                    }
                }
            }
            Ok(Event::Text(t)) => {
                if capturing {
                    let text = t.decode().unwrap_or_default().into_owned();
                    if collect_note {
                        note_buf.push_str(&text);
                    } else if skip_note_depth == 0 {
                        out.push_str(&text);
                    }
                }
            }
            Ok(Event::CData(t)) => {
                if capturing {
                    let text = String::from_utf8_lossy(&t).into_owned();
                    if collect_note {
                        note_buf.push_str(&text);
                    } else if skip_note_depth == 0 {
                        out.push_str(&text);
                    }
                }
            }
            Ok(Event::Eof) => break,
            Err(_) => break,
            _ => {}
        }
        buf.clear();
    }
    if out.trim().is_empty() {
        None
    } else {
        Some(normalize_plain_lines(&out))
    }
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
                    in_head = true;
                    head_buf.clear();
                }
                if lname.as_slice() == b"title" {
                    if path_stack.iter().any(|n| n.as_slice() == b"jhead") {
                        in_jhead_title = true;
                        jhead_buf.clear();
                    }
                }
            }
            Ok(Event::End(e)) => {
                let name_owned = e.name().as_ref().to_owned();
                let lname = local_name(&name_owned);
                if lname == b"head" && in_head {
                    let t = head_buf.split_whitespace().collect::<Vec<_>>().join(" ");
                    if !t.is_empty() {
                        heads.push(t);
                    }
                    in_head = false;
                    head_buf.clear();
                }
                if lname == b"title" && in_jhead_title {
                    let t = jhead_buf.split_whitespace().collect::<Vec<_>>().join(" ");
                    if !t.is_empty() {
                        heads.push(t);
                    }
                    in_jhead_title = false;
                    jhead_buf.clear();
                }
                path_stack.pop();
            }
            Ok(Event::Text(t)) => {
                let tx = t.decode().unwrap_or_default();
                if in_head {
                    head_buf.push_str(&tx);
                }
                if in_jhead_title {
                    jhead_buf.push_str(&tx);
                }
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
                if local_name(&name_owned) == b"head" {
                    in_head = true;
                    head_buf.clear();
                }
            }
            Ok(Event::End(e)) => {
                let name_owned = e.name().as_ref().to_owned();
                if local_name(&name_owned) == b"head" && in_head {
                    let t = head_buf.split_whitespace().collect::<Vec<_>>().join(" ");
                    if !t.is_empty() {
                        heads.push(t);
                    }
                    in_head = false;
                    head_buf.clear();
                }
            }
            Ok(Event::Text(t)) => {
                if in_head {
                    head_buf.push_str(&t.decode().unwrap_or_default());
                }
            }
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
    pub juan_number: Option<String>, // CBETA用
    pub section: Option<String>,     // 構造情報
    pub line_number: Option<usize>,  // マッチした行番号
}

#[derive(Serialize, Debug, Clone)]
pub struct FetchHints {
    pub recommended_parts: Vec<String>,
    pub total_content_size: Option<String>,
    pub structure_info: Vec<String>,
}

pub fn cbeta_grep(
    root: &Path,
    query: &str,
    max_results: usize,
    max_matches_per_file: usize,
) -> Vec<GrepResult> {
    // Build ripgrep matcher once (case-insensitive)
    let matcher = match RegexMatcherBuilder::new()
        .case_insensitive(true)
        .multi_line(true)
        .build(query)
    {
        Ok(m) => m,
        Err(_) => return Vec::new(),
    };

    // 1. まずTフォルダから優先的に検索
    let t_folder = root.join("T");
    let mut all_results = Vec::new();

    if t_folder.exists() {
        let t_results = cbeta_grep_internal(&t_folder, &matcher, max_results, max_matches_per_file);
        all_results.extend(t_results);
    }

    // 2. まだ結果が不足している場合は、他のフォルダも検索
    if all_results.len() < max_results {
        let remaining_limit = max_results - all_results.len();
        let other_results =
            cbeta_grep_internal_exclude_t(root, &matcher, remaining_limit, max_matches_per_file);
        all_results.extend(other_results);
    }

    // NOTE: Avoid building full CBETA index here; it is extremely expensive and
    // dominated cbeta_search latency. Keep ordering deterministic and cheap.
    all_results.sort_by(|a, b| {
        let a_is_t = a.file_id.starts_with('T');
        let b_is_t = b.file_id.starts_with('T');
        match (a_is_t, b_is_t) {
            (true, false) => std::cmp::Ordering::Less,
            (false, true) => std::cmp::Ordering::Greater,
            _ => match b.total_matches.cmp(&a.total_matches) {
                std::cmp::Ordering::Equal => a.file_id.cmp(&b.file_id),
                other => other,
            },
        }
    });

    all_results.truncate(max_results);
    all_results
}

fn grep_sort_best_first(results: &mut Vec<GrepResult>, max_results: usize) {
    // Prefer more matches, then stable file_id ordering.
    let cmp = |a: &GrepResult, b: &GrepResult| match b.total_matches.cmp(&a.total_matches) {
        std::cmp::Ordering::Equal => a.file_id.cmp(&b.file_id),
        other => other,
    };

    if max_results == 0 {
        results.clear();
        return;
    }
    if results.len() > max_results {
        // Keep only the best N candidates without sorting everything.
        let nth = max_results - 1;
        results.select_nth_unstable_by(nth, cmp);
        results.truncate(max_results);
    }
    results.sort_by(cmp);
}

/// Helper struct for collecting ripgrep search matches
#[derive(Debug, Clone)]
struct RgMatch {
    line_number: u64,
    line_content: String,
}

/// Generic ripgrep-based search function that returns matches per file
fn ripgrep_search_file(
    path: &Path,
    matcher: &grep_regex::RegexMatcher,
    max_matches: usize,
) -> Option<Vec<RgMatch>> {
    let mut matches: Vec<RgMatch> = Vec::new();

    // Use memory-mapped I/O for faster file access
    let mut searcher = SearcherBuilder::new()
        .binary_detection(BinaryDetection::quit(b'\x00'))
        .line_number(true)
        .memory_map(unsafe { grep_searcher::MmapChoice::auto() })
        .build();

    let result = searcher.search_path(
        matcher,
        path,
        UTF8(|line_num, line| {
            // Early exit if we have enough matches
            if matches.len() >= max_matches {
                return Ok(false); // Stop searching
            }

            matches.push(RgMatch {
                line_number: line_num,
                line_content: line.trim_end().to_string(),
            });
            Ok(true)
        }),
    );

    match result {
        Ok(_) => (!matches.is_empty()).then_some(matches),
        Err(_) => None,
    }
}

fn cbeta_grep_internal(
    root: &Path,
    matcher: &grep_regex::RegexMatcher,
    max_results: usize,
    max_matches_per_file: usize,
) -> Vec<GrepResult> {
    // Collect XML file paths using ignore crate
    let paths =
        collect_xml_paths_cached(&XML_PATHS_ALL_CACHE, root, |_, name| name.ends_with(".xml"));

    // Search files in parallel using ripgrep
    let mut results: Vec<GrepResult> = paths
        .par_iter()
        .filter_map(|p| {
            let rg_matches = ripgrep_search_file(p, &matcher, max_matches_per_file)?;

            // Get file size without reading entire content (faster)
            let file_size = std::fs::metadata(p).ok().map(|m| m.len()).unwrap_or(0);

            // Convert ripgrep matches to GrepMatch
            let grep_matches: Vec<GrepMatch> = rg_matches
                .iter()
                .map(|m| {
                    // Find the actual matched text in the line
                    let highlight = matcher
                        .find(m.line_content.as_bytes())
                        .ok()
                        .flatten()
                        .map(|mat| {
                            m.line_content
                                .get(mat.start()..mat.end())
                                .unwrap_or("")
                                .to_string()
                        })
                        .unwrap_or_default();

                    GrepMatch {
                        context: m.line_content.clone(),
                        highlight,
                        juan_number: None, // Skip expensive XML parsing for search
                        section: None,
                        line_number: Some(m.line_number as usize),
                    }
                })
                .collect();

            let file_id = stem_from(p);
            let title = file_id.clone();
            let total_matches = grep_matches.len();

            let fetch_hints = FetchHints {
                recommended_parts: vec![], // Populated on fetch, not search
                total_content_size: Some(format!("{}KB", file_size / 1024)),
                structure_info: vec![],
            };

            Some(GrepResult {
                file_path: p.to_string_lossy().to_string(),
                file_id,
                title,
                matches: grep_matches,
                total_matches,
                fetch_hints,
            })
        })
        .collect();

    // Make selection deterministic; otherwise parallel walk + take(N) yields unstable sets.
    grep_sort_best_first(&mut results, max_results);
    results
}

fn cbeta_grep_internal_exclude_t(
    root: &Path,
    matcher: &grep_regex::RegexMatcher,
    max_results: usize,
    max_matches_per_file: usize,
) -> Vec<GrepResult> {
    // Collect XML file paths excluding /T/ folder using ignore crate
    let paths = collect_xml_paths_cached(&CBETA_XML_PATHS_EXCLUDE_T_CACHE, root, |path, name| {
        name.ends_with(".xml") && !path.to_string_lossy().contains("/T/")
    });

    // Search files in parallel using ripgrep
    let mut results: Vec<GrepResult> = paths
        .par_iter()
        .filter_map(|p| {
            let rg_matches = ripgrep_search_file(p, &matcher, max_matches_per_file)?;

            // Get file size without reading entire content (faster)
            let file_size = std::fs::metadata(p).ok().map(|m| m.len()).unwrap_or(0);

            // Convert ripgrep matches to GrepMatch
            let grep_matches: Vec<GrepMatch> = rg_matches
                .iter()
                .map(|m| {
                    let highlight = matcher
                        .find(m.line_content.as_bytes())
                        .ok()
                        .flatten()
                        .map(|mat| {
                            m.line_content
                                .get(mat.start()..mat.end())
                                .unwrap_or("")
                                .to_string()
                        })
                        .unwrap_or_default();

                    GrepMatch {
                        context: m.line_content.clone(),
                        highlight,
                        juan_number: None, // Skip expensive XML parsing for search
                        section: None,
                        line_number: Some(m.line_number as usize),
                    }
                })
                .collect();

            let file_id = stem_from(p);
            let title = file_id.clone();
            let total_matches = grep_matches.len();

            let fetch_hints = FetchHints {
                recommended_parts: vec![], // Populated on fetch, not search
                total_content_size: Some(format!("{}KB", file_size / 1024)),
                structure_info: vec![],
            };

            Some(GrepResult {
                file_path: p.to_string_lossy().to_string(),
                file_id,
                title,
                matches: grep_matches,
                total_matches,
                fetch_hints,
            })
        })
        .collect();

    grep_sort_best_first(&mut results, max_results);
    results
}

/// Helper to read file with UTF-16 support (for Tipitaka)
fn read_file_with_encoding(path: &Path) -> Option<String> {
    let bytes = std::fs::read(path).ok()?;
    if bytes.starts_with(&[0xFF, 0xFE]) {
        match encoding_rs::UTF_16LE.decode(&bytes) {
            (decoded, _, false) => Some(decoded.into_owned()),
            _ => None,
        }
    } else if bytes.starts_with(&[0xFE, 0xFF]) {
        match encoding_rs::UTF_16BE.decode(&bytes) {
            (decoded, _, false) => Some(decoded.into_owned()),
            _ => None,
        }
    } else {
        String::from_utf8(bytes).ok()
    }
}

/// Ripgrep search on a string content (for UTF-16 files that need pre-processing)
fn ripgrep_search_content(
    content: &str,
    matcher: &grep_regex::RegexMatcher,
    max_matches: usize,
) -> Vec<RgMatch> {
    let mut results = Vec::new();
    let mut line_number = 0u64;

    for line in content.lines() {
        line_number += 1;
        if results.len() >= max_matches {
            break;
        }
        if matcher.find(line.as_bytes()).ok().flatten().is_some() {
            results.push(RgMatch {
                line_number,
                line_content: line.to_string(),
            });
        }
    }
    results
}

pub fn tipitaka_grep(
    root: &Path,
    query: &str,
    max_results: usize,
    max_matches_per_file: usize,
) -> Vec<GrepResult> {
    // Build ripgrep matcher (case-insensitive)
    let matcher = match RegexMatcherBuilder::new()
        .case_insensitive(true)
        .multi_line(true)
        .build(query)
    {
        Ok(m) => m,
        Err(_) => return Vec::new(),
    };

    // Collect XML file paths using ignore crate
    let paths = collect_xml_paths_cached(&TIPITAKA_XML_PATHS_CACHE, root, |_, name| {
        name.ends_with(".xml") && !name.contains("toc") && !name.contains("sitemap")
    });

    // Search files in parallel using ripgrep
    paths
        .par_iter()
        .filter_map(|p| {
            // UTF-16対応の読み込み
            let content = read_file_with_encoding(p)?;

            // Search using ripgrep matcher on content
            let rg_matches = ripgrep_search_content(&content, &matcher, max_matches_per_file);
            if rg_matches.is_empty() {
                return None;
            }

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
                if events > 5000 {
                    break;
                }
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
                            if let (Some(n), Some(type_val)) =
                                (attr_val(&e, b"n"), attr_val(&e, b"type"))
                            {
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

            // Convert ripgrep matches to GrepMatch
            let grep_matches: Vec<GrepMatch> = rg_matches
                .iter()
                .map(|m| {
                    let highlight = matcher
                        .find(m.line_content.as_bytes())
                        .ok()
                        .flatten()
                        .map(|mat| {
                            m.line_content
                                .get(mat.start()..mat.end())
                                .unwrap_or("")
                                .to_string()
                        })
                        .unwrap_or_default();

                    GrepMatch {
                        context: m.line_content.clone(),
                        highlight,
                        juan_number: None,
                        section: structure_info.first().cloned(),
                        line_number: Some(m.line_number as usize),
                    }
                })
                .collect();

            let file_id = stem_from(p);
            let title = [nikaya.as_deref(), book.as_deref()]
                .iter()
                .filter_map(|&s| s)
                .collect::<Vec<_>>()
                .join(" · ");
            let title = if title.is_empty() {
                file_id.clone()
            } else {
                title
            };
            let total_matches = grep_matches.len();

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
                total_matches,
                fetch_hints,
            })
        })
        .collect::<Vec<_>>()
        .into_iter()
        .take(max_results)
        .collect()
}

pub fn gretil_grep(
    root: &Path,
    query: &str,
    max_results: usize,
    max_matches_per_file: usize,
) -> Vec<GrepResult> {
    // Build ripgrep matcher (case-insensitive)
    let matcher = match RegexMatcherBuilder::new()
        .case_insensitive(true)
        .multi_line(true)
        .build(query)
    {
        Ok(m) => m,
        Err(_) => return Vec::new(),
    };

    // Collect XML file paths using ignore crate
    let paths =
        collect_xml_paths_cached(&XML_PATHS_ALL_CACHE, root, |_, name| name.ends_with(".xml"));

    // Search files in parallel using ripgrep
    paths
        .par_iter()
        .filter_map(|p| {
            let rg_matches = ripgrep_search_file(p, &matcher, max_matches_per_file)?;

            // Get file size without reading entire content (faster)
            let file_size = std::fs::metadata(p).ok().map(|m| m.len()).unwrap_or(0);

            // Convert ripgrep matches to GrepMatch
            let grep_matches: Vec<GrepMatch> = rg_matches
                .iter()
                .map(|m| {
                    let highlight = matcher
                        .find(m.line_content.as_bytes())
                        .ok()
                        .flatten()
                        .map(|mat| {
                            m.line_content
                                .get(mat.start()..mat.end())
                                .unwrap_or("")
                                .to_string()
                        })
                        .unwrap_or_default();

                    GrepMatch {
                        context: m.line_content.clone(),
                        highlight,
                        juan_number: None,
                        section: None,
                        line_number: Some(m.line_number as usize),
                    }
                })
                .collect();

            let file_id = stem_from(p);
            let title = file_id.clone();
            let total_matches = grep_matches.len();

            let fetch_hints = FetchHints {
                recommended_parts: vec!["full".to_string()],
                total_content_size: Some(format!("{}KB", file_size / 1024)),
                structure_info: Vec::new(),
            };

            Some(GrepResult {
                file_path: p.to_string_lossy().to_string(),
                file_id,
                title,
                matches: grep_matches,
                total_matches,
                fetch_hints,
            })
        })
        .collect::<Vec<_>>()
        .into_iter()
        .take(max_results)
        .collect()
}

pub fn sarit_grep(
    root: &Path,
    query: &str,
    max_results: usize,
    max_matches_per_file: usize,
) -> Vec<GrepResult> {
    let matcher = match RegexMatcherBuilder::new()
        .case_insensitive(true)
        .multi_line(true)
        .build(query)
    {
        Ok(m) => m,
        Err(_) => return Vec::new(),
    };

    let paths = collect_xml_paths_cached(&SARIT_XML_PATHS_CACHE, root, |path, name| {
        is_sarit_xml(path, name)
    });

    let mut results: Vec<GrepResult> = paths
        .par_iter()
        .filter_map(|p| {
            let rg_matches = ripgrep_search_file(p, &matcher, max_matches_per_file)?;
            let file_size = std::fs::metadata(p).ok().map(|m| m.len()).unwrap_or(0);

            let grep_matches: Vec<GrepMatch> = rg_matches
                .iter()
                .map(|m| {
                    let highlight = matcher
                        .find(m.line_content.as_bytes())
                        .ok()
                        .flatten()
                        .map(|mat| {
                            m.line_content
                                .get(mat.start()..mat.end())
                                .unwrap_or("")
                                .to_string()
                        })
                        .unwrap_or_default();

                    GrepMatch {
                        context: m.line_content.clone(),
                        highlight,
                        juan_number: None,
                        section: None,
                        line_number: Some(m.line_number as usize),
                    }
                })
                .collect();

            let file_id = stem_from(p);
            let title = file_id.clone();
            let total_matches = grep_matches.len();
            let fetch_hints = FetchHints {
                recommended_parts: vec!["full".to_string()],
                total_content_size: Some(format!("{}KB", file_size / 1024)),
                structure_info: Vec::new(),
            };
            Some(GrepResult {
                file_path: p.to_string_lossy().to_string(),
                file_id,
                title,
                matches: grep_matches,
                total_matches,
                fetch_hints,
            })
        })
        .collect::<Vec<_>>();

    grep_sort_best_first(&mut results, max_results);
    results
}

mod lib_line_extraction;
pub use lib_line_extraction::*;
#[cfg(test)]
mod tests_gretil {
    use super::*;
    use std::fs;

    #[test]
    fn gretil_grep_finds_match() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("sample.xml");
        let xml = r#"<TEI><text><body><p>namaste devī kṛṣṇa</p></body></text></TEI>"#;
        fs::write(&p, xml).unwrap();
        let results = gretil_grep(dir.path(), "kṛṣṇa", 10, 3);
        assert_eq!(results.len(), 1);
        assert!(results[0].matches.len() >= 1);
    }
}

#[cfg(test)]
mod tests_cbeta_index_and_plain {
    use super::*;
    use std::fs;

    #[test]
    fn build_cbeta_index_prefers_cjk_title_over_collection_title() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("T0001.xml");
        let xml = r#"
<TEI xml:id="T0001">
  <teiHeader>
    <fileDesc>
      <titleStmt>
        <title xml:lang="en">Taishō Tripiṭaka</title>
        <title xml:lang="zh">妙法蓮華經</title>
      </titleStmt>
    </fileDesc>
  </teiHeader>
  <text><body><p>須彌山</p></body></text>
</TEI>
"#;
        fs::write(&p, xml).unwrap();
        let idx = build_cbeta_index(dir.path());
        assert_eq!(idx.len(), 1);
        assert_eq!(idx[0].id, "T0001");
        assert_eq!(idx[0].title, "妙法蓮華經");
        let ver = idx[0]
            .meta
            .as_ref()
            .and_then(|m| m.get("indexVersion"))
            .map(|s| s.as_str());
        assert_eq!(ver, Some("cbeta_index_v2"));
    }

    #[test]
    fn extract_cbeta_plain_skips_header_resolves_gaiji_and_preserves_breaks() {
        let xml = r##"
<TEI>
  <teiHeader>
    <encodingDesc>
      <charDecl>
        <char xml:id="CB00416">
          <mapping type="unicode">佛</mapping>
        </char>
      </charDecl>
    </encodingDesc>
    <fileDesc><titleStmt><title>HEADER TITLE</title></titleStmt></fileDesc>
  </teiHeader>
  <text><body>
    <p>甲<lb n="0001a01"/>乙<g ref="#CB00416"/>丙<pb n="0001a02"/>丁<note>注釈</note></p>
  </body></text>
</TEI>
"##;

        let out = extract_cbeta_plain_from_xml(xml, false);
        assert!(!out.contains("HEADER TITLE"));
        assert!(!out.contains("注釈"));
        assert!(out.contains("甲\n乙佛丙\n\n丁"));

        let out2 = extract_cbeta_plain_from_xml(xml, true);
        assert!(out2.contains("[注] 注釈"));
    }

    #[test]
    fn extract_cbeta_plain_from_snippet_resolves_gaiji() {
        let xml = r#"
<TEI>
  <teiHeader>
    <encodingDesc>
      <charDecl>
        <char xml:id="CB00416"><mapping type="unicode">佛</mapping></char>
      </charDecl>
    </encodingDesc>
  </teiHeader>
  <text><body><p>dummy</p></body></text>
</TEI>
"#;
        let gaiji = cbeta_gaiji_map_fast(xml);
        let snippet = r##"<p>乙<g ref="#CB00416"/>丙</p>"##;
        let out = extract_cbeta_plain_from_snippet(snippet, &gaiji, false);
        assert_eq!(out, "乙佛丙");
    }

    #[test]
    fn cbeta_grep_is_deterministic_and_variant_safe_under_limit() {
        let dir = tempfile::tempdir().unwrap();
        let t01 = dir.path().join("T").join("T01");
        fs::create_dir_all(&t01).unwrap();
        // Two T files with different match counts.
        fs::write(t01.join("T01n0001.xml"), "<TEI>foo</TEI>\n").unwrap();
        fs::write(t01.join("T01n0002.xml"), "<TEI>foo\nfoo</TEI>\n").unwrap();
        // Non-T match should not outrank T results.
        let a01 = dir.path().join("A").join("A01");
        fs::create_dir_all(&a01).unwrap();
        fs::write(a01.join("A01n0001.xml"), "<TEI>foo\nfoo\nfoo</TEI>\n").unwrap();

        // Run multiple times; should always pick the same top-1 due to deterministic selection.
        for _ in 0..10 {
            let r = cbeta_grep(dir.path(), "foo", 1, 10);
            assert_eq!(r.len(), 1);
            assert_eq!(r[0].file_id, "T01n0002");
        }

        let r2 = cbeta_grep(dir.path(), "foo", 2, 10);
        assert_eq!(r2.len(), 2);
        assert_eq!(r2[0].file_id, "T01n0002");
        assert_eq!(r2[1].file_id, "T01n0001");
    }
}
