//! 文件职责：实现 CSS 样式表单行高亮规则。
//! 创建日期：2026-07-15
//! 修改日期：2026-07-15
//! 作者：Argus 开发团队
//! 主要功能：识别选择器、属性、at-rule、自定义变量、字符串、颜色、数字、函数和注释。

use crate::highlight::rules::code_common::{
    contains_word, find_block_comment_end, scan_code_number, scan_operator,
};
use crate::highlight::rules::common::{scan_quoted_string, skip_ascii_spaces};
use crate::highlight::span::{HighlightTokenKind, SpanBuilder};

/// CSS 全局值和条件关键字。
const CSS_KEYWORDS: &[&str] = &[
    "and",
    "auto",
    "currentColor",
    "from",
    "inherit",
    "initial",
    "none",
    "not",
    "only",
    "or",
    "revert",
    "revert-layer",
    "to",
    "unset",
];

/// 高亮 CSS 代码行。
pub(crate) fn highlight_css(line: &str, builder: &mut SpanBuilder) {
    let bytes = line.as_bytes();
    let selector_end = bytes.iter().position(|byte| *byte == b'{');
    let mut index = 0_usize;
    while index < bytes.len() {
        if bytes.get(index..index + 2) == Some(b"/*") {
            let end = find_block_comment_end(bytes, index + 2);
            builder.push(index, end, HighlightTokenKind::Comment);
            index = end;
            continue;
        }

        match bytes[index] {
            b'"' | b'\'' => {
                let end = scan_quoted_string(bytes, index, bytes[index]);
                builder.push(index, end, HighlightTokenKind::String);
                index = end;
            }
            b'@' if bytes
                .get(index + 1)
                .is_some_and(|byte| is_css_name_byte(*byte)) =>
            {
                let end = scan_css_name(bytes, index + 1);
                builder.push(index, end, HighlightTokenKind::Annotation);
                index = end;
            }
            b'-' if bytes.get(index + 1) == Some(&b'-') => {
                let end = scan_css_name(bytes, index + 2);
                builder.push(index, end, HighlightTokenKind::Variable);
                index = end;
            }
            b'#' if bytes.get(index + 1).is_some_and(u8::is_ascii_hexdigit) => {
                let mut end = index + 1;
                while bytes.get(end).is_some_and(u8::is_ascii_hexdigit) {
                    end += 1;
                }
                builder.push(index, end, HighlightTokenKind::Number);
                index = end;
            }
            b'0'..=b'9' => {
                let end = scan_code_number(bytes, index);
                builder.push(index, end, HighlightTokenKind::Number);
                index = end;
            }
            byte if is_css_name_start(byte) => {
                let end = scan_css_name(bytes, index + 1);
                let token = &line[index..end];
                let next = skip_ascii_spaces(bytes, end);
                let kind = if selector_end.is_some_and(|brace| index < brace) {
                    HighlightTokenKind::Selector
                } else if bytes.get(next) == Some(&b':') {
                    HighlightTokenKind::Attribute
                } else if bytes.get(next) == Some(&b'(') {
                    HighlightTokenKind::Function
                } else if contains_word(CSS_KEYWORDS, token, false) {
                    HighlightTokenKind::Keyword
                } else {
                    HighlightTokenKind::Value
                };
                builder.push(index, end, kind);
                index = end;
            }
            b'{' | b'}' | b'(' | b')' | b'[' | b']' | b',' | b';' => {
                builder.push(index, index + 1, HighlightTokenKind::Punctuation);
                index += 1;
            }
            b':' | b'>' | b'+' | b'~' | b'=' | b'*' | b'/' | b'!' => {
                let end = scan_operator(bytes, index);
                builder.push(index, end, HighlightTokenKind::Operator);
                index = end;
            }
            _ => index += 1,
        }
    }
}

/// 扫描允许连字符的 CSS 名称。
fn scan_css_name(bytes: &[u8], mut index: usize) -> usize {
    while bytes.get(index).is_some_and(|byte| is_css_name_byte(*byte)) {
        index += 1;
    }
    index
}

/// 判断 CSS 名称首字节。
fn is_css_name_start(byte: u8) -> bool {
    byte.is_ascii_alphabetic() || matches!(byte, b'_' | b'-')
}

/// 判断 CSS 名称后续字节。
fn is_css_name_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-')
}
