//! 文件职责：渲染 Jstack 线程详情独立窗口。
//! 创建日期：2026-06-16
//! 修改日期：2026-06-25
//! 作者：Argus 开发团队
//! 主要功能：在无系统标题栏窗口中展示线程完整堆栈，并支持在不同快照间切换同名线程。

use std::borrow::Borrow;
use std::ops::Range;

use crate::fonts::{ARGUS_LOG_FONT_FAMILY, ARGUS_UI_FONT_FAMILY};
use crate::highlight::{HighlightLanguage, HighlightTokenKind, SyntaxHighlighter};
use crate::jstack_analysis::{JstackThreadDetail, JstackThreadStackOccurrence};
use crate::text_selection::{
    TextSelectionGranularity, byte_index_for_character, char_column_for_byte_index,
    character_count, slice_character_range, word_range_at,
};
use crate::theme::AppTheme;
use crate::ui::components::icon::{ArgusIcon, render_icon};
use crate::ui::components::icon_button::{IconButtonSize, render_icon_button};
use gpui::{
    AnyElement, Bounds, ClipboardItem, Context, FocusHandle, FontWeight, HighlightStyle,
    IntoElement, KeyDownEvent, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, Pixels,
    Render, ScrollHandle, SharedString, StyledText, TextRun, Window, canvas, div, point,
    prelude::*, px, rgb,
};

/// 详情窗口顶部标题栏高度。
const DETAIL_TITLE_BAR_HEIGHT: f32 = 56.0;
/// 详情窗口线程摘要区域高度。
const DETAIL_SUMMARY_HEIGHT: f32 = 92.0;
/// 详情窗口内容滚动条宽度。
const DETAIL_SCROLLBAR_WIDTH: f32 = 8.0;
/// 详情窗口滚动条轨道内边距。
const DETAIL_SCROLLBAR_PADDING: f32 = 4.0;
/// 详情窗口滚动条最小滑块长度。
const DETAIL_SCROLLBAR_MIN_THUMB: f32 = 32.0;
/// 详情窗口滚动条滑块厚度。
const DETAIL_SCROLLBAR_THUMB_SIZE: f32 = 5.0;
/// 箭头切换按钮尺寸。
const DETAIL_NAV_BUTTON_SIZE: f32 = 28.0;
/// 行号栏宽度。
const DETAIL_LINE_NUMBER_WIDTH: f32 = 48.0;
/// 堆栈正文最小宽度，保证长堆栈在横向滚动中保持阅读节奏。
const DETAIL_STACK_MIN_WIDTH: f32 = 1080.0;
/// 堆栈正文左右内边距总和，用于计算真实横向滚动宽度。
const DETAIL_STACK_TEXT_HORIZONTAL_PADDING: f32 = 24.0;
/// 堆栈正文字号。
const DETAIL_STACK_FONT_SIZE: f32 = 12.0;
/// 堆栈行高，保持与日志阅读区类似的高密度展示。
const DETAIL_STACK_LINE_HEIGHT: f32 = 20.0;

/// 详情窗口滚动条方向。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum DetailScrollbarAxis {
    /// 横向滚动条。
    Horizontal,
    /// 纵向滚动条。
    Vertical,
}

/// 详情窗口滚动条拖拽态，记录点击点在滑块内的相对偏移。
#[derive(Clone, Copy, Debug)]
struct DetailScrollbarDrag {
    /// 当前拖拽的滚动条方向。
    axis: DetailScrollbarAxis,
    /// 鼠标按下位置到滑块起点的距离。
    cursor_offset: Pixels,
}

/// 详情窗口滚动条布局度量。
#[derive(Clone, Copy, Debug)]
struct DetailScrollbarMetrics {
    /// 滑块起点。
    thumb_start: Pixels,
    /// 滑块长度。
    thumb_length: Pixels,
    /// 轨道起点。
    track_start: Pixels,
    /// 轨道长度。
    track_length: Pixels,
    /// 最大滚动距离。
    max_scroll: Pixels,
}

/// 堆栈文本中的字符位置。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct StackTextPosition {
    /// 0 基堆栈行号。
    line_index: usize,
    /// 行内字符列。
    column: usize,
}

