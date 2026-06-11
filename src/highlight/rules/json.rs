//! 文件职责：实现 JSON 配置行的高亮规则。
//! 创建日期：2026-06-11
//! 修改日期：2026-06-11
//! 作者：Argus 开发团队
//! 主要功能：识别 JSON key、字符串、数字、布尔/null 和结构标点。

use crate::highlight::rules::common::{
    scan_ascii_identifier, scan_json_number, scan_quoted_string, skip_ascii_spaces,
    starts_with_literal,
};
use crate::highlight::span::{HighlightTokenKind, SpanBuilder};

/// 高亮 JSON 行。
pub(crate) fn highlight_json(line: &str, builder: &mut SpanBuilder) {
    let bytes = line.as_bytes();
    let mut index = 0_usize;
    while index < bytes.len() {
        match bytes[index] {
            b'"' => {
                let end = scan_quoted_string(bytes, index, b'"');
                let after = skip_ascii_spaces(bytes, end);
                let kind = if bytes.get(after) == Some(&b':') {
                    HighlightTokenKind::Key
                } else {
                    HighlightTokenKind::String
                };
                builder.push(index, end, kind);
                index = end;
            }
            b'{' | b'}' | b'[' | b']' | b':' | b',' => {
                builder.push(index, index + 1, HighlightTokenKind::Punctuation);
                index += 1;
            }
            b'-' | b'0'..=b'9' => {
                let end = scan_json_number(bytes, index);
                builder.push(index, end, HighlightTokenKind::Number);
                index = end;
            }
            _ if starts_with_literal(bytes, index, b"true")
                || starts_with_literal(bytes, index, b"false")
                || starts_with_literal(bytes, index, b"null") =>
            {
                let end = scan_ascii_identifier(bytes, index);
                builder.push(index, end, HighlightTokenKind::Boolean);
                index = end;
            }
            _ => index += 1,
        }
    }
}
