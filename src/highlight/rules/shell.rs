//! 文件职责：实现 Shell 脚本单行高亮规则。
//! 创建日期：2026-07-15
//! 修改日期：2026-07-15
//! 作者：Argus 开发团队
//! 主要功能：识别 Shell 关键字、变量、函数、字符串、数字、注释、重定向和管道运算符。

use crate::highlight::rules::code_common::{
    contains_word, is_code_identifier_start, scan_code_identifier, scan_code_number, scan_operator,
};
use crate::highlight::rules::common::{scan_quoted_string, skip_ascii_spaces};
use crate::highlight::span::{HighlightTokenKind, SpanBuilder};

/// POSIX Shell 与 Bash/Zsh 常用控制关键字。
const SHELL_KEYWORDS: &[&str] = &[
    "case", "coproc", "do", "done", "elif", "else", "esac", "export", "fi", "for", "function",
    "if", "in", "local", "readonly", "select", "then", "time", "trap", "typeset", "until", "while",
];
/// Shell 布尔与空操作字面量。
const SHELL_LITERALS: &[&str] = &["true", "false"];

/// 高亮 Shell 脚本行。
pub(crate) fn highlight_shell(line: &str, builder: &mut SpanBuilder) {
    let bytes = line.as_bytes();
    let mut index = 0_usize;
    let mut is_word_start = true;
    while index < bytes.len() {
        match bytes[index] {
            b'#' if is_word_start => {
                // 只有词起点的井号开始注释；foo#bar、引号后缀与转义空格后的井号仍属于参数。
                builder.push(index, bytes.len(), HighlightTokenKind::Comment);
                return;
            }
            byte if byte.is_ascii_whitespace() => {
                index += 1;
                is_word_start = true;
            }
            b'\\' => {
                index += 1;
                if index < bytes.len() {
                    index += line[index..]
                        .chars()
                        .next()
                        .expect("未到行尾应有字符")
                        .len_utf8();
                }
                is_word_start = false;
            }
            b'$' if bytes.get(index + 1) == Some(&b'\'') => {
                let end = scan_quoted_string(bytes, index + 1, b'\'');
                builder.push(index, end, HighlightTokenKind::String);
                index = end;
                is_word_start = false;
            }
            b'\'' | b'"' | b'`' => {
                let end = scan_shell_quoted_string(bytes, index);
                builder.push(index, end, HighlightTokenKind::String);
                index = end;
                is_word_start = false;
            }
            b'$' => {
                let end = scan_shell_variable(bytes, index);
                builder.push(index, end, HighlightTokenKind::Variable);
                index = end;
                is_word_start = false;
            }
            b'0'..=b'9' => {
                let end = scan_code_number(bytes, index);
                builder.push(index, end, HighlightTokenKind::Number);
                index = end;
                is_word_start = false;
            }
            byte if is_code_identifier_start(byte) => {
                let end = scan_code_identifier(bytes, index);
                let token = &line[index..end];
                let next = skip_ascii_spaces(bytes, end);
                let kind = if contains_word(SHELL_LITERALS, token, false) {
                    Some(HighlightTokenKind::Boolean)
                } else if contains_word(SHELL_KEYWORDS, token, false) {
                    Some(HighlightTokenKind::Keyword)
                } else if bytes.get(next..next + 2) == Some(b"()") {
                    Some(HighlightTokenKind::Function)
                } else {
                    None
                };
                if let Some(kind) = kind {
                    builder.push(index, end, kind);
                }
                index = end;
                is_word_start = false;
            }
            b'|' | b'&' | b'<' | b'>' | b'=' | b'!' | b'+' | b'-' | b'*' | b'/' | b'%' => {
                let end = scan_operator(bytes, index);
                builder.push(index, end, HighlightTokenKind::Operator);
                is_word_start = matches!(bytes[end - 1], b'|' | b'&' | b'<' | b'>');
                index = end;
            }
            b';' | b'(' | b')' | b'{' | b'}' | b'[' | b']' => {
                builder.push(index, index + 1, HighlightTokenKind::Punctuation);
                is_word_start = matches!(bytes[index], b';' | b'(' | b')');
                index += 1;
            }
            _ => {
                index += 1;
                is_word_start = false;
            }
        }
    }
}

/// 普通 Shell 单引号中的反斜杠是字面量；双引号与反引号仍使用转义扫描。
pub(crate) fn scan_shell_quoted_string(bytes: &[u8], start: usize) -> usize {
    if bytes[start] != b'\'' {
        return scan_quoted_string(bytes, start, bytes[start]);
    }
    bytes[start + 1..]
        .iter()
        .position(|byte| *byte == b'\'')
        .map_or(bytes.len(), |offset| start + offset + 2)
}

/// 扫描 `$NAME`、`${NAME}`、位置参数和特殊变量。
fn scan_shell_variable(bytes: &[u8], start: usize) -> usize {
    let mut index = start + 1;
    if bytes.get(index) == Some(&b'{') {
        index += 1;
        while bytes
            .get(index)
            .is_some_and(|byte| *byte != b'}' && !byte.is_ascii_whitespace())
        {
            index += 1;
        }
        return (index + usize::from(bytes.get(index) == Some(&b'}'))).min(bytes.len());
    }
    if bytes.get(index).is_some_and(|byte| {
        byte.is_ascii_digit() || matches!(*byte, b'?' | b'#' | b'@' | b'*' | b'!' | b'$' | b'-')
    }) {
        return index + 1;
    }
    scan_code_identifier(bytes, index).max(start + 1)
}
