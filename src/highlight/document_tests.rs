//! 文件职责：覆盖文档级词法状态、随机访问和显示扫描上限的回归行为。

use std::sync::Arc;

use super::span::{MAX_HIGHLIGHT_BYTES, SpanBuilder};
use super::*;

fn document(lines: &[&str], language: HighlightLanguage) -> DocumentHighlighter {
    DocumentHighlighter::new(
        Arc::new(lines.iter().map(|line| (*line).to_owned()).collect()),
        language,
    )
}

fn assert_token(
    doc: &DocumentHighlighter,
    line: usize,
    text: &str,
    needle: &str,
    kind: HighlightTokenKind,
) {
    let start = text.find(needle).expect("测试文本必须包含目标词");
    let end = start + needle.len();
    assert!(
        doc.highlight_line(line)
            .iter()
            .any(|span| span.kind == kind && span.range.start <= start && span.range.end >= end),
        "第{line}行 {needle:?} 应为 {kind:?}，实际 {:?}",
        doc.highlight_line(line)
    );
}

/// 首先读取注释中间与闭合行，不得依赖此前是否渲染过首行。
#[test]
fn javadoc_highlighting_is_independent_of_random_access_order() {
    let lines = [
        "package demo;",
        "/**",
        " * 中文说明 @return true",
        " */ public class Demo {}",
        "int next = 1;",
    ];
    let doc = document(&lines, HighlightLanguage::Java);
    for index in [3, 2, 4, 1, 0, 2, 3] {
        let spans = doc.highlight_line(index);
        for span in spans {
            assert!(lines[index].is_char_boundary(span.range.start));
            assert!(lines[index].is_char_boundary(span.range.end));
        }
    }
    assert_token(
        &doc,
        2,
        lines[2],
        "中文说明 @return true",
        HighlightTokenKind::Comment,
    );
    assert_token(&doc, 3, lines[3], "*/", HighlightTokenKind::Comment);
    assert_token(&doc, 3, lines[3], "public", HighlightTokenKind::Keyword);
    assert_token(&doc, 4, lines[4], "int", HighlightTokenKind::Type);
    assert!(doc.highlight_line(100).is_empty());
}

/// 共用 C 风格注释覆盖 Java、JavaScript、CSS 和 SQL，并允许紧邻运算符及闭合后代码。
#[test]
fn shared_block_comments_survive_newlines_and_adjacent_operators() {
    for (language, opening, closing, code, kind) in [
        (
            HighlightLanguage::Java,
            "int x=/* note",
            "*/ public class Next {}",
            "public",
            HighlightTokenKind::Keyword,
        ),
        (
            HighlightLanguage::JavaScript,
            "const x=/* note",
            "*/ 1; const next=2;",
            "const",
            HighlightTokenKind::Keyword,
        ),
        (
            HighlightLanguage::Css,
            "a { width:/* note",
            "*/ 1px; color: red; }",
            "color",
            HighlightTokenKind::Attribute,
        ),
        (
            HighlightLanguage::Sql,
            "SELECT 1+/* note",
            "*/2; SELECT 3;",
            "SELECT",
            HighlightTokenKind::Keyword,
        ),
    ] {
        let lines = [opening, "中文中间行", "", closing];
        let doc = document(&lines, language);
        assert_token(&doc, 1, lines[1], lines[1], HighlightTokenKind::Comment);
        assert!(doc.highlight_line(2).is_empty());
        assert_token(&doc, 3, closing, code, kind);
        let inline = format!("{opening} */ {closing}");
        assert!(
            SyntaxHighlighter::highlight(&inline, language)
                .iter()
                .any(|span| span.kind == HighlightTokenKind::Comment)
        );
    }
}

/// 未闭合注释持续到文档末尾；相同行文本属于不同文档时不能复用旧进入状态。
#[test]
fn unclosed_comments_and_document_cache_isolation() {
    let comment = document(&["/*", "public class Same {}"], HighlightLanguage::Java);
    let code = document(&["", "public class Same {}"], HighlightLanguage::Java);
    assert_token(
        &comment,
        1,
        "public class Same {}",
        "public",
        HighlightTokenKind::Comment,
    );
    assert_token(
        &code,
        1,
        "public class Same {}",
        "public",
        HighlightTokenKind::Keyword,
    );
    assert_eq!(comment.highlight_line(1), comment.clone().highlight_line(1));
}

