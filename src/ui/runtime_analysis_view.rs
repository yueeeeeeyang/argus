//! 文件职责：渲染 Runtime 请求日志分析页签内容。
//! 创建日期：2026-06-25
//! 修改日期：2026-06-25
//! 作者：Argus 开发团队
//! 主要功能：展示 Runtime 请求总览、请求详情和 SQL 明细三层可排序表格。

use std::cmp::Ordering;
use std::ops::Range;
use std::sync::Arc;

use chrono::{Datelike, Local, TimeZone, Timelike};

use crate::app::{
    AppTextInputTarget, ArgusApp, RUNTIME_SQL_COLLAPSED_ROW_HEIGHT, RuntimeAnalysisResultType,
    RuntimeAnalysisState, RuntimeAnalysisTaskState, RuntimeAnalysisView, RuntimeFilterInputKind,
    RuntimeRequestSortKey, RuntimeScrollbarDrag, RuntimeScrollbarTable, RuntimeSortDirection,
    RuntimeSqlAnalysisFilterSnapshot, RuntimeSqlCellKey, RuntimeSqlFrequencyDetailRowsCache,
    RuntimeSqlSortKey, RuntimeSqlTextDialog, RuntimeSqlTextSelection, RuntimeSummarySortKey,
    RuntimeTableCellSelection, SettingsTextInputState,
};
use crate::fonts::{ARGUS_LOG_FONT_FAMILY, ARGUS_UI_FONT_FAMILY};
use crate::runtime_analysis::{
    RuntimeAnalysisFilterCriteria, RuntimeAnalysisResult, RuntimeRequestRecord,
    RuntimeRequestSummary, RuntimeSlowSqlSummaryRow, RuntimeSqlFrequencyAnalysisRow,
    RuntimeSqlFrequencyDetailRow, RuntimeSqlRecord, filtered_runtime_summary_from_indices,
    parse_runtime_analysis_filter_criteria, parse_runtime_filter_time_value,
    runtime_request_matches_cross_filters as domain_runtime_request_matches_cross_filters,
    runtime_request_matches_keyword as domain_runtime_request_matches_keyword,
    runtime_sql_matches_keyword as domain_runtime_sql_matches_keyword,
    sort_runtime_sql_frequency_detail_rows,
};
use crate::text_selection::{
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
fn render_runtime_sql_text_dialog(
    analysis_id: usize,
    dialog: RuntimeSqlTextDialog,
    analysis_focus_handle: Option<FocusHandle>,
    theme: &AppTheme,
    cx: &mut Context<ArgusApp>,
) -> AnyElement {
    let content = div()
        .size_full()
        .flex()
        .flex_col()
        .bg(rgb(theme.side_bar))
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(move |app, _, _, cx| {
                cx.stop_propagation();
                if app.clear_runtime_sql_text_selection(analysis_id) {
                    cx.notify();
                }
            }),
        )
        .child(
            div()
                .h(px(46.0))
                .px_3()
                .flex()
                .items_center()
                .justify_between()
                .border_b_1()
                .border_color(rgb(theme.border))
                .child(
                    div()
                        .min_w(px(0.0))
                        .flex()
                        .items_center()
                        .gap_2()
                        .child(render_icon(
                            ArgusIcon::Database,
                            theme.foreground_muted,
                            15.0,
                        ))
                        .child(
                            div()
                                .min_w(px(0.0))
                                .flex()
                                .flex_col()
                                .child(
                                    div()
                                        .text_size(px(13.0))
                                        .font_weight(FontWeight::SEMIBOLD)
                                        .truncate()
                                        .child("完整 SQL"),
                                )
                                .child(
                                    div()
                                        .text_size(px(11.0))
                                        .text_color(rgb(theme.foreground_muted))
                                        .truncate()
                                        .child(dialog.request_path.clone()),
                                ),
                        ),
                )
                .child(render_sql_dialog_close_button(analysis_id, theme, cx)),
        )
        .child(
            div()
                .px_3()
                .py_2()
                .flex()
                .items_center()
                .gap_2()
                .text_size(px(11.0))
                .text_color(rgb(theme.foreground_muted))
                .border_b_1()
                .border_color(rgb(theme.border))
                .child(dialog.request_time_label.clone())
                .child("·")
                .child(display_username(&dialog.username)),
        )
        .child(render_sql_dialog_code_block(
            analysis_id,
            &dialog.sql_text,
            dialog.selection.as_ref(),
            analysis_focus_handle,
            theme,
            cx,
        ));

    render_modal_dialog(
        ModalDialog {
            overlay_id: "runtime-sql-text-dialog-overlay",
            container_id: "runtime-sql-text-dialog-container",
            width: RUNTIME_SQL_DIALOG_WIDTH,
            height: RUNTIME_SQL_DIALOG_HEIGHT,
            content: content.into_any_element(),
        },
        theme.clone(),
        cx,
    )
    .into_any_element()
}

/// 渲染 SQL 弹窗右上角关闭按钮。
fn render_sql_dialog_close_button(
    analysis_id: usize,
    theme: &AppTheme,
    cx: &mut Context<ArgusApp>,
) -> impl IntoElement + use<> {
    div()
        .id("runtime-sql-text-dialog-close")
        .size(px(28.0))
        .flex()
        .items_center()
        .justify_center()
        .rounded_sm()
        .cursor_pointer()
        .hover(|this| this.bg(rgb(theme.current_line)))
        .child(render_icon(ArgusIcon::Close, theme.foreground_muted, 15.0))
        .on_click(cx.listener(move |app, _, _, cx| {
            cx.stop_propagation();
            if app.close_runtime_sql_text_dialog(analysis_id) {
                cx.notify();
            }
        }))
}

/// 渲染完整 SQL 代码块，按原始换行拆行以避开 GPUI 单文本节点换行限制。
fn render_sql_dialog_code_block(
    analysis_id: usize,
    sql_text: &str,
    selection: Option<&RuntimeSqlTextSelection>,
    analysis_focus_handle: Option<FocusHandle>,
    theme: &AppTheme,
    cx: &mut Context<ArgusApp>,
) -> impl IntoElement {
    let lines = runtime_sql_dialog_lines(sql_text);
    div()
        .flex_1()
        .min_h(px(0.0))
        .m_3()
        .rounded_sm()
        .border_1()
        .border_color(rgb(theme.border))
        .bg(rgb(theme.content))
        .occlude()
        .child(
            div()
                .id("runtime-sql-dialog-code-scroll")
                .overflow_y_scroll()
                .scrollbar_width(px(6.0))
                .size_full()
                .occlude()
                .font_family(ARGUS_LOG_FONT_FAMILY)
                .text_size(px(12.0))
                .line_height(px(RUNTIME_SQL_DIALOG_LINE_HEIGHT))
                .text_color(rgb(theme.foreground))
                .child(
                    div()
                        .w_full()
                        .min_w(px(0.0))
                        .p_3()
                        .flex()
                        .flex_col()
                        .children(lines.into_iter().enumerate().map(|(index, line)| {
                            let selection_range = runtime_sql_dialog_selection_range_for_line(
                                selection, index, &line,
                            );
                            render_sql_dialog_line(
                                analysis_id,
                                index,
                                line,
                                selection_range,
                                analysis_focus_handle.clone(),
                                theme,
                                cx,
                            )
                            .into_any_element()
                        })),
                ),
        )
}

/// 渲染 SQL 弹窗中的一行，支持拖拽选中文本。
fn render_sql_dialog_line(
    analysis_id: usize,
    line_index: usize,
    line: String,
    selection_range: Option<Range<usize>>,
    analysis_focus_handle: Option<FocusHandle>,
    theme: &AppTheme,
    cx: &mut Context<ArgusApp>,
) -> impl IntoElement + use<> {
    div()
        .id(SharedString::from(format!(
            "runtime-sql-dialog-line-{line_index}"
        )))
        .min_h(px(RUNTIME_SQL_DIALOG_LINE_HEIGHT))
        .w_full()
        .flex_none()
        .relative()
        .flex()
        .items_center()
        .whitespace_normal()
        .line_height(px(RUNTIME_SQL_DIALOG_LINE_HEIGHT))
        .child(render_runtime_cell_text(
            line.clone(),
            selection_range,
            theme,
        ))
        .child(render_sql_dialog_line_pointer_layer(
            analysis_id,
            line_index,
            line,
            analysis_focus_handle,
            cx,
        ))
}

