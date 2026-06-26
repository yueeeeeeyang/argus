//! 文件职责：渲染 Runtime 请求日志分析页签内容。
//! 创建日期：2026-06-25
//! 修改日期：2026-06-25
//! 作者：Argus 开发团队
//! 主要功能：展示 Runtime 请求总览、请求详情和 SQL 明细三层可排序表格。

use std::cmp::Ordering;
use std::ops::Range;
use std::sync::Arc;

use chrono::{Datelike, Local, NaiveDate, NaiveDateTime, TimeZone, Timelike};

use crate::app::{
    AppTextInputTarget, ArgusApp, RUNTIME_SQL_COLLAPSED_ROW_HEIGHT, RuntimeAnalysisState,
    RuntimeAnalysisTaskState, RuntimeAnalysisView, RuntimeFilterInputKind, RuntimeRequestSortKey,
    RuntimeScrollbarDrag, RuntimeScrollbarTable, RuntimeSortDirection, RuntimeSqlCellKey,
    RuntimeSqlSortKey, RuntimeSqlTextDialog, RuntimeSqlTextSelection, RuntimeSummarySortKey,
    RuntimeTableCellSelection, SettingsTextInputState,
};
use crate::fonts::{ARGUS_LOG_FONT_FAMILY, ARGUS_UI_FONT_FAMILY};
use crate::runtime_analysis::{
    RuntimeAnalysisResult, RuntimeRequestRecord, RuntimeRequestSummary, RuntimeSqlRecord,
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
    let content = match &state.view {
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
            app.clear_runtime_filter_input(analysis_id, input_kind);
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

/// Runtime 三层表格当前过滤条件，解析一次后供排序和行筛选复用。
#[derive(Clone, Debug, Default, Eq, PartialEq)]
struct RuntimeFilterCriteria {
    /// 表格关键字，空字符串表示不启用。
    keyword: String,
    /// 用户名过滤关键字列表，空列表表示不启用。
    usernames: Vec<String>,
    /// 开始时间戳，单位毫秒。
    start_timestamp_ms: Option<i64>,
    /// 结束时间戳，单位毫秒。
    end_timestamp_ms: Option<i64>,
}

impl RuntimeFilterCriteria {
    /// 返回是否配置了任意过滤条件。
    fn is_active(&self) -> bool {
        !self.keyword.is_empty()
            || !self.usernames.is_empty()
            || self.start_timestamp_ms.is_some()
            || self.end_timestamp_ms.is_some()
    }

    /// 判断关键字是否为空或命中文本。
    fn keyword_matches(&self, text: &str) -> bool {
        self.keyword.is_empty() || text.to_lowercase().contains(&self.keyword)
    }
}

/// 从 Runtime 分析状态中构造过滤条件。
fn runtime_filter_criteria(state: &RuntimeAnalysisState) -> RuntimeFilterCriteria {
    RuntimeFilterCriteria {
        keyword: state.filter_keyword_input.value.trim().to_lowercase(),
        usernames: parse_runtime_username_filters(&state.filter_username_input.value),
        start_timestamp_ms: parse_runtime_filter_time(&state.filter_start_time_input.value, false),
        end_timestamp_ms: parse_runtime_filter_time(&state.filter_end_time_input.value, true),
    }
}

/// 解析用户名过滤输入，支持英文逗号和中文逗号分隔多个模糊匹配关键字。
fn parse_runtime_username_filters(raw: &str) -> Vec<String> {
    raw.split([',', '，'])
        .map(|part| part.trim().to_lowercase())
        .filter(|part| !part.is_empty())
        .collect()
}

/// 返回总览表过滤并排序后的聚合行。
fn sorted_summary_rows(
    result: &RuntimeAnalysisResult,
    state: &RuntimeAnalysisState,
) -> Vec<RuntimeRequestSummary> {
    let criteria = runtime_filter_criteria(state);
    let mut rows = result
        .summaries
        .iter()
        .filter_map(|summary| filtered_summary_from_indices(result, summary, &criteria, true))
        .collect::<Vec<_>>();

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
    let mut request_indices = summary
        .request_indices
        .iter()
        .copied()
        .filter(|index| {
            result
                .requests
                .get(*index)
                .is_some_and(|request| runtime_request_matches_cross_filters(request, criteria))
        })
        .collect::<Vec<_>>();

    // 关键字过滤会影响聚合统计本身，而不是只决定总览行是否显示。
    // 这样命中 SQL 文本时，请求次数、平均耗时和慢 SQL 比例都与过滤后的明细保持一致。
    if apply_keyword && !criteria.keyword.is_empty() {
        let cross_filtered_summary = build_runtime_summary_from_indices(
            result,
            &summary.request_path,
            request_indices.clone(),
        )?;
        if !runtime_summary_fields_match_keyword(&cross_filtered_summary, criteria) {
            request_indices.retain(|index| {
                result
                    .requests
                    .get(*index)
                    .is_some_and(|request| runtime_request_matches_keyword(request, criteria))
            });
        }
    }

    build_runtime_summary_from_indices(result, &summary.request_path, request_indices)
}

/// 根据请求索引重新计算 Runtime 聚合行统计。
fn build_runtime_summary_from_indices(
    result: &RuntimeAnalysisResult,
    request_path: &str,
    request_indices: Vec<usize>,
) -> Option<RuntimeRequestSummary> {
    if request_indices.is_empty() {
        return None;
    }

    let request_count = request_indices.len();
    let total_duration = request_indices
        .iter()
        .filter_map(|index| result.requests.get(*index))
        .map(|request| request.request_duration_ms)
        .sum::<u64>();
    let slow_request_count = request_indices
        .iter()
        .filter_map(|index| result.requests.get(*index))
        .filter(|request| request.is_slow_sql_request)
        .count();
    let average_duration_ms = total_duration as f64 / request_count as f64;
    let slow_sql_ratio = slow_request_count as f64 / request_count as f64;

    Some(RuntimeRequestSummary {
        request_path: request_path.to_string(),
        request_count,
        average_duration_ms,
        slow_request_count,
        slow_sql_ratio,
        request_indices,
    })
}

/// 返回请求明细表排序后的请求索引。
fn sorted_request_indices(
    result: &RuntimeAnalysisResult,
    summary: &RuntimeRequestSummary,
    state: &RuntimeAnalysisState,
) -> Vec<usize> {
    let criteria = runtime_filter_criteria(state);
    let mut indices = summary
        .request_indices
        .iter()
        .copied()
        .filter(|index| {
            result.requests.get(*index).is_some_and(|request| {
                runtime_request_matches_cross_filters(request, &criteria)
                    && runtime_request_matches_keyword(request, &criteria)
            })
        })
        .collect::<Vec<_>>();
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

    let mut indices = (0..request.sql_records.len())
        .filter(|sql_index| {
            request
                .sql_records
                .get(*sql_index)
                .is_some_and(|sql| runtime_sql_matches_keyword(request, sql, &criteria))
        })
        .collect::<Vec<_>>();
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
    if !criteria.usernames.is_empty()
        && !criteria
            .usernames
            .iter()
            .any(|username| request.username.to_lowercase().contains(username))
    {
        return false;
    }
    if let Some(start_timestamp_ms) = criteria.start_timestamp_ms
        && request.request_timestamp_ms < start_timestamp_ms
    {
        return false;
    }
    if let Some(end_timestamp_ms) = criteria.end_timestamp_ms
        && request.request_timestamp_ms > end_timestamp_ms
    {
        return false;
    }
    true
}

/// 判断总览行自身可见字段是否命中关键字。
fn runtime_summary_fields_match_keyword(
    summary: &RuntimeRequestSummary,
    criteria: &RuntimeFilterCriteria,
) -> bool {
    let summary_text = format!(
        "{} {} {} {}",
        summary.request_count,
        summary.request_path,
        format_average_duration(summary.average_duration_ms),
        format_ratio(summary.slow_sql_ratio)
    );
    criteria.keyword_matches(&summary_text)
}

/// 判断请求明细行是否命中关键字。
fn runtime_request_matches_keyword(
    request: &RuntimeRequestRecord,
    criteria: &RuntimeFilterCriteria,
) -> bool {
    if criteria.keyword.is_empty() {
        return true;
    }

    let request_text = runtime_request_search_text(request);
    criteria.keyword_matches(&request_text)
        || request
            .sql_records
            .iter()
            .any(|sql| runtime_sql_matches_keyword(request, sql, criteria))
}

/// 判断 SQL 明细行是否命中关键字；同时纳入所属请求元信息，便于跨层检索。
fn runtime_sql_matches_keyword(
    request: &RuntimeRequestRecord,
    sql: &RuntimeSqlRecord,
    criteria: &RuntimeFilterCriteria,
) -> bool {
    if criteria.keyword.is_empty() {
        return true;
    }

    let sql_text = format!(
        "{} {} {} {} {} {} {}",
        runtime_request_search_text(request),
        format_duration(sql.execute_ms),
        format_duration(sql.acquire_connection_ms),
        format_duration(sql.commit_ms),
        format_duration(sql.release_connection_ms),
        format_duration(sql.parse_result_ms),
        sql.sql_text
    );
    criteria.keyword_matches(&sql_text)
}

/// 拼接请求记录可被关键字过滤命中的文本。
fn runtime_request_search_text(request: &RuntimeRequestRecord) -> String {
    format!(
        "{} {} {} {} {} {} {} {}",
        request.request_time_label,
        request.username,
        format_duration(request.request_duration_ms),
        request.request_path,
        request.label,
        request.path,
        format_duration(request.socket_duration_ms),
        format_duration(request.security_check_ms)
    )
}

/// 解析 Runtime 时间过滤输入；支持毫秒时间戳和常见本地时间格式。
fn parse_runtime_filter_time(raw: &str, is_end: bool) -> Option<i64> {
    let value = raw.trim();
    if value.is_empty() {
        return None;
    }
    if let Ok(timestamp_ms) = value.parse::<i64>() {
        return Some(timestamp_ms);
    }

    for format in [
        "%Y-%m-%d %H:%M:%S%.3f",
        "%Y-%m-%d %H:%M:%S",
        "%Y-%m-%d %H:%M",
    ] {
        if let Ok(datetime) = NaiveDateTime::parse_from_str(value, format)
            && let Some(local_datetime) = Local.from_local_datetime(&datetime).single()
        {
            return Some(local_datetime.timestamp_millis());
        }
    }

    if let Ok(date) = NaiveDate::parse_from_str(value, "%Y-%m-%d") {
        let datetime = if is_end {
            date.and_hms_milli_opt(23, 59, 59, 999)
        } else {
            date.and_hms_milli_opt(0, 0, 0, 0)
        }?;
        return Local
            .from_local_datetime(&datetime)
            .single()
            .map(|datetime| datetime.timestamp_millis());
    }

    None
}

/// 返回日期时间选择器当前展示值；输入为空或非法时使用当天边界作为默认值。
fn runtime_datetime_picker_value(
    input: &SettingsTextInputState,
    is_end: bool,
) -> DateTimePickerValue {
    let datetime = parse_runtime_filter_time(&input.value, is_end)
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
    let criteria = runtime_filter_criteria(state);
    if !criteria.is_active() {
        return "未启用过滤".to_string();
    }

    let mut parts = Vec::new();
    if !criteria.keyword.is_empty() {
        parts.push(format!(
            "关键字：{}",
            state.filter_keyword_input.value.trim()
        ));
    }
    if !criteria.usernames.is_empty() {
        parts.push(format!(
            "用户：{}",
            state.filter_username_input.value.trim()
        ));
    }
    if criteria.start_timestamp_ms.is_some() {
        parts.push(format!(
            "开始：{}",
            state.filter_start_time_input.value.trim()
        ));
    }
    if criteria.end_timestamp_ms.is_some() {
        parts.push(format!(
            "结束：{}",
            state.filter_end_time_input.value.trim()
        ));
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
    use gpui::UniformListScrollHandle;

    use crate::app::{
        RuntimeAnalysisTaskState, RuntimeAnalysisView, RuntimeSortDirection, RuntimeSqlSortKey,
        RuntimeSummarySortKey,
    };
    use crate::loader::SourceId;
    use crate::runtime_analysis::{build_runtime_analysis_result, parse_runtime_request_text};

    use super::*;

    /// 构造 Runtime 过滤测试用的默认 UI 状态。
    fn runtime_filter_test_state() -> RuntimeAnalysisState {
        RuntimeAnalysisState {
            id: 1,
            title: "Runtime分析".to_string(),
            targets: Vec::new(),
            generation: 1,
            view: RuntimeAnalysisView::Summary,
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
            open_time_picker: None,
            cell_selection: None,
            cell_selection_drag: None,
            hovered_sql_cell: None,
            sql_text_dialog: None,
            summary_scroll: UniformListScrollHandle::new(),
            request_scroll: UniformListScrollHandle::new(),
            sql_scroll: UniformListScrollHandle::new(),
            scrollbar_drag: None,
            task_state: RuntimeAnalysisTaskState::Loading {
                message: String::new(),
            },
        }
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

    /// 验证用户名和时间区间过滤会跨表格影响总览聚合统计。
    #[test]
    fn runtime_filters_summary_by_username_and_time_range() {
        let result = runtime_filter_test_result();
        let mut state = runtime_filter_test_state();
        state.filter_username_input.value = "alice".to_string();
        state.filter_start_time_input.value = "1500".to_string();
        state.filter_end_time_input.value = "2500".to_string();

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

        let rows = sorted_summary_rows(&result, &state);
        let sql_indices = sorted_sql_indices(&result.requests[1], &state);

        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].request_count, 2);
        assert_eq!(sql_indices, vec![0]);
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
