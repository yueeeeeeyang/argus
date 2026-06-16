//! 文件职责：渲染 Jstack 线程日志分析页签内容。
//! 创建日期：2026-06-16
//! 修改日期：2026-06-16
//! 作者：Argus 开发团队
//! 主要功能：展示线程频率矩阵、状态筛选、分析统计和高性能虚拟滚动列表。

use std::collections::BTreeSet;
use std::ops::Range;
use std::sync::Arc;

use crate::app::{ArgusApp, JstackAnalysisState, JstackAnalysisTaskState};
use crate::fonts::{ARGUS_LOG_FONT_FAMILY, ARGUS_UI_FONT_FAMILY};
use crate::jstack_analysis::{
    JstackAnalysisResult, JstackFrequencyCell, JstackFrequencyRow, JstackThreadState,
};
use crate::theme::AppTheme;
use crate::ui::components::icon::{ArgusIcon, render_icon};
use crate::ui::components::loading_spinner::render_loading_spinner;
use gpui::{
    AnyElement, Context, FontWeight, IntoElement, ListHorizontalSizingBehavior, Render,
    SharedString, UniformListScrollHandle, Window, div, prelude::*, px, rgb, uniform_list,
};

/// 分析页签顶部内边距和矩阵横向留白。
const JSTACK_VIEW_PADDING: f32 = 14.0;
/// 线程名列宽度，确保长线程名前半段稳定可读。
const THREAD_NAME_COLUMN_WIDTH: f32 = 330.0;
/// 快照方块边长。
const SNAPSHOT_CELL_SIZE: f32 = 14.0;
/// 快照方块间距。
const SNAPSHOT_CELL_GAP: f32 = 7.0;
/// 线程矩阵行高。
const MATRIX_ROW_HEIGHT: f32 = 30.0;
/// 自绘滚动条内边距。
const JSTACK_SCROLLBAR_PADDING: f32 = 4.0;
/// 自绘滚动条最小滑块长度。
const JSTACK_SCROLLBAR_MIN_THUMB: f32 = 32.0;
/// 自绘滚动条滑块厚度。
const JSTACK_SCROLLBAR_THUMB_SIZE: f32 = 5.0;

/// 方块 tooltip，展示线程在某个快照中的聚合信息。
struct JstackCellTooltip {
    /// 当前主题令牌。
    theme: AppTheme,
    /// 快照文件名称。
    snapshot_label: String,
    /// 线程名称。
    thread_name: String,
    /// 出现次数。
    count: usize,
    /// 状态标签。
    state_label: String,
    /// 当前线程块前 20 行堆栈预览。
    preview_stack_lines: Option<Arc<[String]>>,
}

impl Render for JstackCellTooltip {
    /// 渲染紧凑 tooltip。
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .max_w(px(420.0))
            .px_3()
            .py_2()
            .rounded_sm()
            .bg(rgb(self.theme.title_bar))
            .border_1()
            .border_color(rgb(self.theme.border))
            .text_size(px(12.0))
            .line_height(px(18.0))
            .text_color(rgb(self.theme.foreground))
            .flex()
            .flex_col()
            .gap_1()
            .child(
                div()
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(self.thread_name.clone()),
            )
            .child(format!("快照：{}", self.snapshot_label))
            .child(format!("出现次数：{}", self.count))
            .child(format!("状态：{}", self.state_label))
            .child(
                div()
                    .pt_1()
                    .text_color(rgb(self.theme.foreground_muted))
                    .child("前 20 行堆栈："),
            )
            .child(
                div()
                    .id("jstack-cell-tooltip-stack-preview")
                    .max_h(px(260.0))
                    .overflow_y_scroll()
                    .scrollbar_width(px(6.0))
                    .rounded_sm()
                    .bg(rgb(self.theme.background))
                    .border_1()
                    .border_color(rgb(self.theme.border))
                    .p_2()
                    .font_family(ARGUS_LOG_FONT_FAMILY)
                    .children(
                        self.preview_stack_lines
                            .as_ref()
                            .map(|lines| {
                                lines
                                    .iter()
                                    .take(20)
                                    .cloned()
                                    .map(|line| {
                                        div()
                                            .min_w(px(0.0))
                                            .whitespace_nowrap()
                                            .line_height(px(18.0))
                                            .child(line)
                                            .into_any_element()
                                    })
                                    .collect::<Vec<_>>()
                            })
                            .filter(|lines| !lines.is_empty())
                            .unwrap_or_else(|| {
                                vec![
                                    div()
                                        .text_color(rgb(self.theme.foreground_muted))
                                        .child("无堆栈内容")
                                        .into_any_element(),
                                ]
                            }),
                    ),
            )
    }
}

