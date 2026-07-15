use super::*;

pub(crate) fn render_sql_analysis_column_gap() -> impl IntoElement {
    div().w(px(SQL_ANALYSIS_COLUMN_GAP)).flex_none()
}

/// 渲染固定表格无数据时覆盖在表体区域的空态。
pub(crate) fn render_table_empty_overlay(
    message: &'static str,
    theme: &AppTheme,
) -> impl IntoElement + use<> {
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
pub(crate) fn render_summary_header(
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
pub(crate) fn render_summary_row(
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
pub(crate) fn render_request_details_topbar(
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
pub(crate) fn render_request_header(
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
pub(crate) fn render_request_row(
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
pub(crate) fn render_sql_topbar(
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
pub(crate) fn render_sql_header(
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
pub(crate) fn render_sql_row(
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

/// 渲染带"更多"入口的 SQL 文本单元格：横向滚动展示 SQL，末尾悬浮按钮可打开完整 SQL 弹窗。
///
/// 统计分析、频率/慢 SQL 分析及其详情列表共用此函数；调用方通过 `hover_key` 区分
/// 具体记录（`Record`）与聚合行（`Summary`），通过 `dialog` 传入弹窗所需上下文。
pub(crate) fn render_sql_text_cell_with_more(
    analysis_id: usize,
    cell_id: String,
    scroll_cell_id: String,
    cell_key: String,
    hover_key: RuntimeSqlCellKey,
    more_button_id: String,
    dialog: RuntimeSqlTextDialog,
    hovered: bool,
    selection: Option<&RuntimeTableCellSelection>,
    analysis_focus_handle: Option<FocusHandle>,
    theme: &AppTheme,
    cx: &mut Context<ArgusApp>,
) -> AnyElement {
    div()
        .id(SharedString::from(cell_id))
        .flex_1()
        .min_w(px(0.0))
        .h(px(SQL_ROW_HEIGHT - 2.0))
        .relative()
        .flex()
        .items_center()
        .on_hover(cx.listener(move |app, is_hovered: &bool, _, cx| {
            if app.set_runtime_sql_cell_hovered(analysis_id, hover_key, *is_hovered) {
                cx.notify();
            }
        }))
        .child(render_selectable_scroll_cell_with_font(
            analysis_id,
            scroll_cell_id,
            cell_key,
            dialog.sql_text.clone(),
            SQL_ROW_HEIGHT,
            ARGUS_LOG_FONT_FAMILY,
            selection,
            analysis_focus_handle,
            theme,
            cx,
        ))
        .child(render_sql_more_button(
            analysis_id,
            more_button_id,
            dialog,
            hovered,
            theme,
            cx,
        ))
        .into_any_element()
}

/// 渲染 SQL 文本单元格；单元格宽度由表格列固定，长 SQL 在单元格内部横向滚动。
pub(crate) fn render_sql_text_cell(
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
    let hover_key = RuntimeSqlCellKey::Record {
        request_index,
        sql_index,
    };
    let is_hovered = hovered_sql_cell == Some(hover_key);
    render_sql_text_cell_with_more(
        analysis_id,
        format!("runtime-sql-text-{request_index}-{sql_index}"),
        format!("runtime-sql-text-scroll-{request_index}-{sql_index}"),
        runtime_sql_cell_key(request_index, sql_index, "text"),
        hover_key,
        format!("{request_index}-{sql_index}"),
        RuntimeSqlTextDialog {
            request_path,
            request_time_label,
            username,
            sql_text: sql.sql_text.clone(),
            selection: None,
            selection_drag: None,
        },
        is_hovered,
        selection,
        analysis_focus_handle,
        theme,
        cx,
    )
}

/// 渲染 SQL 单元格末尾的更多入口，点击后展示保留格式的完整 SQL。
pub(crate) fn render_sql_more_button(
    analysis_id: usize,
    id: String,
    dialog: RuntimeSqlTextDialog,
    is_visible: bool,
    theme: &AppTheme,
    cx: &mut Context<ArgusApp>,
) -> impl IntoElement + use<> {
    div()
        .id(SharedString::from(format!("runtime-sql-more-{id}")))
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
            app.open_runtime_sql_text_dialog(analysis_id, dialog.clone());
            cx.notify();
        }))
}

/// 渲染表格外层表头行；表格随容器宽度伸缩，避免把整页撑出横向滚动。
pub(crate) fn render_table_header(theme: &AppTheme) -> gpui::Div {
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
pub(crate) fn render_header_cell(
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
pub(crate) fn render_flex_header_cell(
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
pub(crate) fn render_static_header_cell(
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
pub(crate) fn render_static_flex_header_cell(
    label: &'static str,
    theme: &AppTheme,
) -> impl IntoElement {
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
pub(crate) fn render_table_row(height: f32, theme: &AppTheme) -> gpui::Div {
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
pub(crate) fn render_selectable_cell(
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
pub(crate) fn render_selectable_scroll_cell(
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
pub(crate) fn render_selectable_scroll_cell_with_font(
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
