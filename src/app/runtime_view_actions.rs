//! 文件职责：在后台计算 Runtime 当前表格的排序索引和 SQL 执行详情，避免重绘阻塞界面。

use std::sync::atomic::{AtomicBool, Ordering};

use super::*;
use crate::analysis::runtime::{
    RuntimeSqlFrequencyDetailRow, build_runtime_analysis_filter_rows,
    sort_runtime_sql_frequency_detail_rows,
};
use crate::infra::perf::PerfSpan;

/// 当前表格的计算条件；仅保存轻量快照，不捕获界面状态。
#[derive(Clone, Debug, Eq, PartialEq)]
enum RuntimeViewRowsKey {
    Summary(RuntimeSummarySortKey, RuntimeSortDirection),
    Requests(String, RuntimeRequestSortKey, RuntimeSortDirection),
    Sql(usize, RuntimeSqlSortKey, RuntimeSortDirection),
    SqlDetail(String),
}

/// 在途任务的完整身份，防止切换排序、详情或过滤后写回过期结果。
#[derive(Clone, Debug, Eq, PartialEq)]
struct RuntimeViewTaskKey {
    filter: RuntimeSqlAnalysisFilterSnapshot,
    rows: RuntimeViewRowsKey,
}

/// 当前可见表格的后台计算生命周期。
#[derive(Debug, Default)]
pub(crate) struct RuntimeViewComputationState {
    generation: usize,
    key: Option<RuntimeViewTaskKey>,
    cancel_token: Option<Arc<AtomicBool>>,
}

impl RuntimeViewComputationState {
    /// 取消旧任务并使其结果失效；已应用缓存由调用方按条件保留或清理。
    pub(crate) fn cancel(&mut self) {
        if let Some(cancel_token) = self.cancel_token.take() {
            cancel_token.store(true, Ordering::Relaxed);
        }
        self.generation = self.generation.wrapping_add(1);
        self.key = None;
    }
}

impl Drop for RuntimeViewComputationState {
    fn drop(&mut self) {
        self.cancel();
    }
}

impl RuntimeViewTaskKey {
    /// 根据当前可见页选取计算目标，聚合列表由共享 SQL 聚合任务负责。
    fn from_state(state: &RuntimeAnalysisState) -> Option<Self> {
        let rows = match state.result_type {
            RuntimeAnalysisResultType::Statistics => match &state.view {
                RuntimeAnalysisView::Summary => RuntimeViewRowsKey::Summary(
                    state.summary_sort_key,
                    state.summary_sort_direction,
                ),
                RuntimeAnalysisView::RequestDetails { request_path } => {
                    RuntimeViewRowsKey::Requests(
                        request_path.clone(),
                        state.request_sort_key,
                        state.request_sort_direction,
                    )
                }
                RuntimeAnalysisView::SqlList { request_index, .. } => RuntimeViewRowsKey::Sql(
                    *request_index,
                    state.sql_sort_key,
                    state.sql_sort_direction,
                ),
            },
            RuntimeAnalysisResultType::SqlFrequency => {
                RuntimeViewRowsKey::SqlDetail(state.sql_frequency_detail_sql.clone()?)
            }
            RuntimeAnalysisResultType::SlowSql => {
                RuntimeViewRowsKey::SqlDetail(state.slow_sql_detail_sql.clone()?)
            }
        };
        Some(Self {
            filter: runtime_filter_applied_snapshot_from_state(state),
            rows,
        })
    }

    /// 仅比较缓存键；空结果也属于已完成缓存。
    fn is_cached(&self, state: &RuntimeAnalysisState) -> bool {
        match &self.rows {
            RuntimeViewRowsKey::Summary(sort_key, sort_direction) => state
                .summary_rows_cache
                .borrow()
                .as_ref()
                .is_some_and(|cache| {
                    cache.filter == self.filter
                        && cache.sort_key == *sort_key
                        && cache.sort_direction == *sort_direction
                }),
            RuntimeViewRowsKey::Requests(request_path, sort_key, sort_direction) => state
                .request_indices_cache
                .borrow()
                .as_ref()
                .is_some_and(|cache| {
                    cache.filter == self.filter
                        && cache.request_path == *request_path
                        && cache.sort_key == *sort_key
                        && cache.sort_direction == *sort_direction
                }),
            RuntimeViewRowsKey::Sql(request_index, sort_key, sort_direction) => state
                .sql_indices_cache
                .borrow()
                .as_ref()
                .is_some_and(|cache| {
                    cache.filter == self.filter
                        && cache.request_index == *request_index
                        && cache.sort_key == *sort_key
                        && cache.sort_direction == *sort_direction
                }),
            RuntimeViewRowsKey::SqlDetail(normalized_sql) => state
                .sql_frequency_detail_rows_cache
                .borrow()
                .as_ref()
                .is_some_and(|cache| {
                    cache.filter == self.filter && cache.normalized_sql == *normalized_sql
                }),
        }
    }
}

