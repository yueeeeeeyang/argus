//! 文件职责：验证 Markdown 行内混排的真实布局及 CommonMark 块、换行与复制边界。

use super::*;
use gpui::{Context, Render, TestAppContext, Window, size};

/// 使用真实消息排版参数，固定可用宽度，让测试覆盖 GPUI 的文本测量与自然换行。
struct MarkdownLayoutView {
    markdown: String,
    width: f32,
}

impl Render for MarkdownLayoutView {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        let theme = AppTheme::dark();
        div()
            .size_full()
            .flex()
            .items_start()
            .font_family(crate::fonts::ARGUS_UI_FONT_FAMILY)
            .child(
                div()
                    .debug_selector(|| "markdown-layout-result".to_string())
                    .w(px(self.width))
                    .child(render_markdown(
                        &self.markdown,
                        MarkdownStyle {
                            font_size: 12.0,
                            line_height: 20.0,
                            color: theme.foreground,
                        },
                        &theme,
                    )),
            )
    }
}

/// 从当次创建的独立窗口取得实际排版高度，避免历史 debug selector 影响断言。
fn markdown_bounds(
    markdown: &str,
    width: f32,
    cx: &mut TestAppContext,
) -> gpui::Bounds<gpui::Pixels> {
    let markdown = markdown.to_string();
    let (_view, cx) = cx.add_window_view(move |_, _| MarkdownLayoutView { markdown, width });
    cx.simulate_resize(size(px(1000.0), px(900.0)));
    cx.debug_bounds("markdown-layout-result")
        .expect("应完成 Markdown 实际布局")
}

/// 紧凑列表的普通文本、强调、代码和链接都是行内节点，宽度足够时必须同排。
#[gpui::test]
fn tight_list_inline_styles_stay_on_one_visual_line(cx: &mut TestAppContext) {
    for markdown in [
        "- prefix `alpha` / `beta` / `gamma` suffix",
        "1. **Important**: a `name` and [link](https://example.invalid) suffix",
        "- [x] **Done** with *emphasis* and ~~old~~ text",
        "prefix **strong** with `code` and [link](https://example.invalid) suffix",
    ] {
        let bounds = markdown_bounds(markdown, 800.0, cx);
        assert!(
            (f32::from(bounds.size.height) - 20.0).abs() <= 1.0,
            "行内节点不能独占块级行：{bounds:?}"
        );
        assert_eq!(bounds.size.width, px(800.0));
    }
}

/// 多个紧凑列表项各占一行，不能在同一项内根据样式节点数量增加高度。
#[gpui::test]
fn tight_list_items_preserve_item_boundaries(cx: &mut TestAppContext) {
    let styled = markdown_bounds(
        "- **First** `alpha` value\n- **Second** `beta` value",
        800.0,
        cx,
    );
    let plain = markdown_bounds("- First alpha value\n- Second beta value", 800.0, cx);
    assert_eq!(styled.size.height, plain.size.height);
    assert!(styled.size.height > px(20.0));
    assert!(styled.size.height < px(60.0));
}

/// 变窄只按可用宽度自然折行，行内样式边界不会产生额外的段落间距或横向溢出。
#[gpui::test]
fn inline_styles_wrap_naturally_in_narrow_list(cx: &mut TestAppContext) {
    let markdown = "- before **important words** and `code value` after [linked words](https://example.invalid) end";
    let wide = markdown_bounds(markdown, 800.0, cx);
    let narrow = markdown_bounds(markdown, 170.0, cx);
    assert!(
        narrow.size.height > wide.size.height,
        "窄宽必须发生真实文本换行"
    );
    assert_eq!(narrow.size.width, px(170.0));
    let lines = f32::from(narrow.size.height) / 20.0;
    assert!(
        (lines - lines.round()).abs() <= 0.05,
        "同一段自然折行不应混入块节点间距：{narrow:?}"
    );
}

