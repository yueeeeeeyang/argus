//! 文件职责：提供高亮规则之间共享的 ASCII 扫描工具。
//! 创建日期：2026-06-11
//! 修改日期：2026-06-11
//! 作者：Argus 开发团队
//! 主要功能：集中处理 key-value、字符串、数字、标识符和字节子串扫描等通用逻辑。

use std::ops::Range;

use crate::highlight::span::{HighlightTokenKind, SpanBuilder};

/// 高亮日志中的 key=value 或 key: value 字段。
pub(crate) fn highlight_key_values(line: &str, builder: &mut SpanBuilder) {
    let bytes = line.as_bytes();
    let mut index = 0_usize;
    while index < bytes.len() {
        if !is_identifier_start(bytes[index]) {
            index += 1;
            continue;
        }
        let key_start = index;
        index += 1;
        while index < bytes.len() && is_identifier_continue(bytes[index]) {
            index += 1;
        }
        let key_end = index;
        let value_sep = skip_ascii_spaces(bytes, index);
        if !bytes
            .get(value_sep)
            .is_some_and(|byte| matches!(*byte, b'=' | b':'))
        {
            continue;
        }
        let value_start = skip_ascii_spaces(bytes, value_sep + 1);
        let value_end = scan_value_end(bytes, value_start);
        builder.push(key_start, key_end, HighlightTokenKind::Key);
        builder.push(value_sep, value_sep + 1, HighlightTokenKind::Punctuation);
        highlight_scalar_value(line, value_start, value_end, builder);
        index = value_end.max(value_start + 1);
    }
}

/// 高亮普通标量值。
pub(crate) fn highlight_scalar_value(
    line: &str,
    mut start: usize,
    mut end: usize,
    builder: &mut SpanBuilder,
) {
    if start >= end || start >= line.len() {
        return;
    }
    let bytes = line.as_bytes();
    while start < end && bytes[start].is_ascii_whitespace() {
        start += 1;
    }
    while end > start && bytes[end - 1].is_ascii_whitespace() {
        end -= 1;
    }
    if start >= end {
        return;
    }

    let value = &line[start..end];
    let kind = if value.starts_with('"') || value.starts_with('\'') {
        HighlightTokenKind::String
    } else if is_boolean_like(value) {
        HighlightTokenKind::Boolean
    } else if is_number_like(value) {
        HighlightTokenKind::Number
    } else {
        HighlightTokenKind::Value
    };
    builder.push(start, end, kind);
}

/// 判断值是否类似布尔值或 null。
pub(crate) fn is_boolean_like(value: &str) -> bool {
    matches!(
        value.trim().to_ascii_lowercase().as_str(),
        "true" | "false" | "null" | "yes" | "no" | "on" | "off"
    )
}

/// 判断值是否为简单数字。
pub(crate) fn is_number_like(value: &str) -> bool {
    let value = value.trim();
    !value.is_empty()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'.' | b'-' | b'+'))
        && value.bytes().any(|byte| byte.is_ascii_digit())
}

/// 查找大小写不敏感的 ASCII 单词范围。
pub(crate) fn find_ascii_word_ranges(line: &str, needle: &str) -> Vec<Range<usize>> {
    find_ascii_case_insensitive_ranges(line, needle)
        .into_iter()
        .filter(|range| is_word_boundary(line.as_bytes(), range.start, range.end))
        .collect()
}

/// 查找大小写不敏感的 ASCII 子串范围。
pub(crate) fn find_ascii_case_insensitive_ranges(line: &str, needle: &str) -> Vec<Range<usize>> {
    let haystack = line.as_bytes();
    let needle = needle.as_bytes();
    if needle.is_empty() || haystack.len() < needle.len() {
        return Vec::new();
    }

    let mut ranges = Vec::new();
    for start in 0..=haystack.len() - needle.len() {
        if haystack[start..start + needle.len()]
            .iter()
            .zip(needle)
            .all(|(left, right)| left.eq_ignore_ascii_case(right))
        {
            ranges.push(start..start + needle.len());
        }
    }
    ranges
}

/// 判断范围两侧是否为单词边界。
pub(crate) fn is_word_boundary(bytes: &[u8], start: usize, end: usize) -> bool {
    let left_ok = start == 0 || !is_identifier_continue(bytes[start - 1]);
    let right_ok = end >= bytes.len() || !is_identifier_continue(bytes[end]);
    left_ok && right_ok
}

/// 跳过 ASCII 空白。
pub(crate) fn skip_ascii_spaces(bytes: &[u8], mut index: usize) -> usize {
    while bytes.get(index).is_some_and(u8::is_ascii_whitespace) {
        index += 1;
    }
    index
}