/// 词法状态必须扫描完整长行，显示上限之外的开启或闭合仍决定下一行颜色。
#[test]
fn lexical_state_advances_beyond_visible_highlight_limit() {
    for language in [
        HighlightLanguage::Java,
        HighlightLanguage::JavaScript,
        HighlightLanguage::Css,
        HighlightLanguage::Sql,
    ] {
        let opening = format!("{}/*", "中".repeat(MAX_HIGHLIGHT_BYTES / 3 + 4));
        let closing = format!("{}*/", "文".repeat(MAX_HIGHLIGHT_BYTES / 3 + 4));
        let lines = Arc::new(vec![
            opening,
            "comment".into(),
            closing,
            "public SELECT const color".into(),
        ]);
        let doc = DocumentHighlighter::new(lines.clone(), language);
        assert_token(&doc, 1, &lines[1], "comment", HighlightTokenKind::Comment);
        assert!(
            doc.highlight_line(3)
                .iter()
                .all(|span| span.kind != HighlightTokenKind::Comment)
        );
        for index in [0, 2] {
            assert!(
                doc.highlight_line(index)
                    .iter()
                    .all(|span| span.range.end <= MAX_HIGHLIGHT_BYTES
                        && lines[index].is_char_boundary(span.range.start)
                        && lines[index].is_char_boundary(span.range.end))
            );
        }
    }
}

/// 普通字符串、Java 文本块及 JavaScript 模板中的注释符号不得改变后续注释状态。
#[test]
fn string_delimiters_do_not_start_comments() {
    let java = [
        r#"String s = "\" /* not */";"#,
        "String block = \"\"\"",
        "/* text */",
        "\"\"\"; public class Next {}",
    ];
    let doc = document(&java, HighlightLanguage::Java);
    assert_token(&doc, 2, java[2], java[2], HighlightTokenKind::String);
    assert_token(&doc, 3, java[3], "public", HighlightTokenKind::Keyword);
    assert!(
        doc.highlight_line(0)
            .iter()
            .all(|span| span.kind != HighlightTokenKind::Comment)
    );
    let javascript = ["const value = `", "/* text */", "`; const next = 1;"];
    let doc = document(&javascript, HighlightLanguage::JavaScript);
    assert_token(
        &doc,
        1,
        javascript[1],
        javascript[1],
        HighlightTokenKind::String,
    );
    assert_token(&doc, 2, javascript[2], "const", HighlightTokenKind::Keyword);
    let sql = ["SELECT 'a''/*", "-- still string", "*/'; SELECT 1;"];
    let doc = document(&sql, HighlightLanguage::Sql);
    assert_token(&doc, 1, sql[1], sql[1], HighlightTokenKind::String);
    assert_token(&doc, 2, sql[2], "SELECT", HighlightTokenKind::Keyword);
}

/// JS 正则中的斜杠和字符类不是注释；跨行注释闭合必须保留原来的操作数上下文。
#[test]
fn javascript_regex_and_division_preserve_comment_boundaries() {
    for lines in [
        vec!["const r = /[/*]/;", "const next=1;"],
        vec![r"const r = /https?:\/\//;", "const next=1;"],
        vec!["return /*", " comment */ /[/*]/;", "const next=1;"],
        vec!["if (ready) /[/*]/.test(value);", "const next=1;"],
        vec!["const r = /[", "const next=1;"],
    ] {
        let doc = document(&lines, HighlightLanguage::JavaScript);
        let last = lines.len() - 1;
        assert_token(
            &doc,
            last,
            lines[last],
            "const",
            HighlightTokenKind::Keyword,
        );
        assert!(
            doc.highlight_line(last)
                .iter()
                .all(|span| span.kind != HighlightTokenKind::Comment)
        );
    }
    for line in [
        "const r = n++ / 2; // tail",
        "const r = n-- / 2; // tail",
        "const r = 中文 / 2; // tail",
    ] {
        let doc = document(&[line], HighlightLanguage::JavaScript);
        assert_token(&doc, 0, line, "// tail", HighlightTokenKind::Comment);
        assert!(
            doc.highlight_line(0)
                .iter()
                .all(|span| span.kind != HighlightTokenKind::String)
        );
    }
}