impl RuntimeAnalysisState {
    /// 判断可见表格是否完成计算，避免把后台等待状态误显示为无结果。
    pub(crate) fn has_runtime_view_rows(&self) -> bool {
        RuntimeViewTaskKey::from_state(self).is_none_or(|key| key.is_cached(self))
    }
}

/// 后台计算结果沿用现有缓存结构，应用时无需再次复制行或索引。
enum RuntimeViewRows {
    Summary(RuntimeSummaryRowsCache),
    Requests(RuntimeRequestIndicesCache),
    Sql(RuntimeSqlIndicesCache),
    SqlDetail(RuntimeSqlFrequencyDetailRowsCache),
}

impl RuntimeViewRows {
    /// 将完成的不可变结果一次性发布到界面缓存。
    fn apply(self, state: &RuntimeAnalysisState) {
        match self {
            Self::Summary(cache) => {
                state.summary_rows_cache.replace(Some(cache));
            }
            Self::Requests(cache) => {
                state.request_indices_cache.replace(Some(cache));
            }
            Self::Sql(cache) => {
                state.sql_indices_cache.replace(Some(cache));
            }
            Self::SqlDetail(cache) => {
                state.sql_frequency_detail_rows_cache.replace(Some(cache));
            }
        }
    }
}

impl ArgusApp {
    /// 在渲染前确保当前表格任务已启动；输入防抖期间只保留已应用缓存。
    pub(crate) fn ensure_runtime_view_rows(&mut self, analysis_id: usize, cx: &mut Context<Self>) {
        let Some(state) = self.runtime_analyses.get_mut(&analysis_id) else {
            return;
        };
        if state.is_filter_pending || state.is_filter_computing {
            return;
        }
        let Some(key) = RuntimeViewTaskKey::from_state(state) else {
            state.view_computation.cancel();
            return;
        };
        if key.is_cached(state) {
            if state
                .view_computation
                .key
                .as_ref()
                .is_some_and(|pending| pending != &key)
            {
                state.view_computation.cancel();
            }
            return;
        }
        if state.view_computation.key.as_ref() == Some(&key) {
            return;
        }
        let RuntimeAnalysisTaskState::Ready(result) = &state.task_state else {
            return;
        };
        let result = result.clone();
        let filtered_rows = state.runtime_filter_rows_cache.clone();
        state.view_computation.cancel();
        let generation = state.view_computation.generation;
        let cancel_token = Arc::new(AtomicBool::new(false));
        state.view_computation.key = Some(key.clone());
        state.view_computation.cancel_token = Some(cancel_token.clone());
        cx.spawn(async move |view, cx| {
            let task_key = key.clone();
            let rows = cx
                .background_executor()
                .spawn(async move {
                    build_runtime_view_rows(
                        &result,
                        filtered_rows.as_ref(),
                        &task_key,
                        &cancel_token,
                    )
                })
                .await;
            let Some(rows) = rows else {
                return;
            };
            view.update(cx, |app, cx| {
                let Some(state) = app.runtime_analyses.get_mut(&analysis_id) else {
                    return;
                };
                if state.view_computation.generation != generation
                    || state.view_computation.key.as_ref() != Some(&key)
                {
                    return;
                }
                // 当前任务已经结束，即使视图先于下一次渲染切换，也必须释放槽位以允许重启。
                state.view_computation.key = None;
                state.view_computation.cancel_token = None;
                if state.is_filter_pending
                    || state.is_filter_computing
                    || RuntimeViewTaskKey::from_state(state).as_ref() != Some(&key)
                {
                    return;
                }
                rows.apply(state);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }
}

/// 按已应用过滤快照计算当前页；所有扫描和排序均只在后台执行。
fn build_runtime_view_rows(
    result: &RuntimeAnalysisResult,
    filtered_rows: Option<&RuntimeAnalysisFilterRows>,
    key: &RuntimeViewTaskKey,
    cancel_token: &AtomicBool,
) -> Option<RuntimeViewRows> {
    if cancel_token.load(Ordering::Relaxed) {
        return None;
    }
    let criteria = parse_runtime_analysis_filter_criteria(&key.filter);
    let existing_rows = filtered_rows.filter(|rows| rows.filter == key.filter);
    let rebuilt_rows = if criteria.is_active() && existing_rows.is_none() {
        Some(build_runtime_analysis_filter_rows(
            result,
            key.filter.clone(),
            cancel_token,
        )?)
    } else {
        None
    };
    let filtered_rows = existing_rows.or(rebuilt_rows.as_ref());
    let summaries = filtered_rows
        .map(|rows| rows.summaries.as_ref())
        .unwrap_or(&result.summaries);
    let summary_indices = filtered_rows
        .map(|rows| rows.summary_indices_by_path.as_ref())
        .unwrap_or(&result.summary_indices_by_path);
    let filter = key.filter.clone();
    let rows = match &key.rows {
        RuntimeViewRowsKey::Summary(sort_key, sort_direction) => {
            let _span = PerfSpan::new("runtime_sorted_summary_rows");
            let mut rows = Vec::with_capacity(summaries.len());
            for summary in summaries {
                if cancel_token.load(Ordering::Relaxed) {
                    return None;
                }
                rows.push(summary.clone());
            }
            rows.sort_by(|left, right| {
                let ordering = match sort_key {
                    RuntimeSummarySortKey::RequestCount => {
                        left.request_count.cmp(&right.request_count)
                    }
                    RuntimeSummarySortKey::RequestPath => {
                        left.request_path.cmp(&right.request_path)
                    }
                    RuntimeSummarySortKey::AverageDuration => left
                        .average_duration_ms
                        .total_cmp(&right.average_duration_ms),
                    RuntimeSummarySortKey::SlowSqlRatio => {
                        left.slow_sql_ratio.total_cmp(&right.slow_sql_ratio)
                    }
                };
                apply_direction(ordering, *sort_direction)
                    .then_with(|| left.request_path.cmp(&right.request_path))
            });
            RuntimeViewRows::Summary(RuntimeSummaryRowsCache {
                filter,
                sort_key: *sort_key,
                sort_direction: *sort_direction,
                rows: Arc::new(rows),
            })
        }
        RuntimeViewRowsKey::Requests(request_path, sort_key, sort_direction) => {
            let _span = PerfSpan::new("runtime_sorted_request_indices");
            let mut indices = summary_indices
                .get(request_path)
                .and_then(|index| summaries.get(*index))
                .map(|summary| summary.request_indices.as_ref().clone())
                .unwrap_or_default();
            if cancel_token.load(Ordering::Relaxed) {
                return None;
            }
            indices.sort_by(|left_index, right_index| {
                let left = &result.requests[*left_index];
                let right = &result.requests[*right_index];
                let ordering = match sort_key {
                    RuntimeRequestSortKey::RequestTime => {
                        left.request_timestamp_ms.cmp(&right.request_timestamp_ms)
                    }
                    RuntimeRequestSortKey::Username => left.username.cmp(&right.username),
                    RuntimeRequestSortKey::RequestDuration => {
                        left.request_duration_ms.cmp(&right.request_duration_ms)
                    }
                    RuntimeRequestSortKey::RequestPath => {
                        left.request_path.cmp(&right.request_path)
                    }
                };
                apply_direction(ordering, *sort_direction)
                    .then_with(|| right.request_timestamp_ms.cmp(&left.request_timestamp_ms))
                    .then_with(|| left.label.cmp(&right.label))
            });
            RuntimeViewRows::Requests(RuntimeRequestIndicesCache {
                filter,
                request_path: request_path.clone(),
                sort_key: *sort_key,
                sort_direction: *sort_direction,
                indices: Arc::new(indices),
            })
        }
        RuntimeViewRowsKey::Sql(request_index, sort_key, sort_direction) => {
            let _span = PerfSpan::new("runtime_sorted_sql_indices");
            let mut indices = Vec::new();
            if let Some(request) = result.requests.get(*request_index) {
                indices = if let Some(rows) = filtered_rows {
                    rows.sql_indices_by_request
                        .get(request_index)
                        .cloned()
                        .unwrap_or_default()
                } else {
                    (0..request.sql_records.len()).collect()
                };
                if cancel_token.load(Ordering::Relaxed) {
                    return None;
                }
                indices.sort_by(|left_index, right_index| {
                    let left = &request.sql_records[*left_index];
                    let right = &request.sql_records[*right_index];
                    let ordering = match sort_key {
                        RuntimeSqlSortKey::ExecuteDuration => {
                            left.execute_ms.cmp(&right.execute_ms)
                        }
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
                    apply_direction(ordering, *sort_direction)
                        .then_with(|| left_index.cmp(right_index))
                });
            }
            RuntimeViewRows::Sql(RuntimeSqlIndicesCache {
                filter,
                request_index: *request_index,
                sort_key: *sort_key,
                sort_direction: *sort_direction,
                indices: Arc::new(indices),
            })
        }
        RuntimeViewRowsKey::SqlDetail(normalized_sql) => {
            let mut rows = Vec::new();
            if let Some(filtered) = filtered_rows {
                for (request_index, indices) in filtered.sql_indices_by_request.iter() {
                    if cancel_token.load(Ordering::Relaxed) {
                        return None;
                    }
                    let request = &result.requests[*request_index];
                    for index in indices {
                        if cancel_token.load(Ordering::Relaxed) {
                            return None;
                        }
                        let sql = &request.sql_records[*index];
                        if sql.normalized_sql == *normalized_sql {
                            rows.push(RuntimeSqlFrequencyDetailRow {
                                request_index: request.index,
                                sql_index: *index,
                                execute_ms: sql.execute_ms,
                            });
                        }
                    }
                }
            } else {
                for request in &result.requests {
                    if cancel_token.load(Ordering::Relaxed) {
                        return None;
                    }
                    for (index, sql) in request.sql_records.iter().enumerate() {
                        if cancel_token.load(Ordering::Relaxed) {
                            return None;
                        }
                        if sql.normalized_sql == *normalized_sql {
                            rows.push(RuntimeSqlFrequencyDetailRow {
                                request_index: request.index,
                                sql_index: index,
                                execute_ms: sql.execute_ms,
                            });
                        }
                    }
                }
            }
            sort_runtime_sql_frequency_detail_rows(&mut rows);
            RuntimeViewRows::SqlDetail(RuntimeSqlFrequencyDetailRowsCache {
                filter,
                normalized_sql: normalized_sql.clone(),
                rows: Arc::new(rows),
            })
        }
    };
    if cancel_token.load(Ordering::Relaxed) {
        None
    } else {
        Some(rows)
    }
}

/// 保留现有表格的排序方向及稳定次序规则。
fn apply_direction(
    ordering: std::cmp::Ordering,
    direction: RuntimeSortDirection,
) -> std::cmp::Ordering {
    match direction {
        RuntimeSortDirection::Ascending => ordering,
        RuntimeSortDirection::Descending => ordering.reverse(),
    }
}

#[cfg(test)]
impl RuntimeAnalysisState {
    pub(crate) fn populate_runtime_view_cache_for_test(&self, result: &RuntimeAnalysisResult) {
        let key = RuntimeViewTaskKey::from_state(self).expect("测试应选择明细或统计页");
        build_runtime_view_rows(
            result,
            self.runtime_filter_rows_cache.as_ref(),
            &key,
            &AtomicBool::new(false),
        )
        .expect("测试任务不应取消")
        .apply(self);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 取消和关闭状态都要通知后台，并使已完成但尚未应用的结果失效。
    #[test]
    fn runtime_view_cancellation_invalidates_task_identity() {
        let cancel_token = Arc::new(AtomicBool::new(false));
        let mut computation = RuntimeViewComputationState {
            generation: 7,
            key: Some(RuntimeViewTaskKey {
                filter: RuntimeSqlAnalysisFilterSnapshot::default(),
                rows: RuntimeViewRowsKey::Summary(
                    RuntimeSummarySortKey::RequestCount,
                    RuntimeSortDirection::Descending,
                ),
            }),
            cancel_token: Some(cancel_token.clone()),
        };
        computation.cancel();
        assert!(cancel_token.load(Ordering::Relaxed));
        assert!(computation.key.is_none());
        assert_eq!(computation.generation, 8);
        let drop_token = Arc::new(AtomicBool::new(false));
        computation.cancel_token = Some(drop_token.clone());
        drop(computation);
        assert!(drop_token.load(Ordering::Relaxed));
    }

    /// 已取消的任务不能发布看似成功的空表格。
    #[test]
    fn runtime_view_cancelled_computation_returns_no_rows() {
        let result = crate::analysis::runtime::build_runtime_analysis_result(
            Vec::new(),
            Vec::new(),
            0,
            false,
        );
        let key = RuntimeViewTaskKey {
            filter: RuntimeSqlAnalysisFilterSnapshot::default(),
            rows: RuntimeViewRowsKey::Summary(
                RuntimeSummarySortKey::RequestCount,
                RuntimeSortDirection::Descending,
            ),
        };
        assert!(build_runtime_view_rows(&result, None, &key, &AtomicBool::new(true)).is_none());
        assert!(build_runtime_view_rows(&result, None, &key, &AtomicBool::new(false)).is_some());
    }
}