/// 堆栈文本选区。
#[derive(Clone, Debug, Eq, PartialEq)]
struct StackTextSelection {
    /// 鼠标按下时的锚点。
    anchor: StackTextPosition,
    /// 当前拖拽到的焦点。
    focus: StackTextPosition,
}

impl StackTextSelection {
    /// 返回按文档顺序排列的选区端点。
    fn normalized(&self) -> (StackTextPosition, StackTextPosition) {
        if stack_text_position_le(self.anchor, self.focus) {
            (self.anchor, self.focus)
        } else {
            (self.focus, self.anchor)
        }
    }

    /// 返回选区是否没有覆盖任何字符。
    fn is_empty(&self) -> bool {
        self.anchor == self.focus
    }
}

/// 堆栈文本拖拽选择状态。
#[derive(Clone, Debug, Eq, PartialEq)]
struct StackTextSelectionDrag {
    /// 开始拖拽时按点击次数得到的锚点范围。
    anchor_range: StackTextSelection,
    /// 本次拖拽的选择粒度。
    granularity: TextSelectionGranularity,
}

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
    /// 堆栈内容滚动句柄，用于显示横纵滚动条并在切换快照时复位。
    stack_scroll: ScrollHandle,
    /// 当前正在拖拽的滚动条；为空表示没有拖拽。
    scrollbar_drag: Option<DetailScrollbarDrag>,
    /// 是否已选中线程名，选中后支持复制快捷键并显示高亮反馈。
    is_thread_name_selected: bool,
    /// 当前堆栈正文选区。
    stack_selection: Option<StackTextSelection>,
    /// 当前堆栈正文拖拽选择状态。
    stack_selection_drag: Option<StackTextSelectionDrag>,
    /// 根视图焦点句柄；堆栈文本选择后仍用它稳定接收 Cmd/Ctrl+C。
    focus_handle: FocusHandle,
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
        cx: &mut Context<Self>,
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
            stack_scroll: ScrollHandle::new(),
            scrollbar_drag: None,
            is_thread_name_selected: false,
            stack_selection: None,
            stack_selection_drag: None,
            focus_handle: cx.focus_handle(),
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
        self.stack_scroll.set_offset(point(px(0.0), px(0.0)));
        self.scrollbar_drag = None;
        self.stack_selection = None;
        self.stack_selection_drag = None;
    }

    /// 选中并复制当前线程详情的线程名。
    fn select_and_copy_thread_name(&mut self, cx: &mut Context<Self>) {
        self.is_thread_name_selected = true;
        self.stack_selection = None;
        self.stack_selection_drag = None;
        let thread_name = self.detail.display_label();
        let app_context: &gpui::App = (&*cx).borrow();
        app_context.write_to_clipboard(ClipboardItem::new_string(thread_name));
    }

    /// 复制当前详情窗口选区；堆栈正文选区优先，没有正文选区时复制线程身份。
    fn copy_detail_selection(&mut self, cx: &mut Context<Self>) {
        if let Some(stack_text) = self.selected_stack_text() {
            let app_context: &gpui::App = (&*cx).borrow();
            app_context.write_to_clipboard(ClipboardItem::new_string(stack_text));
            return;
        }

        if !self.is_thread_name_selected {
            self.is_thread_name_selected = true;
        }
        let thread_name = self.detail.display_label();
        let app_context: &gpui::App = (&*cx).borrow();
        app_context.write_to_clipboard(ClipboardItem::new_string(thread_name));
    }

    /// 根据鼠标位置开始堆栈正文选择。
    fn begin_stack_text_selection(
        &mut self,
        line_index: usize,
        line: &str,
        pointer_x: Pixels,
        click_count: usize,
        window: &mut Window,
    ) {
        self.focus_handle.focus(window);
        let position = self.stack_text_position_from_pointer(line_index, line, pointer_x, window);
        let granularity = stack_text_granularity_for_click_count(click_count);
        let anchor_range =
            stack_text_range_for_granularity(line_index, line, position.column, granularity);
        self.stack_selection = Some(anchor_range.clone());
        self.stack_selection_drag = Some(StackTextSelectionDrag {
            anchor_range,
            granularity,
        });
        self.is_thread_name_selected = false;
    }

    /// 拖拽过程中更新堆栈正文选择。
    fn update_stack_text_selection(
        &mut self,
        line_index: usize,
        line: &str,
        pointer_x: Pixels,
        window: &mut Window,
    ) {
        let Some(drag) = self.stack_selection_drag.clone() else {
            return;
        };
        let position = self.stack_text_position_from_pointer(line_index, line, pointer_x, window);
        let focus_range =
            stack_text_range_for_granularity(line_index, line, position.column, drag.granularity);
        self.stack_selection = Some(merge_stack_text_ranges(&drag.anchor_range, &focus_range));
        self.is_thread_name_selected = false;
    }

    /// 结束堆栈正文选择；没有选中字符时清理选区。
    fn finish_stack_text_selection(&mut self) {
        self.stack_selection_drag = None;
        if self
            .stack_selection
            .as_ref()
            .is_some_and(StackTextSelection::is_empty)
        {
            self.stack_selection = None;
        }
    }

    /// 根据鼠标横坐标计算堆栈正文行内字符列。
    fn stack_text_position_from_pointer(
        &self,
        line_index: usize,
        line: &str,
        pointer_x: Pixels,
        window: &mut Window,
    ) -> StackTextPosition {
        let bounds = self.stack_scroll.bounds();
        let horizontal_offset = self.stack_scroll.offset().x;
        let text_relative_x = pointer_x
            - bounds.left()
            - horizontal_offset
            - px(DETAIL_LINE_NUMBER_WIDTH + DETAIL_STACK_TEXT_HORIZONTAL_PADDING / 2.0);
        if line.is_empty() || text_relative_x <= px(0.0) {
            return StackTextPosition {
                line_index,
                column: 0,
            };
        }

        let mut text_style = window.text_style();
        text_style.font_family = ARGUS_LOG_FONT_FAMILY.into();
        text_style.font_size = px(DETAIL_STACK_FONT_SIZE).into();
        let run = TextRun {
            len: line.len(),
            font: text_style.font(),
            color: text_style.color,
            background_color: None,
            underline: None,
            strikethrough: None,
        };
        let shaped_line = window.text_system().shape_line(
            SharedString::from(line.to_string()),
            text_style.font_size.to_pixels(window.rem_size()),
            &[run],
            None,
        );
        let byte_index = shaped_line.closest_index_for_x(text_relative_x);
        StackTextPosition {
            line_index,
            column: char_column_for_byte_index(line, byte_index),
        }
    }

    /// 返回当前堆栈正文选中的文本。
    fn selected_stack_text(&self) -> Option<String> {
        let selection = self.stack_selection.as_ref()?;
        if selection.is_empty() {
            return None;
        }
        let occurrence = self.active_occurrence()?;
        selected_stack_text_from_lines(occurrence.stack_lines.as_ref(), selection)
    }
}

