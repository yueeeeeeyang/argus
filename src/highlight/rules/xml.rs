//! 文件职责：实现 XML、XSD 和 WSDL 行的高亮规则。
//! 创建日期：2026-06-11
//! 修改日期：2026-06-11
//! 作者：Argus 开发团队
//! 主要功能：识别注释、CDATA、标签名、属性名、属性值和实体引用。

use crate::highlight::rules::common::{
    find_bytes, is_xml_name_byte, is_xml_name_start, skip_ascii_spaces, starts_with_at,
};
use crate::highlight::span::{HighlightTokenKind, SpanBuilder};

/// 高亮 XML 行。
pub(crate) fn highlight_xml(line: &str, builder: &mut SpanBuilder) {
    let bytes = line.as_bytes();
    let mut index = 0_usize;
    while index < bytes.len() {
        if starts_with_at(bytes, index, b"<!--") {
            let end = find_bytes(bytes, index + 4, b"-->")
                .map(|position| position + 3)
                .unwrap_or(bytes.len());
            builder.push(index, end, HighlightTokenKind::Comment);
            index = end;
        } else if starts_with_at(bytes, index, b"<![CDATA[") {
            let end = find_bytes(bytes, index + 9, b"]]>")
                .map(|position| position + 3)
                .unwrap_or(bytes.len());
            builder.push(index, end, HighlightTokenKind::String);
            index = end;
        } else if bytes[index] == b'<' {
            let end = bytes[index..]
                .iter()
                .position(|byte| *byte == b'>')
                .map(|offset| index + offset + 1)
                .unwrap_or(bytes.len());
            highlight_xml_tag(line, index, end, builder);
            index = end;
        } else if bytes[index] == b'&' {
            let end = bytes[index..]
                .iter()
                .position(|byte| *byte == b';')
                .map(|offset| index + offset + 1)
                .unwrap_or(index + 1);
            builder.push(index, end, HighlightTokenKind::String);
            index = end;
        } else {
            index += 1;
        }
    }
}

/// 高亮单个 XML 标签。
fn highlight_xml_tag(line: &str, start: usize, end: usize, builder: &mut SpanBuilder) {
    let bytes = line.as_bytes();
    builder.push(start, start + 1, HighlightTokenKind::Punctuation);
    if end > start + 1 && bytes[end - 1] == b'>' {
        builder.push(end - 1, end, HighlightTokenKind::Punctuation);
    }

    let mut index = start + 1;
    if bytes
        .get(index)
        .is_some_and(|byte| matches!(*byte, b'/' | b'?' | b'!'))
    {
        builder.push(index, index + 1, HighlightTokenKind::Punctuation);
        index += 1;
    }
    index = skip_ascii_spaces(bytes, index);
    let tag_start = index;
    while index < end && is_xml_name_byte(bytes[index]) {
        index += 1;
    }
    builder.push(tag_start, index, HighlightTokenKind::Tag);

    while index < end {
        if matches!(bytes[index], b'/' | b'?') {
            builder.push(index, index + 1, HighlightTokenKind::Punctuation);
            index += 1;
            continue;
        }
        if !is_xml_name_start(bytes[index]) {
            index += 1;
            continue;
        }
        let attr_start = index;
        index += 1;
        while index < end && is_xml_name_byte(bytes[index]) {
            index += 1;
        }
        builder.push(attr_start, index, HighlightTokenKind::Attribute);
        index = skip_ascii_spaces(bytes, index);
        if bytes.get(index) == Some(&b'=') {
            builder.push(index, index + 1, HighlightTokenKind::Punctuation);
            index = skip_ascii_spaces(bytes, index + 1);
            if bytes
                .get(index)
                .is_some_and(|byte| matches!(*byte, b'"' | b'\''))
            {
                let quote = bytes[index];
                let value_start = index;
                index += 1;
                while index < end && bytes[index] != quote {
                    index += 1;
                }
                if index < end {
                    index += 1;
                }
                builder.push(value_start, index, HighlightTokenKind::String);
            }
        }
    }
}