/// 软换行可同行显示，两个空格或反斜杠表达的硬换行必须产生新视觉行。
#[gpui::test]
fn markdown_soft_and_hard_breaks_keep_commonmark_layout(cx: &mut TestAppContext) {
    let soft = markdown_bounds("- first\n  **second**", 800.0, cx);
    let spaces = markdown_bounds("- first  \n  **second**", 800.0, cx);
    let backslash = markdown_bounds("- first\\\n  **second**", 800.0, cx);
    assert!((f32::from(soft.size.height) - 20.0).abs() <= 1.0);
    assert!((f32::from(spaces.size.height) - 40.0).abs() <= 1.0);
    assert_eq!(spaces.size.height, backslash.size.height);
}

/// 真正的松散段落和嵌套列表必须保留，不能为了修行内排版而吞掉源文本的空行。
#[test]
fn child_groups_keep_loose_paragraphs_and_nested_blocks() {
    let document = parse_markdown(
        "- first **paragraph**\n\n  second `paragraph`\n\n  - nested [item](https://example.invalid)\n",
    );
    let item = &document.children[0].children[0];
    let groups = markdown_child_groups(&item.children);
    assert_eq!(groups.len(), 3);
    assert!(matches!(
        groups[0],
        MarkdownChildGroup::Block(MarkdownNode {
            kind: MarkdownNodeKind::Paragraph,
            ..
        })
    ));
    assert!(matches!(
        groups[1],
        MarkdownChildGroup::Block(MarkdownNode {
            kind: MarkdownNodeKind::Paragraph,
            ..
        })
    ));
    assert!(matches!(
        groups[2],
        MarkdownChildGroup::Block(MarkdownNode {
            kind: MarkdownNodeKind::List(_),
            ..
        })
    ));
}

/// 行内分组复用原节点并保留代码、粗体、链接样式，不修改原文本来规避排版。
#[test]
fn tight_list_group_keeps_inline_text_and_style_ranges() {
    let document =
        parse_markdown("- prefix **bold** `code` [link](https://example.invalid) suffix");
    let item = &document.children[0].children[0];
    let groups = markdown_child_groups(&item.children);
    assert_eq!(groups.len(), 1);
    let MarkdownChildGroup::Inline(nodes) = groups[0] else {
        panic!("紧凑列表必须合并为行内组");
    };
    let (text, spans) = inline_nodes_text_and_spans(nodes);
    assert_eq!(text, "prefix bold code link suffix");
    assert!(
        spans
            .iter()
            .any(|span| span.style.is_bold && &text[span.range.clone()] == "bold")
    );
    assert!(
        spans
            .iter()
            .any(|span| span.style.is_code && &text[span.range.clone()] == "code")
    );
    assert!(
        spans
            .iter()
            .any(|span| span.style.is_link && &text[span.range.clone()] == "link")
    );
}

/// 选择复制同样保留列表项终止符，避免下一项的减号粘到上一项正文后。
#[test]
fn plain_text_preserves_list_and_paragraph_boundaries() {
    assert_eq!(
        markdown_plain_text("- **First** `alpha`\n- Second [beta](https://example.invalid)"),
        "- First alpha\n- Second beta"
    );
    assert_eq!(
        markdown_plain_text("**Title**\n\n- alpha\n- beta"),
        "Title\n\n- alpha\n- beta"
    );
    assert_eq!(
        markdown_plain_text("- first\n\n  second"),
        "- first\n\nsecond"
    );
}

/// 展示和复制都保留显式硬换行，而软换行仍折叠成普通空格。
#[test]
fn plain_text_distinguishes_soft_and_hard_breaks() {
    assert_eq!(markdown_plain_text("first\nsecond"), "first second");
    assert_eq!(markdown_plain_text("first  \nsecond"), "first\nsecond");
    assert_eq!(
        markdown_plain_text("- first\\\n  second"),
        "- first\nsecond"
    );
}