impl Render for JstackThreadDetailWindow {
    /// 渲染线程详情窗口主体。
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = self.theme.clone();
        let active_occurrence = self.active_occurrence().cloned();
        let focus_handle = self.focus_handle.clone();
        let click_focus_handle = self.focus_handle.clone();
        if !focus_handle.is_focused(window) {
            focus_handle.focus(window);
        }

        div()
            .id("jstack-thread-detail-window-root")
            .size_full()
            .flex()
            .flex_col()
            .bg(rgb(theme.content))
            .font_family(ARGUS_UI_FONT_FAMILY)
            .text_color(rgb(theme.foreground))
            .occlude()
            .focusable()
            .track_focus(&focus_handle)
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |_view, _event: &MouseDownEvent, window, _cx| {
                    click_focus_handle.focus(window);
                }),
            )
            .on_key_down(cx.listener(|view, event: &KeyDownEvent, _, cx| {
                if event.keystroke.modifiers.platform
                    && event.keystroke.key.eq_ignore_ascii_case("c")
                {
                    cx.stop_propagation();
                    view.copy_detail_selection(cx);
                    cx.notify();
                }
            }))
            .child(render_detail_title_bar(
                &theme,
                &self.detail.display_label(),
                self.is_thread_name_selected,
                cx,
            ))
            .child(render_detail_body(
                &theme,
                &self.detail,
                active_occurrence,
                self.active_index,
                self.highlighted_line,
                self.is_thread_name_selected,
                self.stack_selection.as_ref(),
                &self.stack_scroll,
                window,
                cx,
            ))
    }
}

