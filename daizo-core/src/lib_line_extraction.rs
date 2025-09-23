/// 行数を指定してその周辺のテキストを取得（前後異なる行数指定）
pub fn extract_text_around_line_asymmetric(
    content: &str,
    target_line: usize,
    context_before: usize,
    context_after: usize,
) -> String {
    let lines: Vec<&str> = content.lines().collect();
    let total_lines = lines.len();

    if target_line == 0 || target_line > total_lines {
        return String::new();
    }

    // 0ベースのインデックスに変換
    let target_idx = target_line - 1;

    let start_idx = target_idx.saturating_sub(context_before);
    let end_idx = std::cmp::min(target_idx + context_after + 1, total_lines);

    lines[start_idx..end_idx].join("\n")
}

/// 行数を指定してその周辺のテキストを取得（後方互換性のため）
pub fn extract_text_around_line(content: &str, target_line: usize, context_lines: usize) -> String {
    extract_text_around_line_asymmetric(content, target_line, context_lines, context_lines)
}

/// XMLファイルから行数指定でテキストを抽出（プレーンテキスト化、前後異なる行数指定）
pub fn extract_xml_around_line_asymmetric(
    xml_content: &str,
    target_line: usize,
    context_before: usize,
    context_after: usize,
) -> String {
    use crate::extract_text;

    // まず指定行数の周辺を取得
    let raw_context = extract_text_around_line_asymmetric(
        xml_content,
        target_line,
        context_before,
        context_after,
    );

    // XMLタグを除去してプレーンテキスト化
    extract_text(&raw_context)
}

/// XMLファイルから行数指定でテキストを抽出（プレーンテキスト化、後方互換性のため）
pub fn extract_xml_around_line(
    xml_content: &str,
    target_line: usize,
    context_lines: usize,
) -> String {
    extract_xml_around_line_asymmetric(xml_content, target_line, context_lines, context_lines)
}