/// 扫描 key-value 中的值结束位置。
pub(crate) fn scan_value_end(bytes: &[u8], mut index: usize) -> usize {
    if bytes
        .get(index)
        .is_some_and(|byte| matches!(*byte, b'"' | b'\''))
    {
        return scan_quoted_string(bytes, index, bytes[index]);
    }
    while bytes
        .get(index)
        .is_some_and(|byte| !byte.is_ascii_whitespace() && *byte != b',')
    {
        index += 1;
    }
    index
}

/// 扫描带转义的引号字符串。
pub(crate) fn scan_quoted_string(bytes: &[u8], start: usize, quote: u8) -> usize {
    let mut index = start + 1;
    let mut escaped = false;
    while index < bytes.len() {
        if escaped {
            escaped = false;
        } else if bytes[index] == b'\\' {
            escaped = true;
        } else if bytes[index] == quote {
            return index + 1;
        }
        index += 1;
    }
    bytes.len()
}

/// 扫描 JSON/YAML 数字。
pub(crate) fn scan_json_number(bytes: &[u8], mut index: usize) -> usize {
    if bytes.get(index) == Some(&b'-') {
        index += 1;
    }
    while bytes.get(index).is_some_and(u8::is_ascii_digit) {
        index += 1;
    }
    if bytes.get(index) == Some(&b'.') {
        index += 1;
        while bytes.get(index).is_some_and(u8::is_ascii_digit) {
            index += 1;
        }
    }
    if bytes
        .get(index)
        .is_some_and(|byte| matches!(*byte, b'e' | b'E'))
    {
        index += 1;
        if bytes
            .get(index)
            .is_some_and(|byte| matches!(*byte, b'+' | b'-'))
        {
            index += 1;
        }
        while bytes.get(index).is_some_and(u8::is_ascii_digit) {
            index += 1;
        }
    }
    index
}

/// 判断指定位置是否匹配完整字面量。
pub(crate) fn starts_with_literal(bytes: &[u8], index: usize, literal: &[u8]) -> bool {
    starts_with_at(bytes, index, literal)
        && is_word_boundary(bytes, index, index.saturating_add(literal.len()))
}

/// 判断字节切片在指定位置是否有前缀。
pub(crate) fn starts_with_at(bytes: &[u8], index: usize, prefix: &[u8]) -> bool {
    bytes
        .get(index..index.saturating_add(prefix.len()))
        .is_some_and(|slice| slice == prefix)
}

/// 查找字节子串。
pub(crate) fn find_bytes(bytes: &[u8], start: usize, needle: &[u8]) -> Option<usize> {
    bytes[start..]
        .windows(needle.len())
        .position(|window| window == needle)
        .map(|offset| start + offset)
}

/// 扫描 ASCII 标识符。
pub(crate) fn scan_ascii_identifier(bytes: &[u8], mut index: usize) -> usize {
    while bytes
        .get(index)
        .is_some_and(|byte| byte.is_ascii_alphanumeric() || matches!(*byte, b'_' | b'-' | b'.'))
    {
        index += 1;
    }
    index
}

/// 将点号分隔的 Java/配置 token 拆成多个语法片段，避免选区和颜色把整条链路当成一个词。
///
/// 参数说明：
/// - `builder`：高亮范围构造器。
/// - `token_start`：token 在原始行中的起始字节下标。
/// - `token`：待拆分的 token 文本。
/// - `kind`：非点号片段使用的高亮类型。
pub(crate) fn push_dotted_token_segments(
    builder: &mut SpanBuilder,
    token_start: usize,
    token: &str,
    kind: HighlightTokenKind,
) {
    let mut segment_start = token_start;
    for (offset, character) in token.char_indices() {
        if character != '.' {
            continue;
        }

        let dot_start = token_start + offset;
        builder.push(segment_start, dot_start, kind);
        builder.push(dot_start, dot_start + 1, HighlightTokenKind::Punctuation);
        segment_start = dot_start + 1;
    }

    builder.push(segment_start, token_start + token.len(), kind);
}

/// 判断标识符起始字节。
pub(crate) fn is_identifier_start(byte: u8) -> bool {
    byte.is_ascii_alphabetic() || matches!(byte, b'_' | b'$')
}

/// 判断标识符后续字节。
pub(crate) fn is_identifier_continue(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.' | b'$')
}

/// 判断 Java 标识符起始字节。
pub(crate) fn is_java_identifier_start(byte: u8) -> bool {
    byte.is_ascii_alphabetic() || matches!(byte, b'_' | b'$')
}

/// 判断 Java token 字节。
pub(crate) fn is_java_token_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'$' | b'.')
}

/// 判断 XML 名称起始字节。
pub(crate) fn is_xml_name_start(byte: u8) -> bool {
    byte.is_ascii_alphabetic() || matches!(byte, b'_' | b':')
}

/// 判断 XML 名称字节。
pub(crate) fn is_xml_name_byte(byte: u8) -> bool {
    is_xml_name_start(byte) || byte.is_ascii_digit() || matches!(byte, b'-' | b'.')
}
