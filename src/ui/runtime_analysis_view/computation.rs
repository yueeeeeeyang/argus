use super::*;

pub(crate) fn runtime_filter_criteria(state: &RuntimeAnalysisState) -> RuntimeFilterCriteria {
    parse_runtime_analysis_filter_criteria(&runtime_sql_analysis_filter_snapshot(state))
}

/// 从 Runtime 分析状态中提取 SQL 分析缓存用的原始过滤输入快照。
pub(crate) fn runtime_sql_analysis_filter_snapshot(
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
pub(crate) fn runtime_filter_input_snapshot(
    state: &RuntimeAnalysisState,
) -> RuntimeSqlAnalysisFilterSnapshot {
    RuntimeSqlAnalysisFilterSnapshot {
        keyword: state.filter_keyword_input.value.clone(),
        username: state.filter_username_input.value.clone(),
        start_time: state.filter_start_time_input.value.clone(),
        end_time: state.filter_end_time_input.value.clone(),
    }
}

/// 返回 SQL 频率分析过滤并排序后的行。
pub(crate) fn sql_frequency_rows(
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

/// 只读取后台完成的 SQL 执行详情，缓存缺失时由渲染入口启动任务。
pub(crate) fn sql_frequency_detail_rows(
    _result: &RuntimeAnalysisResult,
    state: &RuntimeAnalysisState,
    normalized_sql: &str,
) -> Arc<Vec<RuntimeSqlFrequencyDetailRow>> {
    let filter = runtime_sql_analysis_filter_snapshot(state);
    state
        .sql_frequency_detail_rows_cache
        .borrow()
        .as_ref()
        .filter(|cache| cache.filter == filter && cache.normalized_sql == normalized_sql)
        .map(|cache| cache.rows.clone())
        .unwrap_or_default()
}

/// 返回慢 SQL 分析过滤并按平均执行耗时降序排列后的聚合行。
pub(crate) fn slow_sql_rows(
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

/// 只读取匹配当前排序条件的总览缓存，重绘期间不复制或重排集合。
pub(crate) fn cached_sorted_summary_rows(
    _result: &RuntimeAnalysisResult,
    state: &RuntimeAnalysisState,
) -> Arc<Vec<RuntimeRequestSummary>> {
    let filter = runtime_sql_analysis_filter_snapshot(state);
    state
        .summary_rows_cache
        .borrow()
        .as_ref()
        .filter(|cache| {
            cache.filter == filter
                && cache.sort_key == state.summary_sort_key
                && cache.sort_direction == state.summary_sort_direction
        })
        .map(|cache| cache.rows.clone())
        .unwrap_or_default()
}

/// 使用路径索引借用现有聚合行，避免详情重绘时重新聚合或复制请求索引。
pub(crate) fn filtered_summary_for_request_path<'a>(
    result: &'a RuntimeAnalysisResult,
    request_path: &str,
    state: &'a RuntimeAnalysisState,
) -> Option<&'a RuntimeRequestSummary> {
    let filter = runtime_sql_analysis_filter_snapshot(state);
    if runtime_filter_criteria(state).is_active() {
        let rows = state
            .runtime_filter_rows_cache
            .as_ref()
            .filter(|rows| rows.filter == filter)?;
        rows.summary_indices_by_path
            .get(request_path)
            .and_then(|index| rows.summaries.get(*index))
    } else {
        result
            .summary_indices_by_path
            .get(request_path)
            .and_then(|index| result.summaries.get(*index))
    }
}

/// 只读取当前请求地址和排序方式对应的索引缓存。
pub(crate) fn cached_sorted_request_indices(
    _result: &RuntimeAnalysisResult,
    summary: &RuntimeRequestSummary,
    state: &RuntimeAnalysisState,
) -> Arc<Vec<usize>> {
    let filter = runtime_sql_analysis_filter_snapshot(state);
    state
        .request_indices_cache
        .borrow()
        .as_ref()
        .filter(|cache| {
            cache.filter == filter
                && cache.request_path == summary.request_path
                && cache.sort_key == state.request_sort_key
                && cache.sort_direction == state.request_sort_direction
        })
        .map(|cache| cache.indices.clone())
        .unwrap_or_default()
}

/// 只读取当前请求和排序方式对应的 SQL 索引缓存。
pub(crate) fn cached_sorted_sql_indices(
    request: &RuntimeRequestRecord,
    state: &RuntimeAnalysisState,
) -> Arc<Vec<usize>> {
    let filter = runtime_sql_analysis_filter_snapshot(state);
    state
        .sql_indices_cache
        .borrow()
        .as_ref()
        .filter(|cache| {
            cache.filter == filter
                && cache.request_index == request.index
                && cache.sort_key == state.sql_sort_key
                && cache.sort_direction == state.sql_sort_direction
        })
        .map(|cache| cache.indices.clone())
        .unwrap_or_default()
}

/// 返回日期时间选择器当前展示值；输入为空或非法时使用当天边界作为默认值。
pub(crate) fn runtime_datetime_picker_value(
    input: &TextInputState,
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
pub(crate) fn default_runtime_datetime_picker_datetime(is_end: bool) -> chrono::DateTime<Local> {
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
pub(crate) fn runtime_filter_status_label(state: &RuntimeAnalysisState) -> String {
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
pub(crate) fn runtime_filter_input_selection_range(
    input: &TextInputState,
) -> Option<std::ops::Range<usize>> {
    input.selection_range()
}

/// 返回 Runtime 过滤输入框使用的焦点句柄。
pub(crate) fn runtime_filter_focus_handle(
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
pub(crate) fn runtime_filter_input_id(input_kind: RuntimeFilterInputKind) -> &'static str {
    match input_kind {
        RuntimeFilterInputKind::Keyword => "runtime-filter-keyword-input",
        RuntimeFilterInputKind::Username => "runtime-filter-username-input",
        RuntimeFilterInputKind::StartTime => "runtime-filter-start-time-input",
        RuntimeFilterInputKind::EndTime => "runtime-filter-end-time-input",
    }
}

/// Runtime 过滤输入框前置图标 ID。
pub(crate) fn runtime_filter_leading_id(input_kind: RuntimeFilterInputKind) -> &'static str {
    match input_kind {
        RuntimeFilterInputKind::Keyword => "runtime-filter-keyword-leading",
        RuntimeFilterInputKind::Username => "runtime-filter-username-leading",
        RuntimeFilterInputKind::StartTime => "runtime-filter-start-time-leading",
        RuntimeFilterInputKind::EndTime => "runtime-filter-end-time-leading",
    }
}

/// Runtime 过滤输入框清除按钮 ID。
pub(crate) fn runtime_filter_clear_id(input_kind: RuntimeFilterInputKind) -> &'static str {
    match input_kind {
        RuntimeFilterInputKind::Keyword => "runtime-filter-clear-keyword",
        RuntimeFilterInputKind::Username => "runtime-filter-clear-username",
        RuntimeFilterInputKind::StartTime => "runtime-filter-clear-start-time",
        RuntimeFilterInputKind::EndTime => "runtime-filter-clear-end-time",
    }
}

/// 格式化整数毫秒耗时。
pub(crate) fn format_duration(duration_ms: u64) -> String {
    format!("{duration_ms} ms")
}

/// 格式化平均耗时，保留一位小数。
pub(crate) fn format_average_duration(duration_ms: f64) -> String {
    format!("{duration_ms:.1} ms")
}

/// 格式化比例，保留一位百分比。
pub(crate) fn format_ratio(ratio: f64) -> String {
    format!("{:.1}%", ratio * 100.0)
}

/// 返回用户名展示文本。
pub(crate) fn display_username(username: &str) -> String {
    if username.is_empty() {
        "-".to_string()
    } else {
        username.to_string()
    }
}