/// 渲染 SQL 弹窗单行透明命中层，将鼠标拖拽转换成跨行文本选区。
fn render_sql_dialog_line_pointer_layer(
    analysis_id: usize,
    line_index: usize,
    line: String,
    analysis_focus_handle: Option<FocusHandle>,
    cx: &mut Context<ArgusApp>,
) -> impl IntoElement + use<> {
    let entity = cx.entity();
    div()
        .id(SharedString::from(format!(
            "runtime-sql-dialog-line-hitbox-{line_index}"
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
                        let line = line.clone();
                        let analysis_focus_handle = analysis_focus_handle.clone();
                        move |event: &MouseDownEvent, phase, window, cx| {
                            if !phase.bubble()
                                || event.button != MouseButton::Left
                                || !visible_bounds.contains(&event.position)
                            {
                                return;
                            }

                            let character_index = runtime_sql_dialog_character_index_from_pointer(
                                &line,
                                event.position,
                                bounds,
                                window,
                            );
                            let granularity =
                                runtime_sql_dialog_granularity_for_click_count(event.click_count);
                            if let Some(focus_handle) = analysis_focus_handle.as_ref() {
                                focus_handle.focus(window);
                            }
                            entity.update(cx, |app, _| {
                                app.begin_runtime_sql_text_selection(
                                    analysis_id,
                                    line_index,
                                    line.clone(),
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
                        let line = line.clone();
                        move |event: &MouseMoveEvent, phase, window, cx| {
                            if !phase.bubble()
                                || !event.dragging()
                                || !visible_bounds.contains(&event.position)
                            {
                                return;
                            }
                            let character_index = runtime_sql_dialog_character_index_from_pointer(
                                &line,
                                event.position,
                                bounds,
                                window,
                            );
                            let handled = entity.update(cx, |app, _| {
                                app.update_runtime_sql_text_selection(
                                    analysis_id,
                                    line_index,
                                    line.clone(),
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
                                app.finish_runtime_sql_text_selection(analysis_id)
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

/// 渲染 Runtime 表格过滤栏，过滤条件会同时作用于总览、请求明细和 SQL 明细。
fn render_filter_bar(
    app: &ArgusApp,
    analysis_id: usize,
    state: &RuntimeAnalysisState,
    theme: &AppTheme,
    cx: &mut Context<ArgusApp>,
) -> impl IntoElement + use<> {
    div()
        .h(px(RUNTIME_FILTER_BAR_HEIGHT))
        .px(px(RUNTIME_VIEW_PADDING))
        .py_2()
        .flex()
        .items_center()
        .gap_2()
        .border_b_1()
        .border_color(rgb(theme.border))
        .bg(rgb(theme.content))
        .child(
            div()
                .w(px(RUNTIME_FILTER_KEYWORD_WIDTH))
                .child(render_runtime_filter_input(
                    app,
                    analysis_id,
                    RuntimeFilterInputKind::Keyword,
                    &state.filter_keyword_input,
                    "任意关键字",
                    "过滤表格内容",
                    ArgusIcon::Search,
                    false,
                    theme,
                    cx,
                )),
        )
        .child(
            div()
                .w(px(RUNTIME_FILTER_USERNAME_WIDTH))
                .child(render_runtime_filter_input(
                    app,
                    analysis_id,
                    RuntimeFilterInputKind::Username,
                    &state.filter_username_input,
                    "用户名",
                    "用户名，逗号分隔",
                    ArgusIcon::Filter,
                    false,
                    theme,
                    cx,
                )),
        )
        .child(render_runtime_time_filter_picker(
            app,
            analysis_id,
            RuntimeFilterInputKind::StartTime,
            &state.filter_start_time_input,
            "开始时间",
            "2026-06-25 00:00:00",
            theme,
            cx,
        ))
        .child(render_runtime_time_filter_picker(
            app,
            analysis_id,
            RuntimeFilterInputKind::EndTime,
            &state.filter_end_time_input,
            "结束时间",
            "2026-06-25 23:59:59",
            theme,
            cx,
        ))
        .child(
            div()
                .flex_1()
                .min_w(px(0.0))
                .text_size(px(12.0))
                .line_height(px(18.0))
                .text_color(rgb(theme.foreground_muted))
                .truncate()
                .child(runtime_filter_status_label(state)),
        )
}

/// 渲染 Runtime 时间过滤输入框和对应的日期时间选择器浮层。
fn render_runtime_time_filter_picker(
    app: &ArgusApp,
    analysis_id: usize,
    input_kind: RuntimeFilterInputKind,
    input_state: &SettingsTextInputState,
    title: &'static str,
    placeholder: &'static str,
    theme: &AppTheme,
    cx: &mut Context<ArgusApp>,
) -> impl IntoElement + use<> {
    div()
        .w(px(RUNTIME_FILTER_TIME_WIDTH))
        .relative()
        .child(render_runtime_filter_input(
            app,
            analysis_id,
            input_kind,
            input_state,
            title,
            placeholder,
            ArgusIcon::Filter,
            true,
            theme,
            cx,
        ))
}

/// 渲染 Runtime 页面级日期时间选择器，避免被下方表格内容覆盖。
fn render_runtime_time_picker_overlay(
    analysis_id: usize,
    input_kind: RuntimeFilterInputKind,
    state: &RuntimeAnalysisState,
    theme: &AppTheme,
    cx: &mut Context<ArgusApp>,
) -> AnyElement {
    let (title, input_state, is_end) = match input_kind {
        RuntimeFilterInputKind::StartTime => ("开始时间", &state.filter_start_time_input, false),
        RuntimeFilterInputKind::EndTime => ("结束时间", &state.filter_end_time_input, true),
        RuntimeFilterInputKind::Keyword | RuntimeFilterInputKind::Username => {
            return div().into_any_element();
        }
    };

    render_datetime_picker(
        analysis_id,
        input_kind,
        title,
        runtime_datetime_picker_value(input_state, is_end),
        runtime_time_picker_left(input_kind),
        RUNTIME_TIME_PICKER_TOP,
        theme,
        cx,
    )
    .into_any_element()
}

/// 返回 Runtime 时间选择器浮层左侧位置，与过滤栏输入框布局保持一致。
fn runtime_time_picker_left(input_kind: RuntimeFilterInputKind) -> f32 {
    let start_left = RUNTIME_VIEW_PADDING
        + RUNTIME_FILTER_KEYWORD_WIDTH
        + RUNTIME_FILTER_GAP
        + RUNTIME_FILTER_USERNAME_WIDTH
        + RUNTIME_FILTER_GAP;
    match input_kind {
        RuntimeFilterInputKind::StartTime => start_left,
        RuntimeFilterInputKind::EndTime => {
            start_left + RUNTIME_FILTER_TIME_WIDTH + RUNTIME_FILTER_GAP
        }
        RuntimeFilterInputKind::Keyword | RuntimeFilterInputKind::Username => start_left,
    }
}

/// 渲染 Runtime 过滤栏中的单个输入框。
fn render_runtime_filter_input(
    app: &ArgusApp,
    analysis_id: usize,
    input_kind: RuntimeFilterInputKind,
    input_state: &SettingsTextInputState,
    id_suffix: &'static str,
    placeholder: &'static str,
    icon: ArgusIcon,
    opens_time_picker: bool,
    theme: &AppTheme,
    cx: &mut Context<ArgusApp>,
) -> AnyElement {
    let focus_handle = runtime_filter_focus_handle(app, input_kind);
    let native_input = focus_handle.clone().map(|focus_handle| {
        app_native_input(
            cx.entity(),
            AppTextInputTarget::RuntimeFilter {
                analysis_id,
                input_kind,
            },
            focus_handle,
        )
    });
    let input_id = runtime_filter_input_id(input_kind);
    render_input(
        Input {
            id: input_id,
            placeholder,
            value: input_state.value.clone(),
            is_disabled: false,
            is_focused: input_state.is_focused,
            cursor_index: input_state.cursor,
            selection_range: runtime_filter_input_selection_range(input_state),
            marked_range: input_state.marked_range.clone(),
            is_pointer_selecting: input_state.selection_drag.is_some(),
            is_secret: false,
            size: InputSize::Compact,
            leading_accessory: Some(InputAccessory {
                id: runtime_filter_leading_id(input_kind),
                icon,
                tooltip: id_suffix,
            }),
            trailing_accessory: Some(InputAccessory {
                id: runtime_filter_clear_id(input_kind),
                icon: ArgusIcon::Close,
                tooltip: "清空",
            }),
            native_input,
        },
        theme,
        cx.listener(move |app, event: &KeyDownEvent, _, cx| {
            cx.stop_propagation();
            app.handle_runtime_filter_input_key(analysis_id, input_kind, &event.keystroke, cx);
            cx.notify();
        }),
        cx.listener(move |app, _, _, cx| {
            cx.stop_propagation();
            app.focus_runtime_filter_input(analysis_id, input_kind);
            if opens_time_picker {
                app.open_runtime_time_picker(analysis_id, input_kind);
            }
            cx.notify();
        }),
        cx.listener(move |app, event: &InputPointerEvent, _, cx| {
            cx.stop_propagation();
            if opens_time_picker {
                app.open_runtime_time_picker(analysis_id, input_kind);
            }
            match event.action {
                InputPointerAction::Begin => app.begin_runtime_filter_input_pointer_selection(
                    analysis_id,
                    input_kind,
                    event.character_index,
                    event.granularity,
                ),
                InputPointerAction::Extend => app.update_runtime_filter_input_pointer_selection(
                    analysis_id,
                    input_kind,
                    event.character_index,
                ),
                InputPointerAction::Finish => {
                    app.finish_runtime_filter_input_pointer_selection(analysis_id, input_kind)
                }
            }
            cx.notify();
        }),
        cx.listener(move |app, _, _, cx| {
            cx.stop_propagation();
            app.clear_runtime_filter_input(analysis_id, input_kind, Some(cx));
            cx.notify();
        }),
    )
    .into_any_element()
}

/// 渲染总览表格。
fn render_summary_table(
    analysis_id: usize,
    state: &RuntimeAnalysisState,
    result: &RuntimeAnalysisResult,
    analysis_focus_handle: Option<FocusHandle>,
    theme: &AppTheme,
    cx: &mut Context<ArgusApp>,
) -> impl IntoElement + use<> {
    let sorted_rows = Arc::new(sorted_summary_rows(result, state));
    let row_count = sorted_rows.len();
    let scroll_handle = state.summary_scroll.clone();

    div()
        .id("runtime-summary-table")
        .flex_1()
        .min_h(px(0.0))
        .relative()
        .overflow_hidden()
        .child(render_summary_header(analysis_id, state, theme, cx))
        .child(
            uniform_list(
                "runtime-summary-list",
                row_count,
                cx.processor(move |app, range: Range<usize>, _window, row_cx| {
                    let theme = app.theme.clone();
                    let Some(state) = app.runtime_analysis_state(analysis_id) else {
                        return Vec::new();
                    };
                    let RuntimeAnalysisTaskState::Ready(_) = &state.task_state else {
                        return Vec::new();
                    };
                    let Some(rows) = sorted_rows.as_slice().get(range.clone()) else {
                        return Vec::new();
                    };

                    rows.iter()
                        .enumerate()
                        .map(|(offset, summary)| {
                            render_summary_row(
                                analysis_id,
                                range.start + offset,
                                summary,
                                state.cell_selection.as_ref(),
                                analysis_focus_handle.clone(),
                                &theme,
                                row_cx,
                            )
                            .into_any_element()
                        })
                        .collect::<Vec<_>>()
                }),
            )
            .absolute()
            .top(px(TABLE_HEADER_HEIGHT))
            .left_0()
            .right_0()
            .bottom_0()
            .block_mouse_except_scroll()
            .track_scroll(scroll_handle.clone()),
        )
        .children(render_table_scrollbars(
            analysis_id,
            RuntimeScrollbarTable::Summary,
            &scroll_handle,
            row_count,
            TABLE_ROW_HEIGHT,
            theme,
            cx,
        ))
}

/// 渲染请求详情表格。
fn render_request_details_table(
    analysis_id: usize,
    state: &RuntimeAnalysisState,
    result: &RuntimeAnalysisResult,
    request_path: &str,
    analysis_focus_handle: Option<FocusHandle>,
    theme: &AppTheme,
    cx: &mut Context<ArgusApp>,
) -> AnyElement {
    let Some(summary) = filtered_summary_for_request_path(result, request_path, state) else {
        return render_empty_message("未找到当前请求地址的详情。", theme);
    };
    let sorted_indices = Arc::new(sorted_request_indices(result, &summary, state));
    let row_count = sorted_indices.len();
    let scroll_handle = state.request_scroll.clone();

    div()
        .id("runtime-request-details")
        .flex_1()
        .min_h(px(0.0))
        .flex()
        .flex_col()
        .overflow_hidden()
        .child(render_request_details_topbar(
            analysis_id,
            &summary,
            theme,
            cx,
        ))
        .child(
            div()
                .relative()
                .flex_1()
                .min_h(px(0.0))
                .overflow_hidden()
                .child(render_request_header(analysis_id, state, theme, cx))
                .child(
                    uniform_list(
                        "runtime-request-list",
                        row_count,
                        cx.processor(move |app, range: Range<usize>, _window, row_cx| {
                            let theme = app.theme.clone();
                            let Some(state) = app.runtime_analysis_state(analysis_id) else {
                                return Vec::new();
                            };
                            let RuntimeAnalysisTaskState::Ready(result) = &state.task_state else {
                                return Vec::new();
                            };
                            let Some(request_indices) = sorted_indices.as_slice().get(range) else {
                                return Vec::new();
                            };

                            request_indices
                                .iter()
                                .filter_map(|request_index| {
                                    result.requests.get(*request_index).map(|request| {
                                        render_request_row(
                                            analysis_id,
                                            request,
                                            state.cell_selection.as_ref(),
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
                    .absolute()
                    .top(px(TABLE_HEADER_HEIGHT))
                    .left_0()
                    .right_0()
                    .bottom_0()
                    .block_mouse_except_scroll()
                    .track_scroll(scroll_handle.clone()),
                )
                .children(render_table_scrollbars(
                    analysis_id,
                    RuntimeScrollbarTable::Request,
                    &scroll_handle,
                    row_count,
                    TABLE_ROW_HEIGHT,
                    theme,
                    cx,
                )),
        )
        .into_any_element()
}

/// 渲染 SQL 明细表格。
fn render_sql_table(
    analysis_id: usize,
    state: &RuntimeAnalysisState,
    request_path: &str,
    request: &RuntimeRequestRecord,
    analysis_focus_handle: Option<FocusHandle>,
    theme: &AppTheme,
    cx: &mut Context<ArgusApp>,
) -> impl IntoElement + use<> {
    let sorted_indices_vec = sorted_sql_indices(request, state);
    let row_count = sorted_indices_vec.len();
    let sorted_indices = Arc::new(sorted_indices_vec);
    let uniform_scroll = state.sql_scroll.clone();
    let request_index = request.index;
    let table_body = div()
        .relative()
        .flex_1()
        .min_h(px(0.0))
        .overflow_hidden()
        .child(render_sql_header(analysis_id, state, theme, cx))
        .child(
            uniform_list(
                "runtime-sql-uniform-list",
                row_count,
                cx.processor(move |app, range: Range<usize>, _window, row_cx| {
                    let theme = app.theme.clone();
                    let Some(state) = app.runtime_analysis_state(analysis_id) else {
                        return Vec::new();
                    };
                    let RuntimeAnalysisTaskState::Ready(result) = &state.task_state else {
                        return Vec::new();
                    };
                    let Some(request) = result.requests.get(request_index) else {
                        return Vec::new();
                    };
                    let Some(sql_indices) = sorted_indices.as_slice().get(range) else {
                        return Vec::new();
                    };

                    sql_indices
                        .iter()
                        .filter_map(|sql_index| {
                            request.sql_records.get(*sql_index).map(|sql| {
                                render_sql_row(
                                    analysis_id,
                                    request.index,
                                    request.request_path.clone(),
                                    request.request_time_label.clone(),
                                    request.username.clone(),
                                    *sql_index,
                                    sql,
                                    state.hovered_sql_cell,
                                    state.cell_selection.as_ref(),
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
            .absolute()
            .top(px(TABLE_HEADER_HEIGHT))
            .left_0()
            .right_0()
            .bottom_0()
            .block_mouse_except_scroll()
            .track_scroll(uniform_scroll.clone()),
        )
        .children(render_table_scrollbars(
            analysis_id,
            RuntimeScrollbarTable::Sql,
            &uniform_scroll,
            row_count,
            SQL_ROW_HEIGHT,
            theme,
            cx,
        ))
        .into_any_element();

    div()
        .id("runtime-sql-list")
        .flex_1()
        .min_h(px(0.0))
        .flex()
        .flex_col()
        .overflow_hidden()
        .child(render_sql_topbar(
            analysis_id,
            request_path,
            request,
            theme,
            cx,
        ))
        .child(table_body)
        .when(row_count == 0, |this| {
            this.child(render_empty_message("当前请求没有 SQL 明细。", theme))
        })
}

/// 渲染 SQL 频率分析表格。
fn render_sql_frequency_table(
    analysis_id: usize,
    state: &RuntimeAnalysisState,
    result: &RuntimeAnalysisResult,
    analysis_focus_handle: Option<FocusHandle>,
    theme: &AppTheme,
    cx: &mut Context<ArgusApp>,
) -> impl IntoElement + use<> {
    let rows = sql_frequency_rows(result, state);
    let row_count = rows.len();
    let scroll_handle = state.sql_frequency_scroll.clone();
    let empty_message = if state.is_sql_frequency_rows_computing {
        "正在计算 SQL 频率分析..."
    } else {
        "当前过滤条件下没有 SQL 频率数据。"
    };

    div()
        .id("runtime-sql-frequency-table")
        .flex_1()
        .min_h(px(0.0))
        .relative()
        .overflow_hidden()
        .child(render_sql_frequency_header(theme))
        .child(
            uniform_list(
                "runtime-sql-frequency-list",
                row_count,
                cx.processor(move |app, range: Range<usize>, _window, row_cx| {
                    let theme = app.theme.clone();
                    let Some(state) = app.runtime_analysis_state(analysis_id) else {
                        return Vec::new();
                    };
                    let Some(rows) = rows.as_slice().get(range.clone()) else {
                        return Vec::new();
                    };

                    rows.iter()
                        .enumerate()
                        .map(|(offset, row)| {
                            render_sql_frequency_row(
                                analysis_id,
                                range.start + offset,
                                row,
                                state.cell_selection.as_ref(),
                                analysis_focus_handle.clone(),
                                &theme,
                                row_cx,
                            )
                            .into_any_element()
                        })
                        .collect::<Vec<_>>()
                }),
            )
            .absolute()
            .top(px(TABLE_HEADER_HEIGHT))
            .left_0()
            .right_0()
            .bottom_0()
            .block_mouse_except_scroll()
            .track_scroll(scroll_handle.clone()),
        )
        .when(row_count == 0, |this| {
            this.child(render_table_empty_overlay(empty_message, theme))
        })
        .children(render_table_scrollbars(
            analysis_id,
            RuntimeScrollbarTable::SqlFrequency,
            &scroll_handle,
            row_count,
            SQL_ROW_HEIGHT,
            theme,
            cx,
        ))
}

/// 渲染 SQL 频率详情表格。
fn render_sql_frequency_detail_table(
    analysis_id: usize,
    state: &RuntimeAnalysisState,
    result: &RuntimeAnalysisResult,
    normalized_sql: &str,
    analysis_focus_handle: Option<FocusHandle>,
    theme: &AppTheme,
    cx: &mut Context<ArgusApp>,
) -> impl IntoElement + use<> {
    let rows = sql_frequency_detail_rows(result, state, normalized_sql);
    let row_count = rows.len();
    let scroll_handle = state.sql_frequency_detail_scroll.clone();

    div()
        .id("runtime-sql-frequency-detail")
        .flex_1()
        .min_h(px(0.0))
        .flex()
        .flex_col()
        .overflow_hidden()
        .child(render_sql_frequency_detail_topbar(
            analysis_id,
            normalized_sql,
            row_count,
            theme,
            cx,
        ))
        .child(
            div()
                .relative()
                .flex_1()
                .min_h(px(0.0))
                .overflow_hidden()
                .child(render_sql_frequency_detail_header(theme))
                .child(
                    uniform_list(
                        "runtime-sql-frequency-detail-list",
                        row_count,
                        cx.processor(move |app, range: Range<usize>, _window, row_cx| {
                            let theme = app.theme.clone();
                            let Some(state) = app.runtime_analysis_state(analysis_id) else {
                                return Vec::new();
                            };
                            let RuntimeAnalysisTaskState::Ready(result) = &state.task_state else {
                                return Vec::new();
                            };
                            let Some(rows) = rows.as_slice().get(range.clone()) else {
                                return Vec::new();
                            };

                            rows.iter()
                                .map(|row| {
                                    render_sql_frequency_detail_row(
                                        analysis_id,
                                        result,
                                        row,
                                        state.cell_selection.as_ref(),
                                        analysis_focus_handle.clone(),
                                        &theme,
                                        row_cx,
                                    )
                                    .into_any_element()
                                })
                                .collect::<Vec<_>>()
                        }),
                    )
                    .absolute()
                    .top(px(TABLE_HEADER_HEIGHT))
                    .left_0()
                    .right_0()
                    .bottom_0()
                    .block_mouse_except_scroll()
                    .track_scroll(scroll_handle.clone()),
                )
                .when(row_count == 0, |this| {
                    this.child(render_table_empty_overlay(
                        "当前过滤条件下没有该 SQL 的执行详情。",
                        theme,
                    ))
                })
                .children(render_table_scrollbars(
                    analysis_id,
                    RuntimeScrollbarTable::SqlFrequencyDetail,
                    &scroll_handle,
                    row_count,
                    SQL_ROW_HEIGHT,
                    theme,
                    cx,
                )),
        )
}

/// 渲染慢 SQL 分析表格。
fn render_slow_sql_table(
    analysis_id: usize,
    state: &RuntimeAnalysisState,
    result: &RuntimeAnalysisResult,
    analysis_focus_handle: Option<FocusHandle>,
    theme: &AppTheme,
    cx: &mut Context<ArgusApp>,
) -> impl IntoElement + use<> {
    let rows = slow_sql_rows(result, state);
    let row_count = rows.len();
    let scroll_handle = state.slow_sql_scroll.clone();
    let empty_message = if state.is_slow_sql_rows_computing {
        "正在计算慢 SQL 分析..."
    } else {
        "当前过滤条件下没有慢 SQL 数据。"
    };

    div()
        .id("runtime-slow-sql-table")
        .flex_1()
        .min_h(px(0.0))
        .relative()
        .overflow_hidden()
        .child(render_slow_sql_header(theme))
        .child(
            uniform_list(
                "runtime-slow-sql-list",
                row_count,
                cx.processor(move |app, range: Range<usize>, _window, row_cx| {
                    let theme = app.theme.clone();
                    let Some(state) = app.runtime_analysis_state(analysis_id) else {
                        return Vec::new();
                    };
                    let Some(rows) = rows.as_slice().get(range.clone()) else {
                        return Vec::new();
                    };

                    rows.iter()
                        .enumerate()
                        .map(|(offset, row)| {
                            render_slow_sql_row(
                                analysis_id,
                                range.start + offset,
                                row,
                                state.cell_selection.as_ref(),
                                analysis_focus_handle.clone(),
                                &theme,
                                row_cx,
                            )
                            .into_any_element()
                        })
                        .collect::<Vec<_>>()
                }),
            )
            .absolute()
            .top(px(TABLE_HEADER_HEIGHT))
            .left_0()
            .right_0()
            .bottom_0()
            .block_mouse_except_scroll()
            .track_scroll(scroll_handle.clone()),
        )
        .when(row_count == 0, |this| {
            this.child(render_table_empty_overlay(empty_message, theme))
        })
        .children(render_table_scrollbars(
            analysis_id,
            RuntimeScrollbarTable::SlowSql,
            &scroll_handle,
            row_count,
            SQL_ROW_HEIGHT,
            theme,
            cx,
        ))
}

/// 渲染慢 SQL 详情表格。
fn render_slow_sql_detail_table(
    analysis_id: usize,
    state: &RuntimeAnalysisState,
    result: &RuntimeAnalysisResult,
    normalized_sql: &str,
    analysis_focus_handle: Option<FocusHandle>,
    theme: &AppTheme,
    cx: &mut Context<ArgusApp>,
) -> impl IntoElement + use<> {
    let rows = sql_frequency_detail_rows(result, state, normalized_sql);
    let row_count = rows.len();
    let scroll_handle = state.slow_sql_scroll.clone();

    div()
        .id("runtime-slow-sql-detail")
        .flex_1()
        .min_h(px(0.0))
        .flex()
        .flex_col()
        .overflow_hidden()
        .child(render_slow_sql_detail_topbar(
            analysis_id,
            normalized_sql,
            row_count,
            theme,
            cx,
        ))
        .child(
            div()
                .relative()
                .flex_1()
                .min_h(px(0.0))
                .overflow_hidden()
                .child(render_slow_sql_detail_header(theme))
                .child(
                    uniform_list(
                        "runtime-slow-sql-detail-list",
                        row_count,
                        cx.processor(move |app, range: Range<usize>, _window, row_cx| {
                            let theme = app.theme.clone();
                            let Some(state) = app.runtime_analysis_state(analysis_id) else {
                                return Vec::new();
                            };
                            let RuntimeAnalysisTaskState::Ready(result) = &state.task_state else {
                                return Vec::new();
                            };
                            let Some(rows) = rows.as_slice().get(range.clone()) else {
                                return Vec::new();
                            };

                            rows.iter()
                                .map(|row| {
                                    render_slow_sql_detail_row(
                                        analysis_id,
                                        result,
                                        row,
                                        state.cell_selection.as_ref(),
                                        analysis_focus_handle.clone(),
                                        &theme,
                                        row_cx,
                                    )
                                    .into_any_element()
                                })
                                .collect::<Vec<_>>()
                        }),
                    )
                    .absolute()
                    .top(px(TABLE_HEADER_HEIGHT))
                    .left_0()
                    .right_0()
                    .bottom_0()
                    .block_mouse_except_scroll()
                    .track_scroll(scroll_handle.clone()),
                )
                .when(row_count == 0, |this| {
                    this.child(render_table_empty_overlay(
                        "当前过滤条件下没有该慢 SQL 的执行详情。",
                        theme,
                    ))
                })
                .children(render_table_scrollbars(
                    analysis_id,
                    RuntimeScrollbarTable::SlowSql,
                    &scroll_handle,
                    row_count,
                    SQL_ROW_HEIGHT,
                    theme,
                    cx,
                )),
        )
}

/// 渲染 SQL 频率分析表头。
fn render_sql_frequency_header(theme: &AppTheme) -> impl IntoElement + use<> {
    render_table_header(theme)
        .child(render_static_flex_header_cell("SQL文本", theme))
        .child(render_sql_analysis_column_gap())
        .child(render_static_header_cell(
            "平均执行时间",
            SQL_FREQUENCY_AVERAGE_COLUMN_WIDTH,
            theme,
        ))
        .child(render_static_header_cell(
            "执行次数",
            SQL_FREQUENCY_COUNT_COLUMN_WIDTH,
            theme,
        ))
        .child(render_static_header_cell(
            "操作",
            SQL_FREQUENCY_ACTION_COLUMN_WIDTH,
            theme,
        ))
}

/// 渲染单条 SQL 频率分析行。
fn render_sql_frequency_row(
    analysis_id: usize,
    row_index: usize,
    row: &RuntimeSqlFrequencyAnalysisRow,
    selection: Option<&RuntimeTableCellSelection>,
    analysis_focus_handle: Option<FocusHandle>,
    theme: &AppTheme,
    cx: &mut Context<ArgusApp>,
) -> impl IntoElement + use<> {
    render_table_row(SQL_ROW_HEIGHT, theme)
        .child(render_selectable_scroll_cell_with_font(
            analysis_id,
            format!("runtime-sql-frequency-text-{row_index}"),
            runtime_cell_key("sql-frequency", row_index, "text"),
            row.normalized_sql.clone(),
            SQL_ROW_HEIGHT,
            ARGUS_LOG_FONT_FAMILY,
            selection,
            analysis_focus_handle.clone(),
            theme,
            cx,
        ))
        .child(render_sql_analysis_column_gap())
        .child(render_selectable_cell(
            analysis_id,
            runtime_cell_key("sql-frequency", row_index, "average"),
            format_average_duration(row.average_execute_ms()),
            SQL_FREQUENCY_AVERAGE_COLUMN_WIDTH,
            selection,
            analysis_focus_handle.clone(),
            theme,
            cx,
        ))
        .child(render_selectable_cell(
            analysis_id,
            runtime_cell_key("sql-frequency", row_index, "count"),
            row.execute_count.to_string(),
            SQL_FREQUENCY_COUNT_COLUMN_WIDTH,
            selection,
            analysis_focus_handle.clone(),
            theme,
            cx,
        ))
        .child(
            div()
                .w(px(SQL_FREQUENCY_ACTION_COLUMN_WIDTH))
                .flex_none()
                .child(render_action_button(
                    format!("runtime-sql-frequency-detail-{row_index}"),
                    "详情",
                    theme,
                    {
                        let normalized_sql = row.normalized_sql.clone();
                        cx.listener(move |app, _, _, cx| {
                            app.open_runtime_sql_frequency_detail(
                                analysis_id,
                                normalized_sql.clone(),
                            );
                            cx.notify();
                        })
                    },
                )),
        )
}

/// 渲染 SQL 频率详情顶部信息。
fn render_sql_frequency_detail_topbar(
    analysis_id: usize,
    normalized_sql: &str,
    row_count: usize,
    theme: &AppTheme,
    cx: &mut Context<ArgusApp>,
) -> impl IntoElement + use<> {
    div()
        .px(px(RUNTIME_VIEW_PADDING))
        .py_2()
        .flex()
        .items_center()
        .justify_between()
        .gap_3()
        .border_b_1()
        .border_color(rgb(theme.border))
        .child(
            div()
                .flex()
                .items_center()
                .gap_2()
                .min_w(px(0.0))
                .child(render_back_button(
                    "runtime-sql-frequency-detail-back",
                    "频率分析",
                    theme,
                    cx.listener(move |app, _, _, cx| {
                        app.show_runtime_sql_frequency_summary(analysis_id);
                        cx.notify();
                    }),
                ))
                .child(
                    div()
                        .min_w(px(0.0))
                        .flex()
                        .flex_col()
                        .child(
                            div()
                                .text_size(px(13.0))
                                .font_weight(FontWeight::SEMIBOLD)
                                .truncate()
                                .child(normalized_sql.to_string()),
                        )
                        .child(
                            div()
                                .text_size(px(12.0))
                                .text_color(rgb(theme.foreground))
                                .child(format!("{row_count} 次执行")),
                        ),
                ),
        )
}

/// 渲染 SQL 频率详情表头。
fn render_sql_frequency_detail_header(theme: &AppTheme) -> impl IntoElement + use<> {
    render_table_header(theme)
        .child(render_static_flex_header_cell("SQL文本", theme))
        .child(render_sql_analysis_column_gap())
        .child(render_static_header_cell(
            "SQL具体耗时",
            SQL_FREQUENCY_DETAIL_DURATION_COLUMN_WIDTH,
            theme,
        ))
        .child(render_static_header_cell(
            "来源请求",
            SQL_FREQUENCY_DETAIL_REQUEST_COLUMN_WIDTH,
            theme,
        ))
        .child(render_static_header_cell(
            "发起时间",
            SQL_FREQUENCY_DETAIL_TIME_COLUMN_WIDTH,
            theme,
        ))
}

/// 渲染单条 SQL 频率详情行。
fn render_sql_frequency_detail_row(
    analysis_id: usize,
    result: &RuntimeAnalysisResult,
    row: &RuntimeSqlFrequencyDetailRow,
    selection: Option<&RuntimeTableCellSelection>,
    analysis_focus_handle: Option<FocusHandle>,
    theme: &AppTheme,
    cx: &mut Context<ArgusApp>,
) -> impl IntoElement + use<> {
    let request = result.requests.get(row.request_index);
    let sql = request.and_then(|request| request.sql_records.get(row.sql_index));
    let sql_text = sql
        .map(|sql| sql.sql_text.clone())
        .unwrap_or_else(|| "SQL 明细已不可用".to_string());
    let request_path = request
        .map(|request| request.request_path.clone())
        .unwrap_or_else(|| "未知请求".to_string());
    let request_time_label = request
        .map(|request| request.request_time_label.clone())
        .unwrap_or_else(|| "-".to_string());

    render_table_row(SQL_ROW_HEIGHT, theme)
        .child(render_selectable_scroll_cell_with_font(
            analysis_id,
            format!(
                "runtime-sql-frequency-detail-text-{}-{}",
                row.request_index, row.sql_index
            ),
            runtime_sql_cell_key(row.request_index, row.sql_index, "frequency-detail-text"),
            sql_text,
            SQL_ROW_HEIGHT,
            ARGUS_LOG_FONT_FAMILY,
            selection,
            analysis_focus_handle.clone(),
            theme,
            cx,
        ))
        .child(render_sql_analysis_column_gap())
        .child(render_selectable_cell(
            analysis_id,
            runtime_sql_cell_key(
                row.request_index,
                row.sql_index,
                "frequency-detail-duration",
            ),
            format_duration(row.execute_ms),
            SQL_FREQUENCY_DETAIL_DURATION_COLUMN_WIDTH,
            selection,
            analysis_focus_handle.clone(),
            theme,
            cx,
        ))
        .child(render_selectable_cell(
            analysis_id,
            runtime_sql_cell_key(row.request_index, row.sql_index, "frequency-detail-request"),
            request_path,
            SQL_FREQUENCY_DETAIL_REQUEST_COLUMN_WIDTH,
            selection,
            analysis_focus_handle.clone(),
            theme,
            cx,
        ))
        .child(render_selectable_cell(
            analysis_id,
            runtime_sql_cell_key(row.request_index, row.sql_index, "frequency-detail-time"),
            request_time_label,
            SQL_FREQUENCY_DETAIL_TIME_COLUMN_WIDTH,
            selection,
            analysis_focus_handle,
            theme,
            cx,
        ))
}

/// 渲染慢 SQL 分析表头。
fn render_slow_sql_header(theme: &AppTheme) -> impl IntoElement + use<> {
    render_table_header(theme)
        .child(render_static_flex_header_cell("SQL文本", theme))
        .child(render_sql_analysis_column_gap())
        .child(render_static_header_cell(
            "平均执行时间",
            SQL_FREQUENCY_AVERAGE_COLUMN_WIDTH,
            theme,
        ))
        .child(render_static_header_cell(
            "执行次数",
            SQL_FREQUENCY_COUNT_COLUMN_WIDTH,
            theme,
        ))
        .child(render_static_header_cell(
            "操作",
            SQL_FREQUENCY_ACTION_COLUMN_WIDTH,
            theme,
        ))
}

/// 渲染单条慢 SQL 聚合行。
fn render_slow_sql_row(
    analysis_id: usize,
    row_index: usize,
    row: &RuntimeSlowSqlSummaryRow,
    selection: Option<&RuntimeTableCellSelection>,
    analysis_focus_handle: Option<FocusHandle>,
    theme: &AppTheme,
    cx: &mut Context<ArgusApp>,
) -> impl IntoElement + use<> {
    render_table_row(SQL_ROW_HEIGHT, theme)
        .child(render_selectable_scroll_cell_with_font(
            analysis_id,
            format!("runtime-slow-sql-text-{row_index}"),
            runtime_cell_key("slow-sql", row_index, "text"),
            row.normalized_sql.clone(),
            SQL_ROW_HEIGHT,
            ARGUS_LOG_FONT_FAMILY,
            selection,
            analysis_focus_handle.clone(),
            theme,
            cx,
        ))
        .child(render_sql_analysis_column_gap())
        .child(render_selectable_cell(
            analysis_id,
            runtime_cell_key("slow-sql", row_index, "average"),
            format_average_duration(row.average_execute_ms()),
            SQL_FREQUENCY_AVERAGE_COLUMN_WIDTH,
            selection,
            analysis_focus_handle.clone(),
            theme,
            cx,
        ))
        .child(render_selectable_cell(
            analysis_id,
            runtime_cell_key("slow-sql", row_index, "count"),
            row.execute_count.to_string(),
            SQL_FREQUENCY_COUNT_COLUMN_WIDTH,
            selection,
            analysis_focus_handle.clone(),
            theme,
            cx,
        ))
        .child(
            div()
                .w(px(SQL_FREQUENCY_ACTION_COLUMN_WIDTH))
                .flex_none()
                .child(render_action_button(
                    format!("runtime-slow-sql-detail-{row_index}"),
                    "详情",
                    theme,
                    {
                        let normalized_sql = row.normalized_sql.clone();
                        cx.listener(move |app, _, _, cx| {
                            app.open_runtime_slow_sql_detail(analysis_id, normalized_sql.clone());
                            cx.notify();
                        })
                    },
                )),
        )
}

/// 渲染慢 SQL 详情顶部信息。
fn render_slow_sql_detail_topbar(
    analysis_id: usize,
    normalized_sql: &str,
    row_count: usize,
    theme: &AppTheme,
    cx: &mut Context<ArgusApp>,
) -> impl IntoElement + use<> {
    div()
        .h(px(42.0))
        .px(px(RUNTIME_VIEW_PADDING))
        .flex()
        .items_center()
        .justify_between()
        .border_b_1()
        .border_color(rgb(theme.border))
        .child(
            div()
                .flex()
                .items_center()
                .gap_2()
                .child(render_action_button(
                    format!("runtime-slow-sql-detail-back-{analysis_id}"),
                    "返回",
                    theme,
                    cx.listener(move |app, _, _, cx| {
                        app.show_runtime_slow_sql_summary(analysis_id);
                        cx.notify();
                    }),
                ))
                .child(
                    div()
                        .min_w(px(0.0))
                        .flex()
                        .flex_col()
                        .child(
                            div()
                                .text_size(px(13.0))
                                .font_weight(FontWeight::SEMIBOLD)
                                .truncate()
                                .child(normalized_sql.to_string()),
                        )
                        .child(
                            div()
                                .text_size(px(12.0))
                                .text_color(rgb(theme.foreground))
                                .child(format!("{row_count} 次执行")),
                        ),
                ),
        )
}

/// 渲染慢 SQL 详情表头。
fn render_slow_sql_detail_header(theme: &AppTheme) -> impl IntoElement + use<> {
    render_table_header(theme)
        .child(render_static_flex_header_cell("SQL文本", theme))
        .child(render_sql_analysis_column_gap())
        .child(render_static_header_cell(
            "执行接口",
            SQL_FREQUENCY_DETAIL_REQUEST_COLUMN_WIDTH,
            theme,
        ))
        .child(render_static_header_cell(
            "执行时间",
            SQL_FREQUENCY_DETAIL_TIME_COLUMN_WIDTH,
            theme,
        ))
        .child(render_static_header_cell(
            "执行耗时",
            SQL_FREQUENCY_DETAIL_DURATION_COLUMN_WIDTH,
            theme,
        ))
}

/// 渲染慢 SQL 详情中的单次执行行。
fn render_slow_sql_detail_row(
    analysis_id: usize,
    result: &RuntimeAnalysisResult,
    row: &RuntimeSqlFrequencyDetailRow,
    selection: Option<&RuntimeTableCellSelection>,
    analysis_focus_handle: Option<FocusHandle>,
    theme: &AppTheme,
    cx: &mut Context<ArgusApp>,
) -> impl IntoElement + use<> {
    let request = result.requests.get(row.request_index);
    let sql = request.and_then(|request| request.sql_records.get(row.sql_index));
    let sql_text = sql
        .map(|sql| sql.sql_text.clone())
        .unwrap_or_else(|| "SQL 明细已不可用".to_string());
    let request_path = request
        .map(|request| request.request_path.clone())
        .unwrap_or_else(|| "未知请求".to_string());
    let request_time_label = request
        .map(|request| request.request_time_label.clone())
        .unwrap_or_else(|| "-".to_string());

    render_table_row(SQL_ROW_HEIGHT, theme)
        .child(render_selectable_scroll_cell_with_font(
            analysis_id,
            format!(
                "runtime-slow-sql-detail-text-{}-{}",
                row.request_index, row.sql_index
            ),
            runtime_sql_cell_key(row.request_index, row.sql_index, "slow-detail-text"),
            sql_text,
            SQL_ROW_HEIGHT,
            ARGUS_LOG_FONT_FAMILY,
            selection,
            analysis_focus_handle.clone(),
            theme,
            cx,
        ))
        .child(render_sql_analysis_column_gap())
        .child(render_selectable_cell(
            analysis_id,
            runtime_sql_cell_key(row.request_index, row.sql_index, "slow-detail-request"),
            request_path,
            SQL_FREQUENCY_DETAIL_REQUEST_COLUMN_WIDTH,
            selection,
            analysis_focus_handle.clone(),
            theme,
            cx,
        ))
        .child(render_selectable_cell(
            analysis_id,
            runtime_sql_cell_key(row.request_index, row.sql_index, "slow-detail-time"),
            request_time_label,
            SQL_FREQUENCY_DETAIL_TIME_COLUMN_WIDTH,
            selection,
            analysis_focus_handle.clone(),
            theme,
            cx,
        ))
        .child(render_selectable_cell(
            analysis_id,
            runtime_sql_cell_key(row.request_index, row.sql_index, "slow-detail-duration"),
            format_duration(row.execute_ms),
            SQL_FREQUENCY_DETAIL_DURATION_COLUMN_WIDTH,
            selection,
            analysis_focus_handle,
            theme,
            cx,
        ))
}

/// 渲染 SQL 分析表的文本列和指标列之间的固定间隔。
fn render_sql_analysis_column_gap() -> impl IntoElement {
    div().w(px(SQL_ANALYSIS_COLUMN_GAP)).flex_none()
}

/// 渲染固定表格无数据时覆盖在表体区域的空态。
fn render_table_empty_overlay(message: &'static str, theme: &AppTheme) -> impl IntoElement + use<> {
    div()
        .absolute()
        .top(px(TABLE_HEADER_HEIGHT))
        .left_0()
        .right_0()
        .bottom_0()
        .flex()
        .items_center()
        .justify_center()
        .text_size(px(13.0))
        .text_color(rgb(theme.foreground_muted))
        .child(message)
}

/// 渲染总览表头。
fn render_summary_header(
    analysis_id: usize,
    state: &RuntimeAnalysisState,
    theme: &AppTheme,
    cx: &mut Context<ArgusApp>,
) -> impl IntoElement + use<> {
    render_table_header(theme)
        .child(render_header_cell(
            "runtime-summary-sort-request-count",
            "请求次数",
            SUMMARY_COUNT_COLUMN_WIDTH,
            state.summary_sort_key == RuntimeSummarySortKey::RequestCount,
            state.summary_sort_direction,
            theme,
            {
                cx.listener(move |app, _, _, cx| {
                    app.set_runtime_summary_sort(analysis_id, RuntimeSummarySortKey::RequestCount);
                    cx.notify();
                })
            },
        ))
        .child(render_flex_header_cell(
            "runtime-summary-sort-request-path",
            "请求地址",
            state.summary_sort_key == RuntimeSummarySortKey::RequestPath,
            state.summary_sort_direction,
            theme,
            {
                cx.listener(move |app, _, _, cx| {
                    app.set_runtime_summary_sort(analysis_id, RuntimeSummarySortKey::RequestPath);
                    cx.notify();
                })
            },
        ))
        .child(render_header_cell(
            "runtime-summary-sort-average-duration",
            "平均耗时",
            SUMMARY_AVERAGE_COLUMN_WIDTH,
            state.summary_sort_key == RuntimeSummarySortKey::AverageDuration,
            state.summary_sort_direction,
            theme,
            {
                cx.listener(move |app, _, _, cx| {
                    app.set_runtime_summary_sort(
                        analysis_id,
                        RuntimeSummarySortKey::AverageDuration,
                    );
                    cx.notify();
                })
            },
        ))
        .child(render_header_cell(
            "runtime-summary-sort-slow-ratio",
            "慢SQL比例",
            SUMMARY_RATIO_COLUMN_WIDTH,
            state.summary_sort_key == RuntimeSummarySortKey::SlowSqlRatio,
            state.summary_sort_direction,
            theme,
            {
                cx.listener(move |app, _, _, cx| {
                    app.set_runtime_summary_sort(analysis_id, RuntimeSummarySortKey::SlowSqlRatio);
                    cx.notify();
                })
            },
        ))
        .child(render_static_header_cell(
            "操作",
            SUMMARY_ACTION_COLUMN_WIDTH,
            theme,
        ))
}

/// 渲染单条总览行。
fn render_summary_row(
    analysis_id: usize,
    summary_index: usize,
    summary: &RuntimeRequestSummary,
    selection: Option<&RuntimeTableCellSelection>,
    analysis_focus_handle: Option<FocusHandle>,
    theme: &AppTheme,
    cx: &mut Context<ArgusApp>,
) -> impl IntoElement + use<> {
    let request_path = summary.request_path.clone();
    render_table_row(TABLE_ROW_HEIGHT, theme)
        .child(render_selectable_cell(
            analysis_id,
            runtime_cell_key("summary", summary_index, "request-count"),
            summary.request_count.to_string(),
            SUMMARY_COUNT_COLUMN_WIDTH,
            selection,
            analysis_focus_handle.clone(),
            theme,
            cx,
        ))
        .child(render_selectable_scroll_cell(
            analysis_id,
            format!("runtime-summary-path-{}", summary.request_path),
            runtime_cell_key("summary", summary_index, "request-path"),
            summary.request_path.clone(),
            TABLE_ROW_HEIGHT,
            selection,
            analysis_focus_handle.clone(),
            theme,
            cx,
        ))
        .child(render_selectable_cell(
            analysis_id,
            runtime_cell_key("summary", summary_index, "average-duration"),
            format_average_duration(summary.average_duration_ms),
            SUMMARY_AVERAGE_COLUMN_WIDTH,
            selection,
            analysis_focus_handle.clone(),
            theme,
            cx,
        ))
        .child(render_selectable_cell(
            analysis_id,
            runtime_cell_key("summary", summary_index, "slow-sql-ratio"),
            format_ratio(summary.slow_sql_ratio),
            SUMMARY_RATIO_COLUMN_WIDTH,
            selection,
            analysis_focus_handle,
            theme,
            cx,
        ))
        .child(
            div()
                .w(px(SUMMARY_ACTION_COLUMN_WIDTH))
                .flex_none()
                .child(render_action_button(
                    format!("runtime-summary-detail-{}", summary.request_path),
                    "详情",
                    theme,
                    cx.listener(move |app, _, _, cx| {
                        app.open_runtime_request_details(analysis_id, request_path.clone());
                        cx.notify();
                    }),
                )),
        )
}

/// 渲染请求详情顶部信息。
fn render_request_details_topbar(
    analysis_id: usize,
    summary: &RuntimeRequestSummary,
    theme: &AppTheme,
    cx: &mut Context<ArgusApp>,
) -> impl IntoElement + use<> {
    div()
        .px(px(RUNTIME_VIEW_PADDING))
        .py_2()
        .flex()
        .items_center()
        .justify_between()
        .gap_3()
        .border_b_1()
        .border_color(rgb(theme.border))
        .child(
            div()
                .flex()
                .items_center()
                .gap_2()
                .min_w(px(0.0))
                .child(render_back_button(
                    "runtime-request-back-summary",
                    "总览",
                    theme,
                    cx.listener(move |app, _, _, cx| {
                        app.show_runtime_summary(analysis_id);
                        cx.notify();
                    }),
                ))
                .child(
                    div()
                        .min_w(px(0.0))
                        .flex()
                        .flex_col()
                        .child(
                            div()
                                .text_size(px(13.0))
                                .font_weight(FontWeight::SEMIBOLD)
                                .truncate()
                                .child(summary.request_path.clone()),
                        )
                        .child(
                            div()
                                .text_size(px(12.0))
                                .text_color(rgb(theme.foreground))
                                .child(format!(
                                    "{} 次请求，平均耗时 {}，慢 SQL 比例 {}",
                                    summary.request_count,
                                    format_average_duration(summary.average_duration_ms),
                                    format_ratio(summary.slow_sql_ratio)
                                )),
                        ),
                ),
        )
}

/// 渲染请求明细表头。
fn render_request_header(
    analysis_id: usize,
    state: &RuntimeAnalysisState,
    theme: &AppTheme,
    cx: &mut Context<ArgusApp>,
) -> impl IntoElement + use<> {
    render_table_header(theme)
        .child(render_header_cell(
            "runtime-request-sort-time",
            "请求时间",
            REQUEST_TIME_COLUMN_WIDTH,
            state.request_sort_key == RuntimeRequestSortKey::RequestTime,
            state.request_sort_direction,
            theme,
            cx.listener(move |app, _, _, cx| {
                app.set_runtime_request_sort(analysis_id, RuntimeRequestSortKey::RequestTime);
                cx.notify();
            }),
        ))
        .child(render_header_cell(
            "runtime-request-sort-username",
            "用户名",
            REQUEST_USERNAME_COLUMN_WIDTH,
            state.request_sort_key == RuntimeRequestSortKey::Username,
            state.request_sort_direction,
            theme,
            cx.listener(move |app, _, _, cx| {
                app.set_runtime_request_sort(analysis_id, RuntimeRequestSortKey::Username);
                cx.notify();
            }),
        ))
        .child(render_header_cell(
            "runtime-request-sort-duration",
            "请求耗时",
            REQUEST_DURATION_COLUMN_WIDTH,
            state.request_sort_key == RuntimeRequestSortKey::RequestDuration,
            state.request_sort_direction,
            theme,
            cx.listener(move |app, _, _, cx| {
                app.set_runtime_request_sort(analysis_id, RuntimeRequestSortKey::RequestDuration);
                cx.notify();
            }),
        ))
        .child(render_flex_header_cell(
            "runtime-request-sort-path",
            "请求地址",
            state.request_sort_key == RuntimeRequestSortKey::RequestPath,
            state.request_sort_direction,
            theme,
            cx.listener(move |app, _, _, cx| {
                app.set_runtime_request_sort(analysis_id, RuntimeRequestSortKey::RequestPath);
                cx.notify();
            }),
        ))
        .child(render_static_header_cell(
            "操作",
            REQUEST_ACTION_COLUMN_WIDTH,
            theme,
        ))
}

/// 渲染单条请求明细行。
fn render_request_row(
    analysis_id: usize,
    request: &RuntimeRequestRecord,
    selection: Option<&RuntimeTableCellSelection>,
    analysis_focus_handle: Option<FocusHandle>,
    theme: &AppTheme,
    cx: &mut Context<ArgusApp>,
) -> impl IntoElement + use<> {
    let request_path = request.request_path.clone();
    let request_index = request.index;
    render_table_row(TABLE_ROW_HEIGHT, theme)
        .child(render_selectable_cell(
            analysis_id,
            runtime_cell_key("request", request_index, "time"),
            request.request_time_label.clone(),
            REQUEST_TIME_COLUMN_WIDTH,
            selection,
            analysis_focus_handle.clone(),
            theme,
            cx,
        ))
        .child(render_selectable_cell(
            analysis_id,
            runtime_cell_key("request", request_index, "username"),
            display_username(&request.username),
            REQUEST_USERNAME_COLUMN_WIDTH,
            selection,
            analysis_focus_handle.clone(),
            theme,
            cx,
        ))
        .child(render_selectable_cell(
            analysis_id,
            runtime_cell_key("request", request_index, "duration"),
            format_duration(request.request_duration_ms),
            REQUEST_DURATION_COLUMN_WIDTH,
            selection,
            analysis_focus_handle.clone(),
            theme,
            cx,
        ))
        .child(render_selectable_scroll_cell(
            analysis_id,
            format!("runtime-request-path-{request_index}"),
            runtime_cell_key("request", request_index, "path"),
            request.request_path.clone(),
            TABLE_ROW_HEIGHT,
            selection,
            analysis_focus_handle,
            theme,
            cx,
        ))
        .child(
            div()
                .w(px(REQUEST_ACTION_COLUMN_WIDTH))
                .flex_none()
                .child(render_action_button(
                    format!("runtime-request-sql-list-{request_index}"),
                    "SQL列表",
                    theme,
                    cx.listener(move |app, _, _, cx| {
                        app.open_runtime_sql_list(analysis_id, request_path.clone(), request_index);
                        cx.notify();
                    }),
                )),
        )
}

/// 渲染 SQL 列表顶部信息。
fn render_sql_topbar(
    analysis_id: usize,
    request_path: &str,
    request: &RuntimeRequestRecord,
    theme: &AppTheme,
    cx: &mut Context<ArgusApp>,
) -> impl IntoElement + use<> {
    let request_path_for_back = request_path.to_string();
    div()
        .px(px(RUNTIME_VIEW_PADDING))
        .py_2()
        .flex()
        .items_center()
        .justify_between()
        .gap_3()
        .border_b_1()
        .border_color(rgb(theme.border))
        .child(
            div()
                .flex()
                .items_center()
                .gap_2()
                .min_w(px(0.0))
                .child(render_back_button(
                    "runtime-sql-back-details",
                    "详情",
                    theme,
                    cx.listener(move |app, _, _, cx| {
                        app.show_runtime_request_details(
                            analysis_id,
                            request_path_for_back.clone(),
                        );
                        cx.notify();
                    }),
                ))
                .child(
                    div()
                        .min_w(px(0.0))
                        .flex()
                        .flex_col()
                        .child(
                            div()
                                .text_size(px(13.0))
                                .font_weight(FontWeight::SEMIBOLD)
                                .truncate()
                                .child(request.request_path.clone()),
                        )
                        .child(
                            div()
                                .text_size(px(12.0))
                                .text_color(rgb(theme.foreground))
                                .child(format!(
                                    "{} · {} · 请求耗时 {} · SQL 累积耗时 {} · 共执行 {} 个 SQL",
                                    request.request_time_label,
                                    display_username(&request.username),
                                    format_duration(request.request_duration_ms),
                                    format_duration(request.sql_total_execute_ms),
                                    request.sql_records.len()
                                )),
                        ),
                ),
        )
}

/// 渲染 SQL 明细表头。
fn render_sql_header(
    analysis_id: usize,
    state: &RuntimeAnalysisState,
    theme: &AppTheme,
    cx: &mut Context<ArgusApp>,
) -> impl IntoElement + use<> {
    render_table_header(theme)
        .child(render_header_cell(
            "runtime-sql-sort-execute",
            "总耗时",
            SQL_DURATION_COLUMN_WIDTH,
            state.sql_sort_key == RuntimeSqlSortKey::ExecuteDuration,
            state.sql_sort_direction,
            theme,
            cx.listener(move |app, _, _, cx| {
                app.set_runtime_sql_sort(analysis_id, RuntimeSqlSortKey::ExecuteDuration);
                cx.notify();
            }),
        ))
        .child(render_header_cell(
            "runtime-sql-sort-acquire",
            "获取链接",
            SQL_DURATION_COLUMN_WIDTH,
            state.sql_sort_key == RuntimeSqlSortKey::AcquireConnectionDuration,
            state.sql_sort_direction,
            theme,
            cx.listener(move |app, _, _, cx| {
                app.set_runtime_sql_sort(analysis_id, RuntimeSqlSortKey::AcquireConnectionDuration);
                cx.notify();
            }),
        ))
        .child(render_header_cell(
            "runtime-sql-sort-commit",
            "事务提交",
            SQL_DURATION_COLUMN_WIDTH,
            state.sql_sort_key == RuntimeSqlSortKey::CommitDuration,
            state.sql_sort_direction,
            theme,
            cx.listener(move |app, _, _, cx| {
                app.set_runtime_sql_sort(analysis_id, RuntimeSqlSortKey::CommitDuration);
                cx.notify();
            }),
        ))
        .child(render_header_cell(
            "runtime-sql-sort-release",
            "释放链接",
            SQL_DURATION_COLUMN_WIDTH,
            state.sql_sort_key == RuntimeSqlSortKey::ReleaseConnectionDuration,
            state.sql_sort_direction,
            theme,
            cx.listener(move |app, _, _, cx| {
                app.set_runtime_sql_sort(analysis_id, RuntimeSqlSortKey::ReleaseConnectionDuration);
                cx.notify();
            }),
        ))
        .child(render_header_cell(
            "runtime-sql-sort-parse",
            "解析结果",
            SQL_DURATION_COLUMN_WIDTH,
            state.sql_sort_key == RuntimeSqlSortKey::ParseResultDuration,
            state.sql_sort_direction,
            theme,
            cx.listener(move |app, _, _, cx| {
                app.set_runtime_sql_sort(analysis_id, RuntimeSqlSortKey::ParseResultDuration);
                cx.notify();
            }),
        ))
        .child(render_flex_header_cell(
            "runtime-sql-sort-text",
            "SQL文本",
            state.sql_sort_key == RuntimeSqlSortKey::SqlText,
            state.sql_sort_direction,
            theme,
            cx.listener(move |app, _, _, cx| {
                app.set_runtime_sql_sort(analysis_id, RuntimeSqlSortKey::SqlText);
                cx.notify();
            }),
        ))
}

/// 渲染单条 SQL 明细行。
fn render_sql_row(
    analysis_id: usize,
    request_index: usize,
    request_path: String,
    request_time_label: String,
    username: String,
    sql_index: usize,
    sql: &RuntimeSqlRecord,
    hovered_sql_cell: Option<RuntimeSqlCellKey>,
    selection: Option<&RuntimeTableCellSelection>,
    analysis_focus_handle: Option<FocusHandle>,
    theme: &AppTheme,
    cx: &mut Context<ArgusApp>,
) -> impl IntoElement + use<> {
    render_table_row(SQL_ROW_HEIGHT, theme)
        .child(render_selectable_cell(
            analysis_id,
            runtime_sql_cell_key(request_index, sql_index, "execute"),
            format_duration(sql.execute_ms),
            SQL_DURATION_COLUMN_WIDTH,
            selection,
            analysis_focus_handle.clone(),
            theme,
            cx,
        ))
        .child(render_selectable_cell(
            analysis_id,
            runtime_sql_cell_key(request_index, sql_index, "acquire"),
            format_duration(sql.acquire_connection_ms),
            SQL_DURATION_COLUMN_WIDTH,
            selection,
            analysis_focus_handle.clone(),
            theme,
            cx,
        ))
        .child(render_selectable_cell(
            analysis_id,
            runtime_sql_cell_key(request_index, sql_index, "commit"),
            format_duration(sql.commit_ms),
            SQL_DURATION_COLUMN_WIDTH,
            selection,
            analysis_focus_handle.clone(),
            theme,
            cx,
        ))
        .child(render_selectable_cell(
            analysis_id,
            runtime_sql_cell_key(request_index, sql_index, "release"),
            format_duration(sql.release_connection_ms),
            SQL_DURATION_COLUMN_WIDTH,
            selection,
            analysis_focus_handle.clone(),
            theme,
            cx,
        ))
        .child(render_selectable_cell(
            analysis_id,
            runtime_sql_cell_key(request_index, sql_index, "parse"),
            format_duration(sql.parse_result_ms),
            SQL_DURATION_COLUMN_WIDTH,
            selection,
            analysis_focus_handle.clone(),
            theme,
            cx,
        ))
        .child(
            div()
                .flex_1()
                .min_w(px(0.0))
                .flex()
                .h_full()
                .items_center()
                .child(render_sql_text_cell(
                    analysis_id,
                    request_index,
                    request_path,
                    request_time_label,
                    username,
                    sql_index,
                    sql,
                    hovered_sql_cell,
                    selection,
                    analysis_focus_handle,
                    theme,
                    cx,
                )),
        )
}

/// 渲染 SQL 文本单元格；单元格宽度由表格列固定，长 SQL 在单元格内部横向滚动。
fn render_sql_text_cell(
    analysis_id: usize,
    request_index: usize,
    request_path: String,
    request_time_label: String,
    username: String,
    sql_index: usize,
    sql: &RuntimeSqlRecord,
    hovered_sql_cell: Option<RuntimeSqlCellKey>,
    selection: Option<&RuntimeTableCellSelection>,
    analysis_focus_handle: Option<FocusHandle>,
    theme: &AppTheme,
    cx: &mut Context<ArgusApp>,
) -> AnyElement {
    let cell_key = runtime_sql_cell_key(request_index, sql_index, "text");
    let display_text = runtime_cell_display_text(&sql.sql_text);
    let sql_cell_key = RuntimeSqlCellKey {
        request_index,
        sql_index,
    };
    let is_hovered = hovered_sql_cell == Some(sql_cell_key);
    let sql_text = sql.sql_text.clone();

    div()
        .id(SharedString::from(format!(
            "runtime-sql-text-{request_index}-{sql_index}"
        )))
        .flex_1()
        .min_w(px(0.0))
        .h(px(SQL_ROW_HEIGHT - 2.0))
        .relative()
        .flex()
        .items_center()
        .font_family(ARGUS_LOG_FONT_FAMILY)
        .text_color(rgb(theme.foreground))
        .on_hover(cx.listener(move |app, is_hovered: &bool, _, cx| {
            if app.set_runtime_sql_cell_hovered(analysis_id, request_index, sql_index, *is_hovered)
            {
                cx.notify();
            }
        }))
        .child(
            div()
                .id(SharedString::from(format!(
                    "runtime-sql-text-scroll-{request_index}-{sql_index}"
                )))
                .flex_1()
                .min_w(px(0.0))
                .h_full()
                .relative()
                .flex()
                .items_center()
                .overflow_x_scroll()
                .scrollbar_width(px(4.0))
                .child(div().flex_none().pr_2().whitespace_nowrap().child(
                    render_runtime_cell_text(
                        display_text.clone(),
                        runtime_cell_selection_range(selection, &cell_key),
                        theme,
                    ),
                ))
                .child(render_runtime_cell_pointer_layer(
                    analysis_id,
                    cell_key,
                    display_text,
                    ARGUS_LOG_FONT_FAMILY,
                    analysis_focus_handle,
                    cx,
                )),
        )
        .child(render_sql_more_button(
            analysis_id,
            request_index,
            sql_index,
            request_path,
            request_time_label,
            username,
            sql_text,
            is_hovered,
            theme,
            cx,
        ))
        .into_any_element()
}

/// 渲染 SQL 单元格末尾的更多入口，点击后展示保留格式的完整 SQL。
fn render_sql_more_button(
    analysis_id: usize,
    request_index: usize,
    sql_index: usize,
    request_path: String,
    request_time_label: String,
    username: String,
    sql_text: String,
    is_visible: bool,
    theme: &AppTheme,
    cx: &mut Context<ArgusApp>,
) -> impl IntoElement + use<> {
    div()
        .id(SharedString::from(format!(
            "runtime-sql-more-{request_index}-{sql_index}"
        )))
        .w(px(30.0))
        .h_full()
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .opacity(if is_visible { 1.0 } else { 0.08 })
        .child(
            div()
                .w(px(26.0))
                .h(px(22.0))
                .flex()
                .items_center()
                .justify_center()
                .rounded_sm()
                .border_1()
                .border_color(rgb(theme.border))
                .bg(rgb(theme.content))
                .cursor_pointer()
                .shadow_sm()
                .hover(|this| this.bg(rgb(theme.current_line)))
                .child(render_icon(ArgusIcon::More, theme.foreground_muted, 15.0)),
        )
        .on_click(cx.listener(move |app, _, _, cx| {
            cx.stop_propagation();
            app.open_runtime_sql_text_dialog(
                analysis_id,
                RuntimeSqlTextDialog {
                    request_path: request_path.clone(),
                    request_time_label: request_time_label.clone(),
                    username: username.clone(),
                    sql_text: sql_text.clone(),
                    selection: None,
                    selection_drag: None,
                },
            );
            cx.notify();
        }))
}

/// 渲染表格外层表头行；表格随容器宽度伸缩，避免把整页撑出横向滚动。
fn render_table_header(theme: &AppTheme) -> gpui::Div {
    div()
        .h(px(TABLE_HEADER_HEIGHT))
        .w_full()
        .px(px(RUNTIME_VIEW_PADDING))
        .flex()
        .items_center()
        .border_b_1()
        .border_color(rgb(theme.border))
        .bg(rgb(theme.content))
        .text_size(px(12.0))
        .line_height(px(18.0))
        .text_color(rgb(theme.foreground))
}

/// 渲染可排序表头单元格。
fn render_header_cell(
    id: &'static str,
    label: &'static str,
    width: f32,
    is_active: bool,
    direction: RuntimeSortDirection,
    theme: &AppTheme,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    let indicator = if is_active { direction.indicator() } else { "" };
    div()
        .id(id)
        .w(px(width))
        .h(px(22.0))
        .flex_none()
        .flex()
        .items_center()
        .rounded_sm()
        .px_2()
        .cursor_pointer()
        .text_color(rgb(if is_active {
            theme.info
        } else {
            theme.foreground
        }))
        .hover(|this| this.bg(rgb(theme.current_line)))
        .child(format!("{label}{indicator}"))
        .on_click(on_click)
}

/// 渲染会吸收剩余宽度的可排序表头单元格。
fn render_flex_header_cell(
    id: &'static str,
    label: &'static str,
    is_active: bool,
    direction: RuntimeSortDirection,
    theme: &AppTheme,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    let indicator = if is_active { direction.indicator() } else { "" };
    div()
        .id(id)
        .flex_1()
        .min_w(px(0.0))
        .h(px(22.0))
        .flex()
        .items_center()
        .rounded_sm()
        .px_2()
        .cursor_pointer()
        .text_color(rgb(if is_active {
            theme.info
        } else {
            theme.foreground
        }))
        .hover(|this| this.bg(rgb(theme.current_line)))
        .child(
            div()
                .min_w(px(0.0))
                .truncate()
                .child(format!("{label}{indicator}")),
        )
        .on_click(on_click)
}

/// 渲染不可排序表头单元格。
fn render_static_header_cell(
    label: &'static str,
    width: f32,
    theme: &AppTheme,
) -> impl IntoElement {
    div()
        .w(px(width))
        .h(px(22.0))
        .flex_none()
        .flex()
        .items_center()
        .px_2()
        .text_color(rgb(theme.foreground))
        .child(label)
}

/// 渲染不可排序且吸收剩余宽度的表头单元格。
fn render_static_flex_header_cell(label: &'static str, theme: &AppTheme) -> impl IntoElement {
    div()
        .flex_1()
        .min_w(px(0.0))
        .h(px(22.0))
        .flex()
        .items_center()
        .px_2()
        .text_color(rgb(theme.foreground))
        .child(div().min_w(px(0.0)).truncate().child(label))
}

/// 渲染普通表格行；行宽固定为容器宽度，长字段由单元格内部滚动承载。
fn render_table_row(height: f32, theme: &AppTheme) -> gpui::Div {
    div()
        .h(px(height))
        .w_full()
        .px(px(RUNTIME_VIEW_PADDING))
        .flex()
        .items_center()
        .border_b_1()
        .border_color(rgb(theme.border))
        .bg(rgb(theme.content))
        .hover(|this| this.bg(rgb(theme.current_line)))
        .text_size(px(12.0))
        .line_height(px(18.0))
}

/// 渲染可选择复制的固定宽度文本单元格。
fn render_selectable_cell(
    analysis_id: usize,
    cell_key: String,
    text: String,
    width: f32,
    selection: Option<&RuntimeTableCellSelection>,
    analysis_focus_handle: Option<FocusHandle>,
    theme: &AppTheme,
    cx: &mut Context<ArgusApp>,
) -> AnyElement {
    let display_text = runtime_cell_display_text(&text);
    div()
        .w(px(width))
        .flex_none()
        .min_w(px(0.0))
        .relative()
        .pr_2()
        .overflow_hidden()
        .text_color(rgb(theme.foreground))
        .child(
            div()
                .min_w(px(0.0))
                .truncate()
                .child(render_runtime_cell_text(
                    display_text.clone(),
                    runtime_cell_selection_range(selection, &cell_key),
                    theme,
                )),
        )
        .child(render_runtime_cell_pointer_layer(
            analysis_id,
            cell_key,
            display_text,
            ARGUS_UI_FONT_FAMILY,
            analysis_focus_handle,
            cx,
        ))
        .into_any_element()
}

/// 渲染可选择复制并在自身内部横向滚动的长文本单元格。
fn render_selectable_scroll_cell(
    analysis_id: usize,
    id: String,
    cell_key: String,
    text: String,
    row_height: f32,
    selection: Option<&RuntimeTableCellSelection>,
    analysis_focus_handle: Option<FocusHandle>,
    theme: &AppTheme,
    cx: &mut Context<ArgusApp>,
) -> AnyElement {
    render_selectable_scroll_cell_with_font(
        analysis_id,
        id,
        cell_key,
        text,
        row_height,
        ARGUS_UI_FONT_FAMILY,
        selection,
        analysis_focus_handle,
        theme,
        cx,
    )
}

/// 渲染指定字体的可选择长文本单元格，SQL 文本列使用等宽字体。
fn render_selectable_scroll_cell_with_font(
    analysis_id: usize,
    id: String,
    cell_key: String,
    text: String,
    row_height: f32,
    font_family: &'static str,
    selection: Option<&RuntimeTableCellSelection>,
    analysis_focus_handle: Option<FocusHandle>,
    theme: &AppTheme,
    cx: &mut Context<ArgusApp>,
) -> AnyElement {
    let display_text = runtime_cell_display_text(&text);
    div()
        .id(SharedString::from(id))
        .flex_1()
        .min_w(px(0.0))
        .h(px(row_height - 2.0))
        .relative()
        .flex()
        .items_center()
        .overflow_x_scroll()
        .scrollbar_width(px(4.0))
        .font_family(font_family)
        .text_color(rgb(theme.foreground))
        .child(
            div()
                .flex_none()
                .pr_2()
                .whitespace_nowrap()
                .child(render_runtime_cell_text(
                    display_text.clone(),
                    runtime_cell_selection_range(selection, &cell_key),
                    theme,
                )),
        )
        .child(render_runtime_cell_pointer_layer(
            analysis_id,
            cell_key,
            display_text,
            font_family,
            analysis_focus_handle,
            cx,
        ))
        .into_any_element()
}

/// 将 Runtime 单元格内容转换为 GPUI 单行文本。
///
/// GPUI 的 `shape_line` 和普通文本节点都要求输入不包含换行；Runtime SQL 原文可能跨行，
/// 因此 UI 单元格统一折叠换行并保留原有词序，过滤和聚合仍继续使用解析层的原始 SQL。
fn runtime_cell_display_text(text: &str) -> String {
    if !text.contains('\n') && !text.contains('\r') {
        return text.to_string();
    }

    text.replace('\r', "\n")
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

/// 将完整 SQL 原文拆成弹窗代码区渲染行，保留空行和缩进。
fn runtime_sql_dialog_lines(sql_text: &str) -> Vec<String> {
    sql_text
        .replace("\r\n", "\n")
        .replace('\r', "\n")
        .split('\n')
        .map(ToString::to_string)
        .collect()
}

/// 返回 SQL 弹窗选区覆盖指定行的字符范围。
fn runtime_sql_dialog_selection_range_for_line(
    selection: Option<&RuntimeSqlTextSelection>,
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
    (start_column < end_column).then_some(start_column..end_column)
}

/// 根据点击次数转换 SQL 弹窗文本选择粒度。
fn runtime_sql_dialog_granularity_for_click_count(click_count: usize) -> TextSelectionGranularity {
    match click_count {
        0 | 1 => TextSelectionGranularity::Character,
        2 => TextSelectionGranularity::Word,
        _ => TextSelectionGranularity::Line,
    }
}

/// 根据鼠标位置计算 SQL 弹窗文本行中的字符列，兼容自动换行后的视觉行命中。
fn runtime_sql_dialog_character_index_from_pointer(
    line: &str,
    pointer_position: gpui::Point<Pixels>,
    bounds: gpui::Bounds<Pixels>,
    window: &mut Window,
) -> usize {
    let text_relative_x = pointer_position.x - bounds.left();
    let text_relative_y = pointer_position.y - bounds.top();
    if line.is_empty() || text_relative_x <= px(0.0) {
        return 0;
    }

    let mut text_style = window.text_style();
    text_style.font_family = ARGUS_LOG_FONT_FAMILY.into();
    text_style.font_size = px(12.0).into();
    let font_size = text_style.font_size.to_pixels(window.rem_size());
    let line_height = px(RUNTIME_SQL_DIALOG_LINE_HEIGHT);
    let run = TextRun {
        len: line.len(),
        font: text_style.font(),
        color: text_style.color,
        background_color: None,
        underline: None,
        strikethrough: None,
    };
    // SQL 弹窗正文启用了自动换行，必须把 y 坐标一起交给 wrapped layout，
    // 否则点击第二条视觉行时会被当成第一条视觉行同一 x 位置。
    if let Ok(mut wrapped_lines) = window.text_system().shape_text(
        SharedString::from(line.to_string()),
        font_size,
        &[run.clone()],
        Some(bounds.size.width.max(px(1.0))),
        None,
    ) && let Some(wrapped_line) = wrapped_lines.pop()
    {
        let byte_index = wrapped_line
            .closest_index_for_position(point(text_relative_x, text_relative_y), line_height)
            .unwrap_or_else(|index| index);
        return char_column_for_byte_index(line, byte_index).min(character_count(line));
    }

    let shaped_line = window.text_system().shape_line(
        SharedString::from(line.to_string()),
        font_size,
        &[run],
        None,
    );
    let byte_index = shaped_line.closest_index_for_x(text_relative_x);
    char_column_for_byte_index(line, byte_index).min(character_count(line))
}

/// 渲染 Runtime 单元格文本并叠加当前选区高亮。
fn render_runtime_cell_text(
    text: String,
    selection_range: Option<Range<usize>>,
    theme: &AppTheme,
) -> AnyElement {
    let Some(selection_range) = selection_range else {
        return text.into_any_element();
    };

    let start = byte_index_for_character(&text, selection_range.start);
    let end = byte_index_for_character(&text, selection_range.end);
    if start >= end {
        return text.into_any_element();
    }

    StyledText::new(text)
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

/// 返回当前选区在指定单元格内的字符范围。
fn runtime_cell_selection_range(
    selection: Option<&RuntimeTableCellSelection>,
    cell_key: &str,
) -> Option<Range<usize>> {
    selection
        .filter(|selection| selection.cell_key == cell_key)
        .and_then(RuntimeTableCellSelection::normalized_range)
}

/// 渲染 Runtime 表格单元格透明鼠标命中层，负责把拖拽选择转换成应用状态。
fn render_runtime_cell_pointer_layer(
    analysis_id: usize,
    cell_key: String,
    text: String,
    font_family: &'static str,
    analysis_focus_handle: Option<FocusHandle>,
    cx: &mut Context<ArgusApp>,
) -> impl IntoElement + use<> {
    let entity = cx.entity();
    div()
        .id(SharedString::from(format!(
            "runtime-cell-pointer-{analysis_id}-{cell_key}"
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
                        let cell_key = cell_key.clone();
                        let text = text.clone();
                        let font_family = font_family;
                        let analysis_focus_handle = analysis_focus_handle.clone();
                        move |event: &MouseDownEvent, phase, window, cx| {
                            if !phase.bubble()
                                || event.button != MouseButton::Left
                                || !visible_bounds.contains(&event.position)
                            {
                                return;
                            }

                            let character_index = runtime_cell_character_index_from_pointer(
                                &text,
                                font_family,
                                event.position.x,
                                bounds,
                                window,
                            );
                            let granularity =
                                runtime_cell_granularity_for_click_count(event.click_count);
                            if let Some(focus_handle) = analysis_focus_handle.as_ref() {
                                focus_handle.focus(window);
                            }
                            entity.update(cx, |app, _| {
                                app.begin_runtime_cell_selection(
                                    analysis_id,
                                    cell_key.clone(),
                                    text.clone(),
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
                        let cell_key = cell_key.clone();
                        let text = text.clone();
                        let font_family = font_family;
                        move |event: &MouseMoveEvent, phase, window, cx| {
                            if !phase.bubble() || !event.dragging() {
                                return;
                            }

                            let character_index = runtime_cell_character_index_from_pointer(
                                &text,
                                font_family,
                                event.position.x,
                                bounds,
                                window,
                            );
                            let handled = entity.update(cx, |app, _| {
                                app.update_runtime_cell_selection(
                                    analysis_id,
                                    &cell_key,
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
                                app.finish_runtime_cell_selection(analysis_id)
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

/// 根据点击次数转换 Runtime 单元格选择粒度；双击按需求选中整格内容。
fn runtime_cell_granularity_for_click_count(click_count: usize) -> TextSelectionGranularity {
    match click_count {
        0 | 1 => TextSelectionGranularity::Character,
        _ => TextSelectionGranularity::Line,
    }
}

/// 根据鼠标横坐标计算 Runtime 单元格文本中的字符列。
fn runtime_cell_character_index_from_pointer(
    text: &str,
    font_family: &'static str,
    pointer_x: Pixels,
    bounds: gpui::Bounds<Pixels>,
    window: &mut Window,
) -> usize {
    let text_relative_x = pointer_x - bounds.left() - px(RUNTIME_CELL_HORIZONTAL_PADDING);
    if text.is_empty() || text_relative_x <= px(0.0) {
        return 0;
    }

    let mut text_style = window.text_style();
    text_style.font_family = font_family.into();
    text_style.font_size = px(12.0).into();
    let run = TextRun {
        len: text.len(),
        font: text_style.font(),
        color: text_style.color,
        background_color: None,
        underline: None,
        strikethrough: None,
    };
    let shaped_line = window.text_system().shape_line(
        SharedString::from(text.to_string()),
        text_style.font_size.to_pixels(window.rem_size()),
        &[run],
        None,
    );
    let byte_index = shaped_line.closest_index_for_x(text_relative_x);
    char_column_for_byte_index(text, byte_index).min(character_count(text))
}

/// 生成 Runtime 普通表格单元格稳定 key。
fn runtime_cell_key(scope: &str, row_index: usize, column: &str) -> String {
    format!("{scope}:{row_index}:{column}")
}

/// 生成 Runtime SQL 表格单元格稳定 key。
fn runtime_sql_cell_key(request_index: usize, sql_index: usize, column: &str) -> String {
    format!("sql:{request_index}:{sql_index}:{column}")
}

/// 渲染操作按钮。
fn render_action_button(
    id: String,
    label: &'static str,
    theme: &AppTheme,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    div()
        .id(SharedString::from(id))
        .h(px(24.0))
        .px_1()
        .flex()
        .items_center()
        .justify_center()
        .rounded_sm()
        .cursor_pointer()
        .hover(|this| this.bg(rgb(theme.current_line)))
        .text_size(px(12.0))
        .line_height(px(24.0))
        .text_color(rgb(theme.info))
        .child(label)
        .on_click(on_click)
}

/// 渲染返回按钮。
fn render_back_button(
    id: &'static str,
    label: &'static str,
    theme: &AppTheme,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    div()
        .id(id)
        .h(px(26.0))
        .px_1()
        .flex()
        .items_center()
        .gap_1()
        .rounded_sm()
        .cursor_pointer()
        .hover(|this| this.bg(rgb(theme.current_line)))
        .text_size(px(12.0))
        .line_height(px(26.0))
        .text_color(rgb(theme.info))
        .child(render_icon(ArgusIcon::ArrowLeft, theme.info, 13.0))
        .child(label)
        .on_click(on_click)
}

/// 渲染空信息。
fn render_empty_message(message: &str, theme: &AppTheme) -> AnyElement {
    div()
        .flex_1()
        .flex()
        .items_center()
        .justify_center()
        .text_size(px(13.0))
        .text_color(rgb(theme.foreground_muted))
        .child(message.to_string())
        .into_any_element()
}

/// Runtime 三层表格当前过滤条件类型，具体解析逻辑复用领域层实现。
type RuntimeFilterCriteria = RuntimeAnalysisFilterCriteria;

/// 从 Runtime 分析状态中构造过滤条件。
fn runtime_filter_criteria(state: &RuntimeAnalysisState) -> RuntimeFilterCriteria {
    parse_runtime_analysis_filter_criteria(&runtime_sql_analysis_filter_snapshot(state))
}

/// 从 Runtime 分析状态中提取 SQL 分析缓存用的原始过滤输入快照。
fn runtime_sql_analysis_filter_snapshot(
    state: &RuntimeAnalysisState,
) -> RuntimeSqlAnalysisFilterSnapshot {
    RuntimeSqlAnalysisFilterSnapshot {
        keyword: state.applied_filter_keyword.clone(),
        username: state.applied_filter_username.clone(),
        start_time: state.applied_filter_start_time.clone(),
        end_time: state.applied_filter_end_time.clone(),
    }
}

/// 从 Runtime 过滤输入框提取当前输入快照，供状态栏展示待应用状态。
fn runtime_filter_input_snapshot(state: &RuntimeAnalysisState) -> RuntimeSqlAnalysisFilterSnapshot {
    RuntimeSqlAnalysisFilterSnapshot {
        keyword: state.filter_keyword_input.value.clone(),
        username: state.filter_username_input.value.clone(),
        start_time: state.filter_start_time_input.value.clone(),
        end_time: state.filter_end_time_input.value.clone(),
    }
}

/// 返回 SQL 频率分析过滤并排序后的行。
fn sql_frequency_rows(
    _result: &RuntimeAnalysisResult,
    state: &RuntimeAnalysisState,
) -> Arc<Vec<RuntimeSqlFrequencyAnalysisRow>> {
    let filter = runtime_sql_analysis_filter_snapshot(state);
    if let Some(cache) = state.sql_frequency_rows_cache.borrow().as_ref()
        && cache.filter == filter
    {
        return cache.rows.clone();
    }
    Arc::new(Vec::new())
}

/// 返回指定 SQL 结构的执行详情行，并按过滤条件缓存结果。
fn sql_frequency_detail_rows(
    result: &RuntimeAnalysisResult,
    state: &RuntimeAnalysisState,
    normalized_sql: &str,
) -> Arc<Vec<RuntimeSqlFrequencyDetailRow>> {
    let criteria = runtime_filter_criteria(state);
    let filter = runtime_sql_analysis_filter_snapshot(state);
    if let Some(cache) = state.sql_frequency_detail_rows_cache.borrow().as_ref()
        && cache.filter == filter
        && cache.normalized_sql == normalized_sql
    {
        return cache.rows.clone();
    }

    let mut rows = Vec::new();
    if let Some(cache) = state.runtime_filter_rows_cache.as_ref()
        && cache.filter == filter
    {
        for (request_index, sql_indices) in cache.sql_indices_by_request.iter() {
            let Some(request) = result.requests.get(*request_index) else {
                continue;
            };
            for sql_index in sql_indices {
                let Some(sql) = request.sql_records.get(*sql_index) else {
                    continue;
                };
                if sql.normalized_sql != normalized_sql {
                    continue;
                }
                rows.push(RuntimeSqlFrequencyDetailRow {
                    request_index: request.index,
                    sql_index: *sql_index,
                    execute_ms: sql.execute_ms,
                });
            }
        }
    } else {
        for request in &result.requests {
            if !runtime_request_matches_cross_filters(request, &criteria) {
                continue;
            }

            for (sql_index, sql) in request.sql_records.iter().enumerate() {
                if sql.normalized_sql != normalized_sql {
                    continue;
                }
                if !runtime_sql_matches_keyword(request, sql, &criteria) {
                    continue;
                }

                rows.push(RuntimeSqlFrequencyDetailRow {
                    request_index: request.index,
                    sql_index,
                    execute_ms: sql.execute_ms,
                });
            }
        }
    }

    sort_runtime_sql_frequency_detail_rows(&mut rows);
    let rows = Arc::new(rows);
    state.sql_frequency_detail_rows_cache.borrow_mut().replace(
        RuntimeSqlFrequencyDetailRowsCache {
            filter,
            normalized_sql: normalized_sql.to_string(),
            rows: rows.clone(),
        },
    );
    rows
}

/// 返回慢 SQL 分析过滤并按平均执行耗时降序排列后的聚合行。
fn slow_sql_rows(
    _result: &RuntimeAnalysisResult,
    state: &RuntimeAnalysisState,
) -> Arc<Vec<RuntimeSlowSqlSummaryRow>> {
    let filter = runtime_sql_analysis_filter_snapshot(state);
    if let Some(cache) = state.slow_sql_rows_cache.borrow().as_ref()
        && cache.filter == filter
    {
        return cache.rows.clone();
    }
    Arc::new(Vec::new())
}

/// 返回总览表过滤并排序后的聚合行。
fn sorted_summary_rows(
    result: &RuntimeAnalysisResult,
    state: &RuntimeAnalysisState,
) -> Vec<RuntimeRequestSummary> {
    let criteria = runtime_filter_criteria(state);
    let filter = runtime_sql_analysis_filter_snapshot(state);
    let mut rows = if criteria.is_active() {
        state
            .runtime_filter_rows_cache
            .as_ref()
            .filter(|cache| cache.filter == filter)
            .map(|cache| cache.summaries.as_ref().clone())
            .unwrap_or_else(|| {
                result
                    .summaries
                    .iter()
                    .filter_map(|summary| {
                        filtered_summary_from_indices(result, summary, &criteria, true)
                    })
                    .collect::<Vec<_>>()
            })
    } else {
        result.summaries.clone()
    };

    rows.sort_by(|left, right| {
        let ordering = match state.summary_sort_key {
            RuntimeSummarySortKey::RequestCount => left.request_count.cmp(&right.request_count),
            RuntimeSummarySortKey::RequestPath => left.request_path.cmp(&right.request_path),
            RuntimeSummarySortKey::AverageDuration => {
                compare_f64(left.average_duration_ms, right.average_duration_ms)
            }
            RuntimeSummarySortKey::SlowSqlRatio => {
                compare_f64(left.slow_sql_ratio, right.slow_sql_ratio)
            }
        };
        apply_sort_direction(ordering, state.summary_sort_direction)
            .then_with(|| left.request_path.cmp(&right.request_path))
    });
    rows
}

/// 返回指定请求地址在当前过滤条件下的聚合行，供详情页顶部和明细列表使用。
fn filtered_summary_for_request_path(
    result: &RuntimeAnalysisResult,
    request_path: &str,
    state: &RuntimeAnalysisState,
) -> Option<RuntimeRequestSummary> {
    let criteria = runtime_filter_criteria(state);
    let filter = runtime_sql_analysis_filter_snapshot(state);
    if criteria.is_active()
        && let Some(cache) = state.runtime_filter_rows_cache.as_ref()
        && cache.filter == filter
    {
        return cache
            .summaries
            .iter()
            .find(|summary| summary.request_path == request_path)
            .cloned();
    }
    result
        .summaries
        .iter()
        .find(|summary| summary.request_path == request_path)
        .and_then(|summary| filtered_summary_from_indices(result, summary, &criteria, false))
}

/// 从原始 summary 的请求索引中应用跨表格过滤并重新计算聚合统计。
fn filtered_summary_from_indices(
    result: &RuntimeAnalysisResult,
    summary: &RuntimeRequestSummary,
    criteria: &RuntimeFilterCriteria,
    apply_keyword: bool,
) -> Option<RuntimeRequestSummary> {
    filtered_runtime_summary_from_indices(result, summary, criteria, apply_keyword)
}

/// 返回请求明细表排序后的请求索引。
fn sorted_request_indices(
    result: &RuntimeAnalysisResult,
    summary: &RuntimeRequestSummary,
    state: &RuntimeAnalysisState,
) -> Vec<usize> {
    let criteria = runtime_filter_criteria(state);
    let filter = runtime_sql_analysis_filter_snapshot(state);
    let mut indices = if criteria.is_active()
        && state
            .runtime_filter_rows_cache
            .as_ref()
            .is_some_and(|cache| cache.filter == filter)
    {
        summary.request_indices.clone()
    } else {
        summary
            .request_indices
            .iter()
            .copied()
            .filter(|index| {
                result.requests.get(*index).is_some_and(|request| {
                    runtime_request_matches_cross_filters(request, &criteria)
                        && runtime_request_matches_keyword(request, &criteria)
                })
            })
            .collect::<Vec<_>>()
    };
    indices.sort_by(|left_index, right_index| {
        let left = &result.requests[*left_index];
        let right = &result.requests[*right_index];
        let ordering = match state.request_sort_key {
            RuntimeRequestSortKey::RequestTime => {
                left.request_timestamp_ms.cmp(&right.request_timestamp_ms)
            }
            RuntimeRequestSortKey::Username => left.username.cmp(&right.username),
            RuntimeRequestSortKey::RequestDuration => {
                left.request_duration_ms.cmp(&right.request_duration_ms)
            }
            RuntimeRequestSortKey::RequestPath => left.request_path.cmp(&right.request_path),
        };
        apply_sort_direction(ordering, state.request_sort_direction)
            .then_with(|| right.request_timestamp_ms.cmp(&left.request_timestamp_ms))
            .then_with(|| left.label.cmp(&right.label))
    });
    indices
}

/// 返回 SQL 明细表排序后的 SQL 索引。
fn sorted_sql_indices(request: &RuntimeRequestRecord, state: &RuntimeAnalysisState) -> Vec<usize> {
    let criteria = runtime_filter_criteria(state);
    if !runtime_request_matches_cross_filters(request, &criteria) {
        return Vec::new();
    }

    let filter = runtime_sql_analysis_filter_snapshot(state);
    let mut indices = if criteria.is_active()
        && let Some(cache) = state.runtime_filter_rows_cache.as_ref()
        && cache.filter == filter
    {
        cache
            .sql_indices_by_request
            .get(&request.index)
            .cloned()
            .unwrap_or_default()
    } else {
        (0..request.sql_records.len())
            .filter(|sql_index| {
                request
                    .sql_records
                    .get(*sql_index)
                    .is_some_and(|sql| runtime_sql_matches_keyword(request, sql, &criteria))
            })
            .collect::<Vec<_>>()
    };
    indices.sort_by(|left_index, right_index| {
        let left = &request.sql_records[*left_index];
        let right = &request.sql_records[*right_index];
        let ordering = match state.sql_sort_key {
            RuntimeSqlSortKey::ExecuteDuration => left.execute_ms.cmp(&right.execute_ms),
            RuntimeSqlSortKey::AcquireConnectionDuration => {
                left.acquire_connection_ms.cmp(&right.acquire_connection_ms)
            }
            RuntimeSqlSortKey::CommitDuration => left.commit_ms.cmp(&right.commit_ms),
            RuntimeSqlSortKey::ReleaseConnectionDuration => {
                left.release_connection_ms.cmp(&right.release_connection_ms)
            }
            RuntimeSqlSortKey::ParseResultDuration => {
                left.parse_result_ms.cmp(&right.parse_result_ms)
            }
            RuntimeSqlSortKey::SqlText => left.sql_text.cmp(&right.sql_text),
        };
        apply_sort_direction(ordering, state.sql_sort_direction)
            .then_with(|| left_index.cmp(right_index))
    });
    indices
}

/// 判断请求是否命中跨表格过滤条件：用户名和时间区间。
fn runtime_request_matches_cross_filters(
    request: &RuntimeRequestRecord,
    criteria: &RuntimeFilterCriteria,
) -> bool {
    domain_runtime_request_matches_cross_filters(request, criteria)
}

/// 判断请求明细行是否命中关键字。
fn runtime_request_matches_keyword(
    request: &RuntimeRequestRecord,
    criteria: &RuntimeFilterCriteria,
) -> bool {
    domain_runtime_request_matches_keyword(request, criteria)
}

/// 判断 SQL 明细行是否命中关键字；同时纳入所属请求元信息，便于跨层检索。
fn runtime_sql_matches_keyword(
    request: &RuntimeRequestRecord,
    sql: &RuntimeSqlRecord,
    criteria: &RuntimeFilterCriteria,
) -> bool {
    domain_runtime_sql_matches_keyword(request, sql, criteria)
}

/// 返回日期时间选择器当前展示值；输入为空或非法时使用当天边界作为默认值。
fn runtime_datetime_picker_value(
    input: &SettingsTextInputState,
    is_end: bool,
) -> DateTimePickerValue {
    let datetime = parse_runtime_filter_time_value(&input.value, is_end)
        .and_then(|timestamp_ms| Local.timestamp_millis_opt(timestamp_ms).single())
        .unwrap_or_else(|| default_runtime_datetime_picker_datetime(is_end));

    DateTimePickerValue {
        year: datetime.year(),
        month: datetime.month(),
        day: datetime.day(),
        hour: datetime.hour(),
        minute: datetime.minute(),
        second: datetime.second(),
    }
}

/// 返回日期时间选择器在输入为空时使用的默认时间。
fn default_runtime_datetime_picker_datetime(is_end: bool) -> chrono::DateTime<Local> {
    let now = Local::now();
    let naive = if is_end {
        now.date_naive().and_hms_opt(23, 59, 59)
    } else {
        now.date_naive().and_hms_opt(0, 0, 0)
    };
    naive
        .and_then(|datetime| Local.from_local_datetime(&datetime).single())
        .unwrap_or(now)
}

/// 返回 Runtime 过滤栏的状态提示。
fn runtime_filter_status_label(state: &RuntimeAnalysisState) -> String {
    let input_filter = runtime_filter_input_snapshot(state);
    let applied_filter = runtime_sql_analysis_filter_snapshot(state);
    if state.is_filter_computing {
        return "正在应用过滤条件".to_string();
    }
    if state.is_filter_pending || input_filter != applied_filter {
        return "过滤条件待应用".to_string();
    }

    let criteria = runtime_filter_criteria(state);
    if !criteria.is_active() {
        return "未启用过滤".to_string();
    }

    let mut parts = Vec::new();
    if !criteria.keyword.is_empty() {
        parts.push(format!("关键字：{}", state.applied_filter_keyword.trim()));
    }
    if !criteria.usernames.is_empty() {
        parts.push(format!("用户：{}", state.applied_filter_username.trim()));
    }
    if criteria.start_timestamp_ms.is_some() {
        parts.push(format!("开始：{}", state.applied_filter_start_time.trim()));
    }
    if criteria.end_timestamp_ms.is_some() {
        parts.push(format!("结束：{}", state.applied_filter_end_time.trim()));
    }
    parts.join("，")
}

/// 返回 Runtime 过滤输入框的规范化非空选区。
fn runtime_filter_input_selection_range(
    input: &SettingsTextInputState,
) -> Option<std::ops::Range<usize>> {
    let anchor = input.selection_anchor?;
    if anchor == input.cursor {
        return None;
    }
    Some(anchor.min(input.cursor)..anchor.max(input.cursor))
}

/// 返回 Runtime 过滤输入框使用的焦点句柄。
fn runtime_filter_focus_handle(
    app: &ArgusApp,
    input_kind: RuntimeFilterInputKind,
) -> Option<FocusHandle> {
    let handles = app.input_focus_handles.as_ref()?;
    Some(match input_kind {
        RuntimeFilterInputKind::Keyword => handles.runtime_filter_keyword.clone(),
        RuntimeFilterInputKind::Username => handles.runtime_filter_username.clone(),
        RuntimeFilterInputKind::StartTime => handles.runtime_filter_start_time.clone(),
        RuntimeFilterInputKind::EndTime => handles.runtime_filter_end_time.clone(),
    })
}

/// Runtime 过滤输入框主体 ID。
fn runtime_filter_input_id(input_kind: RuntimeFilterInputKind) -> &'static str {
    match input_kind {
        RuntimeFilterInputKind::Keyword => "runtime-filter-keyword-input",
        RuntimeFilterInputKind::Username => "runtime-filter-username-input",
        RuntimeFilterInputKind::StartTime => "runtime-filter-start-time-input",
        RuntimeFilterInputKind::EndTime => "runtime-filter-end-time-input",
    }
}

/// Runtime 过滤输入框前置图标 ID。
fn runtime_filter_leading_id(input_kind: RuntimeFilterInputKind) -> &'static str {
    match input_kind {
        RuntimeFilterInputKind::Keyword => "runtime-filter-keyword-leading",
        RuntimeFilterInputKind::Username => "runtime-filter-username-leading",
        RuntimeFilterInputKind::StartTime => "runtime-filter-start-time-leading",
        RuntimeFilterInputKind::EndTime => "runtime-filter-end-time-leading",
    }
}

/// Runtime 过滤输入框清除按钮 ID。
fn runtime_filter_clear_id(input_kind: RuntimeFilterInputKind) -> &'static str {
    match input_kind {
        RuntimeFilterInputKind::Keyword => "runtime-filter-clear-keyword",
        RuntimeFilterInputKind::Username => "runtime-filter-clear-username",
        RuntimeFilterInputKind::StartTime => "runtime-filter-clear-start-time",
        RuntimeFilterInputKind::EndTime => "runtime-filter-clear-end-time",
    }
}

/// 根据排序方向翻转比较结果。
fn apply_sort_direction(ordering: Ordering, direction: RuntimeSortDirection) -> Ordering {
    match direction {
        RuntimeSortDirection::Ascending => ordering,
        RuntimeSortDirection::Descending => ordering.reverse(),
    }
}

/// 比较浮点统计值；当前统计值均为有限数，异常时按相等兜底。
fn compare_f64(left: f64, right: f64) -> Ordering {
    left.partial_cmp(&right).unwrap_or(Ordering::Equal)
}

/// 格式化整数毫秒耗时。
fn format_duration(duration_ms: u64) -> String {
    format!("{duration_ms} ms")
}

/// 格式化平均耗时，保留一位小数。
fn format_average_duration(duration_ms: f64) -> String {
    format!("{duration_ms:.1} ms")
}

/// 格式化比例，保留一位百分比。
fn format_ratio(ratio: f64) -> String {
    format!("{:.1}%", ratio * 100.0)
}

/// 返回用户名展示文本。
fn display_username(username: &str) -> String {
    if username.is_empty() {
        "-".to_string()
    } else {
        username.to_string()
    }
}

/// 根据滚动状态绘制可见滚动条。
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
    if let Some(metrics) =
        runtime_scrollbar_metrics(bounds.size.height, content_height, -scroll_offset.y)
    {
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

/// Runtime 滚动条滑块指标。
#[derive(Clone, Copy, Debug, PartialEq)]
struct RuntimeScrollbarMetrics {
    /// 滑块起始位置。
    thumb_start: Pixels,
    /// 滑块长度。
    thumb_length: Pixels,
    /// 轨道起始位置。
    track_start: Pixels,
    /// 轨道长度。
    track_length: Pixels,
    /// 最大滚动距离。
    max_scroll: Pixels,
}

/// 根据视口、内容高度和当前滚动量计算 Runtime 滚动条滑块指标。
fn runtime_scrollbar_metrics(
    viewport_length: gpui::Pixels,
    content_length: gpui::Pixels,
    scroll_offset: gpui::Pixels,
) -> Option<RuntimeScrollbarMetrics> {
    if viewport_length == px(0.0) || content_length <= viewport_length {
        return None;
    }

    let track_padding = px(RUNTIME_SCROLLBAR_PADDING);
    let track_length = (viewport_length - track_padding * 2.0).max(px(1.0));
    let thumb_length = ((viewport_length / content_length) * track_length)
        .clamp(px(RUNTIME_SCROLLBAR_MIN_THUMB), track_length);
    let max_scroll = (content_length - viewport_length).max(px(1.0));
    let movable_length = (track_length - thumb_length).max(px(0.0));
    let scroll_ratio = (scroll_offset / max_scroll).clamp(0.0, 1.0);
    let thumb_start = track_padding + movable_length * scroll_ratio;

    Some(RuntimeScrollbarMetrics {
        thumb_start,
        thumb_length,
        track_start: track_padding,
        track_length,
        max_scroll,
    })
}

/// 根据拖拽中的鼠标位置换算 Runtime 表格目标滚动距离。
fn runtime_scroll_for_scrollbar_drag(
    pointer: Pixels,
    cursor_offset: Pixels,
    metrics: RuntimeScrollbarMetrics,
) -> Pixels {
    let movable_length = (metrics.track_length - metrics.thumb_length).max(px(1.0));
    let thumb_start =
        (pointer - cursor_offset).clamp(metrics.track_start, metrics.track_start + movable_length);
    let ratio = (thumb_start - metrics.track_start) / movable_length;
    metrics.max_scroll * ratio
}

/// 绘制 Runtime 可拖拽滚动条滑块。
fn render_runtime_scrollbar_thumb(
    analysis_id: usize,
    table: RuntimeScrollbarTable,
    target: RuntimeScrollTarget,
    metrics: RuntimeScrollbarMetrics,
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
                            let scroll = runtime_scroll_for_scrollbar_drag(
                                pointer,
                                drag.cursor_offset,
                                metrics,
                            );
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

    use crate::app::{
        RuntimeAnalysisResultType, RuntimeAnalysisTaskState, RuntimeAnalysisView,
        RuntimeSlowSqlRowsCache, RuntimeSortDirection, RuntimeSqlFrequencyRowsCache,
        RuntimeSqlSortKey, RuntimeSummarySortKey,
    };
    use crate::loader::SourceId;
    use crate::runtime_analysis::{
        build_runtime_analysis_result, build_runtime_slow_sql_rows_for_filter,
        build_runtime_sql_frequency_rows_for_filter, parse_runtime_request_text,
    };

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
            scrollbar_drag: None,
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
        let top = runtime_scrollbar_metrics(px(400.0), px(1200.0), px(0.0))
            .expect("应生成顶部滚动条指标");
        let middle = runtime_scrollbar_metrics(px(400.0), px(1200.0), px(360.0))
            .expect("应生成中部滚动条指标");

        assert_eq!(top.thumb_length, middle.thumb_length);
        assert!(middle.thumb_start > top.thumb_start);
    }

    /// 验证 Runtime 滚动条拖拽会按轨道比例换算为目标滚动距离。
    #[test]
    fn runtime_scrollbar_drag_converts_pointer_to_scroll() {
        let metrics =
            runtime_scrollbar_metrics(px(400.0), px(1200.0), px(0.0)).expect("应生成滚动条指标");
        let scroll =
            runtime_scroll_for_scrollbar_drag(metrics.track_start + px(40.0), px(10.0), metrics);

        assert!(scroll > px(0.0));
        assert!(scroll < metrics.max_scroll);
    }
}
