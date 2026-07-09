//! 文件职责：渲染 Runtime 请求日志分析页签内容。
//! 创建日期：2026-06-25
//! 修改日期：2026-06-25
//! 作者：Argus 开发团队
//! 主要功能：展示 Runtime 请求总览、请求详情和 SQL 明细三层可排序表格。

use std::cmp::Ordering;
use std::ops::Range;
use std::sync::Arc;

use chrono::{Datelike, Local, TimeZone, Timelike};

use crate::analysis::runtime::{
    RuntimeAnalysisFilterCriteria, RuntimeAnalysisResult, RuntimeRequestRecord,
    RuntimeRequestSummary, RuntimeSlowSqlSummaryRow, RuntimeSqlFrequencyAnalysisRow,
    RuntimeSqlFrequencyDetailRow, RuntimeSqlRecord, filtered_runtime_summary_from_indices,
    parse_runtime_analysis_filter_criteria, parse_runtime_filter_time_value,
    runtime_request_matches_cross_filters as domain_runtime_request_matches_cross_filters,
    runtime_request_matches_keyword as domain_runtime_request_matches_keyword,
    runtime_sql_matches_keyword as domain_runtime_sql_matches_keyword,
    sort_runtime_sql_frequency_detail_rows,
};
use crate::app::{
    AppTextInputTarget, ArgusApp, RUNTIME_SQL_COLLAPSED_ROW_HEIGHT, RuntimeAnalysisResultType,
    RuntimeAnalysisState, RuntimeAnalysisTaskState, RuntimeAnalysisView, RuntimeFilterInputKind,
    RuntimeRequestIndicesCache, RuntimeRequestSortKey, RuntimeScrollbarDrag, RuntimeScrollbarTable,
    RuntimeSortDirection, RuntimeSqlAnalysisFilterSnapshot, RuntimeSqlCellKey,
    RuntimeSqlFrequencyDetailRowsCache, RuntimeSqlIndicesCache, RuntimeSqlSortKey,
    RuntimeSqlTextDialog, RuntimeSqlTextSelection, RuntimeSummaryRowsCache, RuntimeSummarySortKey,
    RuntimeTableCellSelection, SettingsTextInputState,
};
use crate::fonts::{ARGUS_LOG_FONT_FAMILY, ARGUS_UI_FONT_FAMILY};
use crate::infra::perf::PerfSpan;
use crate::infra::text_selection::{
    TextSelectionGranularity, byte_index_for_character, char_column_for_byte_index, character_count,
};
use crate::theme::AppTheme;
use crate::ui::components::datetime_picker::{DateTimePickerValue, render_datetime_picker};
use crate::ui::components::icon::{ArgusIcon, render_icon};
use crate::ui::components::input::{
    Input, InputAccessory, InputPointerAction, InputPointerEvent, InputSize, render_input,
};
use crate::ui::components::loading_spinner::render_loading_spinner;
use crate::ui::components::modal_dialog::{ModalDialog, render_modal_dialog};
use crate::ui::components::scrollbar::{
    ScrollbarMetrics, scrollbar_metrics, scrollbar_scroll_for_drag,
};
use crate::ui::input_native::app_native_input;
use gpui::{
    AnyElement, App, ClickEvent, Context, FocusHandle, FontWeight, HighlightStyle, IntoElement,
    KeyDownEvent, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, Pixels, ScrollHandle,
    SharedString, StatefulInteractiveElement, StyledText, TextRun, UniformListScrollHandle, Window,
    canvas, div, point, prelude::*, px, rgb, uniform_list,
};

