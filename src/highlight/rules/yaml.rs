//! 文件职责：实现 YAML 配置行的高亮规则。
//! 创建日期：2026-06-11
//! 修改日期：2026-06-11
//! 作者：Argus 开发团队
//! 主要功能：识别 YAML key、标量值、列表标点、数字、布尔值和注释。

use crate::highlight::rules::common::{
    is_boolean_like, is_identifier_start, scan_ascii_identifier, scan_json_number,
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

    if trimmed.starts_with("- ") {
        builder.push(
            trimmed_start,
            trimmed_start + 1,
            HighlightTokenKind::Punctuation,
        );
    }

    if let Some(colon) = line[..syntax_end].find(':') {
        let key_start = trimmed_start;
        let key_end = line[..colon].trim_end().len();
        builder.push(key_start, key_end, HighlightTokenKind::Key);
        builder.push(colon, colon + 1, HighlightTokenKind::Punctuation);
        let value_start = skip_ascii_spaces(line.as_bytes(), colon + 1);
        highlight_yaml_value(line, value_start, syntax_end, builder);
    } else {
        highlight_yaml_value(line, trimmed_start, syntax_end, builder);
    }
}

/// 查找 YAML 行内注释起点。
fn find_yaml_comment(line: &str) -> Option<usize> {
    let bytes = line.as_bytes();
    let mut quote: Option<u8> = None;
    let mut index = 0_usize;
    while index < bytes.len() {
        match (bytes[index], quote) {
            (b'\'' | b'"', None) => quote = Some(bytes[index]),
            (byte, Some(active)) if byte == active => quote = None,
            (b'#', None) if index == 0 || bytes[index - 1].is_ascii_whitespace() => {
                return Some(index);
            }
            _ => {}
        }
        index += 1;
    }
    None
}

/// 高亮 YAML 值区域。
fn highlight_yaml_value(line: &str, start: usize, end: usize, builder: &mut SpanBuilder) {
    let bytes = line.as_bytes();
    let mut index = start;
    while index < end {
        match bytes[index] {
            b'"' | b'\'' => {
                let end_quote = scan_quoted_string(bytes, index, bytes[index]).min(end);
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
                let token_end = scan_ascii_identifier(bytes, index).min(end);
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