/// 渲染窗口顶部标题和关闭按钮。
fn render_detail_title_bar(
    theme: &AppTheme,
    thread_name: &str,
    is_thread_name_selected: bool,
    cx: &mut Context<JstackThreadDetailWindow>,
) -> impl IntoElement + use<> {
    let close_theme = theme.clone();
    let display_title = format!("线程堆栈 - {thread_name}");
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
                .child(render_copyable_detail_thread_name(
                    "jstack-thread-detail-title-name",
                    display_title,
                    is_thread_name_selected,
                    14.0,
                    true,
                    theme,
                    cx,
                )),
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

/// 渲染可选中复制的线程名文本。
fn render_copyable_detail_thread_name(
    id: &'static str,
    label: String,
    is_selected: bool,
    font_size: f32,
    is_bold: bool,
    theme: &AppTheme,
    cx: &mut Context<JstackThreadDetailWindow>,
) -> impl IntoElement + use<> {
    let tooltip_theme = theme.clone();
    div()
        .id(id)
        .min_w(px(0.0))
        .px_1()
        .rounded_sm()
        .cursor_pointer()
        .bg(rgb(if is_selected {
            theme.selection
        } else {
            theme.content
        }))
        .text_size(px(font_size))
        .line_height(px(font_size + 6.0))
        .text_color(rgb(theme.foreground))
        .truncate()
        .when(is_bold, |this| this.font_weight(FontWeight::SEMIBOLD))
        .child(label.clone())
        .tooltip(move |_, cx| {
            cx.new(|_| DetailTooltip {
                label: "点击复制线程名".to_string(),
                theme: tooltip_theme.clone(),
            })
            .into()
        })
        .on_click(cx.listener(|view, _, _, cx| {
            view.select_and_copy_thread_name(cx);
            cx.notify();
        }))
}

/// 渲染详情主体区域，包括快照切换栏、元信息和完整堆栈。
fn render_detail_body(
    theme: &AppTheme,
    detail: &JstackThreadDetail,
    active_occurrence: Option<JstackThreadStackOccurrence>,
    active_index: usize,
    highlighted_line: Option<usize>,
    is_thread_name_selected: bool,
    stack_selection: Option<&StackTextSelection>,
    stack_scroll: &ScrollHandle,
    window: &mut Window,
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
            is_thread_name_selected,
            cx,
        ))
        .child(render_stack_content(
            theme,
            &active_occurrence,
            highlighted_line,
            stack_selection,
            stack_scroll,
            window,
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
    is_thread_name_selected: bool,
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
                .child(render_copyable_detail_thread_name(
                    "jstack-thread-detail-summary-name",
                    detail.display_label(),
                    is_thread_name_selected,
                    14.0,
                    true,
                    theme,
                    cx,
                ))
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
    stack_selection: Option<&StackTextSelection>,
    stack_scroll: &ScrollHandle,
    window: &mut Window,
    cx: &mut Context<JstackThreadDetailWindow>,
) -> impl IntoElement + use<> {
    let lines = occurrence.stack_lines.iter().cloned().collect::<Vec<_>>();
    let content_width = detail_stack_content_width(&lines, window);

    div()
        .id("jstack-thread-detail-stack")
        .relative()
        .flex_1()
        .min_h(px(0.0))
        .overflow_hidden()
        .bg(rgb(theme.background))
        .child(
            div()
                .id("jstack-thread-detail-stack-scroll")
                .w_full()
                .h_full()
                .overflow_scroll()
                .scrollbar_width(px(DETAIL_SCROLLBAR_WIDTH))
                .track_scroll(stack_scroll)
                .font_family(ARGUS_LOG_FONT_FAMILY)
                .text_size(px(DETAIL_STACK_FONT_SIZE))
                .text_color(rgb(theme.foreground))
                .child(div().min_w(content_width).flex().flex_col().children(
                    lines.into_iter().enumerate().map(|(index, line)| {
                        let selection_range =
                            stack_selection_byte_range_for_line(stack_selection, index, &line);
                        render_stack_line(
                            theme,
                            index,
                            line,
                            highlighted_line == Some(index),
                            selection_range,
                            content_width,
                            cx,
                        )
                        .into_any_element()
                    }),
                )),
        )
        .children(render_detail_scrollbars(stack_scroll, theme, cx))
}

/// 根据堆栈滚动状态绘制横向和纵向滚动条。
fn render_detail_scrollbars(
    scroll_handle: &ScrollHandle,
    theme: &AppTheme,
    cx: &mut Context<JstackThreadDetailWindow>,
) -> Vec<AnyElement> {
    let bounds = scroll_handle.bounds();
    let max_offset = scroll_handle.max_offset();
    let offset = scroll_handle.offset();
    let mut scrollbars = Vec::new();

    if let Some(metrics) = detail_scrollbar_metrics(
        bounds.size.height,
        bounds.size.height + max_offset.height,
        -offset.y,
    ) {
        scrollbars.push(render_detail_scrollbar_thumb(
            DetailScrollbarAxis::Vertical,
            metrics,
            bounds,
            scroll_handle.clone(),
            theme,
            cx,
        ));
    }
    if let Some(metrics) = detail_scrollbar_metrics(
        bounds.size.width,
        bounds.size.width + max_offset.width,
        -offset.x,
    ) {
        scrollbars.push(render_detail_scrollbar_thumb(
            DetailScrollbarAxis::Horizontal,
            metrics,
            bounds,
            scroll_handle.clone(),
            theme,
            cx,
        ));
    }

    scrollbars
}

/// 计算滚动条滑块位置和拖拽换算所需的度量。
fn detail_scrollbar_metrics(
    viewport_length: gpui::Pixels,
    content_length: gpui::Pixels,
    scroll_offset: gpui::Pixels,
) -> Option<DetailScrollbarMetrics> {
    if viewport_length == px(0.0) || content_length <= viewport_length {
        return None;
    }

    let track_padding = px(DETAIL_SCROLLBAR_PADDING);
    let track_length = (viewport_length - track_padding * 2.0).max(px(1.0));
    let thumb_length = ((viewport_length / content_length) * track_length)
        .clamp(px(DETAIL_SCROLLBAR_MIN_THUMB), track_length);
    let max_scroll = (content_length - viewport_length).max(px(1.0));
    let scroll_ratio = (scroll_offset / max_scroll).clamp(0.0, 1.0);
    let thumb_start = track_padding + (track_length - thumb_length) * scroll_ratio;

    Some(DetailScrollbarMetrics {
        thumb_start,
        thumb_length,
        track_start: track_padding,
        track_length,
        max_scroll,
    })
}

/// 渲染可拖拽的详情窗口滚动条滑块。
fn render_detail_scrollbar_thumb(
    axis: DetailScrollbarAxis,
    metrics: DetailScrollbarMetrics,
    viewport_bounds: Bounds<Pixels>,
    scroll_handle: ScrollHandle,
    theme: &AppTheme,
    cx: &mut Context<JstackThreadDetailWindow>,
) -> AnyElement {
    let entity = cx.entity();
    let is_horizontal = axis == DetailScrollbarAxis::Horizontal;
    let mut thumb = div()
        .id(SharedString::from(format!(
            "jstack-thread-detail-scrollbar-{axis:?}"
        )))
        .absolute()
        .rounded_lg()
        .bg(rgb(theme.foreground_muted))
        .opacity(0.5)
        .hover(|this| this.opacity(0.8))
        .cursor_pointer()
        .occlude();

    thumb = if is_horizontal {
        thumb
            .left(metrics.thumb_start)
            .bottom(px(DETAIL_SCROLLBAR_PADDING))
            .w(metrics.thumb_length)
            .h(px(DETAIL_SCROLLBAR_THUMB_SIZE))
    } else {
        thumb
            .top(metrics.thumb_start)
            .right(px(DETAIL_SCROLLBAR_PADDING))
            .w(px(DETAIL_SCROLLBAR_THUMB_SIZE))
            .h(metrics.thumb_length)
    };

    thumb
        .child(
            canvas(
                |_, _, _| (),
                move |thumb_bounds, _, window: &mut Window, _| {
                    window.on_mouse_event({
                        let entity = entity.clone();
                        move |event: &MouseDownEvent, phase, _, cx| {
                            if !phase.bubble()
                                || event.button != MouseButton::Left
                                || !thumb_bounds.contains(&event.position)
                            {
                                return;
                            }

                            cx.stop_propagation();
                            entity.update(cx, |view, _| {
                                let pointer = if is_horizontal {
                                    event.position.x
                                } else {
                                    event.position.y
                                };
                                let thumb_start = if is_horizontal {
                                    thumb_bounds.origin.x
                                } else {
                                    thumb_bounds.origin.y
                                };
                                view.scrollbar_drag = Some(DetailScrollbarDrag {
                                    axis,
                                    cursor_offset: pointer - thumb_start,
                                });
                            });
                            cx.notify(entity.entity_id());
                        }
                    });

                    window.on_mouse_event({
                        let entity = entity.clone();
                        move |event: &MouseUpEvent, phase, _, cx| {
                            if !phase.bubble() || event.button != MouseButton::Left {
                                return;
                            }

                            let handled = entity.update(cx, |view, _| {
                                let handled = view.scrollbar_drag.is_some();
                                view.scrollbar_drag = None;
                                handled
                            });
                            if handled {
                                cx.stop_propagation();
                                cx.notify(entity.entity_id());
                            }
                        }
                    });

                    window.on_mouse_event(move |event: &MouseMoveEvent, phase, _, cx| {
                        if !phase.bubble() || !event.dragging() {
                            return;
                        }

                        let handled = entity.update(cx, |view, _| {
                            let Some(drag) = view.scrollbar_drag else {
                                return false;
                            };
                            if drag.axis != axis {
                                return false;
                            }

                            let pointer = if is_horizontal {
                                event.position.x - viewport_bounds.left()
                            } else {
                                event.position.y - viewport_bounds.top()
                            };
                            let movable =
                                (metrics.track_length - metrics.thumb_length).max(px(1.0));
                            let thumb_start = (pointer - drag.cursor_offset)
                                .clamp(metrics.track_start, metrics.track_start + movable);
                            let ratio = (thumb_start - metrics.track_start) / movable;
                            let scroll = metrics.max_scroll * ratio;
                            let current = scroll_handle.offset();
                            if is_horizontal {
                                scroll_handle.set_offset(point(-scroll, current.y));
                            } else {
                                scroll_handle.set_offset(point(current.x, -scroll));
                            }
                            true
                        });

                        if handled {
                            cx.stop_propagation();
                            cx.notify(entity.entity_id());
                        }
                    });
                },
            )
            .size_full(),
        )
        .into_any_element()
}

/// 计算堆栈内容真实宽度，保证横向滚动条可以覆盖最长堆栈行。
fn detail_stack_content_width(lines: &[String], window: &mut Window) -> Pixels {
    let mut text_style = window.text_style();
    text_style.font_family = ARGUS_LOG_FONT_FAMILY.into();
    text_style.font_size = px(DETAIL_STACK_FONT_SIZE).into();
    let max_text_width = lines
        .iter()
        .map(|line| detail_stack_line_text_width(line, &text_style, window))
        .fold(px(0.0), |max_width, width| max_width.max(width));

    (px(DETAIL_LINE_NUMBER_WIDTH + DETAIL_STACK_TEXT_HORIZONTAL_PADDING) + max_text_width)
        .max(px(DETAIL_STACK_MIN_WIDTH))
}

/// 使用 GPUI 字形排版计算单行堆栈文本宽度。
fn detail_stack_line_text_width(
    line: &str,
    text_style: &gpui::TextStyle,
    window: &mut Window,
) -> Pixels {
    if line.is_empty() {
        return px(0.0);
    }

    let run = TextRun {
        len: line.len(),
        font: text_style.font(),
        color: text_style.color,
        background_color: None,
        underline: None,
        strikethrough: None,
    };
    let shaped_line = window.text_system().shape_line(
        SharedString::from(line.to_string()),
        text_style.font_size.to_pixels(window.rem_size()),
        &[run],
        None,
    );

    shaped_line.x_for_index(line.len())
}

/// 渲染单行堆栈，包含行号、点击高亮和语法高亮文本。
fn render_stack_line(
    theme: &AppTheme,
    line_index: usize,
    line: String,
    is_highlighted: bool,
    selection_range: Option<Range<usize>>,
    content_width: Pixels,
    cx: &mut Context<JstackThreadDetailWindow>,
) -> impl IntoElement + use<> {
    let row_background = is_highlighted.then_some(theme.current_line);
    let text_element = render_highlighted_stack_text(line.clone(), theme, selection_range);
    let line_for_mouse_down = line.clone();
    let line_for_mouse_move = line.clone();

    div()
        .id(SharedString::from(format!(
            "jstack-thread-detail-line-{line_index}"
        )))
        .min_w(content_width)
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
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(move |view, event: &MouseDownEvent, window, cx| {
                cx.stop_propagation();
                view.highlighted_line = Some(line_index);
                view.begin_stack_text_selection(
                    line_index,
                    &line_for_mouse_down,
                    event.position.x,
                    event.click_count,
                    window,
                );
                cx.notify();
            }),
        )
        .on_mouse_move(
            cx.listener(move |view, event: &MouseMoveEvent, window, cx| {
                if !event.dragging() || view.stack_selection_drag.is_none() {
                    return;
                }
                cx.stop_propagation();
                view.update_stack_text_selection(
                    line_index,
                    &line_for_mouse_move,
                    event.position.x,
                    window,
                );
                cx.notify();
            }),
        )
        .on_mouse_up(
            MouseButton::Left,
            cx.listener(move |view, _, _, cx| {
                if view.stack_selection_drag.is_some() {
                    cx.stop_propagation();
                    view.finish_stack_text_selection();
                    cx.notify();
                }
            }),
        )
}

