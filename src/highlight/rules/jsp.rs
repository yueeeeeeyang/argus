//! 文件职责：提供 JSP 指令和 EL 表达式的 token 分类规则。
//! 创建日期：2026-07-15
//! 修改日期：2026-07-15
//! 作者：Argus 开发团队
//! 主要功能：复用统一词法层给出的片段边界，分类 JSP 指令属性和 EL 变量；跨行混合语言由 lexical 模块协调。

use crate::highlight::rules::code_common::{is_code_identifier_start, scan_code_identifier};
use crate::highlight::rules::common::skip_ascii_spaces;
use crate::highlight::span::{HighlightTokenKind, SpanBuilder};

/// JSP 指令名称；指令属性随后按 `name=value` 结构识别。
const JSP_DIRECTIVES: &[&str] = &["page", "include", "taglib", "attribute", "variable", "tag"];

/// 分类已屏蔽字符串的 JSP 指令名称、属性名和等号。
pub(crate) fn highlight_jsp_directive(segment: &str, offset: usize, builder: &mut SpanBuilder) {
    let bytes = segment.as_bytes();
    let mut index = 0_usize;
    while index < bytes.len() {
        if is_code_identifier_start(bytes[index]) {
            let end = scan_code_identifier(bytes, index);
            let next = skip_ascii_spaces(bytes, end);
            let kind = if JSP_DIRECTIVES.contains(&&segment[index..end]) {
                HighlightTokenKind::Keyword
            } else if bytes.get(next) == Some(&b'=') {
                HighlightTokenKind::Attribute
            } else {
                HighlightTokenKind::Value
            };
            builder.push(offset + index, offset + end, kind);
            if bytes.get(next) == Some(&b'=') {
                builder.push(
                    offset + next,
                    offset + next + 1,
                    HighlightTokenKind::Operator,
                );
            }
            index = end;
            continue;
        }
        index += 1;
    }
}

/// 高亮 `${...}` 和 `#{...}` EL 表达式中的变量与边界符号。
pub(crate) fn highlight_expression_language(line: &str, builder: &mut SpanBuilder) {
    let bytes = line.as_bytes();
    let mut index = 0_usize;
    while index + 2 <= bytes.len() {
        if matches!(bytes[index], b'$' | b'#') && bytes.get(index + 1) == Some(&b'{') {
            let end = bytes[index + 2..]
                .iter()
                .position(|byte| *byte == b'}')
                .map(|offset| index + 2 + offset)
                .unwrap_or(bytes.len());
            builder.push(index, index + 2, HighlightTokenKind::Punctuation);
            let mut token_start = index + 2;
            while token_start < end {
                if is_code_identifier_start(bytes[token_start]) {
                    let token_end = scan_code_identifier(bytes, token_start);
                    builder.push(token_start, token_end, HighlightTokenKind::Variable);
                    token_start = token_end;
                } else {
                    token_start += 1;
                }
            }
            if end < bytes.len() {
                builder.push(end, end + 1, HighlightTokenKind::Punctuation);
                index = end + 1;
            } else {
                return;
            }
        } else {
            index += 1;
        }
    }
}
