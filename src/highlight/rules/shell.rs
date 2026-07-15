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
    while index < bytes.len() {
        match bytes[index] {
            b'#' => {
                // Shebang 与普通注释都作为注释色展示；必须在字符串分支之后逐字扫描，避免误伤引号内 `#`。
                builder.push(index, bytes.len(), HighlightTokenKind::Comment);
                return;
            }
            b'\'' | b'"' | b'`' => {
                let end = scan_quoted_string(bytes, index, bytes[index]);
                builder.push(index, end, HighlightTokenKind::String);
                index = end;
            }
            b'$' => {
                let end = scan_shell_variable(bytes, index);
                builder.push(index, end, HighlightTokenKind::Variable);
                index = end;
            }
            b'0'..=b'9' => {
                let end = scan_code_number(bytes, index);
                builder.push(index, end, HighlightTokenKind::Number);
                index = end;
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
            }
            b'|' | b'&' | b';' | b'<' | b'>' | b'=' | b'!' | b'+' | b'-' | b'*' | b'/' | b'%' => {
                let end = scan_operator(bytes, index);
                builder.push(index, end, HighlightTokenKind::Operator);
                index = end;
            }
            b'(' | b')' | b'{' | b'}' | b'[' | b']' => {
                builder.push(index, index + 1, HighlightTokenKind::Punctuation);
                index += 1;
            }
            _ => index += 1,
        }
    }
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
