//! 文件职责：提供 Java、JavaScript 等代码高亮规则共享的词法扫描能力。
//! 创建日期：2026-07-15
//! 修改日期：2026-07-15
//! 作者：Argus 开发团队
//! 主要功能：扫描注释、字符串、数字、关键字、类型、函数和运算符，并安全合并子片段范围。

use crate::highlight::rules::common::{scan_quoted_string, skip_ascii_spaces};
use crate::highlight::span::{HighlightTokenKind, SpanBuilder};

/// C 风格语言的单行词法配置；只保存语言差异，公共边界处理集中在扫描器中。
pub(crate) struct CStyleLanguageSpec {
    /// 语言关键字。
    pub keywords: &'static [&'static str],
    /// 内置类型和常见运行时类型。
    pub types: &'static [&'static str],
    /// 布尔、空值等特殊字面量。
    pub literals: &'static [&'static str],
    /// 是否把反引号内容识别为字符串。
    pub allow_backtick_string: bool,
    /// 是否识别 `@Name` 形式的注解。
    pub allow_annotation: bool,
    /// 是否根据大写首字母补充识别用户定义类型。
    pub infer_capitalized_type: bool,
}

/// 高亮 C 风格代码行。
///
/// 参数说明：
/// - `line`：不包含换行符的单行文本。
/// - `builder`：统一范围构造器，负责去重与边界校验。
/// - `spec`：当前语言的词法差异配置。
pub(crate) fn highlight_c_style(line: &str, builder: &mut SpanBuilder, spec: &CStyleLanguageSpec) {
    let bytes = line.as_bytes();
    let mut index = 0_usize;
    while index < bytes.len() {
        // 注释优先于除法运算符和字符串之外的 token，避免注释正文被重复着色。
        if bytes.get(index..index + 2) == Some(b"//") {
            builder.push(index, bytes.len(), HighlightTokenKind::Comment);
            return;
        }
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
            b'`' if spec.allow_backtick_string => {
                let end = scan_quoted_string(bytes, index, b'`');
                builder.push(index, end, HighlightTokenKind::String);
                index = end;
            }
            b'@' if spec.allow_annotation
                && bytes
                    .get(index + 1)
                    .is_some_and(|byte| is_code_identifier_start(*byte)) =>
            {
                let end = scan_code_identifier(bytes, index + 1);
                builder.push(index, end, HighlightTokenKind::Annotation);
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
                let kind = if contains_word(spec.literals, token, false) {
                    Some(HighlightTokenKind::Boolean)
                } else if contains_word(spec.keywords, token, false) {
                    Some(HighlightTokenKind::Keyword)
                } else if contains_word(spec.types, token, false)
                    || (spec.infer_capitalized_type
                        && token.as_bytes().first().is_some_and(u8::is_ascii_uppercase))
                {
                    Some(HighlightTokenKind::Type)
                } else if bytes.get(next) == Some(&b'(') {
                    Some(HighlightTokenKind::Function)
                } else {
                    None
                };
                if let Some(kind) = kind {
                    builder.push(index, end, kind);
                }
                index = end;
            }
            byte if is_operator_byte(byte) => {
                let end = scan_operator(bytes, index);
                builder.push(index, end, HighlightTokenKind::Operator);
                index = end;
            }
            b'{' | b'}' | b'[' | b']' | b'(' | b')' | b',' | b';' => {
                builder.push(index, index + 1, HighlightTokenKind::Punctuation);
                index += 1;
            }
            _ => index += 1,
        }
    }
}

/// 在独立构造器中高亮子片段，再把范围平移回原始行。
///
/// 该方法供 JSP 等混合语言复用，避免子片段规则误把局部字节下标直接写入整行。
pub(crate) fn highlight_segment_with_offset(
    segment: &str,
    offset: usize,
    builder: &mut SpanBuilder,
    highlighter: impl FnOnce(&str, &mut SpanBuilder),
) {
    if segment.is_empty() {
        return;
    }
    let mut nested = SpanBuilder::new(segment.len());
    highlighter(segment, &mut nested);
    for span in nested.finish() {
        builder.push(
            offset + span.range.start,
            offset + span.range.end,
            span.kind,
        );
    }
}

