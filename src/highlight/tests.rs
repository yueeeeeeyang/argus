//! 文件职责：验证日志与配置文件高亮规则。
//! 创建日期：2026-06-11
//! 修改日期：2026-07-15
//! 作者：Argus 开发团队
//! 主要功能：覆盖日志、配置、代码语言识别、核心 token、范围合法性和超长行扫描上限。

use super::*;
use crate::highlight::span::MAX_HIGHLIGHT_BYTES;

/// 验证 Properties 注释、键和值均能高亮。
#[test]
fn highlights_properties_key_value_and_comment() {
    let comment = SyntaxHighlighter::highlight("# 注释", HighlightLanguage::Properties);
    assert_eq!(comment[0].kind, HighlightTokenKind::Comment);

    let spans = SyntaxHighlighter::highlight("recordcount=120000", HighlightLanguage::Properties);
    assert!(
        spans
            .iter()
            .any(|span| span.kind == HighlightTokenKind::Key)
    );
    assert!(
        spans
            .iter()
            .any(|span| span.kind == HighlightTokenKind::Number)
    );
}

/// 验证 XML 标签、属性和字符串值高亮。
#[test]
fn highlights_xml_tag_attribute_and_value() {
    let spans = SyntaxHighlighter::highlight(
        r#"<bean id="cache" class="weaver.Cache"/>"#,
        HighlightLanguage::Xml,
    );

    assert!(
        spans
            .iter()
            .any(|span| span.kind == HighlightTokenKind::Tag)
    );
    assert!(
        spans
            .iter()
            .any(|span| span.kind == HighlightTokenKind::Attribute)
    );
    assert!(
        spans
            .iter()
            .any(|span| span.kind == HighlightTokenKind::String)
    );
}

/// 验证 JSON key、字符串、数字和布尔值高亮。
#[test]
fn highlights_json_core_tokens() {
    let spans = SyntaxHighlighter::highlight(
        r#"{"enabled": true, "name": "argus", "limit": 3}"#,
        HighlightLanguage::Json,
    );

    assert!(
        spans
            .iter()
            .any(|span| span.kind == HighlightTokenKind::Key)
    );
    assert!(
        spans
            .iter()
            .any(|span| span.kind == HighlightTokenKind::String)
    );
    assert!(
        spans
            .iter()
            .any(|span| span.kind == HighlightTokenKind::Number)
    );
    assert!(
        spans
            .iter()
            .any(|span| span.kind == HighlightTokenKind::Boolean)
    );
}

/// 验证 YAML key、数字和注释高亮。
#[test]
fn highlights_yaml_key_value_and_comment() {
    let spans = SyntaxHighlighter::highlight("limit: 12 # 最大值", HighlightLanguage::Yaml);

    assert!(
        spans
            .iter()
            .any(|span| span.kind == HighlightTokenKind::Key)
    );
    assert!(
        spans
            .iter()
            .any(|span| span.kind == HighlightTokenKind::Number)
    );
    assert!(
        spans
            .iter()
            .any(|span| span.kind == HighlightTokenKind::Comment)
    );
}

/// 验证普通日志时间戳、等级和异常高亮。
#[test]
fn highlights_log_timestamp_level_and_exception() {
    let spans = SyntaxHighlighter::highlight(
        "2026-06-11 10:20:30 [ERROR] java.lang.IllegalStateException",
        HighlightLanguage::Log,
    );

    assert!(
        spans
            .iter()
            .any(|span| span.kind == HighlightTokenKind::Timestamp)
    );
    assert!(
        spans
            .iter()
            .any(|span| span.kind == HighlightTokenKind::Error)
    );
    assert!(
        spans
            .iter()
            .any(|span| span.kind == HighlightTokenKind::Exception)
    );
}

/// 验证点号分隔的 Java 异常名会拆成多个颜色片段，避免双击选词时字体颜色跨段。
#[test]
fn highlights_dotted_java_tokens_as_separate_segments() {
    let line = "java.lang.IllegalStateException";
    let spans = SyntaxHighlighter::highlight(line, HighlightLanguage::Log);
    let exception_texts = spans
        .iter()
        .filter(|span| span.kind == HighlightTokenKind::Exception)
        .map(|span| &line[span.range.clone()])
        .collect::<Vec<_>>();

    assert_eq!(
        exception_texts,
        vec!["java", "lang", "IllegalStateException"]
    );
    assert!(
        spans
            .iter()
            .filter(|span| span.kind == HighlightTokenKind::Punctuation)
            .all(|span| &line[span.range.clone()] == ".")
    );
}

