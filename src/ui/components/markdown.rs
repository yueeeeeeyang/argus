//! 文件职责：把受信任模型返回的 CommonMark/GFM 文本渲染为安全的 GPUI 只读界面。
//! 创建日期：2026-07-16
//! 修改日期：2026-07-16
//! 作者：Argus 开发团队
//! 主要功能：解析标题、行内样式、列表、引用、代码块和表格，不执行 HTML、脚本或远程资源。

use std::ops::Range;

use gpui::{
    AnyElement, FontStyle, FontWeight, HighlightStyle, IntoElement, StrikethroughStyle, StyledText,
    div, prelude::*, px, rgb,
};
use pulldown_cmark::{Alignment, CodeBlockKind, Event, HeadingLevel, Options, Parser, Tag};

use crate::fonts::ARGUS_LOG_FONT_FAMILY;
use crate::theme::AppTheme;

/// Markdown 正文的基础排版参数，由不同消息类型按现有视觉层级传入。
#[derive(Clone, Copy, Debug)]
pub(crate) struct MarkdownStyle {
    /// 普通正文大小，单位为逻辑像素。
    pub font_size: f32,
    /// 普通正文行高，单位为逻辑像素。
    pub line_height: f32,
    /// 普通正文颜色，使用主题的 RGB 整数表示。
    pub color: u32,
}

/// 解析后使用的轻量节点类型；只保留界面展示需要的语义，禁止透传可执行 HTML。
#[derive(Clone, Debug, PartialEq)]
enum MarkdownNodeKind {
    Document,
    Paragraph,
    Heading(u8),
    BlockQuote,
    CodeBlock(Option<String>),
    List(Option<u64>),
    ListItem,
    Table(Vec<Alignment>),
    TableHead,
    TableRow,
    TableCell,
    Emphasis,
    Strong,
    Strikethrough,
    Link,
    Image,
    GenericBlock,
    GenericInline,
    Text(String),
    Code(String),
    SoftBreak,
    HardBreak,
    Rule,
    TaskMarker(bool),
}

/// Markdown 节点树；解析器事件始终平衡，树结构可自然支持嵌套列表、引用和行内样式。
#[derive(Clone, Debug, PartialEq)]
struct MarkdownNode {
    /// 当前节点的展示语义。
    kind: MarkdownNodeKind,
    /// 容器节点的子内容；文本和分隔线等叶子节点保持为空。
    children: Vec<Self>,
}

impl MarkdownNode {
    /// 创建不带子节点的 Markdown 节点。
    fn new(kind: MarkdownNodeKind) -> Self {
        Self {
            kind,
            children: Vec::new(),
        }
    }
}

/// 行内文本当前累积的样式状态，用于把嵌套 Markdown 标签合并为互不重叠的高亮区间。
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct InlineStyle {
    is_bold: bool,
    is_italic: bool,
    is_strikethrough: bool,
    is_code: bool,
    is_link: bool,
}

/// 一段连续且样式相同的 UTF-8 字节范围。
#[derive(Clone, Debug, Eq, PartialEq)]
struct InlineSpan {
    range: Range<usize>,
    style: InlineStyle,
}

/// 渲染一段 CommonMark/GFM 文本。
///
/// 参数：`markdown` 为模型或用户提供的只读文本；`style` 为正文排版；`theme` 为当前主题。
/// 返回值：可直接嵌入消息瀑布流的 GPUI 元素。原始 HTML 只作为普通文本展示，远程图片不会加载。
pub(crate) fn render_markdown(
    markdown: &str,
    style: MarkdownStyle,
    theme: &AppTheme,
) -> AnyElement {
    let document = parse_markdown(markdown);
    render_markdown_children(&document.children, style, theme)
}