/// 渲染 Jstack 分析页签主体。
pub fn render(app: &ArgusApp, analysis_id: usize, cx: &mut Context<ArgusApp>) -> impl IntoElement {
    let theme = app.theme.clone();
    let Some(state) = app.jstack_analysis_state(analysis_id) else {
        return render_missing_state(app, &theme);
    };

    div()
        .size_full()
        .flex()
        .flex_col()
        .overflow_hidden()
        .bg(rgb(theme.content))
        .font_family(ARGUS_UI_FONT_FAMILY)
        .text_color(rgb(theme.foreground))
        .child(render_header(state, analysis_id, &theme, cx))
        .child(match &state.task_state {
            JstackAnalysisTaskState::Loading { message } => {
                render_loading_state(message, &theme).into_any_element()
            }
            JstackAnalysisTaskState::Ready(result) => {
                render_frequency_matrix(analysis_id, state, result, &theme, cx).into_any_element()
            }
            JstackAnalysisTaskState::Failed { message } => {
                render_error_state(message, &theme).into_any_element()
            }
        })
        .into_any_element()
}

/// 渲染状态缺失的空态。
fn render_missing_state(app: &ArgusApp, theme: &AppTheme) -> AnyElement {
    div()
        .size_full()
        .flex()
        .flex_col()
        .items_center()
        .justify_center()
        .gap_2()
        .bg(rgb(theme.content))
        .text_color(rgb(theme.foreground_muted))
        .child(render_icon(ArgusIcon::Logs, theme.foreground_muted, 28.0))
        .child(app.active_tab_title().to_string())
        .child("Jstack 分析结果已释放，请重新从来源树右键发起分析。")
        .into_any_element()
}

/// 渲染标题、统计和状态图例。
fn render_header(
    state: &JstackAnalysisState,
    analysis_id: usize,
    theme: &AppTheme,
    cx: &mut Context<ArgusApp>,
) -> impl IntoElement + use<> {
    let (file_count, snapshot_count, thread_count, skipped_count) = match &state.task_state {
        JstackAnalysisTaskState::Ready(result) => (
            result.total_files,
            result.snapshot_count(),
            result.thread_count(),
            result.skipped_count(),
        ),
        JstackAnalysisTaskState::Loading { .. } | JstackAnalysisTaskState::Failed { .. } => {
            (0, 0, 0, 0)
        }
    };

    div()
        .px(px(JSTACK_VIEW_PADDING))
        .pt(px(8.0))
        .pb(px(7.0))
        .flex()
        .items_center()
        .justify_between()
        .gap_3()
        .border_b_1()
        .border_color(rgb(theme.border))
        .child(
            div()
                .flex()
                .flex_col()
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_1()
                        .text_size(px(13.0))
                        .line_height(px(18.0))
                        .font_weight(FontWeight::SEMIBOLD)
                        .child(render_icon(ArgusIcon::Logs, theme.foreground_muted, 14.0))
                        .child(state.title.clone()),
                )
                .child(
                    div()
                        .text_size(px(12.0))
                        .line_height(px(16.0))
                        .text_color(rgb(theme.foreground_muted))
                        .child(format!(
                            "{file_count} 个文件，{snapshot_count} 个快照，{thread_count} 个线程，跳过 {skipped_count} 个文件"
                        )),
                ),
        )
        .child(render_state_filter(
            analysis_id,
            &state.active_states,
            theme,
            cx,
        ))
}

/// 渲染线程状态筛选器。
fn render_state_filter(
    analysis_id: usize,
    active_states: &BTreeSet<JstackThreadState>,
    theme: &AppTheme,
    cx: &mut Context<ArgusApp>,
) -> impl IntoElement + use<> {
    div().flex().items_center().gap_1().children(
        [
            JstackThreadState::Runnable,
            JstackThreadState::Blocked,
            JstackThreadState::Waiting,
            JstackThreadState::TimedWaiting,
            JstackThreadState::Other,
        ]
        .into_iter()
        .map(|state| {
            render_state_filter_item(
                analysis_id,
                state,
                active_states.contains(&state),
                theme,
                cx,
            )
            .into_any_element()
        }),
    )
}