/// 扫描仅由 ASCII 字母、数字、下划线或美元符号构成的代码标识符。
pub(crate) fn scan_code_identifier(bytes: &[u8], mut index: usize) -> usize {
    while bytes
        .get(index)
        .is_some_and(|byte| is_code_identifier_continue(*byte))
    {
        index += 1;
    }
    index
}

/// 扫描常见十进制、十六进制、指数和带下划线的代码数字。
pub(crate) fn scan_code_number(bytes: &[u8], mut index: usize) -> usize {
    if bytes.get(index..index + 2).is_some_and(|prefix| {
        prefix[0] == b'0' && matches!(prefix[1], b'x' | b'X' | b'b' | b'B' | b'o' | b'O')
    }) {
        index += 2;
        while bytes
            .get(index)
            .is_some_and(|byte| byte.is_ascii_hexdigit() || *byte == b'_')
        {
            index += 1;
        }
        return index;
    }

    while bytes
        .get(index)
        .is_some_and(|byte| byte.is_ascii_digit() || *byte == b'_')
    {
        index += 1;
    }
    if bytes.get(index) == Some(&b'.') && bytes.get(index + 1).is_some_and(u8::is_ascii_digit) {
        index += 1;
        while bytes
            .get(index)
            .is_some_and(|byte| byte.is_ascii_digit() || *byte == b'_')
        {
            index += 1;
        }
    }
    if bytes
        .get(index)
        .is_some_and(|byte| matches!(*byte, b'e' | b'E'))
    {
        let exponent_start = index;
        index += 1;
        if bytes
            .get(index)
            .is_some_and(|byte| matches!(*byte, b'+' | b'-'))
        {
            index += 1;
        }
        let digit_start = index;
        while bytes
            .get(index)
            .is_some_and(|byte| byte.is_ascii_digit() || *byte == b'_')
        {
            index += 1;
        }
        // 无指数数字时把 `e` 留给后续标识符扫描，避免错误吞掉普通变量名。
        if index == digit_start {
            index = exponent_start;
        }
    }
    if bytes
        .get(index)
        .is_some_and(|byte| matches!(*byte, b'f' | b'F' | b'd' | b'D' | b'l' | b'L'))
    {
        index += 1;
    }
    index
}

/// 扫描连续运算符字符。
pub(crate) fn scan_operator(bytes: &[u8], mut index: usize) -> usize {
    while bytes.get(index).is_some_and(|byte| is_operator_byte(*byte)) {
        index += 1;
    }
    index
}

/// 判断 token 是否存在于词表；SQL 等语言可使用大小写不敏感匹配。
pub(crate) fn contains_word(words: &[&str], token: &str, case_insensitive: bool) -> bool {
    words.iter().any(|word| {
        if case_insensitive {
            word.eq_ignore_ascii_case(token)
        } else {
            *word == token
        }
    })
}

/// 找到块注释当前行中的结束位置；未闭合时覆盖至行尾。
pub(crate) fn find_block_comment_end(bytes: &[u8], start: usize) -> usize {
    bytes[start..]
        .windows(2)
        .position(|window| window == b"*/")
        .map(|offset| start + offset + 2)
        .unwrap_or(bytes.len())
}

/// 判断代码标识符首字节。
pub(crate) fn is_code_identifier_start(byte: u8) -> bool {
    byte.is_ascii_alphabetic() || matches!(byte, b'_' | b'$')
}

/// 判断代码标识符后续字节。
pub(crate) fn is_code_identifier_continue(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'$')
}

/// 判断常见编程语言运算符字节。
pub(crate) fn is_operator_byte(byte: u8) -> bool {
    matches!(
        byte,
        b'=' | b'+'
            | b'-'
            | b'*'
            | b'/'
            | b'%'
            | b'!'
            | b'<'
            | b'>'
            | b'?'
            | b':'
            | b'&'
            | b'|'
            | b'^'
            | b'~'
    )
}