/// 使用 CommonMark 和常用 GFM 扩展构造展示树；流式输入即使语法尚未闭合也由解析器安全降级。
fn parse_markdown(markdown: &str) -> MarkdownNode {
    let options =
        Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TASKLISTS;
    let mut stack = vec![MarkdownNode::new(MarkdownNodeKind::Document)];

    for event in Parser::new_ext(markdown, options) {
        match event {
            Event::Start(tag) => stack.push(MarkdownNode::new(node_kind_from_tag(tag))),
            Event::End(_) => {
                // pulldown-cmark 保证 Start/End 平衡；仍保留根节点防护，避免未来解析选项变化造成 panic。
                if stack.len() > 1 {
                    let node = stack.pop().expect("Markdown 子节点必须存在");
                    stack
                        .last_mut()
                        .expect("Markdown 根节点必须存在")
                        .children
                        .push(node);
                }
            }
            Event::Text(text) => push_leaf(&mut stack, MarkdownNodeKind::Text(text.into_string())),
            Event::Code(code) | Event::InlineMath(code) | Event::DisplayMath(code) => {
                push_leaf(&mut stack, MarkdownNodeKind::Code(code.into_string()));
            }
            Event::Html(html) | Event::InlineHtml(html) => {
                // 不解释模型返回的 HTML，按普通文本显示可彻底关闭脚本、样式和远程资源注入。
                push_leaf(&mut stack, MarkdownNodeKind::Text(html.into_string()));
            }
            Event::FootnoteReference(label) => push_leaf(
                &mut stack,
                MarkdownNodeKind::Text(format!("[{}]", label.as_ref())),
            ),
            Event::SoftBreak => push_leaf(&mut stack, MarkdownNodeKind::SoftBreak),
            Event::HardBreak => push_leaf(&mut stack, MarkdownNodeKind::HardBreak),
            Event::Rule => push_leaf(&mut stack, MarkdownNodeKind::Rule),
            Event::TaskListMarker(is_checked) => {
                push_leaf(&mut stack, MarkdownNodeKind::TaskMarker(is_checked));
            }
        }
    }

    // 正常解析只剩根节点；该回收逻辑同时让任何未来新增的容错事件保持内容可见。
    while stack.len() > 1 {
        let node = stack.pop().expect("Markdown 子节点必须存在");
        stack
            .last_mut()
            .expect("Markdown 根节点必须存在")
            .children
            .push(node);
    }
    stack.pop().expect("Markdown 根节点必须存在")
}

/// 把解析器容器标签转换为内部展示语义。
fn node_kind_from_tag(tag: Tag<'_>) -> MarkdownNodeKind {
    match tag {
        Tag::Paragraph => MarkdownNodeKind::Paragraph,
        Tag::Heading { level, .. } => MarkdownNodeKind::Heading(heading_level_number(level)),
        Tag::BlockQuote(_) => MarkdownNodeKind::BlockQuote,
        Tag::CodeBlock(kind) => MarkdownNodeKind::CodeBlock(code_block_language(kind)),
        Tag::HtmlBlock => MarkdownNodeKind::GenericBlock,
        Tag::List(start) => MarkdownNodeKind::List(start),
        Tag::Item => MarkdownNodeKind::ListItem,
        Tag::Table(alignments) => MarkdownNodeKind::Table(alignments),
        Tag::TableHead => MarkdownNodeKind::TableHead,
        Tag::TableRow => MarkdownNodeKind::TableRow,
        Tag::TableCell => MarkdownNodeKind::TableCell,
        Tag::Emphasis => MarkdownNodeKind::Emphasis,
        Tag::Strong => MarkdownNodeKind::Strong,
        Tag::Strikethrough => MarkdownNodeKind::Strikethrough,
        Tag::Link { .. } => MarkdownNodeKind::Link,
        Tag::Image { .. } => MarkdownNodeKind::Image,
        Tag::FootnoteDefinition(_)
        | Tag::DefinitionList
        | Tag::DefinitionListTitle
        | Tag::DefinitionListDefinition
        | Tag::MetadataBlock(_) => MarkdownNodeKind::GenericBlock,
        Tag::Superscript | Tag::Subscript => MarkdownNodeKind::GenericInline,
    }
}

/// 将叶子节点追加到当前最深容器，保证解析函数分支保持简洁。
fn push_leaf(stack: &mut [MarkdownNode], kind: MarkdownNodeKind) {
    stack
        .last_mut()
        .expect("Markdown 根节点必须存在")
        .children
        .push(MarkdownNode::new(kind));
}

/// 将标题枚举转换为便于排版计算的 1～6 数字。
fn heading_level_number(level: HeadingLevel) -> u8 {
    match level {
        HeadingLevel::H1 => 1,
        HeadingLevel::H2 => 2,
        HeadingLevel::H3 => 3,
        HeadingLevel::H4 => 4,
        HeadingLevel::H5 => 5,
        HeadingLevel::H6 => 6,
    }
}