/// 渲染单个状态筛选按钮。
fn render_state_filter_item(
    analysis_id: usize,
    state: JstackThreadState,
    is_active: bool,
    theme: &AppTheme,
    cx: &mut Context<ArgusApp>,
) -> impl IntoElement + use<> {
    let state_for_click = state;
    div()
        .id(SharedString::from(format!(
            "jstack-state-filter-{}-{}",
            analysis_id,
            state.label()
        )))
        .flex()
        .items_center()
        .gap_1()
        .h(px(24.0))
        .px_2()
        .rounded_sm()
        .cursor_pointer()
        .bg(rgb(if is_active {
            theme.selection
        } else {
            theme.current_line
        }))
        .text_size(px(12.0))
        .line_height(px(24.0))
        .text_color(rgb(if is_active {
            theme.foreground
        } else {
            theme.foreground_muted
        }))
        .opacity(if is_active { 1.0 } else { 0.52 })
        .child(
            div()
                .w(px(8.0))
                .h(px(8.0))
                .rounded_sm()
                .bg(rgb(color_for_state(state))),
        )
        .child(
            div()
                .h_full()
                .flex()
                .items_center()
                .line_height(px(24.0))
                .child(state.label()),
        )
        .on_click(cx.listener(move |app, _, _, cx| {
            app.toggle_jstack_state_filter(analysis_id, state_for_click);
            cx.notify();
        }))
}

/// 渲染加载态。
fn render_loading_state(message: &str, theme: &AppTheme) -> impl IntoElement + use<> {
    div()
        .flex_1()
        .flex()
        .items_center()
        .justify_center()
        .gap_3()
        .text_size(px(13.0))
        .text_color(rgb(theme.foreground_muted))
        .child(render_loading_spinner(
            ("jstack-analysis-loading", 0),
            theme.foreground_muted,
            18.0,
        ))
        .child(message.to_string())
}

/// 渲染失败态。
fn render_error_state(message: &str, theme: &AppTheme) -> impl IntoElement + use<> {
    div()
        .flex_1()
        .flex()
        .items_center()
        .justify_center()
        .text_size(px(13.0))
        .text_color(rgb(theme.error))
        .child(message.to_string())
}

/// 渲染线程频率矩阵。
fn render_frequency_matrix(
    analysis_id: usize,
    state: &JstackAnalysisState,
    result: &JstackAnalysisResult,
    theme: &AppTheme,
    cx: &mut Context<ArgusApp>,
) -> impl IntoElement + use<> {
    let visible_indices = visible_row_indices(result, &state.active_states);
    if visible_indices.is_empty() {
        return div()
            .flex_1()
            .flex()
            .items_center()
            .justify_center()
            .text_size(px(13.0))
            .text_color(rgb(theme.foreground_muted))
            .child("当前状态筛选下没有可展示的 Jstack 线程。")
            .into_any_element();
    }

    let matrix_width = THREAD_NAME_COLUMN_WIDTH
        + result.snapshots.len() as f32 * (SNAPSHOT_CELL_SIZE + SNAPSHOT_CELL_GAP)
        + JSTACK_VIEW_PADDING;
    let row_count = visible_indices.len();
    let visible_indices = Arc::new(visible_indices);
    let active_states = Arc::new(state.active_states.clone());
    let row_scroll = state.row_scroll.clone();

    div()
        .id("jstack-analysis-matrix-container")
        .relative()
        .flex_1()
        .min_h(px(0.0))
        .overflow_hidden()
        .border_t_1()
        .border_color(rgb(theme.border))
        .child(
            uniform_list(
                "jstack-analysis-row-list",
                row_count,
                cx.processor(move |app, range: Range<usize>, _window, row_cx| {
                    let theme = app.theme.clone();
                    let Some(state) = app.jstack_analysis_state(analysis_id) else {
                        return Vec::new();
                    };
                    let JstackAnalysisTaskState::Ready(result) = &state.task_state else {
                        return Vec::new();
                    };
                    let Some(row_indices) = visible_indices.as_slice().get(range) else {
                        return Vec::new();
                    };

                    row_indices
                        .iter()
                        .filter_map(|row_index| {
                            result.rows.get(*row_index).map(|row| {
                                render_matrix_row(
                                    analysis_id,
                                    *row_index,
                                    row,
                                    result,
                                    active_states.as_ref(),
                                    matrix_width,
                                    &theme,
                                    row_cx,
                                )
                                .into_any_element()
                            })
                        })
                        .collect::<Vec<_>>()
                }),
            )
            .with_width_from_item(Some(0))
            .with_horizontal_sizing_behavior(ListHorizontalSizingBehavior::Unconstrained)
            .size_full()
            .block_mouse_except_scroll()
            .track_scroll(row_scroll.clone()),
        )
        .children(render_matrix_scrollbars(&row_scroll, theme))
        .into_any_element()
}