/// 用 Java 线程栈规则渲染单行语法高亮。
fn render_highlighted_stack_text(
    line: String,
    theme: &AppTheme,
    selection_range: Option<Range<usize>>,
) -> AnyElement {
    let highlights = SyntaxHighlighter::highlight(&line, HighlightLanguage::JavaThreadDump)
        .into_iter()
        .filter_map(|span| highlight_style_for_span(span.range, span.kind, theme))
        .collect::<Vec<_>>();
    let highlights = merge_detail_stack_highlights(highlights, selection_range, theme);

    if highlights.is_empty() {
        line.into_any_element()
    } else {
        StyledText::new(line)
            .with_highlights(highlights)
            .into_any_element()
    }
}

/// 判断堆栈文本位置是否按文档顺序不晚于另一个位置。
fn stack_text_position_le(left: StackTextPosition, right: StackTextPosition) -> bool {
    left.line_index < right.line_index
        || (left.line_index == right.line_index && left.column <= right.column)
}

/// 根据鼠标点击次数选择堆栈正文的选择粒度。
fn stack_text_granularity_for_click_count(click_count: usize) -> TextSelectionGranularity {
    match click_count {
        0 | 1 => TextSelectionGranularity::Character,
        2 => TextSelectionGranularity::Word,
        _ => TextSelectionGranularity::Line,
    }
}

