//! 文件职责：覆盖 YAML 引号和列表键、Shell 词边界与扫描前进的缺陷回归。

use crate::highlight::{HighlightLanguage, HighlightSpan, HighlightTokenKind, SyntaxHighlighter};

/// 所有词法回归同时验证范围可安全切片，防止中文或转义字符引入非法字节边界。
fn spans(line: &str, language: HighlightLanguage) -> Vec<HighlightSpan> {
    let spans = SyntaxHighlighter::highlight(line, language);
    let mut previous_end = 0;
    for span in &spans {
        assert!(span.range.start >= previous_end);
        assert!(span.range.start < span.range.end);
        assert!(span.range.end <= line.len());
        assert!(line.is_char_boundary(span.range.start));
        assert!(line.is_char_boundary(span.range.end));
        previous_end = span.range.end;
    }
    spans
}

/// 按原始文本检查精确词法边界，不只断言某种颜色曾出现。
fn token_texts<'a>(
    line: &'a str,
    spans: &[HighlightSpan],
    kind: HighlightTokenKind,
) -> Vec<&'a str> {
    spans
        .iter()
        .filter(|span| span.kind == kind)
        .map(|span| &line[span.range.clone()])
        .collect()
}

/// 分号必须被消费，否则正常控制语句会让扫描器永远停在同一个字节。
#[test]
fn shell_semicolons_advance_and_preserve_following_tokens() {
    let line = "if true; then echo 42; fi # real";
    let spans = spans(line, HighlightLanguage::Shell);
    assert_eq!(
        token_texts(line, &spans, HighlightTokenKind::Punctuation),
        [";", ";"]
    );
    assert_eq!(
        token_texts(line, &spans, HighlightTokenKind::Keyword),
        ["if", "then", "fi"]
    );
    assert_eq!(
        token_texts(line, &spans, HighlightTokenKind::Number),
        ["42"]
    );
    assert_eq!(
        token_texts(line, &spans, HighlightTokenKind::Comment),
        ["# real"]
    );
}

/// 井号在普通参数、转义内容及引号后缀中不是注释起点。
#[test]
fn shell_hash_requires_a_word_boundary() {
    for line in [
        "echo foo#bar # real",
        r"echo foo\ #bar # real",
        r"echo \#bar # real",
        r##"echo "foo"#bar # real"##,
        "echo ${#value} # real",
        r"echo \中#文 # real",
        "echo value;# real",
    ] {
        let spans = spans(line, HighlightLanguage::Shell);
        assert_eq!(
            token_texts(line, &spans, HighlightTokenKind::Comment),
            ["# real"],
            "{line}"
        );
    }
    for line in ["echo foo#bar", r"echo foo\ #bar", r##"echo "foo"#bar"##] {
        assert!(
            token_texts(
                line,
                &spans(line, HighlightLanguage::Shell),
                HighlightTokenKind::Comment
            )
            .is_empty(),
            "{line}"
        );
    }
}

/// 普通单引号内的反斜杠不能吞掉结束引号和后面的真实注释。
#[test]
fn shell_single_quotes_do_not_escape_closing_quote() {
    let line = r"echo '\' # real";
    let spans = spans(line, HighlightLanguage::Shell);
    assert_eq!(
        token_texts(line, &spans, HighlightTokenKind::String),
        [r"'\'"]
    );
    assert_eq!(
        token_texts(line, &spans, HighlightTokenKind::Comment),
        ["# real"]
    );
}

/// Bash ANSI C 引号有独立转义语义，不能因修复普通单引号而把内部井号误判成注释。
#[test]
fn shell_ansi_quotes_keep_escape_semantics() {
    let line = r"echo $'it\'s # text' # real";
    let spans = spans(line, HighlightLanguage::Shell);
    assert_eq!(
        token_texts(line, &spans, HighlightTokenKind::String),
        [r"$'it\'s # text'"]
    );
    assert_eq!(
        token_texts(line, &spans, HighlightTokenKind::Comment),
        ["# real"]
    );
}

/// 未闭合引号覆盖当前行尾，不把内容中的井号错误解释为注释。
#[test]
fn shell_unclosed_quotes_are_bounded_by_line_end() {
    let line = "echo '中文 # literal";
    let spans = spans(line, HighlightLanguage::Shell);
    assert_eq!(
        token_texts(line, &spans, HighlightTokenKind::String),
        ["'中文 # literal"]
    );
    assert!(token_texts(line, &spans, HighlightTokenKind::Comment).is_empty());
}

