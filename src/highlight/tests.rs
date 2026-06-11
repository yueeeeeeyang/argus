//! 文件职责：验证日志与配置文件高亮规则。
//! 创建日期：2026-06-11
//! 修改日期：2026-06-11
//! 作者：Argus 开发团队
//! 主要功能：覆盖语言识别、各格式核心 token、范围合法性和超长行扫描上限。

use super::*;

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
}
