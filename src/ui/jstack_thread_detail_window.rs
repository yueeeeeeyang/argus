//! 文件职责：渲染 Jstack 线程详情独立窗口。
//! 创建日期：2026-06-16
//! 修改日期：2026-06-16
//! 作者：Argus 开发团队
//! 主要功能：在无系统标题栏窗口中展示线程完整堆栈，并支持在不同快照间切换同名线程。

use std::ops::Range;

use crate::fonts::{ARGUS_LOG_FONT_FAMILY, ARGUS_UI_FONT_FAMILY};
use crate::highlight::{HighlightLanguage, HighlightTokenKind, SyntaxHighlighter};
use crate::jstack_analysis::{JstackThreadDetail, JstackThreadStackOccurrence};
use crate::theme::AppTheme;
use crate::ui::components::icon::{ArgusIcon, render_icon};
use crate::ui::components::icon_button::{IconButtonSize, render_icon_button};
use gpui::{
    AnyElement, Context, FontWeight, HighlightStyle, IntoElement, Render, SharedString, StyledText,
    Window, div, prelude::*, px, rgb,
};

/// 详情窗口顶部标题栏高度。
const DETAIL_TITLE_BAR_HEIGHT: f32 = 56.0;
/// 详情窗口线程摘要区域高度。
const DETAIL_SUMMARY_HEIGHT: f32 = 92.0;
/// 详情窗口内容滚动条宽度。
const DETAIL_SCROLLBAR_WIDTH: f32 = 8.0;
/// 箭头切换按钮尺寸。
const DETAIL_NAV_BUTTON_SIZE: f32 = 28.0;
/// 行号栏宽度。
const DETAIL_LINE_NUMBER_WIDTH: f32 = 48.0;
/// 堆栈正文最小宽度，保证长堆栈在横向滚动中保持阅读节奏。
const DETAIL_STACK_MIN_WIDTH: f32 = 1080.0;
/// 堆栈正文字号。
const DETAIL_STACK_FONT_SIZE: f32 = 12.0;
/// 堆栈行高，保持与日志阅读区类似的高密度展示。
const DETAIL_STACK_LINE_HEIGHT: f32 = 20.0;

/// Jstack 线程详情窗口根视图。
pub struct JstackThreadDetailWindow {
    /// 打开窗口时的主题快照。
    theme: AppTheme,
    /// 当前线程的跨快照堆栈记录。
    detail: JstackThreadDetail,
    /// 当前选中的记录索引。
    active_index: usize,
    /// 用户点击高亮的堆栈行；切换堆栈时会自动定位到第一条栈帧。
    highlighted_line: Option<usize>,
}

impl JstackThreadDetailWindow {
    /// 创建线程详情窗口。
    ///
    /// 参数说明：
    /// - `theme`：打开窗口时使用的主题令牌。
    /// - `detail`：线程跨快照的完整堆栈记录。
    /// - `active_snapshot_index`：用户点击的矩阵格子快照序号，用于定位初始展示记录。
    /// - `active_occurrence_index`：同快照内的线程出现序号，用于重复线程名时精确定位。
    ///
    /// 返回值：可由 GPUI 独立窗口渲染的详情视图。
    pub fn new(
        theme: AppTheme,
        detail: JstackThreadDetail,
        active_snapshot_index: usize,
        active_occurrence_index: usize,
    ) -> Self {
        let active_index = detail
            .occurrences
            .iter()
            .position(|occurrence| {
                occurrence.snapshot_index == active_snapshot_index
                    && occurrence.occurrence_index == active_occurrence_index
            })
            .or_else(|| {
                detail
                    .occurrences
                    .iter()
                    .position(|occurrence| occurrence.snapshot_index == active_snapshot_index)
            })
            .unwrap_or_default();
        let highlighted_line = detail
            .occurrences
            .get(active_index)
            .and_then(default_highlighted_line);

        Self {
            theme,
            detail,
            active_index,
            highlighted_line,
        }
    }

    /// 返回当前选中的堆栈记录；索引异常时回退到第一条记录。
    fn active_occurrence(&self) -> Option<&JstackThreadStackOccurrence> {
        self.detail
            .occurrences
            .get(self.active_index)
            .or_else(|| self.detail.occurrences.first())
    }

    /// 切换到指定堆栈记录，并把行高亮重置为第一条业务栈帧。
    fn select_occurrence(&mut self, next_index: usize) {
        if self.detail.occurrences.is_empty() {
            self.active_index = 0;
            self.highlighted_line = None;
            return;
        }

        self.active_index = next_index.min(self.detail.occurrences.len() - 1);
        self.highlighted_line = self
            .detail
            .occurrences
            .get(self.active_index)
            .and_then(default_highlighted_line);
    }
}