/// 验证 Java 线程日志专项 token。
#[test]
fn highlights_java_thread_dump_tokens() {
    let header = SyntaxHighlighter::highlight(
        r#""main" #1 prio=5 tid=0x1 nid=0x2 runnable"#,
        HighlightLanguage::JavaThreadDump,
    );
    let state = SyntaxHighlighter::highlight(
        "   java.lang.Thread.State: BLOCKED (on object monitor)",
        HighlightLanguage::JavaThreadDump,
    );
    let frame = SyntaxHighlighter::highlight(
        "    at weaver.cache.CacheManager.get(CacheManager.java:42)",
        HighlightLanguage::JavaThreadDump,
    );
    let lock = SyntaxHighlighter::highlight(
        "    - waiting to lock <0x00000006cafe>",
        HighlightLanguage::JavaThreadDump,
    );

    assert!(
        header
            .iter()
            .any(|span| span.kind == HighlightTokenKind::ThreadName)
    );
    assert!(
        state
            .iter()
            .any(|span| span.kind == HighlightTokenKind::ThreadState)
    );
    assert!(
        frame
            .iter()
            .any(|span| span.kind == HighlightTokenKind::StackClass)
    );
    assert!(
        frame
            .iter()
            .any(|span| span.kind == HighlightTokenKind::StackMethod)
    );
    assert!(
        lock.iter()
            .any(|span| span.kind == HighlightTokenKind::Lock)
    );
}

/// 验证高亮范围不重叠，并且落在 UTF-8 边界上。
#[test]
fn highlight_ranges_are_non_overlapping_and_utf8_boundaries() {
    let line = "中文 key=值 ERROR";
    let spans = SyntaxHighlighter::highlight(line, HighlightLanguage::Log);

    for span in &spans {
        assert!(line.is_char_boundary(span.range.start));
        assert!(line.is_char_boundary(span.range.end));
    }
    for pair in spans.windows(2) {
        assert!(pair[0].range.end <= pair[1].range.start);
    }
}

/// 验证超长行只扫描上限范围。
#[test]
fn long_line_highlighting_is_capped() {
    let line = format!("{} ERROR", "a".repeat(MAX_HIGHLIGHT_BYTES + 32));
    let spans = SyntaxHighlighter::highlight(&line, HighlightLanguage::Log);

    assert!(
        spans
            .iter()
            .all(|span| span.range.end <= MAX_HIGHLIGHT_BYTES)
    );
}

/// 验证文件名和路径后缀能识别高亮语言。
#[test]
fn detects_language_from_label_or_path() {
    assert_eq!(
        detect_highlight_language("app.properties", ""),
        HighlightLanguage::Properties
    );
    assert_eq!(
        detect_highlight_language("dump.txt", "/logs/thread.jstack"),
        HighlightLanguage::JavaThreadDump
    );
    assert_eq!(
        detect_highlight_language("config.yaml", ""),
        HighlightLanguage::Yaml
    );
    assert_eq!(
        detect_highlight_language("Main.java", ""),
        HighlightLanguage::Java
    );
    assert_eq!(
        detect_highlight_language("bundle.min.js", ""),
        HighlightLanguage::JavaScript
    );
    assert_eq!(
        detect_highlight_language("theme.css", ""),
        HighlightLanguage::Css
    );
    assert_eq!(
        detect_highlight_language("page.txt", "/views/index.jsp"),
        HighlightLanguage::Jsp
    );
    assert_eq!(
        detect_highlight_language("query.sql", ""),
        HighlightLanguage::Sql
    );
    assert_eq!(
        detect_highlight_language("deploy.zsh", ""),
        HighlightLanguage::Shell
    );
}

/// 验证 Java 注解、关键字、类型、方法、数字和注释均能识别。
#[test]
fn highlights_java_source_tokens() {
    let spans = SyntaxHighlighter::highlight(
        r#"@Override public String greet() { return "hi" + 42; } // note"#,
        HighlightLanguage::Java,
    );

    assert_has_kinds(
        &spans,
        &[
            HighlightTokenKind::Annotation,
            HighlightTokenKind::Keyword,
            HighlightTokenKind::Type,
            HighlightTokenKind::Function,
            HighlightTokenKind::String,
            HighlightTokenKind::Number,
            HighlightTokenKind::Comment,
        ],
    );
}

/// 验证 JavaScript 关键字、函数、模板字符串和箭头运算符均能识别。
#[test]
fn highlights_javascript_source_tokens() {
    let spans = SyntaxHighlighter::highlight(
        "const load = async () => fetch(`/api/items`);",
        HighlightLanguage::JavaScript,
    );

    assert_has_kinds(
        &spans,
        &[
            HighlightTokenKind::Keyword,
            HighlightTokenKind::Function,
            HighlightTokenKind::String,
            HighlightTokenKind::Operator,
        ],
    );
}