/// 渲染单个线程行。
fn render_matrix_row(
    analysis_id: usize,
    row_index: usize,
    row: &JstackFrequencyRow,
    result: &JstackAnalysisResult,
    active_states: &BTreeSet<JstackThreadState>,
    matrix_width: f32,
    theme: &AppTheme,
    cx: &mut Context<ArgusApp>,
) -> impl IntoElement + use<> {
    let thread_name = row.thread_name.clone();
    let cell_elements = row
        .cells
        .iter()
        .map(|cell| {
            let snapshot_label = result
                .snapshots
                .get(cell.snapshot_index)
                .map(|snapshot| snapshot.label.clone())
                .unwrap_or_else(|| format!("快照 {}", cell.snapshot_index + 1));
            render_frequency_cell(
                analysis_id,
                row_index,
                cell,
                thread_name.clone(),
                snapshot_label,
                active_states,
                theme,
                cx,
            )
            .into_any_element()
        })
        .collect::<Vec<_>>();

    div()
        .h(px(MATRIX_ROW_HEIGHT))
        .min_w(px(matrix_width))
        .px(px(JSTACK_VIEW_PADDING))
        .flex()
        .items_center()
        .border_b_1()
        .border_color(rgb(theme.border))
        .child(
            div()
                .w(px(THREAD_NAME_COLUMN_WIDTH))
                .pr_3()
                .truncate()
                .text_size(px(12.0))
                .line_height(px(MATRIX_ROW_HEIGHT))
                .text_color(rgb(theme.foreground))
                .child(thread_name),
        )
        .child(
            div()
                .flex()
                .items_center()
                .gap(px(SNAPSHOT_CELL_GAP))
                .children(cell_elements),
        )
}

/// 渲染单个快照方块。
fn render_frequency_cell(
    analysis_id: usize,
    row_index: usize,
    cell: &JstackFrequencyCell,
    thread_name: String,
    snapshot_label: String,
    active_states: &BTreeSet<JstackThreadState>,
    theme: &AppTheme,
    cx: &mut Context<ArgusApp>,
) -> impl IntoElement + use<> {
    let state = cell.state;
    let is_visible = state.is_some_and(|state| active_states.contains(&state));
    let background = if is_visible {
        state.map(color_for_state).unwrap_or(theme.current_line)
    } else {
        theme.current_line
    };
    let state_label = if is_visible {
        state
            .map(|state| state.label().to_string())
            .unwrap_or_else(|| "未出现".to_string())
    } else {
        "已被筛选隐藏".to_string()
    };
    let tooltip_state_label = state
        .map(|state| state.label().to_string())
        .unwrap_or_else(|| "未出现".to_string());
    let tooltip_theme = theme.clone();
    let tooltip_thread_name = thread_name.clone();
    let tooltip_count = cell.count;
    let tooltip_preview_stack_lines = if is_visible {
        cell.stack_occurrences
            .first()
            .map(|occurrence| occurrence.stack_lines.clone())
    } else {
        None
    };
    let detail_snapshot_index = cell.snapshot_index;
    let detail_occurrence_index = cell
        .stack_occurrences
        .iter()
        .find(|occurrence| Some(occurrence.state) == state)
        .or_else(|| cell.stack_occurrences.first())
        .map(|occurrence| occurrence.occurrence_index)
        .unwrap_or(1);
    let can_open_detail = is_visible && cell.count > 0;

    div()
        .id(SharedString::from(format!(
            "jstack-cell-{}-{}",
            thread_name, cell.snapshot_index
        )))
        .w(px(SNAPSHOT_CELL_SIZE))
        .h(px(SNAPSHOT_CELL_SIZE))
        .rounded(px(3.0))
        .bg(rgb(background))
        .opacity(if is_visible { 1.0 } else { 0.18 })
        .when(can_open_detail, |this| this.cursor_pointer())
        .border_1()
        .border_color(rgb(if !is_visible {
            theme.border
        } else {
            background
        }))
        .tooltip(move |_, cx| {
            cx.new(|_| JstackCellTooltip {
                theme: tooltip_theme.clone(),
                snapshot_label: snapshot_label.clone(),
                thread_name: tooltip_thread_name.clone(),
                count: tooltip_count,
                state_label: if is_visible {
                    tooltip_state_label.clone()
                } else {
                    state_label.clone()
                },
                preview_stack_lines: tooltip_preview_stack_lines.clone(),
            })
            .into()
        })
        .when(can_open_detail, |this| {
            this.on_click(cx.listener(move |app, _, _, cx| {
                app.open_jstack_thread_detail_for_cell(
                    analysis_id,
                    row_index,
                    detail_snapshot_index,
                    detail_occurrence_index,
                    cx,
                );
                cx.notify();
            }))
        })
}