/// 按指定粒度把鼠标命中的堆栈位置扩展成可拖拽合并的选区范围。
fn stack_text_range_for_granularity(
    line_index: usize,
    line: &str,
    column: usize,
    granularity: TextSelectionGranularity,
) -> StackTextSelection {
    let character_count = character_count(line);
    let range = match granularity {
        TextSelectionGranularity::Character => {
            column.min(character_count)..column.min(character_count)
        }
        TextSelectionGranularity::Word => word_range_at(line, column)
            .unwrap_or_else(|| column.min(character_count)..column.min(character_count)),
        TextSelectionGranularity::Line => 0..character_count,
    };

    StackTextSelection {
        anchor: StackTextPosition {
            line_index,
            column: range.start,
        },
        focus: StackTextPosition {
            line_index,
            column: range.end,
        },
    }
}

/// 合并拖拽起点和当前命中范围，得到跨行或跨词的最终正文选区。
fn merge_stack_text_ranges(
    anchor_range: &StackTextSelection,
    focus_range: &StackTextSelection,
) -> StackTextSelection {
    let (anchor_start, anchor_end) = anchor_range.normalized();
    let (focus_start, focus_end) = focus_range.normalized();
    StackTextSelection {
        anchor: if stack_text_position_le(anchor_start, focus_start) {
            anchor_start
        } else {
            focus_start
        },
        focus: if stack_text_position_le(anchor_end, focus_end) {
            focus_end
        } else {
            anchor_end
        },
    }
}

