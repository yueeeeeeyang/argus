use super::*;

pub(crate) fn render_summary_table(
    analysis_id: usize,
    state: &RuntimeAnalysisState,
    result: &RuntimeAnalysisResult,
    analysis_focus_handle: Option<FocusHandle>,
    theme: &AppTheme,
    cx: &mut Context<ArgusApp>,
) -> impl IntoElement + use<> {
    let sorted_rows = cached_sorted_summary_rows(result, state);
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
pub(crate) fn render_request_details_table(
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
    let sorted_indices = cached_sorted_request_indices(result, &summary, state);
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
pub(crate) fn render_sql_table(
    analysis_id: usize,
    state: &RuntimeAnalysisState,
    request_path: &str,
    request: &RuntimeRequestRecord,
    analysis_focus_handle: Option<FocusHandle>,
    theme: &AppTheme,
    cx: &mut Context<ArgusApp>,
) -> impl IntoElement + use<> {
    let sorted_indices = cached_sorted_sql_indices(request, state);
    let row_count = sorted_indices.len();
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
pub(crate) fn render_sql_frequency_table(
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
                                state.hovered_sql_cell,
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
pub(crate) fn render_sql_frequency_detail_table(
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
                                        state.hovered_sql_cell,
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
pub(crate) fn render_slow_sql_table(
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
                                state.hovered_sql_cell,
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
pub(crate) fn render_slow_sql_detail_table(
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
                                        state.hovered_sql_cell,
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
pub(crate) fn render_sql_frequency_header(theme: &AppTheme) -> impl IntoElement + use<> {
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
pub(crate) fn render_sql_frequency_row(
    analysis_id: usize,
    row_index: usize,
    row: &RuntimeSqlFrequencyAnalysisRow,
    hovered_sql_cell: Option<RuntimeSqlCellKey>,
    selection: Option<&RuntimeTableCellSelection>,
    analysis_focus_handle: Option<FocusHandle>,
    theme: &AppTheme,
    cx: &mut Context<ArgusApp>,
) -> impl IntoElement + use<> {
    let hover_key = RuntimeSqlCellKey::Summary { row_index };
    let is_hovered = hovered_sql_cell == Some(hover_key);
    render_table_row(SQL_ROW_HEIGHT, theme)
        .child(render_sql_text_cell_with_more(
            analysis_id,
            format!("runtime-sql-frequency-text-{row_index}"),
            format!("runtime-sql-frequency-text-scroll-{row_index}"),
            runtime_cell_key("sql-frequency", row_index, "text"),
            hover_key,
            format!("sql-frequency-{row_index}"),
            RuntimeSqlTextDialog {
                request_path: "SQL频率分析".to_string(),
                request_time_label: format!("共 {} 次执行", row.execute_count),
                username: String::new(),
                sql_text: row.normalized_sql.clone(),
                selection: None,
                selection_drag: None,
            },
            is_hovered,
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
pub(crate) fn render_sql_frequency_detail_topbar(
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
pub(crate) fn render_sql_frequency_detail_header(theme: &AppTheme) -> impl IntoElement + use<> {
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
pub(crate) fn render_sql_frequency_detail_row(
    analysis_id: usize,
    result: &RuntimeAnalysisResult,
    row: &RuntimeSqlFrequencyDetailRow,
    hovered_sql_cell: Option<RuntimeSqlCellKey>,
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
    let username = request
        .map(|request| request.username.clone())
        .unwrap_or_default();
    let hover_key = RuntimeSqlCellKey::Record {
        request_index: row.request_index,
        sql_index: row.sql_index,
    };
    let is_hovered = hovered_sql_cell == Some(hover_key);

    render_table_row(SQL_ROW_HEIGHT, theme)
        .child(render_sql_text_cell_with_more(
            analysis_id,
            format!(
                "runtime-sql-frequency-detail-text-{}-{}",
                row.request_index, row.sql_index
            ),
            format!(
                "runtime-sql-frequency-detail-text-scroll-{}-{}",
                row.request_index, row.sql_index
            ),
            runtime_sql_cell_key(row.request_index, row.sql_index, "frequency-detail-text"),
            hover_key,
            format!("frequency-detail-{}-{}", row.request_index, row.sql_index),
            RuntimeSqlTextDialog {
                request_path: request_path.clone(),
                request_time_label: request_time_label.clone(),
                username,
                sql_text,
                selection: None,
                selection_drag: None,
            },
            is_hovered,
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
pub(crate) fn render_slow_sql_header(theme: &AppTheme) -> impl IntoElement + use<> {
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
pub(crate) fn render_slow_sql_row(
    analysis_id: usize,
    row_index: usize,
    row: &RuntimeSlowSqlSummaryRow,
    hovered_sql_cell: Option<RuntimeSqlCellKey>,
    selection: Option<&RuntimeTableCellSelection>,
    analysis_focus_handle: Option<FocusHandle>,
    theme: &AppTheme,
    cx: &mut Context<ArgusApp>,
) -> impl IntoElement + use<> {
    let hover_key = RuntimeSqlCellKey::Summary { row_index };
    let is_hovered = hovered_sql_cell == Some(hover_key);
    render_table_row(SQL_ROW_HEIGHT, theme)
        .child(render_sql_text_cell_with_more(
            analysis_id,
            format!("runtime-slow-sql-text-{row_index}"),
            format!("runtime-slow-sql-text-scroll-{row_index}"),
            runtime_cell_key("slow-sql", row_index, "text"),
            hover_key,
            format!("slow-sql-{row_index}"),
            RuntimeSqlTextDialog {
                request_path: "慢SQL分析".to_string(),
                request_time_label: format!("共 {} 次执行", row.execute_count),
                username: String::new(),
                sql_text: row.normalized_sql.clone(),
                selection: None,
                selection_drag: None,
            },
            is_hovered,
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
pub(crate) fn render_slow_sql_detail_topbar(
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
pub(crate) fn render_slow_sql_detail_header(theme: &AppTheme) -> impl IntoElement + use<> {
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
pub(crate) fn render_slow_sql_detail_row(
    analysis_id: usize,
    result: &RuntimeAnalysisResult,
    row: &RuntimeSqlFrequencyDetailRow,
    hovered_sql_cell: Option<RuntimeSqlCellKey>,
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
    let username = request
        .map(|request| request.username.clone())
        .unwrap_or_default();
    let hover_key = RuntimeSqlCellKey::Record {
        request_index: row.request_index,
        sql_index: row.sql_index,
    };
    let is_hovered = hovered_sql_cell == Some(hover_key);

    render_table_row(SQL_ROW_HEIGHT, theme)
        .child(render_sql_text_cell_with_more(
            analysis_id,
            format!(
                "runtime-slow-sql-detail-text-{}-{}",
                row.request_index, row.sql_index
            ),
            format!(
                "runtime-slow-sql-detail-text-scroll-{}-{}",
                row.request_index, row.sql_index
            ),
            runtime_sql_cell_key(row.request_index, row.sql_index, "slow-detail-text"),
            hover_key,
            format!("slow-detail-{}-{}", row.request_index, row.sql_index),
            RuntimeSqlTextDialog {
                request_path: request_path.clone(),
                request_time_label: request_time_label.clone(),
                username,
                sql_text,
                selection: None,
                selection_drag: None,
            },
            is_hovered,
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
