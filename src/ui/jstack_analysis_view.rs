//! 文件职责：渲染 Jstack 线程日志分析页签内容。
//! 创建日期：2026-06-16
//! 修改日期：2026-06-25
//! 作者：Argus 开发团队
//! 主要功能：展示线程频率矩阵、状态筛选、分析统计和高性能虚拟滚动列表。

use std::collections::BTreeSet;
use std::ops::Range;
use std::sync::Arc;

use crate::app::{
    ArgusApp, JstackAnalysisState, JstackAnalysisTaskState, JstackThreadNameSelection,
    jstack_cell_selection_key,
};
use crate::fonts::{ARGUS_LOG_FONT_FAMILY, ARGUS_UI_FONT_FAMILY};
use crate::highlight::{HighlightLanguage, HighlightTokenKind, SyntaxHighlighter};
use crate::analysis::jstack::{
    JstackAnalysisResult, JstackFrequencyCell, JstackFrequencyRow, JstackThreadState,
};
use crate::infra::text_selection::{
    TextSelectionGranularity, byte_index_for_character, char_column_for_byte_index, character_count,
};
use crate::theme::AppTheme;
use crate::ui::components::icon::{ArgusIcon, render_icon};
use crate::ui::components::loading_spinner::render_loading_spinner;
use gpui::{
    AnyElement, Context, FocusHandle, FontWeight, HighlightStyle, IntoElement, KeyDownEvent,
    ListHorizontalSizingBehavior, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent,
    Pixels, Point, Render, SharedString, StyledText, TextRun, UniformListScrollHandle, Window,
    canvas, div, point, prelude::*, px, rgb, uniform_list,
};

/// 分析页签顶部内边距和矩阵横向留白。
const JSTACK_VIEW_PADDING: f32 = 14.0;
/// 线程名列宽度，确保长线程名前半段稳定可读。
const THREAD_NAME_COLUMN_WIDTH: f32 = 330.0;
/// 快照方块边长。
const SNAPSHOT_CELL_SIZE: f32 = 16.0;
/// 快照方块间距。
const SNAPSHOT_CELL_GAP: f32 = 7.0;
/// 线程矩阵行高。
const MATRIX_ROW_HEIGHT: f32 = 30.0;
/// 线程名文本左侧内边距；鼠标命中和文本渲染必须保持一致。
const THREAD_NAME_TEXT_LEFT_PADDING: f32 = 4.0;
/// 自绘滚动条内边距。
const JSTACK_SCROLLBAR_PADDING: f32 = 4.0;
/// 自绘滚动条最小滑块长度。
const JSTACK_SCROLLBAR_MIN_THUMB: f32 = 32.0;
/// 自绘滚动条滑块厚度。
const JSTACK_SCROLLBAR_THUMB_SIZE: f32 = 5.0;
/// 方块悬浮气泡相对鼠标的横向偏移。
const HOVER_PREVIEW_OFFSET_X: f32 = 14.0;
/// 方块悬浮气泡相对鼠标的纵向偏移。
const HOVER_PREVIEW_OFFSET_Y: f32 = 14.0;
/// 方块悬浮气泡宽度，保持轻量不遮挡过多矩阵。
const HOVER_PREVIEW_WIDTH: f32 = 520.0;
/// 方块悬浮气泡贴近矩阵边缘时保留的安全边距。
const HOVER_PREVIEW_EDGE_PADDING: f32 = 8.0;
/// 方块悬浮气泡只展示前若干行堆栈，完整内容由详情窗口承载。
const HOVER_STACK_PREVIEW_LINE_LIMIT: usize = 10;

/// Jstack 方块悬浮预览数据。
#[derive(Clone)]
pub struct JstackCellPreviewData {
    /// 当前主题令牌。
    pub theme: AppTheme,
    /// 快照文件名称。
    pub snapshot_label: String,
    /// 线程名称。
    pub thread_name: String,
    /// 出现次数。
    pub count: usize,
    /// 状态标签。
    pub state_label: String,
    /// 当前线程块完整堆栈预览。
    pub stack_lines: Option<Arc<[String]>>,
}

