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

/// 返回指定 SQL 结构的执行详情行，并按过滤条件缓存结果。
pub(crate) fn sql_frequency_detail_rows(
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

/// 返回总览表缓存后的排序结果，避免渲染期反复 clone 和排序全量行。
pub(crate) fn cached_sorted_summary_rows(
    result: &RuntimeAnalysisResult,
    state: &RuntimeAnalysisState,
) -> Arc<Vec<RuntimeRequestSummary>> {
    let filter = runtime_sql_analysis_filter_snapshot(state);
    if let Some(cache) = state.summary_rows_cache.borrow().as_ref()
        && cache.filter == filter
        && cache.sort_key == state.summary_sort_key
        && cache.sort_direction == state.summary_sort_direction
    {
        return cache.rows.clone();
    }

    let _span = PerfSpan::new("runtime_sorted_summary_rows");
    let rows = Arc::new(sorted_summary_rows(result, state));
    state
        .summary_rows_cache
        .borrow_mut()
        .replace(RuntimeSummaryRowsCache {
            filter,
            sort_key: state.summary_sort_key,
            sort_direction: state.summary_sort_direction,
            rows: rows.clone(),
        });
    rows
}

/// 返回总览表过滤并排序后的聚合行。
pub(crate) fn sorted_summary_rows(
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
pub(crate) fn filtered_summary_for_request_path(
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
pub(crate) fn filtered_summary_from_indices(
    result: &RuntimeAnalysisResult,
    summary: &RuntimeRequestSummary,
    criteria: &RuntimeFilterCriteria,
    apply_keyword: bool,
) -> Option<RuntimeRequestSummary> {
    filtered_runtime_summary_from_indices(result, summary, criteria, apply_keyword)
}

/// 返回请求明细表缓存后的排序索引，避免切回详情页时重复排序。
pub(crate) fn cached_sorted_request_indices(
    result: &RuntimeAnalysisResult,
    summary: &RuntimeRequestSummary,
    state: &RuntimeAnalysisState,
) -> Arc<Vec<usize>> {
    let filter = runtime_sql_analysis_filter_snapshot(state);
    if let Some(cache) = state.request_indices_cache.borrow().as_ref()
        && cache.filter == filter
        && cache.request_path == summary.request_path
        && cache.sort_key == state.request_sort_key
        && cache.sort_direction == state.request_sort_direction
    {
        return cache.indices.clone();
    }

    let _span = PerfSpan::new("runtime_sorted_request_indices");
    let indices = Arc::new(sorted_request_indices(result, summary, state));
    state
        .request_indices_cache
        .borrow_mut()
        .replace(RuntimeRequestIndicesCache {
            filter,
            request_path: summary.request_path.clone(),
            sort_key: state.request_sort_key,
            sort_direction: state.request_sort_direction,
            indices: indices.clone(),
        });
    indices
}

/// 返回请求明细表排序后的请求索引。
pub(crate) fn sorted_request_indices(
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

/// 返回 SQL 明细表缓存后的排序索引，避免切回同一请求时重复排序。
pub(crate) fn cached_sorted_sql_indices(
    request: &RuntimeRequestRecord,
    state: &RuntimeAnalysisState,
) -> Arc<Vec<usize>> {
    let filter = runtime_sql_analysis_filter_snapshot(state);
    if let Some(cache) = state.sql_indices_cache.borrow().as_ref()
        && cache.filter == filter
        && cache.request_index == request.index
        && cache.sort_key == state.sql_sort_key
        && cache.sort_direction == state.sql_sort_direction
    {
        return cache.indices.clone();
    }

    let _span = PerfSpan::new("runtime_sorted_sql_indices");
    let indices = Arc::new(sorted_sql_indices(request, state));
    state
        .sql_indices_cache
        .borrow_mut()
        .replace(RuntimeSqlIndicesCache {
            filter,
            request_index: request.index,
            sort_key: state.sql_sort_key,
            sort_direction: state.sql_sort_direction,
            indices: indices.clone(),
        });
    indices
}

/// 返回 SQL 明细表排序后的 SQL 索引。
pub(crate) fn sorted_sql_indices(
    request: &RuntimeRequestRecord,
    state: &RuntimeAnalysisState,
) -> Vec<usize> {
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
pub(crate) fn runtime_request_matches_cross_filters(
    request: &RuntimeRequestRecord,
    criteria: &RuntimeFilterCriteria,
) -> bool {
    domain_runtime_request_matches_cross_filters(request, criteria)
}

/// 判断请求明细行是否命中关键字。
pub(crate) fn runtime_request_matches_keyword(
    request: &RuntimeRequestRecord,
    criteria: &RuntimeFilterCriteria,
) -> bool {
    domain_runtime_request_matches_keyword(request, criteria)
}

/// 判断 SQL 明细行是否命中关键字；同时纳入所属请求元信息，便于跨层检索。
pub(crate) fn runtime_sql_matches_keyword(
    request: &RuntimeRequestRecord,
    sql: &RuntimeSqlRecord,
    criteria: &RuntimeFilterCriteria,
) -> bool {
    domain_runtime_sql_matches_keyword(request, sql, criteria)
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

/// 根据排序方向翻转比较结果。
pub(crate) fn apply_sort_direction(
    ordering: Ordering,
    direction: RuntimeSortDirection,
) -> Ordering {
    match direction {
        RuntimeSortDirection::Ascending => ordering,
        RuntimeSortDirection::Descending => ordering.reverse(),
    }
}

/// 比较浮点统计值；当前统计值均为有限数，异常时按相等兜底。
pub(crate) fn compare_f64(left: f64, right: f64) -> Ordering {
    left.partial_cmp(&right).unwrap_or(Ordering::Equal)
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