/// 计算当前行被堆栈正文选区覆盖的 UTF-8 字节范围，用于叠加选择背景。
fn stack_selection_byte_range_for_line(
    selection: Option<&StackTextSelection>,
    line_index: usize,
    line: &str,
) -> Option<Range<usize>> {
    let selection = selection?;
    let (start, end) = selection.normalized();
    if line_index < start.line_index || line_index > end.line_index {
        return None;
    }

    let line_character_count = character_count(line);
    let start_column = if line_index == start.line_index {
        start.column.min(line_character_count)
    } else {
        0
    };
    let end_column = if line_index == end.line_index {
        end.column.min(line_character_count)
    } else {
        line_character_count
    };
    (start_column < end_column).then(|| {
        byte_index_for_character(line, start_column)..byte_index_for_character(line, end_column)
    })
}

/// 从堆栈行集合中提取当前正文选区文本，保留跨行换行符以便复制后仍可阅读。
fn selected_stack_text_from_lines(
    lines: &[String],
    selection: &StackTextSelection,
) -> Option<String> {
    if selection.is_empty() || lines.is_empty() {
        return None;
    }

    let (start, end) = selection.normalized();
    if start.line_index >= lines.len() {
        return None;
    }

    let end_line = end.line_index.min(lines.len().saturating_sub(1));
    let mut selected = String::new();
    for line_index in start.line_index..=end_line {
        if line_index > start.line_index {
            selected.push('\n');
        }
        let line = &lines[line_index];
        let line_character_count = character_count(line);
        let start_column = if line_index == start.line_index {
            start.column.min(line_character_count)
        } else {
            0
        };
        let end_column = if line_index == end.line_index {
            end.column.min(line_character_count)
        } else {
            line_character_count
        };
        if start_column < end_column {
            selected.push_str(&slice_character_range(line, start_column..end_column));
        }
    }

    (!selected.is_empty()).then_some(selected)
}