/// 代码块内的空行和列表标记必须原样解析，列表项内的空行也不能被强制移除。
#[test]
fn fenced_code_and_literal_list_markers_keep_source_structure() {
    let document = parse_markdown("- intro\n\n  ```text\n  first\n\n  +\n\n  last\n  ```\n");
    let item = &document.children[0].children[0];
    assert_eq!(item.children.len(), 2);
    assert!(matches!(item.children[0].kind, MarkdownNodeKind::Paragraph));
    assert!(matches!(
        item.children[1].kind,
        MarkdownNodeKind::CodeBlock(_)
    ));
    assert_eq!(
        collect_plain_text(&item.children[1]),
        "first\n\n+\n\nlast\n"
    );
}

/// 迁移原纯文本回归：段落和列表仍完整可复制，并进一步校验列表项之间的换行。
#[test]
fn markdown_plain_text_keeps_paragraphs_and_lists() {
    let text = markdown_plain_text("**重点**：连接池\n\n- 数据库\n- JVM");
    assert!(text.contains("重点：连接池"));
    assert!(text.contains("- 数据库"));
    assert!(text.contains("- JVM"));
    assert_eq!(text, "重点：连接池\n\n- 数据库\n- JVM");
}

/// 迁移原代码块回归：通过真实解析路径检查空行和孤立标记，不再调用已移除的文本改写步骤。
#[test]
fn fenced_code_blocks_are_not_normalized() {
    let markdown = "```text\na\n\n+\n\n```\n";
    let document = parse_markdown(markdown);
    assert_eq!(document.children.len(), 1);
    assert!(matches!(
        document.children[0].kind,
        MarkdownNodeKind::CodeBlock(_)
    ));
    assert_eq!(collect_plain_text(&document.children[0]), "a\n\n+\n\n");
}

/// 替代原吞空行断言：源文本中的独立段落和合法嵌套列表应保留，不以改写文本掩盖行内布局问题。
#[test]
fn fragmented_list_blank_lines_preserve_explicit_paragraphs() {
    let markdown = "1. title\n\n   : detail\n\n   `alpha`\n\n   +\n\n   `beta`\n\n   tail\n";
    let document = parse_markdown(markdown);
    let item = &document.children[0].children[0];
    assert_eq!(
        item.children
            .iter()
            .filter(|node| matches!(node.kind, MarkdownNodeKind::Paragraph))
            .count(),
        5
    );
    assert!(
        item.children
            .iter()
            .any(|node| matches!(node.kind, MarkdownNodeKind::List(_)))
    );
    assert_eq!(inline_text_and_spans(&item.children[0]).0, "title");
}

/// 替代原硬换行转空格断言：作者明确写入的硬换行必须按原语义显示和复制。
#[test]
fn hard_breaks_are_rendered_as_explicit_newlines() {
    let document = parse_markdown("第一行  \n第二行");
    let paragraph = &document.children[0];
    let (text, _) = inline_text_and_spans(paragraph);
    assert_eq!(text, "第一行\n第二行");
    assert_eq!(markdown_plain_text("第一行  \n第二行"), text);
}

/// 空消息和纯空白的流式初始状态应保持零内容，不产生占位列表或额外段落。
#[gpui::test]
fn empty_markdown_has_no_text_or_layout_height(cx: &mut TestAppContext) {
    for source in ["", " \n\n\t"] {
        let document = parse_markdown(source);
        assert!(document.children.is_empty());
        assert!(markdown_plain_text(source).is_empty());
        assert!(markdown_bounds(source, 800.0, cx).size.height <= px(1.0));
    }
}

/// 未闭合的流式行内标记保留可见原文；闭合后只改变样式，不因列表解析产生额外视觉行。
#[gpui::test]
fn unfinished_streaming_inline_markers_remain_visible(cx: &mut TestAppContext) {
    for (source, expected) in [
        ("- before **unfinished", "- before **unfinished"),
        ("- before `unfinished", "- before `unfinished"),
        ("- before [unfinished", "- before [unfinished"),
        ("- before **finished**", "- before finished"),
    ] {
        assert_eq!(markdown_plain_text(source), expected);
        let bounds = markdown_bounds(source, 800.0, cx);
        assert!(
            (f32::from(bounds.size.height) - 20.0).abs() <= 1.0,
            "未闭合标记不能把流式列表拆成多个块：{bounds:?}"
        );
    }
}