/// 提取 fenced code 的语言提示；空提示和缩进代码统一不显示语言标签。
fn code_block_language(kind: CodeBlockKind<'_>) -> Option<String> {
    match kind {
        CodeBlockKind::Indented => None,
        CodeBlockKind::Fenced(language) => {
            let language = language.trim();
            (!language.is_empty()).then(|| language.to_string())
        }
    }
}

/// 渲染同一容器下的一组块节点，并使用统一间距形成自然文档流。
fn render_markdown_children(
    children: &[MarkdownNode],
    style: MarkdownStyle,
    theme: &AppTheme,
) -> AnyElement {
    div()
        .w_full()
        .min_w(px(0.0))
        .flex()
        .flex_col()
        .gap_2()
        .children(
            children
                .iter()
                .map(|node| render_markdown_node(node, style, theme)),
        )
        .into_any_element()
}

/// 根据节点语义选择对应的块级排版；行内容器由 `render_inline_text` 合并样式。
fn render_markdown_node(node: &MarkdownNode, style: MarkdownStyle, theme: &AppTheme) -> AnyElement {
    match &node.kind {
        MarkdownNodeKind::Paragraph => render_inline_block(node, style, FontWeight::NORMAL, theme),
        MarkdownNodeKind::Heading(level) => render_heading(node, *level, style, theme),
        MarkdownNodeKind::BlockQuote => div()
            .w_full()
            .min_w(px(0.0))
            .pl_3()
            .py_1()
            .border_l_1()
            .border_color(rgb(theme.info))
            .text_color(rgb(theme.foreground_muted))
            .child(render_markdown_children(&node.children, style, theme))
            .into_any_element(),
        MarkdownNodeKind::CodeBlock(language) => {
            render_code_block(node, language.as_deref(), style, theme)
        }
        MarkdownNodeKind::List(start) => render_list(node, *start, style, theme),
        MarkdownNodeKind::ListItem => render_markdown_children(&node.children, style, theme),
        MarkdownNodeKind::Table(alignments) => render_table(node, alignments, style, theme),
        MarkdownNodeKind::Rule => div()
            .w_full()
            .h(px(1.0))
            .my_2()
            .bg(rgb(theme.border))
            .into_any_element(),
        MarkdownNodeKind::GenericBlock | MarkdownNodeKind::Document => {
            render_markdown_children(&node.children, style, theme)
        }
        // 顶层出现行内节点时仍按正文展示，保证流式半成品和非标准模型输出不会丢失。
        _ => render_inline_block(node, style, FontWeight::NORMAL, theme),
    }
}

/// 渲染标题并限制字号跨度，保持消息瀑布流紧凑而不牺牲层级识别。
fn render_heading(
    node: &MarkdownNode,
    level: u8,
    style: MarkdownStyle,
    theme: &AppTheme,
) -> AnyElement {
    let font_size = match level {
        1 => style.font_size + 5.0,
        2 => style.font_size + 3.0,
        3 => style.font_size + 1.5,
        _ => style.font_size,
    };
    div()
        .w_full()
        .min_w(px(0.0))
        .mt(px(if level <= 2 { 5.0 } else { 2.0 }))
        .text_size(px(font_size))
        .line_height(px((font_size + 7.0).max(style.line_height)))
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(rgb(style.color))
        .child(render_inline_text(node, theme))
        .into_any_element()
}

/// 渲染普通行内内容，父元素负责基础字号、行高和颜色。
fn render_inline_block(
    node: &MarkdownNode,
    style: MarkdownStyle,
    weight: FontWeight,
    theme: &AppTheme,
) -> AnyElement {
    div()
        .w_full()
        .min_w(px(0.0))
        .text_size(px(style.font_size))
        .line_height(px(style.line_height))
        .font_weight(weight)
        .text_color(rgb(style.color))
        .child(render_inline_text(node, theme))
        .into_any_element()
}