/// 合并语法高亮和正文选区；选区背景优先，避免普通 token 背景覆盖选择反馈。
fn merge_detail_stack_highlights(
    syntax_highlights: Vec<(Range<usize>, HighlightStyle)>,
    selection_range: Option<Range<usize>>,
    theme: &AppTheme,
) -> Vec<(Range<usize>, HighlightStyle)> {
    let Some(selection_range) = selection_range.filter(|range| range.start < range.end) else {
        return syntax_highlights;
    };

    let mut merged = Vec::new();
    for (range, style) in syntax_highlights {
        for visible_range in subtract_detail_selection_range(range, &selection_range) {
            merged.push((visible_range, style.clone()));
        }
    }
    merged.push((
        selection_range,
        HighlightStyle {
            color: Some(rgb(theme.foreground).into()),
            background_color: Some(rgb(theme.selection).into()),
            ..Default::default()
        },
    ));
    merged.sort_by_key(|(range, _)| range.start);
    merged
}

/// 从一个高亮范围中扣除选区覆盖部分，防止两个高亮背景相互叠加。
fn subtract_detail_selection_range(
    range: Range<usize>,
    selection_range: &Range<usize>,
) -> Vec<Range<usize>> {
    if range.end <= selection_range.start || range.start >= selection_range.end {
        return vec![range];
    }

    let mut ranges = Vec::new();
    if range.start < selection_range.start {
        ranges.push(range.start..selection_range.start.min(range.end));
    }
    if range.end > selection_range.end {
        ranges.push(selection_range.end.max(range.start)..range.end);
    }
    ranges
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
