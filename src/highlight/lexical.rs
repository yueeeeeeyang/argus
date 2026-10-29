//! 文件职责：统一代码与标记语言的注释、字符串和混合语言边界扫描。
//! 主要功能：全文索引仅推进状态；可见行复用同一扫描器屏蔽字面量后执行既有 token 规则。

use std::ops::Range;

use super::HighlightLanguage;
use super::rules;
use super::span::{HighlightSpan, HighlightTokenKind, SpanBuilder, capped_scan_len};

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
enum JspContext {
    #[default]
    Markup,
    Java,
    Directive,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Delimited {
    CComment,
    XmlComment,
    JspComment,
    CData,
    Quote {
        byte: u8,
        triple: bool,
        escapes: bool,
        doubled: bool,
        multiline: bool,
    },
}

/// 每行仅保存常量大小的词法状态，不保存已扫描 token 或前缀文本。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct LineState {
    active: Option<Delimited>,
    jsp_context: JspContext,
    markup_in_tag: bool,
    js_expects_operand: bool,
    js_control_paren: bool,
    js_paren_depth: usize,
    js_control_depth: Option<usize>,
}

impl Default for LineState {
    fn default() -> Self {
        Self {
            active: None,
            jsp_context: JspContext::Markup,
            markup_in_tag: false,
            js_expects_operand: true,
            js_control_paren: false,
            js_paren_depth: 0,
            js_control_depth: None,
        }
    }
}

enum Event {
    Opaque {
        range: Range<usize>,
        kind: HighlightTokenKind,
        expression_language: bool,
    },
    Code {
        range: Range<usize>,
        context: JspContext,
        starts_in_tag: bool,
    },
    Delimiter {
        range: Range<usize>,
        kind: HighlightTokenKind,
    },
}

/// 判断语言是否需要在文档行之间保留词法上下文。
pub(super) fn uses_document_state(language: HighlightLanguage) -> bool {
    matches!(
        language,
        HighlightLanguage::Java
            | HighlightLanguage::JavaScript
            | HighlightLanguage::Css
            | HighlightLanguage::Sql
            | HighlightLanguage::Xml
            | HighlightLanguage::Jsp
    )
}

/// 状态索引扫描完整文本，包括显示高亮上限之外的关闭分隔符；不分配范围数组。
pub(super) fn advance_line_state(
    line: &str,
    language: HighlightLanguage,
    state: LineState,
) -> LineState {
    scan_line(line, language, state, |_| {})
}