impl Render for JstackThreadDetailWindow {
    /// 渲染线程详情窗口主体。
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = self.theme.clone();
        let active_occurrence = self.active_occurrence().cloned();

        div()
            .id("jstack-thread-detail-window-root")
            .size_full()
            .flex()
            .flex_col()
            .bg(rgb(theme.content))
            .font_family(ARGUS_UI_FONT_FAMILY)
            .text_color(rgb(theme.foreground))
            .occlude()
            .child(render_detail_title_bar(&theme, &self.detail.thread_name))
            .child(render_detail_body(
                &theme,
                &self.detail,
                active_occurrence,
                self.active_index,
                self.highlighted_line,
                window,
                cx,
            ))
    }
}

/// 渲染窗口顶部标题和关闭按钮。
fn render_detail_title_bar(theme: &AppTheme, thread_name: &str) -> impl IntoElement + use<> {
    let close_theme = theme.clone();
    div()
        .h(px(DETAIL_TITLE_BAR_HEIGHT))
        .px_5()
        .flex()
        .items_center()
        .justify_between()
        .child(
            div()
                .flex()
                .items_center()
                .gap_2()
                .text_size(px(14.0))
                .line_height(px(18.0))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(rgb(theme.foreground))
                .truncate()
                .child(render_icon(ArgusIcon::Logs, theme.foreground_muted, 16.0))
                .child(format!("线程堆栈 - {thread_name}")),
        )
        .child(render_icon_button(
            "jstack-thread-detail-close",
            ArgusIcon::Close,
            "关闭",
            false,
            IconButtonSize::Small,
            &close_theme,
            move |_, window, _| {
                window.remove_window();
            },
        ))
}

/// 渲染详情主体区域，包括快照切换栏、元信息和完整堆栈。
fn render_detail_body(
    theme: &AppTheme,
    detail: &JstackThreadDetail,
    active_occurrence: Option<JstackThreadStackOccurrence>,
    active_index: usize,
    highlighted_line: Option<usize>,
    _window: &mut Window,
    cx: &mut Context<JstackThreadDetailWindow>,
) -> impl IntoElement + use<> {
    let Some(active_occurrence) = active_occurrence else {
        return div()
            .flex_1()
            .flex()
            .items_center()
            .justify_center()
            .text_size(px(13.0))
            .text_color(rgb(theme.foreground_muted))
            .child("未找到线程堆栈记录。")
            .into_any_element();
    };

    div()
        .flex_1()
        .min_h(px(0.0))
        .flex()
        .flex_col()
        .child(render_occurrence_summary(
            theme,
            detail,
            &active_occurrence,
            active_index,
            cx,
        ))
        .child(render_stack_content(
            theme,
            &active_occurrence,
            highlighted_line,
            cx,
        ))
        .into_any_element()
}

/// 渲染当前堆栈记录的摘要信息。
fn render_occurrence_summary(
    theme: &AppTheme,
    detail: &JstackThreadDetail,
    occurrence: &JstackThreadStackOccurrence,
    active_index: usize,
    cx: &mut Context<JstackThreadDetailWindow>,
) -> impl IntoElement + use<> {
    let total_count = detail.occurrences.len().max(1);
    let position = active_index.min(total_count - 1) + 1;
    let previous_index = active_index.saturating_sub(1);
    let next_index = (active_index + 1).min(total_count - 1);
    let can_previous = active_index > 0;
    let can_next = active_index + 1 < total_count;
    let occurrence_suffix = if occurrence.occurrence_index > 1 {
        format!(" · #{}", occurrence.occurrence_index)
    } else {
        String::new()
    };

    div()
        .id("jstack-thread-detail-summary")
        .h(px(DETAIL_SUMMARY_HEIGHT))
        .px_5()
        .py_3()
        .flex()
        .items_center()
        .justify_between()
        .gap_3()
        .border_b_1()
        .border_color(rgb(theme.border))
        .child(
            div()
                .min_w(px(0.0))
                .flex_1()
                .flex()
                .flex_col()
                .gap_1()
                .child(
                    div()
                        .text_size(px(14.0))
                        .line_height(px(20.0))
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(rgb(theme.foreground))
                        .truncate()
                        .child(detail.thread_name.clone()),
                )
                .child(
                    div()
                        .text_size(px(12.0))
                        .line_height(px(18.0))
                        .text_color(rgb(theme.foreground_muted))
                        .truncate()
                        .child(format!(
                            "{}{} · 第 {position} / {total_count} 个堆栈 · 状态 {}",
                            occurrence.snapshot_label,
                            occurrence_suffix,
                            occurrence.state.label()
                        )),
                )
                .child(
                    div()
                        .text_size(px(12.0))
                        .line_height(px(18.0))
                        .text_color(rgb(theme.foreground_muted))
                        .truncate()
                        .child(occurrence.snapshot_path.clone()),
                ),
        )
        .child(
            div()
                .flex()
                .items_center()
                .gap_2()
                .child(render_stack_nav_button(
                    "jstack-thread-detail-prev",
                    ArgusIcon::ArrowLeft,
                    "上一个堆栈",
                    can_previous,
                    previous_index,
                    theme,
                    cx,
                ))
                .child(render_stack_nav_button(
                    "jstack-thread-detail-next",
                    ArgusIcon::ArrowRight,
                    "下一个堆栈",
                    can_next,
                    next_index,
                    theme,
                    cx,
                )),
        )
}

