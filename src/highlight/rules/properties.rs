//! 文件职责：实现 Properties、INI 和 CONF 行的高亮规则。
//! 创建日期：2026-06-11
//! 修改日期：2026-06-11
//! 作者：Argus 开发团队
//! 主要功能：识别注释、键、分隔符和值类型。

use crate::highlight::rules::common::{highlight_scalar_value, skip_ascii_spaces};
use crate::highlight::span::{HighlightTokenKind, SpanBuilder};

/// 高亮 Properties/INI/CONF 行。
pub(crate) fn highlight_properties(line: &str, builder: &mut SpanBuilder) {
    let trimmed_start = line.len() - line.trim_start().len();
    let trimmed = &line[trimmed_start..];
    if trimmed.starts_with('#') || trimmed.starts_with('!') {
        builder.push(trimmed_start, line.len(), HighlightTokenKind::Comment);
        return;
    }

    let Some(separator) = find_property_separator(line) else {
        builder.push(trimmed_start, line.len(), HighlightTokenKind::Value);
        return;
    };
    let key_start = trimmed_start;
    let key_end = line[..separator].trim_end().len();
    builder.push(key_start, key_end, HighlightTokenKind::Key);
    builder.push(separator, separator + 1, HighlightTokenKind::Punctuation);

    let value_start = skip_ascii_spaces(line.as_bytes(), separator + 1);
    highlight_scalar_value(line, value_start, line.len(), builder);
}

/// 查找 properties 分隔符，忽略反斜杠转义的 `=` 和 `:`。
fn find_property_separator(line: &str) -> Option<usize> {
    let bytes = line.as_bytes();
    let mut escaped = false;
    for (index, byte) in bytes.iter().copied().enumerate() {
        if escaped {
            escaped = false;
            continue;
        }
        if byte == b'\\' {
            escaped = true;
            continue;
        }
        if matches!(byte, b'=' | b':') {
            return Some(index);
        }
    }
    None
}