/// 用同一词法边界生成可见行 token，普通代码仍交给各语言已有分类规则。
pub(super) fn highlight_stateful_line(
    line: &str,
    language: HighlightLanguage,
    state: LineState,
) -> Vec<HighlightSpan> {
    let line = &line[..capped_scan_len(line)];
    if line.is_empty() {
        return Vec::new();
    }
    let mut events = Vec::new();
    scan_line(line, language, state, |event| events.push(event));
    let mut masked = line.as_bytes().to_vec();
    let mut spans = Vec::new();
    for event in &events {
        match event {
            Event::Opaque {
                range,
                kind,
                expression_language,
            } => {
                masked[range.clone()].fill(b' ');
                if *expression_language {
                    let mut nested = SpanBuilder::new(range.len());
                    rules::jsp::highlight_expression_language(&line[range.clone()], &mut nested);
                    let mut cursor = range.start;
                    for span in nested.finish() {
                        let start = range.start + span.range.start;
                        let end = range.start + span.range.end;
                        if cursor < start {
                            spans.push(HighlightSpan {
                                range: cursor..start,
                                kind: *kind,
                            });
                        }
                        spans.push(HighlightSpan {
                            range: start..end,
                            kind: span.kind,
                        });
                        cursor = end;
                    }
                    if cursor < range.end {
                        spans.push(HighlightSpan {
                            range: cursor..range.end,
                            kind: *kind,
                        });
                    }
                } else if !range.is_empty() {
                    spans.push(HighlightSpan {
                        range: range.clone(),
                        kind: *kind,
                    });
                }
            }
            Event::Delimiter { range, kind } => spans.push(HighlightSpan {
                range: range.clone(),
                kind: *kind,
            }),
            Event::Code { .. } => {}
        }
    }
    // 只把完整 UTF-8 token 替换为等长 ASCII 空白，保留全部原始字节坐标。
    let masked = String::from_utf8(masked).expect("词法范围必须位于 UTF-8 边界");
    for event in events {
        let Event::Code {
            range,
            context,
            starts_in_tag,
        } = event
        else {
            continue;
        };
        let segment = &masked[range.clone()];
        let mut builder = SpanBuilder::new(segment.len());
        match language {
            HighlightLanguage::Java => rules::java::highlight_java(segment, &mut builder),
            HighlightLanguage::JavaScript => {
                rules::javascript::highlight_javascript(segment, &mut builder)
            }
            HighlightLanguage::Css => rules::css::highlight_css(segment, &mut builder),
            HighlightLanguage::Sql => rules::sql::highlight_sql(segment, &mut builder),
            HighlightLanguage::Xml => {
                rules::xml::highlight_xml_with_context(segment, &mut builder, starts_in_tag)
            }
            HighlightLanguage::Jsp => match context {
                JspContext::Java => rules::java::highlight_java(segment, &mut builder),
                JspContext::Directive => {
                    rules::jsp::highlight_jsp_directive(segment, 0, &mut builder)
                }
                JspContext::Markup => {
                    rules::jsp::highlight_expression_language(segment, &mut builder);
                    rules::xml::highlight_xml_with_context(segment, &mut builder, starts_in_tag);
                }
            },
            _ => unreachable!("只有支持文档状态的语言进入该路径"),
        }
        spans.extend(builder.finish().into_iter().map(|mut span| {
            span.range.start += range.start;
            span.range.end += range.start;
            span
        }));
    }
    spans.sort_by_key(|span| span.range.start);
    spans
}