/// 渲染左右箭头切换按钮；不可用时保留尺寸但降低透明度。
fn render_stack_nav_button(
    id: &'static str,
    icon: ArgusIcon,
    tooltip: &'static str,
    is_enabled: bool,
    target_index: usize,
    theme: &AppTheme,
    cx: &mut Context<JstackThreadDetailWindow>,
) -> impl IntoElement + use<> {
    let tooltip_theme = theme.clone();
    div()
        .id(id)
        .w(px(DETAIL_NAV_BUTTON_SIZE))
        .h(px(DETAIL_NAV_BUTTON_SIZE))
        .flex()
        .items_center()
        .justify_center()
        .rounded_sm()
        .border_1()
        .border_color(rgb(theme.border))
        .bg(rgb(theme.content))
        .opacity(if is_enabled { 1.0 } else { 0.35 })
        .when(is_enabled, |button| {
            button
                .cursor_pointer()
                .hover(|button| button.bg(rgb(tooltip_theme.current_line)))
                .on_click(cx.listener(move |view, _, _, cx| {
                    view.select_occurrence(target_index);
                    cx.notify();
                }))
        })
        .child(render_icon(icon, theme.foreground, 16.0))
        .tooltip(move |_, cx| {
            cx.new(|_| DetailTooltip {
                label: tooltip.to_string(),
                theme: tooltip_theme.clone(),
            })
            .into()
        })
}

/// 渲染完整堆栈内容，保留原始行顺序和缩进。
fn render_stack_content(
    theme: &AppTheme,
    occurrence: &JstackThreadStackOccurrence,
    highlighted_line: Option<usize>,
    cx: &mut Context<JstackThreadDetailWindow>,
) -> impl IntoElement + use<> {
    let lines = occurrence.stack_lines.iter().cloned().collect::<Vec<_>>();

    div()
        .id("jstack-thread-detail-stack")
        .flex_1()
        .min_h(px(0.0))
        .overflow_scroll()
        .scrollbar_width(px(DETAIL_SCROLLBAR_WIDTH))
        .bg(rgb(theme.background))
        .font_family(ARGUS_LOG_FONT_FAMILY)
        .text_size(px(DETAIL_STACK_FONT_SIZE))
        .text_color(rgb(theme.foreground))
        .child(
            div()
                .min_w(px(DETAIL_STACK_MIN_WIDTH))
                .flex()
                .flex_col()
                .children(lines.into_iter().enumerate().map(|(index, line)| {
                    render_stack_line(theme, index, line, highlighted_line == Some(index), cx)
                        .into_any_element()
                })),
        )
}