/// 渲染 fenced/indented code，使用日志等宽字体并在消息宽度内折行，避免代码块撑破瀑布流。
fn render_code_block(
    node: &MarkdownNode,
    language: Option<&str>,
    style: MarkdownStyle,
    theme: &AppTheme,
) -> AnyElement {
    let code = collect_plain_text(node).trim_end_matches('\n').to_string();
    div()
        .w_full()
        .min_w(px(0.0))
        .rounded_md()
        .border_1()
        .border_color(rgb(theme.border))
        .bg(rgb(theme.current_line))
        .when_some(language.map(str::to_string), |this, language| {
            this.child(
                div()
                    .px_3()
                    .pt_2()
                    .text_size(px(9.0))
                    .text_color(rgb(theme.syntax.comment))
                    .child(language),
            )
        })
        .child(
            div()
                .w_full()
                .min_w(px(0.0))
                .p_3()
                .overflow_hidden()
                .font_family(ARGUS_LOG_FONT_FAMILY)
                .text_size(px((style.font_size - 1.0).max(10.0)))
                .line_height(px(style.line_height))
                .text_color(rgb(theme.foreground))
                .child(div().whitespace_normal().child(code)),
        )
        .into_any_element()
}

/// 渲染有序或无序列表，嵌套块内容继续复用同一 Markdown 渲染器。
fn render_list(
    node: &MarkdownNode,
    start: Option<u64>,
    style: MarkdownStyle,
    theme: &AppTheme,
) -> AnyElement {
    let items = node
        .children
        .iter()
        .filter(|child| child.kind == MarkdownNodeKind::ListItem)
        .enumerate()
        .map(|(index, item)| {
            let marker = start
                .map(|start| format!("{}.", start.saturating_add(index as u64)))
                .unwrap_or_else(|| "•".to_string());
            div()
                .w_full()
                .min_w(px(0.0))
                .flex()
                .items_start()
                .gap_2()
                .child(
                    div()
                        .w(px(22.0))
                        .flex_none()
                        .text_right()
                        .text_size(px(style.font_size))
                        .line_height(px(style.line_height))
                        .text_color(rgb(theme.info))
                        .child(marker),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w(px(0.0))
                        .child(render_markdown_children(&item.children, style, theme)),
                )
        });
    div()
        .w_full()
        .min_w(px(0.0))
        .flex()
        .flex_col()
        .gap_1()
        .children(items)
        .into_any_element()
}

/// 渲染 GFM 表格；列宽平均分配，单元格文本按声明的对齐方式自动折行。
fn render_table(
    node: &MarkdownNode,
    alignments: &[Alignment],
    style: MarkdownStyle,
    theme: &AppTheme,
) -> AnyElement {
    let rows = collect_table_rows(node);
    div()
        .w_full()
        .min_w(px(0.0))
        .overflow_hidden()
        .rounded_md()
        .border_1()
        .border_color(rgb(theme.border))
        .children(rows.into_iter().enumerate().map(|(row_index, row)| {
            let cell_count = row.cells.len();
            div()
                .w_full()
                .min_w(px(0.0))
                .flex()
                .when(row.is_header, |this| this.bg(rgb(theme.current_line)))
                .when(row_index > 0, |this| {
                    this.border_t_1().border_color(rgb(theme.border))
                })
                .children(
                    row.cells
                        .into_iter()
                        .enumerate()
                        .map(|(column_index, cell)| {
                            let alignment = alignments
                                .get(column_index)
                                .cloned()
                                .unwrap_or(Alignment::None);
                            let cell_element = div()
                                .flex_1()
                                .min_w(px(0.0))
                                .px_2()
                                .py_2()
                                .text_size(px((style.font_size - 1.0).max(10.0)))
                                .line_height(px((style.line_height - 1.0).max(16.0)))
                                .font_weight(if row.is_header {
                                    FontWeight::SEMIBOLD
                                } else {
                                    FontWeight::NORMAL
                                })
                                .text_color(rgb(style.color))
                                .when(column_index + 1 < cell_count, |this| {
                                    this.border_r_1().border_color(rgb(theme.border))
                                })
                                .child(render_inline_text(cell, theme));
                            match alignment {
                                Alignment::Center => cell_element.text_center(),
                                Alignment::Right => cell_element.text_right(),
                                Alignment::None | Alignment::Left => cell_element.text_left(),
                            }
                        }),
                )
        }))
        .into_any_element()
}

/// 表格渲染前使用的借用行，避免复制流式消息中的单元格文本。
struct TableRowView<'a> {
    is_header: bool,
    cells: Vec<&'a MarkdownNode>,
}

