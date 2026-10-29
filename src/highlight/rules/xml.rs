//! 文件职责：实现 XML、XSD 和 WSDL 行的高亮规则。
//! 创建日期：2026-06-11
//! 修改日期：2026-06-11
//! 作者：Argus 开发团队
//! 主要功能：在文档标签上下文中识别标签、属性和实体；注释、CDATA及属性字符串由统一词法层处理。

use crate::highlight::rules::common::{is_xml_name_byte, is_xml_name_start, skip_ascii_spaces};
use crate::highlight::span::{HighlightTokenKind, SpanBuilder};

/// 继续上一行未结束的标签；引号由统一文档扫描器屏蔽，属性仍沿用同一分类规则。
pub(crate) fn highlight_xml_with_context(
    line: &str,
    builder: &mut SpanBuilder,
    starts_in_tag: bool,
) {
    let bytes = line.as_bytes();
    let mut index = 0_usize;
    if starts_in_tag {
        let end = xml_tag_end(bytes, 0);
        highlight_xml_attributes(line, 0, end, builder);
        if end > 0 && bytes[end - 1] == b'>' {
            builder.push(end - 1, end, HighlightTokenKind::Punctuation);
        }
        index = end;
    }
    while index < bytes.len() {
        if bytes[index] == b'<' {
            let end = xml_tag_end(bytes, index + 1);
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

    highlight_xml_attributes(line, index, end, builder);
}

/// 扫描标签内属性；支持从跨行标签的中间位置继续。
fn highlight_xml_attributes(line: &str, mut index: usize, end: usize, builder: &mut SpanBuilder) {
    let bytes = line.as_bytes();
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
        }
    }
}

/// 属性字符串内的 > 不终止标签，避免截断属性与随后同一行内容。
fn xml_tag_end(bytes: &[u8], mut index: usize) -> usize {
    let mut quote = None;
    while index < bytes.len() {
        if let Some(active) = quote {
            if bytes[index] == active {
                quote = None;
            }
        } else if matches!(bytes[index], b'\'' | b'"') {
            quote = Some(bytes[index]);
        } else if bytes[index] == b'>' {
            return index + 1;
        }
        index += 1;
    }
    bytes.len()
}