/// 渲染单行堆栈，包含行号、点击高亮和语法高亮文本。
fn render_stack_line(
    theme: &AppTheme,
    line_index: usize,
    line: String,
    is_highlighted: bool,
    cx: &mut Context<JstackThreadDetailWindow>,
) -> impl IntoElement + use<> {
    let row_background = is_highlighted.then_some(theme.current_line);
    let text_element = render_highlighted_stack_text(line, theme);

    div()
        .id(SharedString::from(format!(
            "jstack-thread-detail-line-{line_index}"
        )))
        .min_w(px(DETAIL_STACK_MIN_WIDTH))
        .h(px(DETAIL_STACK_LINE_HEIGHT))
        .flex()
        .items_center()
        .line_height(px(DETAIL_STACK_LINE_HEIGHT))
        .when_some(row_background, |row, color| row.bg(rgb(color)))
        .hover(move |row| row.bg(rgb(theme.current_line)))
        .cursor_pointer()
        .child(
            div()
                .w(px(DETAIL_LINE_NUMBER_WIDTH))
                .h_full()
                .pr_2()
                .flex()
                .items_center()
                .justify_end()
                .border_r_1()
                .border_color(rgb(theme.border))
                .text_color(rgb(theme.foreground_muted))
                .child((line_index + 1).to_string()),
        )
        .child(
            div()
                .flex_1()
                .min_w(px(0.0))
                .pl_3()
                .pr_3()
                .whitespace_nowrap()
                .child(text_element),
        )
        .on_click(cx.listener(move |view, _, _, cx| {
            view.highlighted_line = Some(line_index);
            cx.notify();
        }))
}

/// 用 Java 线程栈规则渲染单行语法高亮。
fn render_highlighted_stack_text(line: String, theme: &AppTheme) -> AnyElement {
    let highlights = SyntaxHighlighter::highlight(&line, HighlightLanguage::JavaThreadDump)
        .into_iter()
        .filter_map(|span| highlight_style_for_span(span.range, span.kind, theme))
        .collect::<Vec<_>>();

    if highlights.is_empty() {
        line.into_any_element()
    } else {
        StyledText::new(line)
            .with_highlights(highlights)
            .into_any_element()
    }
}

/// 把高亮 token 转换成当前主题下的 GPUI 文本样式。
fn highlight_style_for_span(
    range: Range<usize>,
    kind: HighlightTokenKind,
    theme: &AppTheme,
) -> Option<(Range<usize>, HighlightStyle)> {
    (range.start < range.end).then(|| {
        (
            range,
            HighlightStyle {
                color: Some(rgb(color_for_detail_highlight_token(kind, theme)).into()),
                ..Default::default()
            },
        )
    })
}

/// 返回详情窗口专用高亮色；线程状态使用成功色，更接近线程分析语义。
fn color_for_detail_highlight_token(kind: HighlightTokenKind, theme: &AppTheme) -> u32 {
    match kind {
        HighlightTokenKind::Trace => theme.foreground_muted,
        HighlightTokenKind::Debug => theme.debug,
        HighlightTokenKind::Info => theme.info,
        HighlightTokenKind::Warning => theme.warning,
        HighlightTokenKind::Error | HighlightTokenKind::Fatal => theme.error,
        HighlightTokenKind::Timestamp => theme.syntax.timestamp,
        HighlightTokenKind::Comment => theme.syntax.comment,
        HighlightTokenKind::Key => theme.syntax.key,
        HighlightTokenKind::Value => theme.syntax.string,
        HighlightTokenKind::String => theme.syntax.string,
        HighlightTokenKind::Number => theme.syntax.number,
        HighlightTokenKind::Boolean => theme.syntax.boolean,
        HighlightTokenKind::Punctuation => theme.syntax.punctuation,
        HighlightTokenKind::Tag => theme.syntax.tag,
        HighlightTokenKind::Attribute => theme.syntax.attribute,
        HighlightTokenKind::ThreadName => theme.info,
        HighlightTokenKind::ThreadState => theme.success,
        HighlightTokenKind::StackClass => theme.syntax.class,
        HighlightTokenKind::StackMethod => theme.info,
        HighlightTokenKind::StackLocation => theme.syntax.string,
        HighlightTokenKind::Lock => theme.syntax.lock,
        HighlightTokenKind::Exception => theme.syntax.exception,
    }
}

/// 默认高亮第一条 `at ...` 栈帧，便于打开窗口后快速定位代码路径。
fn default_highlighted_line(occurrence: &JstackThreadStackOccurrence) -> Option<usize> {
    occurrence
        .stack_lines
        .iter()
        .position(|line| line.trim_start().starts_with("at "))
}

/// 箭头按钮 tooltip。
struct DetailTooltip {
    /// tooltip 文案。
    label: String,
    /// 当前主题令牌。
    theme: AppTheme,
}

impl Render for DetailTooltip {
    /// 渲染紧凑 tooltip。
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .px_2()
            .py_1()
            .rounded_sm()
            .bg(rgb(self.theme.current_line))
            .border_1()
            .border_color(rgb(self.theme.border))
            .text_size(px(12.0))
            .text_color(rgb(self.theme.foreground))
            .child(self.label.clone())
    }
}