/// 验证 CSS 选择器、属性、颜色、函数和数值均能识别。
#[test]
fn highlights_css_source_tokens() {
    let spans = SyntaxHighlighter::highlight(
        ".card { color: #fff; width: calc(100% - 2px); }",
        HighlightLanguage::Css,
    );

    assert_has_kinds(
        &spans,
        &[
            HighlightTokenKind::Selector,
            HighlightTokenKind::Attribute,
            HighlightTokenKind::Function,
            HighlightTokenKind::Number,
        ],
    );
}

/// 验证 JSP 指令、属性、HTML 标签和 EL 变量可以在同一行组合高亮。
#[test]
fn highlights_jsp_mixed_language_tokens() {
    let spans = SyntaxHighlighter::highlight(
        r#"<%@ page import="java.util.List" %><span>${user.name}</span>"#,
        HighlightLanguage::Jsp,
    );

    assert_has_kinds(
        &spans,
        &[
            HighlightTokenKind::Keyword,
            HighlightTokenKind::Attribute,
            HighlightTokenKind::String,
            HighlightTokenKind::Tag,
            HighlightTokenKind::Variable,
        ],
    );
}

/// 验证 JSP 注释优先于内部 EL 表达式，避免注释内容呈现为可执行模板。
#[test]
fn highlights_jsp_comment_as_single_token() {
    let line = "<%-- ${ignored.value} --%>";
    let spans = SyntaxHighlighter::highlight(line, HighlightLanguage::Jsp);

    assert_eq!(spans.len(), 1);
    assert_eq!(spans[0].kind, HighlightTokenKind::Comment);
    assert_eq!(&line[spans[0].range.clone()], line);
}

/// 验证 SQL 大小写不敏感关键字、函数、参数、字面量和注释均能识别。
#[test]
fn highlights_sql_source_tokens() {
    let spans = SyntaxHighlighter::highlight(
        "SELECT count(*) FROM users WHERE id = :id AND active = true; -- note",
        HighlightLanguage::Sql,
    );

    assert_has_kinds(
        &spans,
        &[
            HighlightTokenKind::Keyword,
            HighlightTokenKind::Function,
            HighlightTokenKind::Variable,
            HighlightTokenKind::Boolean,
            HighlightTokenKind::Comment,
        ],
    );
}

/// 验证 Shell 关键字、变量、运算符和行尾注释均能识别。
#[test]
fn highlights_shell_source_tokens() {
    let spans = SyntaxHighlighter::highlight(
        "export PATH=$PATH:/opt/bin && true # note",
        HighlightLanguage::Shell,
    );

    assert_has_kinds(
        &spans,
        &[
            HighlightTokenKind::Keyword,
            HighlightTokenKind::Variable,
            HighlightTokenKind::Operator,
            HighlightTokenKind::Boolean,
            HighlightTokenKind::Comment,
        ],
    );
}

/// 验证新增代码语言在中英文混排时仍产生有序、合法且不重叠的 UTF-8 范围。
#[test]
fn code_highlight_ranges_are_non_overlapping_utf8_boundaries() {
    let cases = [
        (HighlightLanguage::Java, "String 名称 = \"Argus\";"),
        (HighlightLanguage::JavaScript, "const 名称 = `Argus`;"),
        (HighlightLanguage::Css, ".标题 { color: #fff; }"),
        (HighlightLanguage::Jsp, "<span>${用户.name}</span>"),
        (HighlightLanguage::Sql, "SELECT 名称 FROM 表名;"),
        (HighlightLanguage::Shell, "export 名称=$PATH"),
    ];

    for (language, line) in cases {
        let spans = SyntaxHighlighter::highlight(line, language);
        for span in &spans {
            assert!(line.is_char_boundary(span.range.start));
            assert!(line.is_char_boundary(span.range.end));
        }
        for pair in spans.windows(2) {
            assert!(pair[0].range.end <= pair[1].range.start);
        }
    }
}

/// 断言高亮结果至少包含所有期望 token 类型，避免测试绑定具体分词数量和范围。
fn assert_has_kinds(spans: &[HighlightSpan], expected: &[HighlightTokenKind]) {
    for kind in expected {
        assert!(
            spans.iter().any(|span| span.kind == *kind),
            "缺少期望的高亮 token：{kind:?}，实际为 {spans:?}"
        );
    }
}