/// 从 pulldown-cmark 的 TableHead/TableRow 结构中提取统一行模型。
fn collect_table_rows(node: &MarkdownNode) -> Vec<TableRowView<'_>> {
    let mut rows = Vec::new();
    for child in &node.children {
        match child.kind {
            MarkdownNodeKind::TableHead => {
                let cells = table_cells(child);
                if !cells.is_empty() {
                    rows.push(TableRowView {
                        is_header: true,
                        cells,
                    });
                }
            }
            MarkdownNodeKind::TableRow => {
                let cells = table_cells(child);
                if !cells.is_empty() {
                    rows.push(TableRowView {
                        is_header: false,
                        cells,
                    });
                }
            }
            _ => {}
        }
    }
    rows
}

/// 兼容表头直接包含单元格和额外包裹 TableRow 两种结构。
fn table_cells(node: &MarkdownNode) -> Vec<&MarkdownNode> {
    let direct = node
        .children
        .iter()
        .filter(|child| child.kind == MarkdownNodeKind::TableCell)
        .collect::<Vec<_>>();
    if !direct.is_empty() {
        return direct;
    }
    node.children
        .iter()
        .filter(|child| child.kind == MarkdownNodeKind::TableRow)
        .flat_map(|row| {
            row.children
                .iter()
                .filter(|cell| cell.kind == MarkdownNodeKind::TableCell)
        })
        .collect()
}

/// 把行内节点展平为 StyledText，嵌套粗体、斜体、删除线、代码和链接可同时生效。
fn render_inline_text(node: &MarkdownNode, theme: &AppTheme) -> StyledText {
    let (text, spans) = inline_text_and_spans(node);
    let highlights = spans.into_iter().filter_map(|span| {
        let highlight = highlight_for_inline_style(span.style, theme);
        (highlight != HighlightStyle::default()).then_some((span.range, highlight))
    });
    StyledText::new(text).with_highlights(highlights)
}

/// 生成无 Markdown 标记的行内正文和互不重叠的样式区间，便于单元测试与 GPUI 渲染复用。
fn inline_text_and_spans(node: &MarkdownNode) -> (String, Vec<InlineSpan>) {
    let mut text = String::new();
    let mut spans = Vec::new();
    append_inline(node, InlineStyle::default(), &mut text, &mut spans);
    (text, spans)
}

/// 深度遍历行内节点，并将容器样式合并到叶子文本。
fn append_inline(
    node: &MarkdownNode,
    inherited_style: InlineStyle,
    text: &mut String,
    spans: &mut Vec<InlineSpan>,
) {
    let mut style = inherited_style;
    match &node.kind {
        MarkdownNodeKind::Strong => style.is_bold = true,
        MarkdownNodeKind::Emphasis | MarkdownNodeKind::GenericInline => style.is_italic = true,
        MarkdownNodeKind::Strikethrough => style.is_strikethrough = true,
        MarkdownNodeKind::Link => style.is_link = true,
        MarkdownNodeKind::Code(value) => {
            style.is_code = true;
            append_inline_text(value, style, text, spans);
            return;
        }
        MarkdownNodeKind::Text(value) => {
            append_inline_text(value, style, text, spans);
            return;
        }
        MarkdownNodeKind::SoftBreak => {
            append_inline_text(" ", style, text, spans);
            return;
        }
        MarkdownNodeKind::HardBreak => {
            append_inline_text("\n", style, text, spans);
            return;
        }
        MarkdownNodeKind::TaskMarker(is_checked) => {
            append_inline_text(if *is_checked { "☑ " } else { "☐ " }, style, text, spans);
            return;
        }
        MarkdownNodeKind::Image => {
            append_inline_text(
                "图片：",
                InlineStyle {
                    is_italic: true,
                    ..style
                },
                text,
                spans,
            );
        }
        MarkdownNodeKind::Rule => {
            append_inline_text("────────", style, text, spans);
            return;
        }
        _ => {}
    }

    for child in &node.children {
        append_inline(child, style, text, spans);
    }
}

/// 追加一个文本叶子并合并相邻同样式范围，减少长模型输出的 TextRun 数量。
fn append_inline_text(
    value: &str,
    style: InlineStyle,
    text: &mut String,
    spans: &mut Vec<InlineSpan>,
) {
    if value.is_empty() {
        return;
    }
    let start = text.len();
    text.push_str(value);
    let end = text.len();
    if let Some(last) = spans.last_mut()
        && last.style == style
        && last.range.end == start
    {
        last.range.end = end;
    } else {
        spans.push(InlineSpan {
            range: start..end,
            style,
        });
    }
}