/// Jstack 方块内部悬浮气泡状态，位置以矩阵容器为坐标系。
#[derive(Clone)]
pub struct JstackCellHoverPreview {
    /// 当前气泡对应的稳定方块 key。
    pub key: String,
    /// 分析页 ID，避免跨页签显示旧气泡。
    pub analysis_id: usize,
    /// 气泡左上角相对矩阵容器的位置。
    pub position: Point<Pixels>,
    /// 气泡展示数据。
    pub data: JstackCellPreviewData,
}

/// 线程名文本选择提示气泡。
struct ThreadNameSelectionTooltip {
    /// 当前主题令牌。
    theme: AppTheme,
    /// 提示文本。
    label: String,
}

impl Render for ThreadNameSelectionTooltip {
    /// 渲染单行提示。
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .px_2()
            .py_1()
            .rounded_sm()
            .bg(rgb(self.theme.title_bar))
            .border_1()
            .border_color(rgb(self.theme.border))
            .text_size(px(12.0))
            .line_height(px(18.0))
            .text_color(rgb(self.theme.foreground))
            .child(self.label.clone())
    }
}

/// 渲染 Jstack 分析页签主体。
pub fn render(app: &ArgusApp, analysis_id: usize, cx: &mut Context<ArgusApp>) -> impl IntoElement {
    let theme = app.theme.clone();
    let Some(state) = app.jstack_analysis_state(analysis_id) else {
        return render_missing_state(app, &theme);
    };
    let analysis_focus_handle = app
        .input_focus_handles
        .as_ref()
        .map(|handles| handles.jstack_analysis.clone());
    let analysis_focus_for_track = analysis_focus_handle.clone();

    div()
        .id(SharedString::from(format!(
            "jstack-analysis-view-{analysis_id}"
        )))
        .size_full()
        .flex()
        .flex_col()
        .overflow_hidden()
        .bg(rgb(theme.content))
        .font_family(ARGUS_UI_FONT_FAMILY)
        .text_color(rgb(theme.foreground))
        .focusable()
        .when_some(analysis_focus_for_track, |this, focus_handle| {
            this.track_focus(&focus_handle)
        })
        .on_key_down(cx.listener(move |app, event: &KeyDownEvent, _, cx| {
            if event.keystroke.modifiers.platform && event.keystroke.key.eq_ignore_ascii_case("c") {
                cx.stop_propagation();
                app.copy_selected_jstack_thread_name(analysis_id, cx);
                cx.notify();
            }
        }))
        .child(render_header(app, state, analysis_id, &theme, cx))
        .child(match &state.task_state {
            JstackAnalysisTaskState::Loading { message } => {
                render_loading_state(message, &theme).into_any_element()
            }
            JstackAnalysisTaskState::Ready(result) => render_frequency_matrix(
                app,
                analysis_id,
                state,
                result,
                analysis_focus_handle,
                &theme,
                cx,
            )
            .into_any_element(),
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

/// 渲染统计和状态图例。
fn render_header(
    app: &ArgusApp,
    state: &JstackAnalysisState,
    analysis_id: usize,
    theme: &AppTheme,
    cx: &mut Context<ArgusApp>,
) -> impl IntoElement + use<> {
    let configured_filter = app.jstack_thread_filter();
    let has_filter_rules = !configured_filter.is_empty();
    let (file_count, snapshot_count, thread_count, skipped_count, filtered_count) =
        match &state.task_state {
            JstackAnalysisTaskState::Ready(result) => (
                result.total_files,
                result.snapshot_count(),
                result.thread_count(),
                result.skipped_count(),
                state.filtered_row_count,
            ),
            JstackAnalysisTaskState::Loading { .. } | JstackAnalysisTaskState::Failed { .. } => {
                (0, 0, 0, 0, 0)
            }
        };
    let filter_summary = if filtered_count > 0 {
        format!("，过滤 {filtered_count} 个线程")
    } else {
        String::new()
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
                .min_w(px(0.0))
                .text_size(px(12.0))
                .line_height(px(16.0))
                .text_color(rgb(theme.foreground))
                .truncate()
                .child(format!(
                    "{file_count} 个文件，{snapshot_count} 个快照，{thread_count} 个线程{filter_summary}，跳过 {skipped_count} 个文件"
                )),
        )
        .child(
            div()
                .flex()
                .items_center()
                .gap_2()
                .child(render_thread_filter_toggle(
                    analysis_id,
                    state.is_thread_filter_enabled,
                    has_filter_rules,
                    theme,
                    cx,
                ))
                .child(render_state_filter(
                    analysis_id,
                    &state.active_states,
                    theme,
                    cx,
                )),
        )
}

/// 渲染设置页线程堆栈过滤开关。
fn render_thread_filter_toggle(
    analysis_id: usize,
    is_enabled: bool,
    has_filter_rules: bool,
    theme: &AppTheme,
    cx: &mut Context<ArgusApp>,
) -> impl IntoElement + use<> {
    div()
        .id(SharedString::from(format!(
            "jstack-thread-filter-toggle-{analysis_id}"
        )))
        .h(px(24.0))
        .px_2()
        .flex()
        .items_center()
        .gap_1()
        .rounded_sm()
        .cursor_pointer()
        .bg(rgb(if is_enabled {
            theme.selection
        } else {
            theme.current_line
        }))
        .text_size(px(12.0))
        .line_height(px(24.0))
        .text_color(rgb(if is_enabled {
            theme.foreground
        } else {
            theme.foreground_muted
        }))
        .opacity(if has_filter_rules || !is_enabled {
            1.0
        } else {
            0.55
        })
        .child(render_icon(
            if is_enabled {
                ArgusIcon::ToggleRight
            } else {
                ArgusIcon::ToggleLeft
            },
            if is_enabled {
                theme.foreground
            } else {
                theme.foreground_muted
            },
            14.0,
        ))
        .child("配置过滤")
        .on_click(cx.listener(move |app, _, _, cx| {
            app.toggle_jstack_thread_filter(analysis_id);
            cx.notify();
        }))
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
    app: &ArgusApp,
    analysis_id: usize,
    state: &JstackAnalysisState,
    result: &JstackAnalysisResult,
    analysis_focus_handle: Option<FocusHandle>,
    theme: &AppTheme,
    cx: &mut Context<ArgusApp>,
) -> impl IntoElement + use<> {
    let visible_indices = state.visible_row_indices.clone();
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
    let hover_preview = app
        .jstack_cell_hover_preview
        .as_ref()
        .filter(|preview| preview.analysis_id == analysis_id)
        .cloned();

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
                    let thread_name_selection = state.thread_name_selection.clone();
                    let selected_cell_key = state.selected_cell_key.clone();
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
                                    thread_name_selection.as_ref(),
                                    selected_cell_key.as_deref(),
                                    matrix_width,
                                    &state.row_scroll,
                                    analysis_focus_handle.clone(),
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
        .when_some(hover_preview, |this, preview| {
            this.child(render_cell_hover_preview(preview, theme))
        })
        .into_any_element()
}

/// 渲染单个线程行。
fn render_matrix_row(
    analysis_id: usize,
    row_index: usize,
    row: &JstackFrequencyRow,
    result: &JstackAnalysisResult,
    active_states: &BTreeSet<JstackThreadState>,
    thread_name_selection: Option<&JstackThreadNameSelection>,
    selected_cell_key: Option<&str>,
    matrix_width: f32,
    row_scroll: &UniformListScrollHandle,
    analysis_focus_handle: Option<FocusHandle>,
    theme: &AppTheme,
    cx: &mut Context<ArgusApp>,
) -> impl IntoElement + use<> {
    let thread_name = row.thread_name.clone();
    let thread_identity = row.display_label();
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
                selected_cell_key,
                row_scroll,
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
                .child(render_selectable_thread_name(
                    analysis_id,
                    thread_name,
                    thread_identity,
                    thread_name_selection,
                    analysis_focus_handle,
                    theme,
                    cx,
                )),
        )
        .child(
            div()
                .flex()
                .items_center()
                .gap(px(SNAPSHOT_CELL_GAP))
                .children(cell_elements),
        )
}

/// 渲染可选中复制的线程名标签。
fn render_selectable_thread_name(
    analysis_id: usize,
    thread_name: String,
    thread_identity: String,
    thread_name_selection: Option<&JstackThreadNameSelection>,
    analysis_focus_handle: Option<FocusHandle>,
    theme: &AppTheme,
    cx: &mut Context<ArgusApp>,
) -> impl IntoElement + use<> {
    let tooltip_theme = theme.clone();
    let selection_range = thread_name_selection
        .filter(|selection| selection.thread_identity == thread_identity)
        .and_then(JstackThreadNameSelection::normalized_range);
    div()
        .id(SharedString::from(format!(
            "jstack-thread-name-{analysis_id}-{thread_identity}"
        )))
        .h(px(MATRIX_ROW_HEIGHT - 6.0))
        .w_full()
        .pl(px(THREAD_NAME_TEXT_LEFT_PADDING))
        .flex()
        .items_center()
        .rounded_sm()
        .relative()
        .overflow_hidden()
        .bg(rgb(theme.content))
        .text_size(px(12.0))
        .line_height(px(MATRIX_ROW_HEIGHT - 6.0))
        .text_color(rgb(theme.foreground))
        .whitespace_nowrap()
        .child(render_thread_name_text(
            thread_name.clone(),
            selection_range,
            theme,
        ))
        .child(render_thread_name_pointer_layer(
            analysis_id,
            thread_identity,
            thread_name,
            analysis_focus_handle,
            cx,
        ))
        .tooltip({
            move |_, cx| {
                cx.new(|_| ThreadNameSelectionTooltip {
                    label: "拖选线程名后按快捷键复制".to_string(),
                    theme: tooltip_theme.clone(),
                })
                .into()
            }
        })
}

/// 渲染线程名文本，并把当前字符选区转换为 GPUI 字节高亮范围。
fn render_thread_name_text(
    thread_name: String,
    selection_range: Option<Range<usize>>,
    theme: &AppTheme,
) -> AnyElement {
    let Some(selection_range) = selection_range else {
        return thread_name.into_any_element();
    };
    let start = byte_index_for_character(&thread_name, selection_range.start);
    let end = byte_index_for_character(&thread_name, selection_range.end);
    if start >= end {
        return thread_name.into_any_element();
    }

    StyledText::new(thread_name)
        .with_highlights(vec![(
            start..end,
            HighlightStyle {
                background_color: Some(rgb(theme.selection).into()),
                color: Some(rgb(theme.foreground).into()),
                ..Default::default()
            },
        )])
        .into_any_element()
}

/// 渲染线程名透明鼠标命中层，负责把拖拽选择转换成应用状态。
fn render_thread_name_pointer_layer(
    analysis_id: usize,
    thread_identity: String,
    thread_name: String,
    analysis_focus_handle: Option<FocusHandle>,
    cx: &mut Context<ArgusApp>,
) -> impl IntoElement + use<> {
    let entity = cx.entity();
    div()
        .id(SharedString::from(format!(
            "jstack-thread-name-pointer-{analysis_id}-{thread_identity}"
        )))
        .absolute()
        .left_0()
        .top_0()
        .right_0()
        .bottom_0()
        .child(
            canvas(
                |_, _, _| (),
                move |bounds, _, window: &mut Window, _| {
                    let visible_bounds = bounds.intersect(&window.content_mask().bounds);

                    window.on_mouse_event({
                        let entity = entity.clone();
                        let thread_identity = thread_identity.clone();
                        let thread_name = thread_name.clone();
                        let analysis_focus_handle = analysis_focus_handle.clone();
                        move |event: &MouseDownEvent, phase, window, cx| {
                            if !phase.bubble()
                                || event.button != MouseButton::Left
                                || !visible_bounds.contains(&event.position)
                            {
                                return;
                            }

                            let character_index = thread_name_character_index_from_pointer(
                                &thread_name,
                                event.position.x,
                                bounds,
                                window,
                            );
                            let granularity =
                                thread_name_granularity_for_click_count(event.click_count);
                            // 线程名选择层会拦截鼠标事件，需要主动把焦点交给分析页，
                            // 这样拖选后立即使用 Cmd/Ctrl+C 可以命中当前页快捷键。
                            if let Some(focus_handle) = analysis_focus_handle.as_ref() {
                                focus_handle.focus(window);
                            }
                            entity.update(cx, |app, _| {
                                app.begin_jstack_thread_name_selection(
                                    analysis_id,
                                    thread_identity.clone(),
                                    thread_name.clone(),
                                    character_index,
                                    granularity,
                                );
                            });
                            cx.stop_propagation();
                            cx.notify(entity.entity_id());
                        }
                    });

                    window.on_mouse_event({
                        let entity = entity.clone();
                        let thread_identity = thread_identity.clone();
                        let thread_name = thread_name.clone();
                        move |event: &MouseMoveEvent, phase, window, cx| {
                            if !phase.bubble() || !event.dragging() {
                                return;
                            }

                            let character_index = thread_name_character_index_from_pointer(
                                &thread_name,
                                event.position.x,
                                bounds,
                                window,
                            );
                            let handled = entity.update(cx, |app, _| {
                                app.update_jstack_thread_name_selection(
                                    analysis_id,
                                    &thread_identity,
                                    character_index,
                                )
                            });
                            if handled {
                                cx.stop_propagation();
                                cx.notify(entity.entity_id());
                            }
                        }
                    });

                    window.on_mouse_event({
                        let entity = entity.clone();
                        move |event: &MouseUpEvent, phase, _, cx| {
                            if !phase.bubble() || event.button != MouseButton::Left {
                                return;
                            }

                            let handled = entity.update(cx, |app, _| {
                                app.finish_jstack_thread_name_selection(analysis_id)
                            });
                            if handled {
                                cx.stop_propagation();
                                cx.notify(entity.entity_id());
                            }
                        }
                    });
                },
            )
            .size_full(),
        )
}

/// 根据线程名点击次数转换选择粒度。
fn thread_name_granularity_for_click_count(click_count: usize) -> TextSelectionGranularity {
    match click_count {
        0 | 1 => TextSelectionGranularity::Character,
        2 => TextSelectionGranularity::Word,
        _ => TextSelectionGranularity::Line,
    }
}

/// 根据鼠标横坐标计算线程名中的字符列。
fn thread_name_character_index_from_pointer(
    thread_name: &str,
    pointer_x: Pixels,
    bounds: gpui::Bounds<Pixels>,
    window: &mut Window,
) -> usize {
    let text_relative_x = pointer_x - bounds.left() - px(THREAD_NAME_TEXT_LEFT_PADDING);
    if thread_name.is_empty() || text_relative_x <= px(0.0) {
        return 0;
    }

    let mut text_style = window.text_style();
    text_style.font_family = ARGUS_UI_FONT_FAMILY.into();
    text_style.font_size = px(12.0).into();
    let run = TextRun {
        len: thread_name.len(),
        font: text_style.font(),
        color: text_style.color,
        background_color: None,
        underline: None,
        strikethrough: None,
    };
    let shaped_line = window.text_system().shape_line(
        SharedString::from(thread_name.to_string()),
        text_style.font_size.to_pixels(window.rem_size()),
        &[run],
        None,
    );
    let byte_index = shaped_line.closest_index_for_x(text_relative_x);
    char_column_for_byte_index(thread_name, byte_index).min(character_count(thread_name))
}

/// 渲染单个快照方块。
fn render_frequency_cell(
    analysis_id: usize,
    row_index: usize,
    cell: &JstackFrequencyCell,
    thread_name: String,
    snapshot_label: String,
    active_states: &BTreeSet<JstackThreadState>,
    selected_cell_key: Option<&str>,
    row_scroll: &UniformListScrollHandle,
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
    let tooltip_stack_lines = if is_visible {
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
    let cell_key = jstack_cell_selection_key(row_index, cell.snapshot_index);
    let is_selected_cell = selected_cell_key == Some(cell_key.as_str());
    let preview_key = format!("{analysis_id}:{cell_key}");
    let preview_data = JstackCellPreviewData {
        theme: theme.clone(),
        snapshot_label,
        thread_name: thread_name.clone(),
        count: cell.count,
        state_label: if is_visible {
            tooltip_state_label
        } else {
            state_label
        },
        stack_lines: tooltip_stack_lines,
    };

    div()
        .id(SharedString::from(format!(
            "jstack-cell-{analysis_id}-{row_index}-{}",
            cell.snapshot_index
        )))
        .w(px(SNAPSHOT_CELL_SIZE))
        .h(px(SNAPSHOT_CELL_SIZE))
        .rounded(px(3.0))
        .bg(rgb(background))
        .opacity(if is_visible { 1.0 } else { 0.18 })
        .when(can_open_detail, |this| this.cursor_pointer())
        .border_1()
        .border_color(rgb(if is_selected_cell {
            theme.info
        } else if !is_visible {
            theme.border
        } else {
            background
        }))
        .when(is_selected_cell, |this| this.border_2().shadow_sm())
        .on_mouse_move({
            let preview_key = preview_key.clone();
            let preview_data = preview_data.clone();
            let row_scroll = row_scroll.clone();
            cx.listener(move |app, event: &MouseMoveEvent, _window, cx| {
                let viewport_bounds = row_scroll.0.borrow().base_handle.bounds();
                let local_x = event.position.x - viewport_bounds.left();
                let local_y = event.position.y - viewport_bounds.top();
                let right_x = local_x + px(HOVER_PREVIEW_OFFSET_X);
                let left_x = (local_x - px(HOVER_PREVIEW_OFFSET_X) - px(HOVER_PREVIEW_WIDTH))
                    .max(px(HOVER_PREVIEW_EDGE_PADDING));
                let max_right_x = (viewport_bounds.size.width
                    - px(HOVER_PREVIEW_WIDTH)
                    - px(HOVER_PREVIEW_EDGE_PADDING))
                .max(px(HOVER_PREVIEW_EDGE_PADDING));
                // 优先在鼠标右侧展示；右侧空间不足时自动翻到左侧，避免气泡被内容区裁掉。
                let preview_x = if right_x + px(HOVER_PREVIEW_WIDTH)
                    > viewport_bounds.size.width - px(HOVER_PREVIEW_EDGE_PADDING)
                {
                    left_x
                } else {
                    right_x.min(max_right_x)
                };
                let preview_position = point(preview_x, local_y + px(HOVER_PREVIEW_OFFSET_Y));
                app.show_jstack_cell_hover_preview(JstackCellHoverPreview {
                    key: preview_key.clone(),
                    analysis_id,
                    position: preview_position,
                    data: preview_data.clone(),
                });
                cx.notify();
            })
        })
        .on_hover(cx.listener(move |app, is_hovered: &bool, _, cx| {
            if !*is_hovered {
                app.clear_jstack_cell_hover_preview();
                cx.notify();
            }
        }))
        .when(can_open_detail, |this| {
            this.on_click(cx.listener(move |app, _, _, cx| {
                app.clear_jstack_cell_hover_preview();
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

/// 渲染 Jstack 方块的内部悬浮气泡，仅展示前 10 行高亮堆栈。
fn render_cell_hover_preview(preview: JstackCellHoverPreview, theme: &AppTheme) -> AnyElement {
    let data = preview.data;
    let stack_lines = data
        .stack_lines
        .as_ref()
        .map(|lines| {
            lines
                .iter()
                .take(HOVER_STACK_PREVIEW_LINE_LIMIT)
                .cloned()
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let hidden_line_count = data
        .stack_lines
        .as_ref()
        .map(|lines| lines.len().saturating_sub(HOVER_STACK_PREVIEW_LINE_LIMIT))
        .unwrap_or_default();

    div()
        .id(SharedString::from(format!(
            "jstack-cell-hover-preview-{}",
            preview.key
        )))
        .absolute()
        .left(preview.position.x)
        .top(preview.position.y)
        .w(px(HOVER_PREVIEW_WIDTH))
        .max_w(px(HOVER_PREVIEW_WIDTH))
        .rounded_sm()
        .shadow_lg()
        .occlude()
        .bg(rgb(theme.title_bar))
        .border_1()
        .border_color(rgb(theme.border))
        .p_2()
        .flex()
        .flex_col()
        .gap_1()
        .text_size(px(12.0))
        .line_height(px(18.0))
        .text_color(rgb(theme.foreground))
        .child(
            div()
                .font_weight(FontWeight::SEMIBOLD)
                .truncate()
                .child(data.thread_name.clone()),
        )
        .child(
            div()
                .flex()
                .gap_2()
                .text_color(rgb(theme.foreground_muted))
                .child(format!("快照：{}", data.snapshot_label))
                .child(format!("次数：{}", data.count))
                .child(format!("状态：{}", data.state_label)),
        )
        .child(
            div()
                .mt_1()
                .rounded_sm()
                .bg(rgb(theme.current_line))
                .border_1()
                .border_color(rgb(theme.border))
                .p_2()
                .font_family(ARGUS_LOG_FONT_FAMILY)
                .children(if stack_lines.is_empty() {
                    vec![
                        div()
                            .text_color(rgb(theme.foreground_muted))
                            .child("无堆栈内容")
                            .into_any_element(),
                    ]
                } else {
                    stack_lines
                        .into_iter()
                        .enumerate()
                        .map(|(index, line)| {
                            div()
                                .flex()
                                .items_center()
                                .h(px(18.0))
                                .min_w(px(0.0))
                                .child(
                                    div()
                                        .w(px(26.0))
                                        .pr_1()
                                        .text_color(rgb(theme.foreground_muted))
                                        .child((index + 1).to_string()),
                                )
                                .child(
                                    div()
                                        .flex_1()
                                        .min_w(px(0.0))
                                        .whitespace_nowrap()
                                        .overflow_hidden()
                                        .child(render_highlighted_preview_stack_line(line, theme)),
                                )
                                .into_any_element()
                        })
                        .collect::<Vec<_>>()
                }),
        )
        .when(hidden_line_count > 0, |this| {
            this.child(
                div()
                    .text_color(rgb(theme.foreground_muted))
                    .child(format!("还有 {hidden_line_count} 行，点击方块查看完整堆栈")),
            )
        })
        .into_any_element()
}

/// 渲染悬浮气泡中的单行 Java 线程堆栈高亮文本。
fn render_highlighted_preview_stack_line(line: String, theme: &AppTheme) -> AnyElement {
    let highlights = SyntaxHighlighter::highlight(&line, HighlightLanguage::JavaThreadDump)
        .into_iter()
        .filter_map(|span| preview_highlight_style_for_span(span.range, span.kind, theme))
        .collect::<Vec<_>>();

    if highlights.is_empty() {
        line.into_any_element()
    } else {
        StyledText::new(line)
            .with_highlights(highlights)
            .into_any_element()
    }
}

/// 把高亮 token 转换成悬浮预览使用的文本样式。
fn preview_highlight_style_for_span(
    range: Range<usize>,
    kind: HighlightTokenKind,
    theme: &AppTheme,
) -> Option<(Range<usize>, HighlightStyle)> {
    (range.start < range.end).then(|| {
        (
            range,
            HighlightStyle {
                color: Some(rgb(color_for_preview_highlight_token(kind, theme)).into()),
                ..Default::default()
            },
        )
    })
}

/// 返回悬浮预览的语法高亮颜色。
fn color_for_preview_highlight_token(kind: HighlightTokenKind, theme: &AppTheme) -> u32 {
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