/// Runtime 分析页整体边距。
const RUNTIME_VIEW_PADDING: f32 = 14.0;
/// 表头固定高度。
const TABLE_HEADER_HEIGHT: f32 = 30.0;
/// 总览和请求明细行高。
const TABLE_ROW_HEIGHT: f32 = 34.0;
/// SQL 明细默认行高；收起态通过列内横向滚动查看，展开态按内容增高。
const SQL_ROW_HEIGHT: f32 = RUNTIME_SQL_COLLAPSED_ROW_HEIGHT;
/// 自绘滚动条内边距。
const RUNTIME_SCROLLBAR_PADDING: f32 = 4.0;
/// 自绘滚动条最小滑块长度。
const RUNTIME_SCROLLBAR_MIN_THUMB: f32 = 32.0;
/// 自绘滚动条滑块厚度。
const RUNTIME_SCROLLBAR_THUMB_SIZE: f32 = 5.0;
/// 总览表固定列宽：请求次数。
const SUMMARY_COUNT_COLUMN_WIDTH: f32 = 96.0;
/// 总览表固定列宽：平均耗时。
const SUMMARY_AVERAGE_COLUMN_WIDTH: f32 = 118.0;
/// 总览表固定列宽：慢 SQL 比例。
const SUMMARY_RATIO_COLUMN_WIDTH: f32 = 118.0;
/// 总览表固定列宽：操作。
const SUMMARY_ACTION_COLUMN_WIDTH: f32 = 86.0;
/// 请求明细表固定列宽：请求时间。
const REQUEST_TIME_COLUMN_WIDTH: f32 = 172.0;
/// 请求明细表固定列宽：用户名。
const REQUEST_USERNAME_COLUMN_WIDTH: f32 = 118.0;
/// 请求明细表固定列宽：请求耗时。
const REQUEST_DURATION_COLUMN_WIDTH: f32 = 110.0;
/// 请求明细表固定列宽：操作。
const REQUEST_ACTION_COLUMN_WIDTH: f32 = 94.0;
/// SQL 明细表固定列宽：耗时类字段。
const SQL_DURATION_COLUMN_WIDTH: f32 = 112.0;
/// SQL 频率分析表固定列宽：平均耗时。
const SQL_FREQUENCY_AVERAGE_COLUMN_WIDTH: f32 = 128.0;
/// SQL 频率分析表固定列宽：执行次数。
const SQL_FREQUENCY_COUNT_COLUMN_WIDTH: f32 = 104.0;
/// SQL 频率分析表固定列宽：操作。
const SQL_FREQUENCY_ACTION_COLUMN_WIDTH: f32 = 86.0;
/// SQL 频率详情表固定列宽：SQL 具体耗时。
const SQL_FREQUENCY_DETAIL_DURATION_COLUMN_WIDTH: f32 = 112.0;
/// SQL 频率详情表固定列宽：来源请求。
const SQL_FREQUENCY_DETAIL_REQUEST_COLUMN_WIDTH: f32 = 260.0;
/// SQL 频率详情表固定列宽：发起时间。
const SQL_FREQUENCY_DETAIL_TIME_COLUMN_WIDTH: f32 = 172.0;
/// SQL 分析表文本列和指标列之间的固定间隔，避免长 SQL 与耗时数字贴在一起。
const SQL_ANALYSIS_COLUMN_GAP: f32 = 28.0;
/// Runtime 过滤栏高度。
const RUNTIME_FILTER_BAR_HEIGHT: f32 = 44.0;
/// Runtime 过滤栏控件间距，需与 `gap_2` 保持一致。
const RUNTIME_FILTER_GAP: f32 = 8.0;
/// Runtime 过滤栏关键字输入框宽度。
const RUNTIME_FILTER_KEYWORD_WIDTH: f32 = 230.0;
/// Runtime 过滤栏用户名输入框宽度。
const RUNTIME_FILTER_USERNAME_WIDTH: f32 = 170.0;
/// Runtime 过滤栏时间输入框宽度。
const RUNTIME_FILTER_TIME_WIDTH: f32 = 190.0;
/// Runtime 时间选择器浮层顶部位置，略低于过滤栏底部以贴合输入框。
const RUNTIME_TIME_PICKER_TOP: f32 = RUNTIME_FILTER_BAR_HEIGHT - 2.0;
/// Runtime 表格单元格左右内边距，用于文本命中测试和视觉留白保持一致。
const RUNTIME_CELL_HORIZONTAL_PADDING: f32 = 0.0;
/// Runtime SQL 完整文本弹窗宽度。
const RUNTIME_SQL_DIALOG_WIDTH: f32 = 760.0;
/// Runtime SQL 完整文本弹窗高度。
const RUNTIME_SQL_DIALOG_HEIGHT: f32 = 520.0;
/// Runtime SQL 完整文本弹窗代码行高。
const RUNTIME_SQL_DIALOG_LINE_HEIGHT: f32 = 20.0;

mod computation;
mod filter_bar;
mod helpers;
mod sql_dialog;
mod table_parts;
mod tables;

pub use computation::*;
pub use filter_bar::*;
pub use helpers::*;
pub use sql_dialog::*;
pub use table_parts::*;
pub use tables::*;