/// 注释扫描与字符串扫描必须采用相同的 YAML 引号规则。
#[test]
fn yaml_comments_skip_escaped_and_doubled_quotes() {
    for (line, expected_string) in [
        (
            r#"message: "a\" # not comment" # real"#,
            r#""a\" # not comment""#,
        ),
        ("message: 'it''s # text' # real", "'it''s # text'"),
        (r"path: '\' # real", r"'\'"),
        ("message: '中文 # 文本' # real", "'中文 # 文本'"),
    ] {
        let spans = spans(line, HighlightLanguage::Yaml);
        assert_eq!(
            token_texts(line, &spans, HighlightTokenKind::String),
            [expected_string],
            "{line}"
        );
        assert_eq!(
            token_texts(line, &spans, HighlightTokenKind::Comment),
            ["# real"],
            "{line}"
        );
    }
}

/// 列表标记与键不应重叠，否则高优先级标点会使整个键被丢弃。
#[test]
fn yaml_list_mapping_keys_exclude_sequence_marker() {
    for (line, expected_key) in [
        ("- name: example", "name"),
        ("  - 名称: \"中文\" # real", "名称"),
        ("  -   enabled: true", "enabled"),
    ] {
        let spans = spans(line, HighlightLanguage::Yaml);
        assert_eq!(
            token_texts(line, &spans, HighlightTokenKind::Key),
            [expected_key]
        );
        assert_eq!(
            token_texts(line, &spans, HighlightTokenKind::Punctuation),
            ["-", ":"]
        );
    }
}

/// 引号内的冒号不会截断映射键，也不会把独立字符串当作映射。
#[test]
fn yaml_mapping_colon_ignores_quoted_content() {
    let mapping = r#""scheme:key": true"#;
    let mapping_spans = spans(mapping, HighlightLanguage::Yaml);
    assert_eq!(
        token_texts(mapping, &mapping_spans, HighlightTokenKind::Key),
        [r#""scheme:key""#]
    );
    assert_eq!(
        token_texts(mapping, &mapping_spans, HighlightTokenKind::Boolean),
        ["true"]
    );
    let scalar = r#""scheme:value""#;
    let scalar_spans = spans(scalar, HighlightLanguage::Yaml);
    assert!(token_texts(scalar, &scalar_spans, HighlightTokenKind::Key).is_empty());
    assert_eq!(
        token_texts(scalar, &scalar_spans, HighlightTokenKind::String),
        [scalar]
    );
}

/// 协议冒号属于普通标量；带空白的映射及带引号的 JSON 风格键仍正常识别。
#[test]
fn yaml_url_scalar_is_not_a_mapping_key() {
    for line in ["- https://example.test/path", "https://example.test/path"] {
        let spans = spans(line, HighlightLanguage::Yaml);
        assert!(token_texts(line, &spans, HighlightTokenKind::Key).is_empty());
        assert!(!token_texts(line, &spans, HighlightTokenKind::Punctuation).contains(&":"));
    }
    for (line, key) in [
        ("url: https://example.test/path", "url"),
        (r#""url":true"#, r#""url""#),
        ("key:", "key"),
    ] {
        assert_eq!(
            token_texts(
                line,
                &spans(line, HighlightLanguage::Yaml),
                HighlightTokenKind::Key
            ),
            [key]
        );
    }
}

/// 美元符号可作为标量内容，扫描器必须消费该字符并继续识别后面的注释。
#[test]
fn yaml_dollar_prefixed_values_advance_and_preserve_following_tokens() {
    for (line, expected_values) in [
        ("value: $HOME # real", vec!["$HOME"]),
        ("value: $ # real", vec!["$"]),
        ("value: ${ENV} # real", vec!["$", "ENV"]),
        ("value: prefix$SUFFIX # real", vec!["prefix$SUFFIX"]),
        ("value: $中文 # real", vec!["$"]),
    ] {
        let spans = spans(line, HighlightLanguage::Yaml);
        assert_eq!(
            token_texts(line, &spans, HighlightTokenKind::Value),
            expected_values,
            "{line}"
        );
        assert_eq!(
            token_texts(line, &spans, HighlightTokenKind::Comment),
            ["# real"],
            "{line}"
        );
    }
}

/// 未结束的双引号仍把井号视为字符串内容，扫描不能越界或产生重叠范围。
#[test]
fn yaml_unclosed_quote_does_not_start_comment() {
    let line = "message: \"中文 # literal";
    let spans = spans(line, HighlightLanguage::Yaml);
    assert_eq!(
        token_texts(line, &spans, HighlightTokenKind::String),
        ["\"中文 # literal"]
    );
    assert!(token_texts(line, &spans, HighlightTokenKind::Comment).is_empty());
}