/// 返回当前筛选条件下需要渲染的行索引。
fn visible_row_indices(
    result: &JstackAnalysisResult,
    active_states: &BTreeSet<JstackThreadState>,
) -> Vec<usize> {
    if active_states.is_empty() {
        return Vec::new();
    }

    result
        .rows
        .iter()
        .enumerate()
        .filter_map(|(index, row)| {
            row.cells
                .iter()
                .any(|cell| {
                    cell.count > 0
                        && cell
                            .state
                            .is_some_and(|state| active_states.contains(&state))
                })
                .then_some(index)
        })
        .collect()
}

/// 根据矩阵滚动状态绘制可见滚动条。
fn render_matrix_scrollbars(
    row_scroll: &UniformListScrollHandle,
    theme: &AppTheme,
) -> Vec<AnyElement> {
    let scroll_state = row_scroll.0.borrow();
    let bounds = scroll_state.base_handle.bounds();
    let scroll_offset = scroll_state.base_handle.offset();
    let content_size = scroll_state
        .last_item_size
        .map(|item_size| item_size.contents)
        .unwrap_or_default();
    drop(scroll_state);

    let mut scrollbars = Vec::new();
    if let Some(vertical) = render_passive_scrollbar(
        false,
        bounds.size.height,
        content_size.height,
        -scroll_offset.y,
        theme,
    ) {
        scrollbars.push(vertical);
    }
    if let Some(horizontal) = render_passive_scrollbar(
        true,
        bounds.size.width,
        content_size.width,
        -scroll_offset.x,
        theme,
    ) {
        scrollbars.push(horizontal);
    }

    scrollbars
}

/// 绘制单个被动滚动条滑块；真实滚动由 GPUI 列表处理。
fn render_passive_scrollbar(
    is_horizontal: bool,
    viewport_length: gpui::Pixels,
    content_length: gpui::Pixels,
    scroll_offset: gpui::Pixels,
    theme: &AppTheme,
) -> Option<AnyElement> {
    if viewport_length == px(0.0) || content_length <= viewport_length {
        return None;
    }

    let track_padding = px(JSTACK_SCROLLBAR_PADDING);
    let track_length = (viewport_length - track_padding * 2.0).max(px(1.0));
    let thumb_length = ((viewport_length / content_length) * track_length)
        .clamp(px(JSTACK_SCROLLBAR_MIN_THUMB), track_length);
    let max_scroll = (content_length - viewport_length).max(px(1.0));
    let scroll_ratio = (scroll_offset / max_scroll).clamp(0.0, 1.0);
    let thumb_start = track_padding + (track_length - thumb_length) * scroll_ratio;

    let thumb = div()
        .absolute()
        .rounded_lg()
        .bg(rgb(theme.foreground_muted))
        .opacity(0.48)
        .hover(|this| this.opacity(0.78));

    Some(
        if is_horizontal {
            thumb
                .left(thumb_start)
                .bottom(px(JSTACK_SCROLLBAR_PADDING))
                .w(thumb_length)
                .h(px(JSTACK_SCROLLBAR_THUMB_SIZE))
        } else {
            thumb
                .top(thumb_start)
                .right(px(JSTACK_SCROLLBAR_PADDING))
                .w(px(JSTACK_SCROLLBAR_THUMB_SIZE))
                .h(thumb_length)
        }
        .into_any_element(),
    )
}

/// 返回状态颜色，使用固定语义色避免主题变化破坏状态识别。
fn color_for_state(state: JstackThreadState) -> u32 {
    match state {
        JstackThreadState::Runnable => 0x22c55e,
        JstackThreadState::Blocked => 0xb33b3b,
        JstackThreadState::Waiting => 0xa16b12,
        JstackThreadState::TimedWaiting => 0x2f82a3,
        JstackThreadState::Other => 0x64748b,
    }
}