/// 大段未闭合正则只尝试一次本行候选，不从内部斜杠反复扫描后缀，也不污染下一行。
#[test]
fn large_unclosed_regex_is_bounded_to_its_own_line() {
    let lines = Arc::new(vec!["/[".repeat(256 * 1024), "const next=1;".into()]);
    let doc = DocumentHighlighter::new(lines.clone(), HighlightLanguage::JavaScript);
    assert_token(&doc, 1, &lines[1], "const", HighlightTokenKind::Keyword);
    assert_eq!(
        doc.highlight_line(0),
        vec![HighlightSpan {
            range: 0..MAX_HIGHLIGHT_BYTES,
            kind: HighlightTokenKind::String,
        }]
    );
}

/// XML 注释、CDATA 和多行属性独立推进，属性引号内 > 和 <!-- 不终止标签或开启注释。
#[test]
fn xml_comments_cdata_and_quoted_tag_boundaries() {
    let lines = [
        "<!-- 注释",
        "中文 --> <node test=\"1 > 0\" next=\"ok\"/>",
        "<![CDATA[",
        "<!-- literal -->",
        "]]> <node",
        " title=\"<!-- text",
        "still text >\" next=\"ok\"/>",
    ];
    let doc = document(&lines, HighlightLanguage::Xml);
    assert_token(&doc, 1, lines[1], "中文 -->", HighlightTokenKind::Comment);
    assert_token(&doc, 1, lines[1], "next", HighlightTokenKind::Attribute);
    assert_token(&doc, 1, lines[1], "1 > 0", HighlightTokenKind::String);
    assert_token(&doc, 3, lines[3], lines[3], HighlightTokenKind::String);
    assert_token(
        &doc,
        6,
        lines[6],
        "still text >",
        HighlightTokenKind::String,
    );
    assert_token(&doc, 6, lines[6], "next", HighlightTokenKind::Attribute);
    assert!(
        doc.highlight_line(6)
            .iter()
            .all(|span| span.kind != HighlightTokenKind::Comment)
    );
}

/// JSP 注释屏蔽内部模板，scriptlet 跨行继续 Java，闭合后恢复 XML；属性 EL 保留周围字符串。
#[test]
fn jsp_comment_scriptlet_and_attribute_string_regions() {
    let lines = [
        "<%-- 注释",
        "${ignored} --%><%",
        "/* Java 注释",
        "*/ int value=1; %><span>ok</span>",
        r#"<div title="hello ${user.name}!">"#,
    ];
    let doc = document(&lines, HighlightLanguage::Jsp);
    assert_token(&doc, 1, lines[1], "${ignored}", HighlightTokenKind::Comment);
    assert_token(&doc, 2, lines[2], "Java 注释", HighlightTokenKind::Comment);
    assert_token(&doc, 3, lines[3], "int", HighlightTokenKind::Type);
    assert_token(&doc, 3, lines[3], "span", HighlightTokenKind::Tag);
    assert_token(&doc, 4, lines[4], "hello ", HighlightTokenKind::String);
    assert_token(&doc, 4, lines[4], "user", HighlightTokenKind::Variable);
    assert_token(&doc, 4, lines[4], "!", HighlightTokenKind::String);
}

/// 快速有序追加不能破坏旧规则的乱序优先级与重叠拒绝语义。
#[test]
fn span_builder_fast_append_preserves_out_of_order_priority() {
    let mut builder = SpanBuilder::new(30);
    builder.push(20, 25, HighlightTokenKind::Comment);
    builder.push(0, 5, HighlightTokenKind::String);
    builder.push(22, 23, HighlightTokenKind::Keyword);
    builder.push(5, 10, HighlightTokenKind::Number);
    builder.push(25, 30, HighlightTokenKind::Value);
    assert_eq!(
        builder
            .finish()
            .iter()
            .map(|span| span.range.clone())
            .collect::<Vec<_>>(),
        vec![0..5, 5..10, 20..25, 25..30]
    );
}
