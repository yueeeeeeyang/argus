//! 文件职责：实现 JSP 页面模板单行高亮规则。
//! 创建日期：2026-07-15
//! 修改日期：2026-07-15
//! 作者：Argus 开发团队
//! 主要功能：在同一行内组合 XML/HTML、JSP 指令、Java scriptlet 与 EL 表达式高亮。

use crate::highlight::rules::code_common::{
    highlight_segment_with_offset, is_code_identifier_start, scan_code_identifier,
};
use crate::highlight::rules::common::{scan_quoted_string, skip_ascii_spaces};
use crate::highlight::rules::{java, xml};
use crate::highlight::span::{HighlightTokenKind, SpanBuilder};

/// JSP 指令名称；指令属性随后按 `name=value` 结构识别。
const JSP_DIRECTIVES: &[&str] = &["page", "include", "taglib", "attribute", "variable", "tag"];

/// 高亮 JSP 模板行。
pub(crate) fn highlight_jsp(line: &str, builder: &mut SpanBuilder) {
    // JSP 注释优先级最高，注释中的 `${...}` 或 `<%...%>` 不应被当作可执行模板继续着色。
    highlight_jsp_comments(line, builder);
    // EL 表达式可能位于 HTML 属性字符串中，先加入以保证变量语义优先于整段字符串颜色。
    highlight_expression_language(line, builder);

    let bytes = line.as_bytes();
    let mut cursor = 0_usize;
    while let Some(relative_start) = find_bytes(bytes, cursor, b"<%") {
        let start = relative_start;
        highlight_segment_with_offset(&line[cursor..start], cursor, builder, xml::highlight_xml);

        if bytes.get(start..start + 4) == Some(b"<%--") {
            cursor = find_bytes(bytes, start + 4, b"--%>")
                .map(|end| end + 4)
                .unwrap_or(bytes.len());
            continue;
        }

        let content_marker = bytes.get(start + 2).copied();
        let content_start =
            start + 2 + usize::from(matches!(content_marker, Some(b'@' | b'=' | b'!')));
        let end_marker = find_bytes(bytes, content_start, b"%>");
        let content_end = end_marker.unwrap_or(bytes.len());
        builder.push(start, start + 2, HighlightTokenKind::Punctuation);
        if content_start > start + 2 {
            builder.push(
                start + 2,
                content_start,
                if content_marker == Some(b'@') {
                    HighlightTokenKind::Annotation
                } else {
                    HighlightTokenKind::Operator
                },
            );
        }

        if content_marker == Some(b'@') {
            highlight_jsp_directive(&line[content_start..content_end], content_start, builder);
        } else {
            highlight_segment_with_offset(
                &line[content_start..content_end],
                content_start,
                builder,
                java::highlight_java,
            );
        }

        if let Some(end_marker) = end_marker {
            builder.push(end_marker, end_marker + 2, HighlightTokenKind::Punctuation);
            cursor = end_marker + 2;
        } else {
            cursor = bytes.len();
            break;
        }
    }
    highlight_segment_with_offset(&line[cursor..], cursor, builder, xml::highlight_xml);
}

/// 高亮 JSP 专用 `<%-- ... --%>` 注释；未闭合时覆盖到当前行末尾。
fn highlight_jsp_comments(line: &str, builder: &mut SpanBuilder) {
    let bytes = line.as_bytes();
    let mut cursor = 0_usize;
    while let Some(start) = find_bytes(bytes, cursor, b"<%--") {
        let end = find_bytes(bytes, start + 4, b"--%>")
            .map(|position| position + 4)
            .unwrap_or(bytes.len());
        builder.push(start, end, HighlightTokenKind::Comment);
        if end == bytes.len() {
            return;
        }
        cursor = end;
    }
}

/// 高亮 JSP 指令名称、属性名、等号和属性字符串。
fn highlight_jsp_directive(segment: &str, offset: usize, builder: &mut SpanBuilder) {
    let bytes = segment.as_bytes();
    let mut index = 0_usize;
    while index < bytes.len() {
        if matches!(bytes[index], b'"' | b'\'') {
            let end = scan_quoted_string(bytes, index, bytes[index]);
            builder.push(offset + index, offset + end, HighlightTokenKind::String);
            index = end;
            continue;
        }
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
fn highlight_expression_language(line: &str, builder: &mut SpanBuilder) {
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

/// 从指定位置查找字节子串，并返回原始切片中的绝对位置。
fn find_bytes(bytes: &[u8], start: usize, needle: &[u8]) -> Option<usize> {
    bytes
        .get(start..)?
        .windows(needle.len())
        .position(|window| window == needle)
        .map(|offset| start + offset)
}
