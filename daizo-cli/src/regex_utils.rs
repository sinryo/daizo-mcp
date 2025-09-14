pub fn ws_fuzzy_regex(s: &str) -> String {
    let mut out = String::new();
    let mut in_ws = false;
    for ch in s.chars() {
        if ch.is_whitespace() {
            if !in_ws { out.push_str("\\s*"); in_ws = true; }
        } else {
            in_ws = false;
            out.push_str(&regex::escape(&ch.to_string()));
        }
    }
    out
}