/// 只扫描分隔符及 JS 正则上下文；回调为无操作时不生成 token 或正文副本。
fn scan_line(
    line: &str,
    language: HighlightLanguage,
    mut state: LineState,
    mut emit: impl FnMut(Event),
) -> LineState {
    let bytes = line.as_bytes();
    let mut index = 0;
    let mut code_start = 0;
    let mut starts_in_tag = state.markup_in_tag;
    while index < bytes.len() {
        let is_markup = matches!(language, HighlightLanguage::Xml | HighlightLanguage::Jsp)
            && state.jsp_context == JspContext::Markup;
        if let Some(active) = state.active {
            let (end, closed, escaped_newline) = delimited_end(bytes, index, active);
            emit_opaque(index..end, active, language, state.jsp_context, &mut emit);
            index = end;
            if closed {
                state.active = None;
                if matches!(active, Delimited::Quote { .. }) {
                    state.js_expects_operand = false;
                }
            } else if let Delimited::Quote {
                multiline: false, ..
            } = active
                && !(escaped_newline
                    && matches!(
                        language,
                        HighlightLanguage::JavaScript | HighlightLanguage::Css
                    ))
            {
                state.active = None;
            }
            continue;
        }

        let remaining = &bytes[index..];
        let block = if is_markup && remaining.starts_with(b"<!--") {
            Some((Delimited::XmlComment, 4))
        } else if is_markup && language == HighlightLanguage::Jsp && remaining.starts_with(b"<%--")
        {
            Some((Delimited::JspComment, 4))
        } else if is_markup && remaining.starts_with(b"<![CDATA[") {
            Some((Delimited::CData, 9))
        } else if !is_markup && remaining.starts_with(b"/*") {
            Some((Delimited::CComment, 2))
        } else {
            None
        };
        if let Some((active, opening_len)) = block {
            let (end, closed, _) = delimited_end(bytes, index + opening_len, active);
            emit_opaque(index..end, active, language, state.jsp_context, &mut emit);
            state.active = (!closed).then_some(active);
            index = end;
            continue;
        }
        if !is_markup
            && ((matches!(
                language,
                HighlightLanguage::Java | HighlightLanguage::JavaScript | HighlightLanguage::Jsp
            ) && remaining.starts_with(b"//"))
                || (language == HighlightLanguage::Sql
                    && (remaining.starts_with(b"--") || bytes[index] == b'#')))
        {
            emit(Event::Opaque {
                range: index..bytes.len(),
                kind: HighlightTokenKind::Comment,
                expression_language: false,
            });
            break;
        }
        if language == HighlightLanguage::Jsp {
            if is_markup && remaining.starts_with(b"<%") {
                emit(Event::Code {
                    range: code_start..index,
                    context: state.jsp_context,
                    starts_in_tag,
                });
                emit(Event::Delimiter {
                    range: index..index + 2,
                    kind: HighlightTokenKind::Punctuation,
                });
                index += 2;
                state.jsp_context = if bytes.get(index) == Some(&b'@') {
                    JspContext::Directive
                } else {
                    JspContext::Java
                };
                if bytes
                    .get(index)
                    .is_some_and(|byte| matches!(*byte, b'@' | b'=' | b'!'))
                {
                    emit(Event::Delimiter {
                        range: index..index + 1,
                        kind: if bytes[index] == b'@' {
                            HighlightTokenKind::Annotation
                        } else {
                            HighlightTokenKind::Operator
                        },
                    });
                    index += 1;
                }
                code_start = index;
                starts_in_tag = false;
                continue;
            } else if !is_markup && remaining.starts_with(b"%>") {
                emit(Event::Code {
                    range: code_start..index,
                    context: state.jsp_context,
                    starts_in_tag,
                });
                emit(Event::Delimiter {
                    range: index..index + 2,
                    kind: HighlightTokenKind::Punctuation,
                });
                index += 2;
                state.jsp_context = JspContext::Markup;
                code_start = index;
                starts_in_tag = state.markup_in_tag;
                continue;
            }
        }
        let byte = bytes[index];
        let is_quote = matches!(byte, b'\'' | b'"') && (!is_markup || state.markup_in_tag)
            || byte == b'`'
                && matches!(
                    language,
                    HighlightLanguage::JavaScript | HighlightLanguage::Sql
                )
                && !is_markup;
        if is_quote {
            let triple = byte == b'"'
                && (language == HighlightLanguage::Java
                    || language == HighlightLanguage::Jsp && !is_markup)
                && remaining.starts_with(b"\"\"\"");
            let active = Delimited::Quote {
                byte,
                triple,
                escapes: !is_markup,
                doubled: language == HighlightLanguage::Sql,
                multiline: is_markup
                    || language == HighlightLanguage::Sql
                    || triple
                    || byte == b'`',
            };
            let (end, closed, escaped_newline) =
                delimited_end(bytes, index + if triple { 3 } else { 1 }, active);
            emit_opaque(index..end, active, language, state.jsp_context, &mut emit);
            if !closed
                && (matches!(
                    active,
                    Delimited::Quote {
                        multiline: true,
                        ..
                    }
                ) || escaped_newline
                    && matches!(
                        language,
                        HighlightLanguage::JavaScript | HighlightLanguage::Css
                    ))
            {
                state.active = Some(active);
            }
            state.js_expects_operand = false;
            index = end;
            continue;
        }
        if language == HighlightLanguage::JavaScript && byte == b'/' && state.js_expects_operand {
            // 不完整正则只覆盖本行；失败后不从内部斜杠反复重试，保持状态索引扫描线性。
            let end = regex_end(bytes, index + 1).unwrap_or(bytes.len());
            emit(Event::Opaque {
                range: index..end,
                kind: HighlightTokenKind::String,
                expression_language: false,
            });
            state.js_expects_operand = false;
            index = end;
            continue;
        }
        if is_markup {
            if byte == b'<' {
                state.markup_in_tag = true;
            } else if byte == b'>' {
                state.markup_in_tag = false;
            }
        } else if language == HighlightLanguage::JavaScript {
            if remaining.starts_with(b"++") || remaining.starts_with(b"--") {
                index += 2;
                continue;
            }
            if byte.is_ascii_alphabetic() || byte >= 0x80 || matches!(byte, b'_' | b'$') {
                let start = index;
                while bytes.get(index).is_some_and(|byte| {
                    byte.is_ascii_alphanumeric() || *byte >= 0x80 || matches!(*byte, b'_' | b'$')
                }) {
                    index += 1;
                }
                let word = &line[start..index];
                state.js_control_paren =
                    matches!(word, "if" | "while" | "for" | "with" | "switch" | "catch");
                state.js_expects_operand = matches!(
                    word,
                    "return"
                        | "throw"
                        | "case"
                        | "delete"
                        | "void"
                        | "typeof"
                        | "new"
                        | "in"
                        | "of"
                        | "yield"
                        | "await"
                        | "else"
                        | "do"
                );
                continue;
            }
            match byte {
                b'(' => {
                    state.js_paren_depth += 1;
                    if state.js_control_paren {
                        state.js_control_depth = Some(state.js_paren_depth);
                    }
                    state.js_control_paren = false;
                    state.js_expects_operand = true;
                }
                b')' => {
                    state.js_expects_operand = state.js_control_depth == Some(state.js_paren_depth);
                    if state.js_expects_operand {
                        state.js_control_depth = None;
                    }
                    state.js_paren_depth = state.js_paren_depth.saturating_sub(1);
                }
                b']' => state.js_expects_operand = false,
                b'0'..=b'9' => state.js_expects_operand = false,
                byte if byte.is_ascii_whitespace() => {}
                _ => state.js_expects_operand = true,
            }
        }
        index += 1;
    }
    emit(Event::Code {
        range: code_start..bytes.len(),
        context: state.jsp_context,
        starts_in_tag,
    });
    state
}

