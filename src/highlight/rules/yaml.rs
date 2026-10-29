//! 文件职责：实现 YAML 配置行的高亮规则。
//! 创建日期：2026-06-11
//! 修改日期：2026-06-11
//! 作者：Argus 开发团队
//! 主要功能：识别 YAML key、标量值、列表标点、数字、布尔值和注释。

use crate::highlight::rules::common::{
    is_boolean_like, is_identifier_continue, is_identifier_start, scan_json_number,
    scan_quoted_string, skip_ascii_spaces,
};
use crate::highlight::span::{HighlightTokenKind, SpanBuilder};

/// 高亮 YAML 行。
pub(crate) fn highlight_yaml(line: &str, builder: &mut SpanBuilder) {
    let trimmed_start = line.len() - line.trim_start().len();
    let trimmed = &line[trimmed_start..];
    if trimmed.starts_with('#') {
        builder.push(trimmed_start, line.len(), HighlightTokenKind::Comment);
        return;
    }

    let comment_start = find_yaml_comment(line);
    let syntax_end = comment_start.unwrap_or(line.len());
    if let Some(comment_start) = comment_start {
        builder.push(comment_start, line.len(), HighlightTokenKind::Comment);
    }

    let content_start = if trimmed.starts_with("- ") {
        builder.push(
            trimmed_start,
            trimmed_start + 1,
            HighlightTokenKind::Punctuation,
        );
        skip_ascii_spaces(line.as_bytes(), trimmed_start + 1)
    } else {
        trimmed_start
    };

    if let Some(colon) = find_yaml_mapping_colon(line, content_start, syntax_end) {
        let key_start = content_start;
        let key_end = line[..colon].trim_end().len();
        builder.push(key_start, key_end, HighlightTokenKind::Key);
        builder.push(colon, colon + 1, HighlightTokenKind::Punctuation);
        let value_start = skip_ascii_spaces(line.as_bytes(), colon + 1);
        highlight_yaml_value(line, value_start, syntax_end, builder);
    } else {
        highlight_yaml_value(line, content_start, syntax_end, builder);
    }
}

/// 查找 YAML 行内注释起点。
fn find_yaml_comment(line: &str) -> Option<usize> {
    let bytes = line.as_bytes();
    let mut index = 0_usize;
    while index < bytes.len() {
        match bytes[index] {
            b'\'' | b'"' => {
                index = scan_yaml_quoted_string(bytes, index);
                continue;
            }
            b'#' if index == 0 || bytes[index - 1].is_ascii_whitespace() => {
                return Some(index);
            }
            _ => {}
        }
        index += 1;
    }
    None
}

/// 引号中的冒号属于字符串内容，不能截断键或把纯字符串误识别为映射。
fn find_yaml_mapping_colon(line: &str, start: usize, end: usize) -> Option<usize> {
    let bytes = line.as_bytes();
    let is_quoted_key = matches!(bytes.get(start), Some(b'\'' | b'"'));
    let mut index = start;
    while index < end {
        match bytes[index] {
            b'\'' | b'"' => index = scan_yaml_quoted_string(bytes, index),
            // 普通键后的分隔冒号需要空白或行尾，URL 的协议冒号仍属于标量。
            // 带引号的键沿用 YAML 支持的 JSON 风格，可以直接接值。
            b':' if is_quoted_key || index + 1 == end || bytes[index + 1].is_ascii_whitespace() => {
                return Some(index);
            }
            _ => index += 1,
        }
    }
    None
}

/// YAML 双引号使用反斜杠转义，单引号仅以连续两个单引号表示引号本身。
pub(crate) fn scan_yaml_quoted_string(bytes: &[u8], start: usize) -> usize {
    if bytes[start] == b'"' {
        return scan_quoted_string(bytes, start, b'"');
    }
    let mut index = start + 1;
    while index < bytes.len() {
        if bytes[index] == b'\'' {
            if bytes.get(index + 1) == Some(&b'\'') {
                index += 2;
                continue;
            }
            return index + 1;
        }
        index += 1;
    }
    bytes.len()
}

/// 高亮 YAML 值区域。
fn highlight_yaml_value(line: &str, start: usize, end: usize, builder: &mut SpanBuilder) {
    let bytes = line.as_bytes();
    let mut index = start;
    while index < end {
        match bytes[index] {
            b'"' | b'\'' => {
                let end_quote = scan_yaml_quoted_string(bytes, index).min(end);
                builder.push(index, end_quote, HighlightTokenKind::String);
                index = end_quote;
            }
            b'[' | b']' | b'{' | b'}' | b',' => {
                builder.push(index, index + 1, HighlightTokenKind::Punctuation);
                index += 1;
            }
            b'-' | b'0'..=b'9' => {
                let number_end = scan_json_number(bytes, index).min(end);
                builder.push(index, number_end, HighlightTokenKind::Number);
                index = number_end;
            }
            _ if is_identifier_start(bytes[index]) => {
                // 起点与后续字符采用同一规则，确保美元符号开头的标量也能前进。
                let mut token_end = index + 1;
                while token_end < end && is_identifier_continue(bytes[token_end]) {
                    token_end += 1;
                }
                let token = &line[index..token_end];
                if is_boolean_like(token) {
                    builder.push(index, token_end, HighlightTokenKind::Boolean);
                } else {
                    builder.push(index, token_end, HighlightTokenKind::Value);
                }
                index = token_end;
            }
            _ => index += 1,
        }
    }
}
