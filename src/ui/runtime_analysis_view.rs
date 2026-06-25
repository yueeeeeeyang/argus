//! 文件职责：渲染 Runtime 请求日志分析页签内容。
//! 创建日期：2026-06-25
//! 修改日期：2026-06-25
//! 作者：Argus 开发团队
//! 主要功能：展示 Runtime 请求总览、请求详情和 SQL 明细三层可排序表格。

use std::cmp::Ordering;
use std::ops::Range;
use std::sync::Arc;

use crate::app::{
    ArgusApp, RuntimeAnalysisState, RuntimeAnalysisTaskState, RuntimeAnalysisView,
    RuntimeRequestSortKey, RuntimeSortDirection, RuntimeSqlSortKey, RuntimeSummarySortKey,
    runtime_sql_row_key,
};
use crate::fonts::{ARGUS_LOG_FONT_FAMILY, ARGUS_UI_FONT_FAMILY};
use crate::runtime_analysis::{
    RuntimeAnalysisResult, RuntimeRequestRecord, RuntimeRequestSummary, RuntimeSqlRecord,
};
use crate::theme::AppTheme;
use crate::ui::components::icon::{ArgusIcon, render_icon};
use crate::ui::components::loading_spinner::render_loading_spinner;
use gpui::{
    AnyElement, App, ClickEvent, Context, FontWeight, IntoElement, ListState, SharedString,
    UniformListScrollHandle, Window, div, list, prelude::*, px, rgb, uniform_list,
};

/// Runtime 分析页整体边距。
const RUNTIME_VIEW_PADDING: f32 = 14.0;
/// 表头固定高度。
const TABLE_HEADER_HEIGHT: f32 = 30.0;
/// 总览和请求明细行高。
const TABLE_ROW_HEIGHT: f32 = 34.0;
/// SQL 明细默认行高；收起态通过列内横向滚动查看，展开态按内容增高。
const SQL_ROW_HEIGHT: f32 = 36.0;
/// SQL 文本展开按钮预留宽度；用于估算 SQL 列在收起态还能容纳多少字符。
const SQL_EXPAND_BUTTON_RESERVED_WIDTH: f32 = 56.0;
/// 12px 等宽字体的字符宽度估算值；用于避免窗口较窄时漏掉展开入口。
const SQL_TEXT_CHAR_WIDTH_ESTIMATE: f32 = 7.2;
/// 展开入口的最小字符阈值；即使窗口很宽，也避免过短 SQL 反复出现无意义按钮。
const SQL_EXPAND_MIN_TEXT_CHAR_THRESHOLD: usize = 36;
/// 展开态单行最多字符数；宽窗口下也限制行长，避免单行 SQL 过宽影响阅读。
const SQL_EXPANDED_MAX_WRAP_CHARS: usize = 96;
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