fn emit_opaque(
    range: Range<usize>,
    active: Delimited,
    language: HighlightLanguage,
    context: JspContext,
    emit: &mut impl FnMut(Event),
) {
    let kind = match active {
        Delimited::CComment | Delimited::XmlComment | Delimited::JspComment => {
            HighlightTokenKind::Comment
        }
        _ => HighlightTokenKind::String,
    };
    emit(Event::Opaque {
        range,
        kind,
        expression_language: matches!(active, Delimited::Quote { .. })
            && language == HighlightLanguage::Jsp
            && context == JspContext::Markup,
    });
}

/// 返回闭合位置和转义换行标记；ASCII 分隔符保证返回值始终是 UTF-8 边界。
fn delimited_end(bytes: &[u8], mut index: usize, active: Delimited) -> (usize, bool, bool) {
    let terminator: &[u8] = match active {
        Delimited::CComment => b"*/",
        Delimited::XmlComment => b"-->",
        Delimited::JspComment => b"--%>",
        Delimited::CData => b"]]>",
        Delimited::Quote { .. } => b"",
    };
    while index < bytes.len() {
        if let Delimited::Quote {
            byte,
            triple,
            escapes,
            doubled,
            ..
        } = active
        {
            if escapes && bytes[index] == b'\\' {
                if index + 1 == bytes.len() {
                    return (bytes.len(), false, true);
                }
                index += 2;
                continue;
            }
            if bytes[index] == byte {
                if triple {
                    if bytes.get(index..index + 3) == Some(b"\"\"\"") {
                        return (index + 3, true, false);
                    }
                } else if doubled && bytes.get(index + 1) == Some(&byte) {
                    index += 2;
                    continue;
                } else {
                    return (index + 1, true, false);
                }
            }
        } else if bytes[index..].starts_with(terminator) {
            return (index + terminator.len(), true, false);
        }
        index += 1;
    }
    (bytes.len(), false, false)
}

/// JavaScript 正则不跨行，字符类中的斜杠及转义斜杠都不能开启注释。
fn regex_end(bytes: &[u8], mut index: usize) -> Option<usize> {
    let mut in_class = false;
    while index < bytes.len() {
        match bytes[index] {
            b'\\' => {
                index += 2;
                continue;
            }
            b'[' => in_class = true,
            b']' => in_class = false,
            b'/' if !in_class => {
                index += 1;
                while bytes.get(index).is_some_and(u8::is_ascii_alphabetic) {
                    index += 1;
                }
                return Some(index);
            }
            _ => {}
        }
        index += 1;
    }
    None
}