/// 渲染 Runtime 分析页签主体。
pub fn render(app: &ArgusApp, analysis_id: usize, cx: &mut Context<ArgusApp>) -> impl IntoElement {
    let theme = app.theme.clone();
    let Some(state) = app.runtime_analysis_state(analysis_id) else {
        return render_missing_state(app, &theme);
    };
    let analysis_focus_handle = app
        .input_focus_handles
        .as_ref()
        .map(|handles| handles.runtime_analysis.clone());
    let analysis_focus_for_track = analysis_focus_handle.clone();

    div()
        .id(SharedString::from(format!(
            "runtime-analysis-view-{analysis_id}"
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
                if !app.copy_runtime_sql_text_selection(analysis_id, cx) {
                    app.copy_selected_runtime_cell(analysis_id, cx);
                }
                cx.notify();
            }
        }))
        .child(render_header(state, &theme))
        .child(match &state.task_state {
            RuntimeAnalysisTaskState::Loading { message } => {
                render_loading_state(message, &theme).into_any_element()
            }
            RuntimeAnalysisTaskState::Ready(result) => render_ready_view(
                app,
                analysis_id,
                state,
                result,
                analysis_focus_handle,
                &theme,
                cx,
            )
            .into_any_element(),
            RuntimeAnalysisTaskState::Failed { message } => {
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
        .child(render_icon(
            ArgusIcon::Database,
            theme.foreground_muted,
            28.0,
        ))
        .child(app.active_tab_title().to_string())
        .child("Runtime 分析结果已释放，请重新从来源树右键发起分析。")
        .into_any_element()
}

/// 渲染标题和统计信息。
fn render_header(state: &RuntimeAnalysisState, theme: &AppTheme) -> impl IntoElement + use<> {
    let (file_count, summary_count, request_count, sql_count, skipped_count) =
        match &state.task_state {
            RuntimeAnalysisTaskState::Ready(result) => (
                result.total_files,
                result.summary_count(),
                result.request_count(),
                result.total_sql_records,
                result.skipped_count(),
            ),
            RuntimeAnalysisTaskState::Loading { .. } | RuntimeAnalysisTaskState::Failed { .. } => {
                (0, 0, 0, 0, 0)
            }
        };

    div()
        .px(px(RUNTIME_VIEW_PADDING))
        .py(px(8.0))
        .flex()
        .items_center()
        .justify_between()
        .gap_3()
        .border_b_1()
        .border_color(rgb(theme.border))
        .child(
            div()
                .text_size(px(12.0))
                .line_height(px(16.0))
                .text_color(rgb(theme.foreground))
                .child(format!(
                    "{file_count} 个文件，{summary_count} 个请求地址，{request_count} 个请求，{sql_count} 条 SQL，跳过 {skipped_count} 个文件"
                )),
        )
        .child(
            div()
                .text_size(px(12.0))
                .text_color(rgb(theme.foreground))
                .child("慢 SQL 请求：SQL 累积耗时 > 请求总耗时 90%"),
        )
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
            ("runtime-analysis-loading", 0),
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

/// 根据 Runtime 当前 drill-down 层级渲染内容。
fn render_ready_view(
    app: &ArgusApp,
    analysis_id: usize,
    state: &RuntimeAnalysisState,
    result: &RuntimeAnalysisResult,
    analysis_focus_handle: Option<FocusHandle>,
    theme: &AppTheme,
    cx: &mut Context<ArgusApp>,
) -> AnyElement {
    let sql_dialog_focus_handle = analysis_focus_handle.clone();
    let content = match state.result_type {
        RuntimeAnalysisResultType::Statistics => match &state.view {
            RuntimeAnalysisView::Summary => {
                render_summary_table(analysis_id, state, result, analysis_focus_handle, theme, cx)
                    .into_any_element()
            }
            RuntimeAnalysisView::RequestDetails { request_path } => render_request_details_table(
                analysis_id,
                state,
                result,
                request_path,
                analysis_focus_handle,
                theme,
                cx,
            ),
            RuntimeAnalysisView::SqlList {
                request_path,
                request_index,
            } => {
                let Some(request) = result.requests.get(*request_index) else {
                    return render_empty_message("未找到当前 Runtime 请求记录。", theme);
                };
                render_sql_table(
                    analysis_id,
                    state,
                    request_path,
                    request,
                    analysis_focus_handle,
                    theme,
                    cx,
                )
                .into_any_element()
            }
        },
        RuntimeAnalysisResultType::SqlFrequency => match state.sql_frequency_detail_sql.as_ref() {
            Some(normalized_sql) => render_sql_frequency_detail_table(
                analysis_id,
                state,
                result,
                normalized_sql,
                analysis_focus_handle,
                theme,
                cx,
            )
            .into_any_element(),
            None => render_sql_frequency_table(
                analysis_id,
                state,
                result,
                analysis_focus_handle,
                theme,
                cx,
            )
            .into_any_element(),
        },
        RuntimeAnalysisResultType::SlowSql => match state.slow_sql_detail_sql.as_ref() {
            Some(normalized_sql) => render_slow_sql_detail_table(
                analysis_id,
                state,
                result,
                normalized_sql,
                analysis_focus_handle,
                theme,
                cx,
            )
            .into_any_element(),
            None => {
                render_slow_sql_table(analysis_id, state, result, analysis_focus_handle, theme, cx)
                    .into_any_element()
            }
        },
    };

    div()
        .flex_1()
        .min_h(px(0.0))
        .relative()
        .flex()
        .flex_col()
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(move |app, _: &MouseDownEvent, _, cx| {
                if app.close_runtime_time_picker(analysis_id) {
                    cx.notify();
                }
            }),
        )
        .child(render_result_type_selector(analysis_id, state, theme, cx))
        .child(render_filter_bar(app, analysis_id, state, theme, cx))
        .child(content)
        .when_some(state.open_time_picker, |this, input_kind| {
            this.child(render_runtime_time_picker_overlay(
                analysis_id,
                input_kind,
                state,
                theme,
                cx,
            ))
        })
        .when_some(state.sql_text_dialog.clone(), |this, dialog| {
            this.child(render_runtime_sql_text_dialog(
                analysis_id,
                dialog,
                sql_dialog_focus_handle.clone(),
                state.sql_dialog_scroll.clone(),
                theme,
                cx,
            ))
        })
        .into_any_element()
}

/// 渲染 Runtime 结果类型切换控件。
fn render_result_type_selector(
    analysis_id: usize,
    state: &RuntimeAnalysisState,
    theme: &AppTheme,
    cx: &mut Context<ArgusApp>,
) -> impl IntoElement + use<> {
    div()
        .px(px(RUNTIME_VIEW_PADDING))
        .py(px(8.0))
        .flex()
        .items_center()
        .gap_1()
        .border_b_1()
        .border_color(rgb(theme.border))
        .child(render_result_type_button(
            analysis_id,
            state.result_type,
            RuntimeAnalysisResultType::Statistics,
            "统计分析",
            theme,
            cx,
        ))
        .child(render_result_type_button(
            analysis_id,
            state.result_type,
            RuntimeAnalysisResultType::SqlFrequency,
            "SQL频率分析",
            theme,
            cx,
        ))
        .child(render_result_type_button(
            analysis_id,
            state.result_type,
            RuntimeAnalysisResultType::SlowSql,
            "慢SQL分析",
            theme,
            cx,
        ))
}

/// 渲染单个 Runtime 结果类型按钮。
fn render_result_type_button(
    analysis_id: usize,
    current_type: RuntimeAnalysisResultType,
    target_type: RuntimeAnalysisResultType,
    label: &'static str,
    theme: &AppTheme,
    cx: &mut Context<ArgusApp>,
) -> impl IntoElement + use<> {
    let is_active = current_type == target_type;
    let id = match target_type {
        RuntimeAnalysisResultType::Statistics => "runtime-result-type-statistics",
        RuntimeAnalysisResultType::SqlFrequency => "runtime-result-type-sql-frequency",
        RuntimeAnalysisResultType::SlowSql => "runtime-result-type-slow-sql",
    };
    div()
        .id(id)
        .h(px(26.0))
        .px_3()
        .flex()
        .items_center()
        .rounded_sm()
        .cursor_pointer()
        .text_size(px(12.0))
        .line_height(px(18.0))
        .text_color(rgb(if is_active {
            theme.foreground
        } else {
            theme.foreground_muted
        }))
        .bg(rgb(if is_active {
            theme.current_line
        } else {
            theme.content
        }))
        .hover(|this| this.bg(rgb(theme.current_line)))
        .child(label)
        .on_click(cx.listener(move |app, _, _, cx| {
            app.set_runtime_result_type(analysis_id, target_type, Some(cx));
            cx.notify();
        }))
}

/// 渲染 Runtime SQL 完整文本弹窗。
fn render_table_scrollbars(
    analysis_id: usize,
    table: RuntimeScrollbarTable,
    scroll_handle: &UniformListScrollHandle,
    row_count: usize,
    row_height: f32,
    theme: &AppTheme,
    cx: &mut Context<ArgusApp>,
) -> Vec<AnyElement> {
    let scroll_state = scroll_handle.0.borrow();
    let bounds = scroll_state.base_handle.bounds();
    let scroll_offset = scroll_state.base_handle.offset();
    let base_handle = scroll_state.base_handle.clone();
    drop(scroll_state);

    let mut scrollbars = Vec::new();
    // 固定行高表格不能使用 uniform_list 的 last_item_size 推算内容高度：
    // 它会随虚拟列表滚动测量范围变化，导致滚动条滑块长度动态变化。
    let content_height = runtime_fixed_table_content_height(row_count, row_height);
    // Runtime 表格不再暴露整体横向滚动；长字段由单元格内部横向滚动承载。
    if let Some(metrics) = scrollbar_metrics(
        bounds.size.height,
        content_height,
        -scroll_offset.y,
        RUNTIME_SCROLLBAR_PADDING,
        RUNTIME_SCROLLBAR_MIN_THUMB,
    ) {
        scrollbars.push(render_runtime_scrollbar_thumb(
            analysis_id,
            table,
            RuntimeScrollTarget::Uniform(base_handle),
            metrics,
            px(TABLE_HEADER_HEIGHT),
            bounds,
            theme,
            cx,
        ));
    }

    scrollbars
}

/// 计算固定行高 Runtime 表格的稳定内容高度。
fn runtime_fixed_table_content_height(row_count: usize, row_height: f32) -> gpui::Pixels {
    px(row_count as f32 * row_height)
}

/// Runtime 滚动目标，封装固定行高表格使用的基础滚动句柄。
#[derive(Clone)]
enum RuntimeScrollTarget {
    /// `uniform_list` 使用的基础滚动句柄。
    Uniform(ScrollHandle),
}

/// 绘制 Runtime 可拖拽滚动条滑块。
fn render_runtime_scrollbar_thumb(
    analysis_id: usize,
    table: RuntimeScrollbarTable,
    target: RuntimeScrollTarget,
    metrics: ScrollbarMetrics,
    track_top_offset: Pixels,
    viewport_bounds: gpui::Bounds<Pixels>,
    theme: &AppTheme,
    cx: &mut Context<ArgusApp>,
) -> AnyElement {
    let entity = cx.entity();
    div()
        .absolute()
        .top(track_top_offset + metrics.thumb_start)
        .right(px(RUNTIME_SCROLLBAR_PADDING))
        .w(px(RUNTIME_SCROLLBAR_THUMB_SIZE))
        .h(metrics.thumb_length)
        .rounded_lg()
        .bg(rgb(theme.foreground_muted))
        .opacity(0.48)
        .hover(|this| this.opacity(0.78))
        .cursor_pointer()
        .occlude()
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

                            let cursor_offset = event.position.y - thumb_bounds.top();
                            entity.update(cx, |app, _| {
                                if let Some(state) = app.runtime_analysis_state_mut(analysis_id) {
                                    state.scrollbar_drag = Some(RuntimeScrollbarDrag {
                                        table,
                                        cursor_offset,
                                    });
                                }
                            });
                            cx.stop_propagation();
                            cx.notify(entity.entity_id());
                        }
                    });

                    window.on_mouse_event({
                        let entity = entity.clone();
                        move |event: &MouseUpEvent, phase, _, cx| {
                            if !phase.bubble() || event.button != MouseButton::Left {
                                return;
                            }

                            let handled = entity.update(cx, |app, _| {
                                let Some(state) = app.runtime_analysis_state_mut(analysis_id)
                                else {
                                    return false;
                                };
                                let handled =
                                    state.scrollbar_drag.is_some_and(|drag| drag.table == table);
                                if handled {
                                    state.scrollbar_drag = None;
                                }
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

                        let handled = entity.update(cx, |app, _| {
                            let Some(state) = app.runtime_analysis_state(analysis_id) else {
                                return false;
                            };
                            let Some(drag) = state.scrollbar_drag else {
                                return false;
                            };
                            if drag.table != table {
                                return false;
                            }

                            let pointer = event.position.y - viewport_bounds.top();
                            let scroll =
                                scrollbar_scroll_for_drag(pointer, drag.cursor_offset, &metrics);
                            match &target {
                                RuntimeScrollTarget::Uniform(scroll_handle) => {
                                    let current = scroll_handle.offset();
                                    scroll_handle.set_offset(point(current.x, -scroll));
                                }
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

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::sync::Arc;

    use gpui::UniformListScrollHandle;

    use crate::analysis::runtime::{
        build_runtime_analysis_result, build_runtime_slow_sql_rows_for_filter,
        build_runtime_sql_frequency_rows_for_filter, parse_runtime_request_text,
    };
    use crate::app::{
        RuntimeAnalysisResultType, RuntimeAnalysisTaskState, RuntimeAnalysisView,
        RuntimeSlowSqlRowsCache, RuntimeSortDirection, RuntimeSqlFrequencyRowsCache,
        RuntimeSqlSortKey, RuntimeSummarySortKey,
    };
    use crate::loader::SourceId;

    use super::*;

    /// 构造 Runtime 过滤测试用的默认 UI 状态。
    fn runtime_filter_test_state() -> RuntimeAnalysisState {
        RuntimeAnalysisState {
            id: 1,
            title: "Runtime分析".to_string(),
            targets: Vec::new(),
            generation: 1,
            view: RuntimeAnalysisView::Summary,
            result_type: RuntimeAnalysisResultType::Statistics,
            summary_sort_key: RuntimeSummarySortKey::RequestCount,
            summary_sort_direction: RuntimeSortDirection::Descending,
            request_sort_key: crate::app::RuntimeRequestSortKey::RequestTime,
            request_sort_direction: RuntimeSortDirection::Descending,
            sql_sort_key: RuntimeSqlSortKey::ExecuteDuration,
            sql_sort_direction: RuntimeSortDirection::Descending,
            filter_keyword_input: SettingsTextInputState::default(),
            filter_username_input: SettingsTextInputState::default(),
            filter_start_time_input: SettingsTextInputState::default(),
            filter_end_time_input: SettingsTextInputState::default(),
            applied_filter_keyword: String::new(),
            applied_filter_username: String::new(),
            applied_filter_start_time: String::new(),
            applied_filter_end_time: String::new(),
            filter_input_generation: 0,
            filter_task_generation: 0,
            is_filter_pending: false,
            is_filter_computing: false,
            open_time_picker: None,
            cell_selection: None,
            cell_selection_drag: None,
            hovered_sql_cell: None,
            sql_text_dialog: None,
            summary_scroll: UniformListScrollHandle::new(),
            request_scroll: UniformListScrollHandle::new(),
            sql_scroll: UniformListScrollHandle::new(),
            sql_frequency_scroll: UniformListScrollHandle::new(),
            sql_frequency_detail_scroll: UniformListScrollHandle::new(),
            slow_sql_scroll: UniformListScrollHandle::new(),
            sql_frequency_detail_sql: None,
            slow_sql_detail_sql: None,
            runtime_filter_rows_cache: None,
            sql_frequency_rows_task_generation: 0,
            slow_sql_rows_task_generation: 0,
            is_sql_frequency_rows_computing: false,
            is_slow_sql_rows_computing: false,
            sql_frequency_rows_computing_filter: None,
            slow_sql_rows_computing_filter: None,
            sql_frequency_rows_cache: RefCell::new(None),
            sql_frequency_detail_rows_cache: RefCell::new(None),
            slow_sql_rows_cache: RefCell::new(None),
            summary_rows_cache: RefCell::new(None),
            request_indices_cache: RefCell::new(None),
            sql_indices_cache: RefCell::new(None),
            scrollbar_drag: None,
            sql_dialog_scroll: ScrollHandle::new(),
            task_state: RuntimeAnalysisTaskState::Loading {
                message: String::new(),
            },
        }
    }

    /// 测试中模拟防抖和后台过滤已经完成，把输入框值同步到已应用过滤值。
    fn apply_runtime_filter_inputs_for_test(state: &mut RuntimeAnalysisState) {
        state.applied_filter_keyword = state.filter_keyword_input.value.clone();
        state.applied_filter_username = state.filter_username_input.value.clone();
        state.applied_filter_start_time = state.filter_start_time_input.value.clone();
        state.applied_filter_end_time = state.filter_end_time_input.value.clone();
        state.is_filter_pending = false;
        state.is_filter_computing = false;
        state.runtime_filter_rows_cache = None;
        state.sql_frequency_rows_cache.borrow_mut().take();
        state.sql_frequency_detail_rows_cache.borrow_mut().take();
        state.slow_sql_rows_cache.borrow_mut().take();
    }

    /// 构造 Runtime 过滤测试结果，并补齐稳定请求索引。
    fn runtime_filter_test_result() -> RuntimeAnalysisResult {
        let mut requests = vec![
            parse_runtime_request_text(
                SourceId(1),
                "100&alice&_api_one&1000&0&0.log",
                "/tmp/one-a.log",
                "10 0 0 0 0 select * from users",
            )
            .expect("应能解析 Runtime 测试日志"),
            parse_runtime_request_text(
                SourceId(2),
                "200&alice&_api_one&2000&0&0.log",
                "/tmp/one-b.log",
                "20 0 0 0 0 select * from orders",
            )
            .expect("应能解析 Runtime 测试日志"),
            parse_runtime_request_text(
                SourceId(3),
                "300&bob&_api_one&2000&0&0.log",
                "/tmp/one-c.log",
                "30 0 0 0 0 select * from orders",
            )
            .expect("应能解析 Runtime 测试日志"),
        ];
        for (index, request) in requests.iter_mut().enumerate() {
            request.index = index;
        }
        build_runtime_analysis_result(requests, Vec::new(), 3)
    }

    /// 构造 SQL 频率和慢 SQL 分析测试结果。
    fn runtime_sql_analysis_test_result() -> RuntimeAnalysisResult {
        let mut requests = vec![
            parse_runtime_request_text(
                SourceId(1),
                "100&alice&_api_sql&1000&0&0.log",
                "/tmp/sql-a.log",
                "10 0 0 0 0 select * from users where id = 1",
            )
            .expect("应能解析第一条 SQL 测试日志"),
            parse_runtime_request_text(
                SourceId(2),
                "100&alice&_api_sql&2000&0&0.log",
                "/tmp/sql-b.log",
                "30 0 0 0 0 select * from users where id = 2",
            )
            .expect("应能解析第二条 SQL 测试日志"),
            parse_runtime_request_text(
                SourceId(3),
                "100&bob&_api_sql&3000&0&0.log",
                "/tmp/sql-c.log",
                "50 0 0 0 0 select * from orders where status = 'PAID'",
            )
            .expect("应能解析第三条 SQL 测试日志"),
        ];
        for (index, request) in requests.iter_mut().enumerate() {
            request.index = index;
        }
        build_runtime_analysis_result(requests, Vec::new(), 3)
    }

    /// 验证用户名和时间区间过滤会跨表格影响总览聚合统计。
    #[test]
    fn runtime_filters_summary_by_username_and_time_range() {
        let result = runtime_filter_test_result();
        let mut state = runtime_filter_test_state();
        state.filter_username_input.value = "alice".to_string();
        state.filter_start_time_input.value = "1500".to_string();
        state.filter_end_time_input.value = "2500".to_string();
        apply_runtime_filter_inputs_for_test(&mut state);

        let rows = sorted_summary_rows(&result, &state);

        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].request_path, "/api/one");
        assert_eq!(rows[0].request_count, 1);
        assert_eq!(rows[0].average_duration_ms, 200.0);
        assert_eq!(rows[0].request_indices, vec![1]);
    }

    /// 验证用户名过滤支持逗号分隔的多个模糊匹配关键字。
    #[test]
    fn runtime_filters_support_multiple_usernames() {
        let result = runtime_filter_test_result();
        let mut state = runtime_filter_test_state();
        state.filter_username_input.value = " bob, carol ".to_string();
        apply_runtime_filter_inputs_for_test(&mut state);

        let rows = sorted_summary_rows(&result, &state);

        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].request_count, 1);
        assert_eq!(rows[0].request_indices, vec![2]);
    }

    /// 验证任意关键字过滤可以命中 SQL 文本，并同步过滤 SQL 明细列表。
    #[test]
    fn runtime_keyword_filter_matches_sql_text() {
        let result = runtime_filter_test_result();
        let mut state = runtime_filter_test_state();
        state.filter_keyword_input.value = "orders".to_string();
        apply_runtime_filter_inputs_for_test(&mut state);

        let rows = sorted_summary_rows(&result, &state);
        let sql_indices = sorted_sql_indices(&result.requests[1], &state);

        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].request_count, 2);
        assert_eq!(sql_indices, vec![0]);
    }

    /// 验证 SQL 频率分析会按归一化结构聚合，并按执行次数降序展示。
    #[test]
    fn runtime_sql_frequency_groups_by_normalized_sql() {
        let result = runtime_sql_analysis_test_result();
        let state = runtime_filter_test_state();
        let filter = runtime_sql_analysis_filter_snapshot(&state);

        let rows = build_runtime_sql_frequency_rows_for_filter(&result, &filter);

        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].normalized_sql, "select * from users where id = ?");
        assert_eq!(rows[0].execute_count, 2);
        assert_eq!(rows[0].average_execute_ms(), 20.0);
        assert_eq!(rows[1].execute_count, 1);
    }

    /// 验证慢 SQL 分析按归一化 SQL 聚合，并按平均执行耗时从高到低展示。
    #[test]
    fn runtime_slow_sql_rows_group_by_normalized_sql_and_sort_by_average_duration() {
        let result = runtime_sql_analysis_test_result();
        let state = runtime_filter_test_state();
        let filter = runtime_sql_analysis_filter_snapshot(&state);

        let rows = build_runtime_slow_sql_rows_for_filter(&result, &filter);

        assert_eq!(rows.len(), 2);
        assert!(rows[0].normalized_sql.contains("orders"));
        assert_eq!(rows[0].average_execute_ms(), 50.0);
        assert_eq!(rows[0].execute_count, 1);
        assert!(rows[1].normalized_sql.contains("users"));
        assert_eq!(rows[1].average_execute_ms(), 20.0);
        assert_eq!(rows[1].execute_count, 2);
    }

    /// 验证现有用户名和关键字过滤会同时作用于新增 SQL 分析结果。
    #[test]
    fn runtime_sql_analysis_rows_share_existing_filters() {
        let result = runtime_sql_analysis_test_result();
        let mut state = runtime_filter_test_state();
        state.filter_username_input.value = "alice".to_string();
        apply_runtime_filter_inputs_for_test(&mut state);
        let filter = runtime_sql_analysis_filter_snapshot(&state);

        let frequency_rows = build_runtime_sql_frequency_rows_for_filter(&result, &filter);
        let slow_rows = build_runtime_slow_sql_rows_for_filter(&result, &filter);

        assert_eq!(frequency_rows.len(), 1);
        assert_eq!(frequency_rows[0].execute_count, 2);
        assert_eq!(slow_rows.len(), 1);
        assert_eq!(slow_rows[0].execute_count, 2);
        assert_eq!(slow_rows[0].average_execute_ms(), 20.0);

        state.filter_keyword_input.value = "id = 2".to_string();
        apply_runtime_filter_inputs_for_test(&mut state);
        let filter = runtime_sql_analysis_filter_snapshot(&state);
        let frequency_rows = build_runtime_sql_frequency_rows_for_filter(&result, &filter);
        let slow_rows = build_runtime_slow_sql_rows_for_filter(&result, &filter);

        assert_eq!(frequency_rows.len(), 1);
        assert_eq!(frequency_rows[0].execute_count, 1);
        assert_eq!(slow_rows.len(), 1);
        assert_eq!(slow_rows[0].execute_count, 1);
        assert_eq!(slow_rows[0].average_execute_ms(), 30.0);
    }

    /// 验证 SQL 频率详情会列出同一 SQL 结构下的每次执行记录。
    #[test]
    fn runtime_sql_frequency_detail_lists_each_execution() {
        let result = runtime_sql_analysis_test_result();
        let state = runtime_filter_test_state();

        let rows = sql_frequency_detail_rows(&result, &state, "select * from users where id = ?");

        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].execute_ms, 30);
        assert_eq!(rows[1].execute_ms, 10);
        assert_eq!(
            result.requests[rows[0].request_index].request_path,
            "/api/sql"
        );
        assert!(
            result.requests[rows[0].request_index].sql_records[rows[0].sql_index]
                .sql_text
                .contains("id = 2")
        );
    }

    /// 验证 SQL 频率详情继续沿用 Runtime 过滤条件。
    #[test]
    fn runtime_sql_frequency_detail_shares_existing_filters() {
        let result = runtime_sql_analysis_test_result();
        let mut state = runtime_filter_test_state();
        state.filter_keyword_input.value = "id = 2".to_string();
        apply_runtime_filter_inputs_for_test(&mut state);

        let rows = sql_frequency_detail_rows(&result, &state, "select * from users where id = ?");

        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].execute_ms, 30);
    }

    /// 验证 SQL 频率详情在过滤条件不变时复用缓存，避免详情滚动时重复扫描。
    #[test]
    fn runtime_sql_frequency_detail_reuses_filtered_cache() {
        let result = runtime_sql_analysis_test_result();
        let mut state = runtime_filter_test_state();
        state.filter_keyword_input.value = "users".to_string();
        apply_runtime_filter_inputs_for_test(&mut state);

        let first = sql_frequency_detail_rows(&result, &state, "select * from users where id = ?");
        let second = sql_frequency_detail_rows(&result, &state, "select * from users where id = ?");

        assert!(Arc::ptr_eq(&first, &second));
    }

    /// 验证 SQL 频率和慢 SQL 分析在懒计算完成前不做同步全量计算。
    #[test]
    fn runtime_sql_analysis_waits_for_lazy_rows_cache() {
        let result = runtime_sql_analysis_test_result();
        let state = runtime_filter_test_state();

        let frequency_rows = sql_frequency_rows(&result, &state);
        let slow_rows = slow_sql_rows(&result, &state);

        assert!(frequency_rows.is_empty());
        assert!(slow_rows.is_empty());
    }

    /// 验证过滤条件不变时 SQL 分析结果复用缓存，避免滚动重绘重复全量计算。
    #[test]
    fn runtime_sql_analysis_reuses_filtered_cache() {
        let result = runtime_sql_analysis_test_result();
        let mut state = runtime_filter_test_state();
        state.filter_keyword_input.value = "users".to_string();
        apply_runtime_filter_inputs_for_test(&mut state);
        let filter = runtime_sql_analysis_filter_snapshot(&state);
        let frequency_rows = Arc::new(build_runtime_sql_frequency_rows_for_filter(
            &result, &filter,
        ));
        let slow_rows = Arc::new(build_runtime_slow_sql_rows_for_filter(&result, &filter));
        state
            .sql_frequency_rows_cache
            .borrow_mut()
            .replace(RuntimeSqlFrequencyRowsCache {
                filter: filter.clone(),
                rows: frequency_rows,
            });
        state
            .slow_sql_rows_cache
            .borrow_mut()
            .replace(RuntimeSlowSqlRowsCache {
                filter,
                rows: slow_rows,
            });

        let first_frequency_rows = sql_frequency_rows(&result, &state);
        let second_frequency_rows = sql_frequency_rows(&result, &state);
        let first_slow_rows = slow_sql_rows(&result, &state);
        let second_slow_rows = slow_sql_rows(&result, &state);

        assert!(Arc::ptr_eq(&first_frequency_rows, &second_frequency_rows));
        assert!(Arc::ptr_eq(&first_slow_rows, &second_slow_rows));
    }

    /// 验证多行 SQL 在表格单元格展示前会折叠为单行，避免 GPUI 单行文本布局 panic。
    #[test]
    fn runtime_cell_display_text_flattens_multiline_sql() {
        let display_text = runtime_cell_display_text("select *\r\n  from table_a\n\nwhere id = 1");

        assert_eq!(display_text, "select * from table_a where id = 1");
        assert!(!display_text.contains('\n'));
        assert!(!display_text.contains('\r'));
    }

    /// 验证完整 SQL 弹窗拆行时保留空行和行首缩进。
    #[test]
    fn runtime_sql_dialog_lines_preserve_original_layout() {
        let lines = runtime_sql_dialog_lines("select *\r\n  from table_a\n\nwhere id = 1");

        assert_eq!(
            lines,
            vec![
                "select *".to_string(),
                "  from table_a".to_string(),
                String::new(),
                "where id = 1".to_string(),
            ]
        );
    }

    /// 验证时间选择器展示值与过滤输入框解析口径保持一致。
    #[test]
    fn runtime_datetime_picker_value_reads_filter_input() {
        let input = SettingsTextInputState::from_value("2026-06-25 14:25:03".to_string());

        let value = runtime_datetime_picker_value(&input, false);

        assert_eq!(value.year, 2026);
        assert_eq!(value.month, 6);
        assert_eq!(value.day, 25);
        assert_eq!(value.hour, 14);
        assert_eq!(value.minute, 25);
        assert_eq!(value.second, 3);
    }

    /// 验证开始和结束时间选择器使用页面级稳定定位，避免浮层和输入框内部布局耦合。
    #[test]
    fn runtime_time_picker_positions_match_filter_inputs() {
        let start_left = runtime_time_picker_left(RuntimeFilterInputKind::StartTime);
        let end_left = runtime_time_picker_left(RuntimeFilterInputKind::EndTime);

        assert_eq!(
            start_left,
            RUNTIME_VIEW_PADDING
                + RUNTIME_FILTER_KEYWORD_WIDTH
                + RUNTIME_FILTER_GAP
                + RUNTIME_FILTER_USERNAME_WIDTH
                + RUNTIME_FILTER_GAP
        );
        assert_eq!(
            end_left,
            start_left + RUNTIME_FILTER_TIME_WIDTH + RUNTIME_FILTER_GAP
        );
    }

    /// 验证固定行高表格内容高度只由行数决定，避免滚动过程中滑块长度漂移。
    #[test]
    fn runtime_fixed_table_content_height_is_stable() {
        let height = runtime_fixed_table_content_height(42, TABLE_ROW_HEIGHT);

        assert_eq!(height, px(42.0 * TABLE_ROW_HEIGHT));
    }

    /// 验证同一内容高度下，滚动条滑块长度不会因为向下滚动而变化。
    #[test]
    fn runtime_scrollbar_thumb_length_does_not_change_with_offset() {
        let top = scrollbar_metrics(
            px(400.0),
            px(1200.0),
            px(0.0),
            RUNTIME_SCROLLBAR_PADDING,
            RUNTIME_SCROLLBAR_MIN_THUMB,
        )
        .expect("应生成顶部滚动条指标");
        let middle = scrollbar_metrics(
            px(400.0),
            px(1200.0),
            px(360.0),
            RUNTIME_SCROLLBAR_PADDING,
            RUNTIME_SCROLLBAR_MIN_THUMB,
        )
        .expect("应生成中部滚动条指标");

        assert_eq!(top.thumb_length, middle.thumb_length);
        assert!(middle.thumb_start > top.thumb_start);
    }

    /// 验证 Runtime 滚动条拖拽会按轨道比例换算为目标滚动距离。
    #[test]
    fn runtime_scrollbar_drag_converts_pointer_to_scroll() {
        let metrics = scrollbar_metrics(
            px(400.0),
            px(1200.0),
            px(0.0),
            RUNTIME_SCROLLBAR_PADDING,
            RUNTIME_SCROLLBAR_MIN_THUMB,
        )
        .expect("应生成滚动条指标");
        let scroll = scrollbar_scroll_for_drag(metrics.track_start + px(40.0), px(10.0), &metrics);

        assert!(scroll > px(0.0));
        assert!(scroll < metrics.max_scroll);
    }
}