/// 将内部行内样式转换为 GPUI 高亮，链接只展示语义颜色，不自动打开任何 URL。
fn highlight_for_inline_style(style: InlineStyle, theme: &AppTheme) -> HighlightStyle {
    HighlightStyle {
        color: if style.is_link {
            Some(rgb(theme.info).into())
        } else if style.is_code {
            Some(rgb(theme.syntax.string).into())
        } else {
            None
        },
        font_weight: style.is_bold.then_some(FontWeight::BOLD),
        font_style: style.is_italic.then_some(FontStyle::Italic),
        background_color: style.is_code.then_some(rgb(theme.current_line).into()),
        underline: None,
        strikethrough: style.is_strikethrough.then_some(StrikethroughStyle {
            thickness: px(1.0),
            color: Some(rgb(theme.foreground_muted).into()),
        }),
        fade_out: None,
    }
}

/// 提取代码块等节点中的原始可见文本，换行事件按真实换行保留。
fn collect_plain_text(node: &MarkdownNode) -> String {
    let mut value = String::new();
    collect_plain_text_into(node, &mut value);
    value
}

/// `collect_plain_text` 的递归实现。
fn collect_plain_text_into(node: &MarkdownNode, value: &mut String) {
    match &node.kind {
        MarkdownNodeKind::Text(text) | MarkdownNodeKind::Code(text) => value.push_str(text),
        MarkdownNodeKind::SoftBreak | MarkdownNodeKind::HardBreak => value.push('\n'),
        MarkdownNodeKind::TaskMarker(is_checked) => {
            value.push_str(if *is_checked { "[x] " } else { "[ ] " });
        }
        _ => {
            for child in &node.children {
                collect_plain_text_into(child, value);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        InlineStyle, MarkdownNode, MarkdownNodeKind, collect_table_rows, inline_text_and_spans,
        parse_markdown,
    };

    /// 在文档根下查找第一个指定类型节点。
    fn find_node(
        node: &MarkdownNode,
        predicate: impl Fn(&MarkdownNodeKind) -> bool + Copy,
    ) -> Option<&MarkdownNode> {
        if predicate(&node.kind) {
            return Some(node);
        }
        node.children
            .iter()
            .find_map(|child| find_node(child, predicate))
    }

    #[test]
    fn markdown_table_is_parsed_into_header_and_body_rows() {
        let document = parse_markdown(
            "| # | 假设 | 机制 |\n|---|---|---|\n| H1 | **连接泄漏** | 堆内存耗尽 |",
        );
        let table = find_node(&document, |kind| matches!(kind, MarkdownNodeKind::Table(_)))
            .expect("GFM 表格必须被识别");
        let rows = collect_table_rows(table);

        assert_eq!(rows.len(), 2);
        assert!(rows[0].is_header);
        assert_eq!(rows[0].cells.len(), 3);
        assert!(!rows[1].is_header);
        let (text, spans) = inline_text_and_spans(rows[1].cells[1]);
        assert_eq!(text, "连接泄漏");
        assert!(spans.iter().any(|span| span.style.is_bold));
    }

    #[test]
    fn nested_inline_styles_use_plain_text_and_combined_ranges() {
        let document = parse_markdown("这是 ***重要*** 的 `memory.log`，~~旧结论~~。");
        let paragraph = find_node(&document, |kind| *kind == MarkdownNodeKind::Paragraph)
            .expect("正文必须包含段落");
        let (text, spans) = inline_text_and_spans(paragraph);

        assert_eq!(text, "这是 重要 的 memory.log，旧结论。");
        assert!(spans.iter().any(|span| {
            span.style
                == InlineStyle {
                    is_bold: true,
                    is_italic: true,
                    ..InlineStyle::default()
                }
        }));
        assert!(spans.iter().any(|span| span.style.is_code));
        assert!(spans.iter().any(|span| span.style.is_strikethrough));
    }

    #[test]
    fn streaming_and_html_input_remain_visible_without_execution() {
        let document = parse_markdown("**仍在流式输出\n<script>alert('x')</script>");
        let (text, _) = inline_text_and_spans(&document);

        assert!(text.contains("仍在流式输出"));
        assert!(text.contains("<script>"));
        assert!(text.contains("alert('x')"));
    }
}