/// 渲染 Runtime 分析页签主体。
pub fn render(app: &ArgusApp, analysis_id: usize, cx: &mut Context<ArgusApp>) -> impl IntoElement {
    let theme = app.theme.clone();
    let Some(state) = app.runtime_analysis_state(analysis_id) else {
        return render_missing_state(app, &theme);
    };

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
        .child(render_header(state, &theme))
        .child(match &state.task_state {
            RuntimeAnalysisTaskState::Loading { message } => {
                render_loading_state(message, &theme).into_any_element()
            }
            RuntimeAnalysisTaskState::Ready(result) => {
                render_ready_view(app, analysis_id, state, result, &theme, cx).into_any_element()
            }
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
                        .child(render_icon(ArgusIcon::Database, theme.foreground_muted, 14.0))
                        .child(state.title.clone()),
                )
                        .child(
                            div()
                                .text_size(px(12.0))
                                .line_height(px(16.0))
                                .text_color(rgb(theme.foreground))
                                .child(format!(
                                    "{file_count} 个文件，{summary_count} 个请求地址，{request_count} 个请求，{sql_count} 条 SQL，跳过 {skipped_count} 个文件"
                                )),
                ),
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
    theme: &AppTheme,
    cx: &mut Context<ArgusApp>,
) -> AnyElement {
    match &state.view {
        RuntimeAnalysisView::Summary => {
            render_summary_table(analysis_id, state, result, theme, cx).into_any_element()
        }
        RuntimeAnalysisView::RequestDetails { request_path } => {
            render_request_details_table(analysis_id, state, result, request_path, theme, cx)
        }
        RuntimeAnalysisView::SqlList {
            request_path,
            request_index,
        } => {
            let Some(request) = result.requests.get(*request_index) else {
                return render_empty_message("未找到当前 Runtime 请求记录。", theme);
            };
            render_sql_table(app, analysis_id, state, request_path, request, theme, cx)
                .into_any_element()
        }
    }
}

/// 渲染总览表格。
fn render_summary_table(
    analysis_id: usize,
    state: &RuntimeAnalysisState,
    result: &RuntimeAnalysisResult,
    theme: &AppTheme,
    cx: &mut Context<ArgusApp>,
) -> impl IntoElement + use<> {
    let sorted_indices = Arc::new(sorted_summary_indices(result, state));
    let row_count = sorted_indices.len();
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
                    let RuntimeAnalysisTaskState::Ready(result) = &state.task_state else {
                        return Vec::new();
                    };
                    let Some(indices) = sorted_indices.as_slice().get(range) else {
                        return Vec::new();
                    };

                    indices
                        .iter()
                        .filter_map(|summary_index| {
                            result.summaries.get(*summary_index).map(|summary| {
                                render_summary_row(analysis_id, summary, &theme, row_cx)
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
        .children(render_table_scrollbars(&scroll_handle, theme))
}

/// 渲染请求详情表格。
fn render_request_details_table(
    analysis_id: usize,
    state: &RuntimeAnalysisState,
    result: &RuntimeAnalysisResult,
    request_path: &str,
    theme: &AppTheme,
    cx: &mut Context<ArgusApp>,
) -> AnyElement {
    let Some(summary) = result
        .summaries
        .iter()
        .find(|summary| summary.request_path == request_path)
    else {
        return render_empty_message("未找到当前请求地址的详情。", theme);
    };
    let sorted_indices = Arc::new(sorted_request_indices(result, summary, state));
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
            summary,
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
                                        render_request_row(analysis_id, request, &theme, row_cx)
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
                .children(render_table_scrollbars(&scroll_handle, theme)),
        )
        .into_any_element()
}

/// 渲染 SQL 明细表格。
fn render_sql_table(
    app: &ArgusApp,
    analysis_id: usize,
    state: &RuntimeAnalysisState,
    request_path: &str,
    request: &RuntimeRequestRecord,
    theme: &AppTheme,
    cx: &mut Context<ArgusApp>,
) -> impl IntoElement + use<> {
    let sorted_indices = Arc::new(sorted_sql_indices(request, state));
    let row_count = sorted_indices.len();
    let list_state = state.sql_list.clone();
    if list_state.item_count() != row_count {
        list_state.reset(row_count);
    }
    let request_index = request.index;
    let expanded_keys = Arc::new(state.expanded_sql_rows.clone());

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
        .child(
            div()
                .relative()
                .flex_1()
                .min_h(px(0.0))
                .overflow_hidden()
                .child(render_sql_header(analysis_id, state, theme, cx))
                .child(
                    list(
                        list_state.clone(),
                        cx.processor(move |app, item_index: usize, window, row_cx| {
                            let theme = app.theme.clone();
                            let Some(state) = app.runtime_analysis_state(analysis_id) else {
                                return div().into_any_element();
                            };
                            let RuntimeAnalysisTaskState::Ready(result) = &state.task_state else {
                                return div().into_any_element();
                            };
                            let Some(request) = result.requests.get(request_index) else {
                                return div().into_any_element();
                            };
                            let Some(sql_index) = sorted_indices.get(item_index).copied() else {
                                return div().into_any_element();
                            };
                            let Some(sql) = request.sql_records.get(sql_index) else {
                                return div().into_any_element();
                            };
                            let sql_text_capacity =
                                estimated_sql_text_collapsed_capacity(app, window);
                            let needs_expand = sql_text_needs_expand_for_capacity(
                                &sql.sql_text,
                                sql_text_capacity,
                            );
                            let is_expanded = needs_expand
                                && expanded_keys
                                    .contains(&runtime_sql_row_key(request.index, sql_index));
                            let expanded_wrap_chars =
                                estimated_expanded_sql_wrap_chars(sql_text_capacity);
                            render_sql_row(
                                analysis_id,
                                request.index,
                                sql_index,
                                sql,
                                needs_expand,
                                is_expanded,
                                expanded_wrap_chars,
                                &theme,
                                row_cx,
                            )
                            .into_any_element()
                        }),
                    )
                    .absolute()
                    .top(px(TABLE_HEADER_HEIGHT))
                    .left_0()
                    .right_0()
                    .bottom_0()
                    .size_full(),
                )
                .children(render_list_scrollbars(&list_state, theme)),
        )
        .when(row_count == 0, |this| {
            this.child(render_empty_message("当前请求没有 SQL 明细。", &app.theme))
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
    summary: &RuntimeRequestSummary,
    theme: &AppTheme,
    cx: &mut Context<ArgusApp>,
) -> impl IntoElement + use<> {
    let request_path = summary.request_path.clone();
    render_table_row(TABLE_ROW_HEIGHT, theme)
        .child(render_cell(
            summary.request_count.to_string(),
            SUMMARY_COUNT_COLUMN_WIDTH,
            theme,
        ))
        .child(render_scroll_cell(
            format!("runtime-summary-path-{}", summary.request_path),
            summary.request_path.clone(),
            TABLE_ROW_HEIGHT,
            theme,
        ))
        .child(render_cell(
            format_average_duration(summary.average_duration_ms),
            SUMMARY_AVERAGE_COLUMN_WIDTH,
            theme,
        ))
        .child(render_cell(
            format_ratio(summary.slow_sql_ratio),
            SUMMARY_RATIO_COLUMN_WIDTH,
            theme,
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
    theme: &AppTheme,
    cx: &mut Context<ArgusApp>,
) -> impl IntoElement + use<> {
    let request_path = request.request_path.clone();
    let request_index = request.index;
    render_table_row(TABLE_ROW_HEIGHT, theme)
        .child(render_cell(
            request.request_time_label.clone(),
            REQUEST_TIME_COLUMN_WIDTH,
            theme,
        ))
        .child(render_cell(
            display_username(&request.username),
            REQUEST_USERNAME_COLUMN_WIDTH,
            theme,
        ))
        .child(render_cell(
            format_duration(request.request_duration_ms),
            REQUEST_DURATION_COLUMN_WIDTH,
            theme,
        ))
        .child(render_scroll_cell(
            format!("runtime-request-path-{request_index}"),
            request.request_path.clone(),
            TABLE_ROW_HEIGHT,
            theme,
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
                                    "{} · {} · 请求耗时 {} · SQL 累积耗时 {}",
                                    request.request_time_label,
                                    display_username(&request.username),
                                    format_duration(request.request_duration_ms),
                                    format_duration(request.sql_total_execute_ms)
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
    sql_index: usize,
    sql: &RuntimeSqlRecord,
    needs_expand: bool,
    is_expanded: bool,
    expanded_wrap_chars: usize,
    theme: &AppTheme,
    cx: &mut Context<ArgusApp>,
) -> impl IntoElement + use<> {
    let button_label = if is_expanded { "收起" } else { "展开" };
    let row = if is_expanded {
        render_expanded_table_row(theme)
    } else {
        render_table_row(SQL_ROW_HEIGHT, theme)
    };

    row.child(render_cell(
        format_duration(sql.execute_ms),
        SQL_DURATION_COLUMN_WIDTH,
        theme,
    ))
    .child(render_cell(
        format_duration(sql.acquire_connection_ms),
        SQL_DURATION_COLUMN_WIDTH,
        theme,
    ))
    .child(render_cell(
        format_duration(sql.commit_ms),
        SQL_DURATION_COLUMN_WIDTH,
        theme,
    ))
    .child(render_cell(
        format_duration(sql.release_connection_ms),
        SQL_DURATION_COLUMN_WIDTH,
        theme,
    ))
    .child(render_cell(
        format_duration(sql.parse_result_ms),
        SQL_DURATION_COLUMN_WIDTH,
        theme,
    ))
    .child(
        div()
            .flex_1()
            .min_w(px(0.0))
            .flex()
            .when(!is_expanded, |this| this.h_full().items_center())
            .when(is_expanded, |this| this.items_start())
            .gap_2()
            .child(render_sql_text_cell(
                request_index,
                sql_index,
                sql,
                is_expanded,
                expanded_wrap_chars,
                theme,
            ))
            .when(needs_expand, |this| {
                this.child(render_action_button(
                    format!("runtime-sql-expand-{request_index}-{sql_index}"),
                    button_label,
                    theme,
                    cx.listener(move |app, _, _, cx| {
                        app.toggle_runtime_sql_row_expanded(analysis_id, request_index, sql_index);
                        cx.notify();
                    }),
                ))
            }),
    )
}

/// 渲染可随内容增高的表格行，用于 SQL 展开态完整展示换行内容。
fn render_expanded_table_row(theme: &AppTheme) -> gpui::Div {
    div()
        .min_h(px(SQL_ROW_HEIGHT))
        .w_full()
        .px(px(RUNTIME_VIEW_PADDING))
        .py_2()
        .flex()
        .items_start()
        .border_b_1()
        .border_color(rgb(theme.border))
        .bg(rgb(theme.content))
        .hover(|this| this.bg(rgb(theme.current_line)))
        .text_size(px(12.0))
        .line_height(px(18.0))
}

/// 渲染 SQL 文本单元格；收起态列内横向滚动，展开态换行展示完整 SQL。
fn render_sql_text_cell(
    request_index: usize,
    sql_index: usize,
    sql: &RuntimeSqlRecord,
    is_expanded: bool,
    expanded_wrap_chars: usize,
    theme: &AppTheme,
) -> AnyElement {
    if is_expanded {
        return div()
            .flex_1()
            .min_w(px(0.0))
            .font_family(ARGUS_LOG_FONT_FAMILY)
            .text_size(px(12.0))
            .line_height(px(18.0))
            .text_color(rgb(theme.foreground))
            .children(render_expanded_sql_text(&sql.sql_text, expanded_wrap_chars))
            .into_any_element();
    }

    div()
        .id(SharedString::from(format!(
            "runtime-sql-text-{request_index}-{sql_index}"
        )))
        .flex_1()
        .min_w(px(0.0))
        .h(px(SQL_ROW_HEIGHT - 2.0))
        .flex()
        .items_center()
        .overflow_x_scroll()
        .scrollbar_width(px(4.0))
        .font_family(ARGUS_LOG_FONT_FAMILY)
        .text_size(px(12.0))
        .line_height(px(18.0))
        .text_color(rgb(theme.foreground))
        .child(
            div()
                .flex_none()
                .whitespace_nowrap()
                .child(sql.sql_text.clone()),
        )
        .into_any_element()
}

/// 将完整 SQL 拆成多行元素，保留 SQL 文本中的真实换行并按当前列宽拆分长 token。
fn render_expanded_sql_text(text: &str, wrap_chars: usize) -> Vec<AnyElement> {
    let mut lines = Vec::new();
    for line in text.split('\n') {
        for wrapped_line in wrap_expanded_sql_line(line, wrap_chars) {
            lines.push(
                div()
                    .w_full()
                    .min_w(px(0.0))
                    .whitespace_normal()
                    .child(wrapped_line)
                    .into_any_element(),
            );
        }
    }
    if lines.is_empty() {
        lines.push(div().child("").into_any_element());
    }
    lines
}

/// 将展开态 SQL 的单个物理行拆成短段，避免没有空格的长 token 撑破列宽。
fn wrap_expanded_sql_line(line: &str, wrap_chars: usize) -> Vec<String> {
    if line.is_empty() {
        return vec![" ".to_string()];
    }

    let wrap_chars = wrap_chars
        .max(SQL_EXPAND_MIN_TEXT_CHAR_THRESHOLD)
        .min(SQL_EXPANDED_MAX_WRAP_CHARS);
    let mut wrapped_lines = Vec::new();
    let mut current = String::new();
    let mut current_len = 0usize;
    for character in line.chars() {
        current.push(character);
        current_len += 1;
        if current_len >= wrap_chars {
            wrapped_lines.push(std::mem::take(&mut current));
            current_len = 0;
        }
    }
    if !current.is_empty() {
        wrapped_lines.push(current);
    }
    wrapped_lines
}

/// 根据窗口和侧栏宽度估算 SQL 文本列收起态能容纳的字符数。
fn estimated_sql_text_collapsed_capacity(app: &ArgusApp, window: &Window) -> usize {
    let viewport_width = window.viewport_size().width / px(1.0);
    let source_panel_width = if app.is_source_panel_collapsed {
        0.0
    } else {
        app.source_panel_width
    };
    let content_width = (viewport_width - source_panel_width).max(320.0);
    let sql_text_width = content_width
        - RUNTIME_VIEW_PADDING * 2.0
        - SQL_DURATION_COLUMN_WIDTH * 5.0
        - SQL_EXPAND_BUTTON_RESERVED_WIDTH;
    let estimated_capacity = (sql_text_width / SQL_TEXT_CHAR_WIDTH_ESTIMATE).floor() as isize;
    estimated_capacity
        .max(SQL_EXPAND_MIN_TEXT_CHAR_THRESHOLD as isize)
        .try_into()
        .unwrap_or(SQL_EXPAND_MIN_TEXT_CHAR_THRESHOLD)
}

/// 展开态拆行长度使用当前列宽估算并设置上限，确保窄窗口下也不会撑破列。
fn estimated_expanded_sql_wrap_chars(collapsed_capacity: usize) -> usize {
    collapsed_capacity
        .max(SQL_EXPAND_MIN_TEXT_CHAR_THRESHOLD)
        .min(SQL_EXPANDED_MAX_WRAP_CHARS)
}

/// 根据可容纳字符数判断 SQL 是否需要展开入口，供渲染和单测复用。
fn sql_text_needs_expand_for_capacity(text: &str, capacity: usize) -> bool {
    text.contains('\n') || text.chars().count() > capacity.max(SQL_EXPAND_MIN_TEXT_CHAR_THRESHOLD)
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

/// 渲染普通文本单元格。
fn render_cell(text: String, width: f32, theme: &AppTheme) -> impl IntoElement {
    div()
        .w(px(width))
        .flex_none()
        .min_w(px(0.0))
        .pr_2()
        .truncate()
        .text_color(rgb(theme.foreground))
        .child(text)
}

/// 渲染会在自身内部横向滚动的长文本单元格。
fn render_scroll_cell(
    id: String,
    text: String,
    row_height: f32,
    theme: &AppTheme,
) -> impl IntoElement {
    div()
        .id(SharedString::from(id))
        .flex_1()
        .min_w(px(0.0))
        .h(px(row_height - 2.0))
        .flex()
        .items_center()
        .overflow_x_scroll()
        .scrollbar_width(px(4.0))
        .text_color(rgb(theme.foreground))
        .child(div().flex_none().pr_2().whitespace_nowrap().child(text))
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
        .px_2()
        .flex()
        .items_center()
        .justify_center()
        .rounded_sm()
        .border_1()
        .border_color(rgb(theme.border))
        .cursor_pointer()
        .bg(rgb(theme.selection))
        .hover(|this| this.bg(rgb(theme.info)).text_color(rgb(theme.background)))
        .text_size(px(12.0))
        .line_height(px(24.0))
        .text_color(rgb(theme.foreground))
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
        .px_2()
        .flex()
        .items_center()
        .gap_1()
        .rounded_sm()
        .border_1()
        .border_color(rgb(theme.border))
        .cursor_pointer()
        .bg(rgb(theme.selection))
        .hover(|this| this.bg(rgb(theme.info)).text_color(rgb(theme.background)))
        .text_size(px(12.0))
        .child(render_icon(ArgusIcon::ArrowLeft, theme.foreground, 13.0))
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

/// 返回总览表排序后的行索引。
fn sorted_summary_indices(
    result: &RuntimeAnalysisResult,
    state: &RuntimeAnalysisState,
) -> Vec<usize> {
    let mut indices = (0..result.summaries.len()).collect::<Vec<_>>();
    indices.sort_by(|left_index, right_index| {
        let left = &result.summaries[*left_index];
        let right = &result.summaries[*right_index];
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
    indices
}

/// 返回请求明细表排序后的请求索引。
fn sorted_request_indices(
    result: &RuntimeAnalysisResult,
    summary: &RuntimeRequestSummary,
    state: &RuntimeAnalysisState,
) -> Vec<usize> {
    let mut indices = summary.request_indices.clone();
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
    let mut indices = (0..request.sql_records.len()).collect::<Vec<_>>();
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
    scroll_handle: &UniformListScrollHandle,
    theme: &AppTheme,
) -> Vec<AnyElement> {
    let scroll_state = scroll_handle.0.borrow();
    let bounds = scroll_state.base_handle.bounds();
    let scroll_offset = scroll_state.base_handle.offset();
    let content_size = scroll_state
        .last_item_size
        .map(|item_size| item_size.contents)
        .unwrap_or_default();
    drop(scroll_state);

    let mut scrollbars = Vec::new();
    // Runtime 表格不再暴露整体横向滚动；长字段由单元格内部横向滚动承载。
    if let Some(vertical) = render_passive_scrollbar(
        false,
        bounds.size.height,
        content_size.height,
        -scroll_offset.y,
        theme,
    ) {
        scrollbars.push(vertical);
    }

    scrollbars
}

/// 根据可变高度列表状态绘制被动滚动条；真实滚动由 GPUI `list` 元素处理。
fn render_list_scrollbars(list_state: &ListState, theme: &AppTheme) -> Vec<AnyElement> {
    let bounds = list_state.viewport_bounds();
    let max_offset = list_state.max_offset_for_scrollbar();
    let scroll_offset = list_state.scroll_px_offset_for_scrollbar();
    let content_height = bounds.size.height + max_offset.height;

    render_passive_scrollbar(
        false,
        bounds.size.height,
        content_height,
        -scroll_offset.y,
        theme,
    )
    .into_iter()
    .collect()
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

    let track_padding = px(RUNTIME_SCROLLBAR_PADDING);
    let track_length = (viewport_length - track_padding * 2.0).max(px(1.0));
    let thumb_length = ((viewport_length / content_length) * track_length)
        .clamp(px(RUNTIME_SCROLLBAR_MIN_THUMB), track_length);
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
                .bottom(px(RUNTIME_SCROLLBAR_PADDING))
                .w(thumb_length)
                .h(px(RUNTIME_SCROLLBAR_THUMB_SIZE))
        } else {
            thumb
                .top(thumb_start)
                .right(px(RUNTIME_SCROLLBAR_PADDING))
                .w(px(RUNTIME_SCROLLBAR_THUMB_SIZE))
                .h(thumb_length)
        }
        .into_any_element(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 验证窗口较窄时，低于旧固定阈值的 SQL 也会显示展开入口。
    #[test]
    fn sql_expand_detection_uses_current_capacity() {
        let sql = "select id, name, status, created_at from workflow_runtime_table";

        assert!(sql_text_needs_expand_for_capacity(sql, 42));
        assert!(!sql_text_needs_expand_for_capacity("select 1", 42));
    }

    /// 验证多行 SQL 不依赖字符长度也会出现展开入口。
    #[test]
    fn multiline_sql_always_needs_expand() {
        assert!(sql_text_needs_expand_for_capacity(
            "select *\nfrom runtime_log",
            200
        ));
    }

    /// 验证展开态会拆分没有空格的超长 SQL 片段，避免继续撑出单元格。
    #[test]
    fn expanded_sql_line_wraps_long_tokens() {
        let wrap_chars = 42;
        let long_token = "x".repeat(wrap_chars + 8);
        let wrapped = wrap_expanded_sql_line(&long_token, wrap_chars);

        assert_eq!(wrapped.len(), 2);
        assert_eq!(wrapped[0].chars().count(), wrap_chars);
        assert_eq!(wrapped[1].chars().count(), 8);
    }
}
