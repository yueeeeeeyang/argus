//! 文件职责：提取 Runtime 请求日志分析页签状态类型与辅助函数
//! 创建日期：2026-07-08
//! 作者：Argus 开发团队
//! 主要功能：定义分析任务状态、三层下钻视图、SQL 弹窗、表格选区、排序缓存和过滤辅助函数

use std::cell::RefCell;
use std::ops::Range;
use std::sync::Arc;

use chrono::{Datelike, Local, NaiveDate, NaiveDateTime, TimeZone, Timelike};
use gpui::{Pixels, ScrollHandle, UniformListScrollHandle};

use crate::analysis::runtime::{
    RuntimeAnalysisFilterRows, RuntimeAnalysisFilterSnapshot as RuntimeSqlAnalysisFilterSnapshot,
    RuntimeAnalysisResult, RuntimeRequestSummary, RuntimeSlowSqlSummaryRow,
    RuntimeSqlFrequencyAnalysisRow, RuntimeSqlFrequencyDetailRow,
};
use crate::infra::text_selection::{
    TextSelectionGranularity, character_count, slice_character_range, word_range_at,
};

use super::types::{RuntimeDateTimePart, RuntimeFilterInputKind, TextInputState};

/// Runtime 分析任务状态，供内容区页签展示加载、结果或失败。
#[derive(Clone, Debug)]
pub(crate) enum RuntimeAnalysisTaskState {
    /// 后台任务正在读取和聚合 Runtime 日志。
    Loading {
        /// 当前加载提示。
        message: String,
    },
    /// 分析完成，可渲染三层统计表格。
    Ready(Arc<RuntimeAnalysisResult>),
}

/// Runtime 分析页当前显示层级。
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum RuntimeAnalysisView {
    /// 总解析结果总览。
    Summary,
    /// 指定请求地址的请求明细表。
    RequestDetails {
        /// 请求地址。
        request_path: String,
    },
    /// 指定请求日志的 SQL 明细表。
    SqlList {
        /// 请求地址，用于返回上一级详情页。
        request_path: String,
        /// 请求记录在结果集中的稳定索引。
        request_index: usize,
    },
}

/// Runtime 分析结果类型。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum RuntimeAnalysisResultType {
    /// 当前请求统计总览和下钻表格。
    Statistics,
    /// 按 SQL 结构聚合后的执行频率分析。
    SqlFrequency,
    /// 按 SQL 结构聚合后的平均执行耗时分析。
    SlowSql,
}

/// Runtime SQL 频率分析缓存。
#[derive(Clone, Debug)]
pub(crate) struct RuntimeSqlFrequencyRowsCache {
    /// 生成缓存时使用的过滤输入快照。
    pub filter: RuntimeSqlAnalysisFilterSnapshot,
    /// 已过滤和排序的 SQL 频率行。
    pub rows: Arc<Vec<RuntimeSqlFrequencyAnalysisRow>>,
}

/// Runtime SQL 频率详情缓存。
#[derive(Clone, Debug)]
pub(crate) struct RuntimeSqlFrequencyDetailRowsCache {
    /// 生成缓存时使用的过滤输入快照。
    pub filter: RuntimeSqlAnalysisFilterSnapshot,
    /// 当前详情页对应的 SQL 结构文本。
    pub normalized_sql: String,
    /// 已过滤和排序的 SQL 执行详情行。
    pub rows: Arc<Vec<RuntimeSqlFrequencyDetailRow>>,
}

/// Runtime 慢 SQL 分析缓存。
#[derive(Clone, Debug)]
pub(crate) struct RuntimeSlowSqlRowsCache {
    /// 生成缓存时使用的过滤输入快照。
    pub filter: RuntimeSqlAnalysisFilterSnapshot,
    /// 已过滤和排序的慢 SQL 聚合行。
    pub rows: Arc<Vec<RuntimeSlowSqlSummaryRow>>,
}

/// Runtime 总览表排序结果缓存。
#[derive(Clone, Debug)]
pub(crate) struct RuntimeSummaryRowsCache {
    /// 生成缓存时使用的过滤输入快照。
    pub filter: RuntimeSqlAnalysisFilterSnapshot,
    /// 总览表排序字段。
    pub sort_key: RuntimeSummarySortKey,
    /// 总览表排序方向。
    pub sort_direction: RuntimeSortDirection,
    /// 已过滤和排序的总览行。
    pub rows: Arc<Vec<RuntimeRequestSummary>>,
}

/// Runtime 请求明细表排序索引缓存。
#[derive(Clone, Debug)]
pub(crate) struct RuntimeRequestIndicesCache {
    /// 生成缓存时使用的过滤输入快照。
    pub filter: RuntimeSqlAnalysisFilterSnapshot,
    /// 当前请求地址。
    pub request_path: String,
    /// 请求明细表排序字段。
    pub sort_key: RuntimeRequestSortKey,
    /// 请求明细表排序方向。
    pub sort_direction: RuntimeSortDirection,
    /// 已过滤和排序的请求索引。
    pub indices: Arc<Vec<usize>>,
}

/// Runtime SQL 明细表排序索引缓存。
#[derive(Clone, Debug)]
pub(crate) struct RuntimeSqlIndicesCache {
    /// 生成缓存时使用的过滤输入快照。
    pub filter: RuntimeSqlAnalysisFilterSnapshot,
    /// 当前请求在结果集中的稳定索引。
    pub request_index: usize,
    /// SQL 明细表排序字段。
    pub sort_key: RuntimeSqlSortKey,
    /// SQL 明细表排序方向。
    pub sort_direction: RuntimeSortDirection,
    /// 已过滤和排序的 SQL 索引。
    pub indices: Arc<Vec<usize>>,
}

/// Runtime 表格排序方向。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum RuntimeSortDirection {
    /// 升序。
    Ascending,
    /// 降序。
    Descending,
}

/// Runtime 表格滚动条所属表格。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum RuntimeScrollbarTable {
    /// 总览表。
    Summary,
    /// 请求详情表。
    Request,
    /// SQL 明细表。
    Sql,
    /// SQL 频率分析表。
    SqlFrequency,
    /// SQL 频率详情表。
    SqlFrequencyDetail,
    /// 慢 SQL 分析表。
    SlowSql,
    /// 完整 SQL 弹窗代码块。
    SqlDialog,
}

/// Runtime 表格滚动条拖拽状态。
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct RuntimeScrollbarDrag {
    /// 当前被拖拽的表格。
    pub table: RuntimeScrollbarTable,
    /// 鼠标按下位置相对滑块顶部的偏移。
    pub cursor_offset: Pixels,
}

impl RuntimeSortDirection {
    /// 返回切换后的排序方向。
    pub(crate) fn toggled(self) -> Self {
        match self {
            Self::Ascending => Self::Descending,
            Self::Descending => Self::Ascending,
        }
    }

    /// 返回表头展示箭头。
    pub(crate) fn indicator(self) -> &'static str {
        match self {
            Self::Ascending => " ↑",
            Self::Descending => " ↓",
        }
    }
}

/// Runtime 总览表排序字段。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum RuntimeSummarySortKey {
    /// 请求次数。
    RequestCount,
    /// 请求地址。
    RequestPath,
    /// 平均耗时。
    AverageDuration,
    /// 慢 SQL 比例。
    SlowSqlRatio,
}

/// Runtime 请求明细表排序字段。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum RuntimeRequestSortKey {
    /// 请求时间。
    RequestTime,
    /// 用户名。
    Username,
    /// 请求耗时。
    RequestDuration,
    /// 请求地址。
    RequestPath,
}

/// Runtime SQL 明细表排序字段。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum RuntimeSqlSortKey {
    /// SQL 执行总耗时。
    ExecuteDuration,
    /// 获取连接耗时。
    AcquireConnectionDuration,
    /// 事务提交耗时。
    CommitDuration,
    /// 释放连接耗时。
    ReleaseConnectionDuration,
    /// 解析结果集耗时。
    ParseResultDuration,
    /// SQL 文本。
    SqlText,
}

/// 单个 Runtime 分析页签的持久状态。
#[derive(Clone, Debug)]
pub(crate) struct RuntimeAnalysisState {
    /// 后台任务 generation，避免旧任务覆盖新结果。
    pub generation: usize,
    /// 当前三层 drill-down 视图。
    pub view: RuntimeAnalysisView,
    /// 当前展示的 Runtime 分析结果类型。
    pub result_type: RuntimeAnalysisResultType,
    /// 总览表排序字段。
    pub summary_sort_key: RuntimeSummarySortKey,
    /// 总览表排序方向。
    pub summary_sort_direction: RuntimeSortDirection,
    /// 请求明细表排序字段。
    pub request_sort_key: RuntimeRequestSortKey,
    /// 请求明细表排序方向。
    pub request_sort_direction: RuntimeSortDirection,
    /// SQL 明细表排序字段。
    pub sql_sort_key: RuntimeSqlSortKey,
    /// SQL 明细表排序方向。
    pub sql_sort_direction: RuntimeSortDirection,
    /// 任意关键字过滤输入框状态。
    pub filter_keyword_input: TextInputState,
    /// 用户名过滤输入框状态。
    pub filter_username_input: TextInputState,
    /// 请求开始时间过滤输入框状态。
    pub filter_start_time_input: TextInputState,
    /// 请求结束时间过滤输入框状态。
    pub filter_end_time_input: TextInputState,
    /// 已应用到结果缓存的关键字过滤值，输入防抖完成前仍保持旧值。
    pub applied_filter_keyword: String,
    /// 已应用到结果缓存的用户名过滤值。
    pub applied_filter_username: String,
    /// 已应用到结果缓存的开始时间过滤值。
    pub applied_filter_start_time: String,
    /// 已应用到结果缓存的结束时间过滤值。
    pub applied_filter_end_time: String,
    /// 过滤输入 generation，用于丢弃过期防抖任务。
    pub filter_input_generation: usize,
    /// 过滤后台任务 generation，用于丢弃过期计算结果。
    pub filter_task_generation: usize,
    /// 是否存在等待防抖应用的过滤输入。
    pub is_filter_pending: bool,
    /// 是否正在后台构建过滤结果缓存。
    pub is_filter_computing: bool,
    /// 当前展开的时间选择器输入框；为空表示没有打开时间面板。
    pub open_time_picker: Option<RuntimeFilterInputKind>,
    /// 当前 Runtime 表格单元格中的文本选区。
    pub cell_selection: Option<RuntimeTableCellSelection>,
    /// 当前 Runtime 表格单元格拖拽状态。
    pub cell_selection_drag: Option<RuntimeTableCellSelectionDrag>,
    /// 当前悬浮的 Runtime SQL 文本单元格；用于只在该单元格末尾展示更多入口。
    pub hovered_sql_cell: Option<RuntimeSqlCellKey>,
    /// 当前打开的 Runtime SQL 完整文本弹窗。
    pub sql_text_dialog: Option<RuntimeSqlTextDialog>,
    /// 总览表滚动句柄。
    pub summary_scroll: UniformListScrollHandle,
    /// 请求明细表滚动句柄。
    pub request_scroll: UniformListScrollHandle,
    /// SQL 明细表滚动句柄。
    pub sql_scroll: UniformListScrollHandle,
    /// SQL 频率分析表滚动句柄。
    pub sql_frequency_scroll: UniformListScrollHandle,
    /// SQL 频率详情表滚动句柄。
    pub sql_frequency_detail_scroll: UniformListScrollHandle,
    /// 慢 SQL 分析表滚动句柄。
    pub slow_sql_scroll: UniformListScrollHandle,
    /// SQL 频率分析当前打开的详情 SQL；为空时展示频率列表。
    pub sql_frequency_detail_sql: Option<String>,
    /// 慢 SQL 分析当前打开的详情 SQL；为空时展示慢 SQL 聚合列表。
    pub slow_sql_detail_sql: Option<String>,
    /// Runtime 三类结果共享的过滤行缓存，避免切换页面和滚动时重复全量扫描。
    pub runtime_filter_rows_cache: Option<RuntimeAnalysisFilterRows>,
    /// SQL 频率分析后台计算 generation，用于丢弃过期结果。
    pub sql_frequency_rows_task_generation: usize,
    /// 慢 SQL 分析后台计算 generation，用于丢弃过期结果。
    pub slow_sql_rows_task_generation: usize,
    /// SQL 频率分析是否正在后台计算。
    pub is_sql_frequency_rows_computing: bool,
    /// 慢 SQL 分析是否正在后台计算。
    pub is_slow_sql_rows_computing: bool,
    /// 当前正在计算的 SQL 频率过滤快照。
    pub sql_frequency_rows_computing_filter: Option<RuntimeSqlAnalysisFilterSnapshot>,
    /// 当前正在计算的慢 SQL 过滤快照。
    pub slow_sql_rows_computing_filter: Option<RuntimeSqlAnalysisFilterSnapshot>,
    /// SQL 频率分析过滤结果缓存，避免滚动重绘时重复全量聚合。
    pub sql_frequency_rows_cache: RefCell<Option<RuntimeSqlFrequencyRowsCache>>,
    /// SQL 频率详情过滤结果缓存，避免详情滚动重绘时重复全量扫描。
    pub sql_frequency_detail_rows_cache: RefCell<Option<RuntimeSqlFrequencyDetailRowsCache>>,
    /// 慢 SQL 分析过滤结果缓存，避免滚动重绘时重复全量排序。
    pub slow_sql_rows_cache: RefCell<Option<RuntimeSlowSqlRowsCache>>,
    /// 总览表排序缓存，避免切换页签或重绘时重复排序。
    pub summary_rows_cache: RefCell<Option<RuntimeSummaryRowsCache>>,
    /// 请求明细表排序缓存，避免切换页签或重绘时重复排序。
    pub request_indices_cache: RefCell<Option<RuntimeRequestIndicesCache>>,
    /// SQL 明细表排序缓存，避免切换页签或重绘时重复排序。
    pub sql_indices_cache: RefCell<Option<RuntimeSqlIndicesCache>>,
    /// 当前 Runtime 表格滚动条拖拽状态。
    pub scrollbar_drag: Option<RuntimeScrollbarDrag>,
    /// 完整 SQL 弹窗代码块滚动句柄，用于自定义可拖拽滚动条。
    pub sql_dialog_scroll: ScrollHandle,
    /// 当前任务状态。
    pub task_state: RuntimeAnalysisTaskState,
}

/// Runtime SQL 文本单元格悬浮目标，用于在单元格末尾展示"更多"入口。
///
/// 统计分析、频率/慢 SQL 详情中的具体 SQL 记录使用 `Record`；
/// 频率/慢 SQL 分析中的聚合行使用 `Summary`。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum RuntimeSqlCellKey {
    /// 具体请求中的某条 SQL 记录。
    Record {
        /// 请求记录在分析结果中的稳定索引。
        request_index: usize,
        /// SQL 记录在当前请求中的稳定索引。
        sql_index: usize,
    },
    /// 频率/慢 SQL 分析中的聚合行。
    Summary {
        /// 聚合行在当前结果列表中的索引。
        row_index: usize,
    },
}

/// Runtime SQL 完整文本弹窗状态。
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct RuntimeSqlTextDialog {
    /// 请求地址。
    pub request_path: String,
    /// 请求时间展示文本。
    pub request_time_label: String,
    /// 用户名展示文本。
    pub username: String,
    /// SQL 原文，保留解析结果中的换行和缩进。
    pub sql_text: String,
    /// 当前 SQL 弹窗正文选区。
    pub selection: Option<RuntimeSqlTextSelection>,
    /// 当前 SQL 弹窗正文拖拽状态。
    pub selection_drag: Option<RuntimeSqlTextSelectionDrag>,
}

/// Runtime SQL 弹窗正文中的文本位置，使用行号和字符列表达。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct RuntimeSqlTextPosition {
    /// 0 基 SQL 行号。
    pub line_index: usize,
    /// 行内字符列，按 Unicode 标量值计数。
    pub column: usize,
}

/// Runtime SQL 弹窗正文选区，支持跨行复制。
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct RuntimeSqlTextSelection {
    /// 鼠标按下时的选区锚点。
    pub anchor: RuntimeSqlTextPosition,
    /// 当前拖拽到的焦点位置。
    pub focus: RuntimeSqlTextPosition,
}

impl RuntimeSqlTextSelection {
    /// 返回选区是否为空。
    pub(crate) fn is_empty(&self) -> bool {
        self.anchor == self.focus
    }

    /// 返回按文档顺序排列后的起止位置。
    pub(crate) fn normalized(&self) -> (RuntimeSqlTextPosition, RuntimeSqlTextPosition) {
        if runtime_sql_text_position_le(self.anchor, self.focus) {
            (self.anchor, self.focus)
        } else {
            (self.focus, self.anchor)
        }
    }
}

/// Runtime SQL 弹窗正文拖拽选择状态。
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct RuntimeSqlTextSelectionDrag {
    /// 鼠标按下时形成的基础选区。
    pub anchor_range: RuntimeSqlTextSelection,
    /// 当前拖拽粒度，决定后续移动时按字符、词或整行扩展。
    pub granularity: TextSelectionGranularity,
}

/// Runtime 表格单元格的单行文本选区。
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct RuntimeTableCellSelection {
    /// 单元格稳定 key，包含当前分析页内的层级、行和列身份。
    pub cell_key: String,
    /// 当前单元格完整文本；复制时从该文本中截取选区。
    pub text: String,
    /// 选区锚点字符列。
    pub anchor: usize,
    /// 选区焦点字符列。
    pub focus: usize,
}

impl RuntimeTableCellSelection {
    /// 返回按字符顺序归一化后的非空选区。
    pub(crate) fn normalized_range(&self) -> Option<Range<usize>> {
        let text_length = character_count(&self.text);
        let start = self.anchor.min(self.focus).min(text_length);
        let end = self.anchor.max(self.focus).min(text_length);
        (start < end).then_some(start..end)
    }
}

/// Runtime 表格单元格拖拽选择状态。
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct RuntimeTableCellSelectionDrag {
    /// 本次拖拽起始的单元格 key。
    pub cell_key: String,
    /// 本次拖拽起始的单元格完整文本。
    pub text: String,
    /// 鼠标按下时按点击次数扩展后的基础字符范围。
    pub anchor_range: Range<usize>,
    /// 当前选择粒度，单击按字符，双击及以上按整格内容。
    pub granularity: TextSelectionGranularity,
}

/// 根据点击次数返回 Runtime 表格单元格文本的字符选区。
///
/// 参数说明：
/// - `text`：当前单元格完整文本。
/// - `character_index`：命中的字符列。
/// - `granularity`：选择粒度；Runtime 表格要求双击选中整格内容。
///
/// 返回值：按字符索引表示的选区范围。
pub(super) fn runtime_cell_range_for_granularity(
    text: &str,
    character_index: usize,
    granularity: TextSelectionGranularity,
) -> Range<usize> {
    let text_length = character_count(text);
    let cursor = character_index.min(text_length);
    match granularity {
        TextSelectionGranularity::Character => cursor..cursor,
        TextSelectionGranularity::Word | TextSelectionGranularity::Line => 0..text_length,
    }
}

/// 判断 Runtime SQL 弹窗文本位置是否按文档顺序不晚于另一个位置。
pub(super) fn runtime_sql_text_position_le(
    left: RuntimeSqlTextPosition,
    right: RuntimeSqlTextPosition,
) -> bool {
    left.line_index < right.line_index
        || (left.line_index == right.line_index && left.column <= right.column)
}

/// 按点击粒度生成 Runtime SQL 弹窗正文选区。
pub(super) fn runtime_sql_text_range_for_granularity(
    line_index: usize,
    line: &str,
    character_index: usize,
    granularity: TextSelectionGranularity,
) -> RuntimeSqlTextSelection {
    let line_length = character_count(line);
    let cursor = character_index.min(line_length);
    let range = match granularity {
        TextSelectionGranularity::Character => cursor..cursor,
        TextSelectionGranularity::Word => word_range_at(line, cursor).unwrap_or(cursor..cursor),
        TextSelectionGranularity::Line => 0..line_length,
    };

    RuntimeSqlTextSelection {
        anchor: RuntimeSqlTextPosition {
            line_index,
            column: range.start,
        },
        focus: RuntimeSqlTextPosition {
            line_index,
            column: range.end,
        },
    }
}

/// 将 SQL 原文按弹窗展示规则拆成行，保留空行和缩进。
pub(super) fn runtime_sql_text_lines(sql_text: &str) -> Vec<String> {
    sql_text
        .replace("\r\n", "\n")
        .replace('\r', "\n")
        .split('\n')
        .map(ToString::to_string)
        .collect()
}

/// 从 Runtime SQL 弹窗行集合中提取当前选区文本，保留跨行换行符。
pub(super) fn selected_runtime_sql_text_from_lines(
    lines: &[String],
    selection: &RuntimeSqlTextSelection,
) -> Option<String> {
    if selection.is_empty() || lines.is_empty() {
        return None;
    }

    let (start, end) = selection.normalized();
    if start.line_index >= lines.len() {
        return None;
    }

    let end_line = end.line_index.min(lines.len().saturating_sub(1));
    let mut selected = String::new();
    for (line_index, line) in lines
        .iter()
        .enumerate()
        .take(end_line + 1)
        .skip(start.line_index)
    {
        if line_index > start.line_index {
            selected.push('\n');
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
        if start_column < end_column {
            selected.push_str(&slice_character_range(line, start_column..end_column));
        }
    }

    (!selected.is_empty()).then_some(selected)
}

/// 清理 Runtime 分析页所有过滤输入框焦点态。
pub(super) fn clear_runtime_filter_inputs_focus(state: &mut RuntimeAnalysisState) {
    clear_runtime_filter_input_focus(&mut state.filter_keyword_input);
    clear_runtime_filter_input_focus(&mut state.filter_username_input);
    clear_runtime_filter_input_focus(&mut state.filter_start_time_input);
    clear_runtime_filter_input_focus(&mut state.filter_end_time_input);
    state.open_time_picker = None;
}

/// 从 Runtime 过滤输入框状态生成原始输入快照。
pub(super) fn runtime_filter_input_snapshot_from_state(
    state: &RuntimeAnalysisState,
) -> RuntimeSqlAnalysisFilterSnapshot {
    RuntimeSqlAnalysisFilterSnapshot {
        keyword: state.filter_keyword_input.value.clone(),
        username: state.filter_username_input.value.clone(),
        start_time: state.filter_start_time_input.value.clone(),
        end_time: state.filter_end_time_input.value.clone(),
    }
}

/// 从 Runtime 已应用过滤值生成快照。
pub(super) fn runtime_filter_applied_snapshot_from_state(
    state: &RuntimeAnalysisState,
) -> RuntimeSqlAnalysisFilterSnapshot {
    RuntimeSqlAnalysisFilterSnapshot {
        keyword: state.applied_filter_keyword.clone(),
        username: state.applied_filter_username.clone(),
        start_time: state.applied_filter_start_time.clone(),
        end_time: state.applied_filter_end_time.clone(),
    }
}

/// 将过滤快照写入 Runtime 已应用状态。
pub(super) fn apply_runtime_filter_snapshot_to_state(
    state: &mut RuntimeAnalysisState,
    filter: &RuntimeSqlAnalysisFilterSnapshot,
) {
    state.applied_filter_keyword = filter.keyword.clone();
    state.applied_filter_username = filter.username.clone();
    state.applied_filter_start_time = filter.start_time.clone();
    state.applied_filter_end_time = filter.end_time.clone();
}

/// 过滤结果真正生效后清理表格滚动、选区和旧的局部缓存。
pub(super) fn reset_runtime_filter_result_view_state(state: &mut RuntimeAnalysisState) {
    state.summary_scroll = UniformListScrollHandle::new();
    state.request_scroll = UniformListScrollHandle::new();
    state.sql_scroll = UniformListScrollHandle::new();
    state.sql_frequency_scroll = UniformListScrollHandle::new();
    state.sql_frequency_detail_scroll = UniformListScrollHandle::new();
    state.slow_sql_scroll = UniformListScrollHandle::new();
    state.cell_selection = None;
    state.cell_selection_drag = None;
    state.hovered_sql_cell = None;
    state.sql_text_dialog = None;
    state.sql_dialog_scroll = ScrollHandle::new();
    // 弹窗关闭后清理可能残留的弹窗滚动条拖拽状态，避免影响表格滚动条。
    state.scrollbar_drag = state
        .scrollbar_drag
        .filter(|drag| drag.table != RuntimeScrollbarTable::SqlDialog);
    state.sql_frequency_rows_cache.borrow_mut().take();
    state.sql_frequency_detail_rows_cache.borrow_mut().take();
    state.slow_sql_rows_cache.borrow_mut().take();
    state.summary_rows_cache.borrow_mut().take();
    state.request_indices_cache.borrow_mut().take();
    state.sql_indices_cache.borrow_mut().take();
    state.sql_frequency_rows_task_generation =
        state.sql_frequency_rows_task_generation.saturating_add(1);
    state.slow_sql_rows_task_generation = state.slow_sql_rows_task_generation.saturating_add(1);
    state.is_sql_frequency_rows_computing = false;
    state.is_slow_sql_rows_computing = false;
    state.sql_frequency_rows_computing_filter = None;
    state.slow_sql_rows_computing_filter = None;
    state.scrollbar_drag = None;
}

/// 清理单个 Runtime 过滤输入框焦点态，保留文本和光标位置。
pub(super) fn clear_runtime_filter_input_focus(input: &mut TextInputState) {
    input.clear_focus();
}

/// 返回 Runtime 过滤输入框规范化后的非空选区。
pub(super) fn normalized_runtime_filter_input_selection_range(
    input: &TextInputState,
) -> Option<Range<usize>> {
    input.selection_range()
}

/// 解析 Runtime 时间过滤输入，支持毫秒时间戳和常见本地日期时间格式。
pub(super) fn parse_runtime_filter_datetime_value(
    raw: &str,
    is_end: bool,
) -> Option<chrono::DateTime<Local>> {
    let value = raw.trim();
    if value.is_empty() {
        return None;
    }
    if let Ok(timestamp_ms) = value.parse::<i64>() {
        return Local.timestamp_millis_opt(timestamp_ms).single();
    }

    for format in [
        "%Y-%m-%d %H:%M:%S%.3f",
        "%Y-%m-%d %H:%M:%S",
        "%Y-%m-%d %H:%M",
    ] {
        if let Ok(datetime) = NaiveDateTime::parse_from_str(value, format)
            && let Some(local_datetime) = Local.from_local_datetime(&datetime).single()
        {
            return Some(local_datetime);
        }
    }

    if let Ok(date) = NaiveDate::parse_from_str(value, "%Y-%m-%d") {
        let datetime = if is_end {
            date.and_hms_milli_opt(23, 59, 59, 999)
        } else {
            date.and_hms_milli_opt(0, 0, 0, 0)
        }?;
        return Local.from_local_datetime(&datetime).single();
    }

    None
}

/// 把 Runtime 时间过滤值格式化为用户可读且可再次解析的本地时间。
pub(super) fn format_runtime_filter_datetime_value(datetime: chrono::DateTime<Local>) -> String {
    datetime.format("%Y-%m-%d %H:%M:%S").to_string()
}

/// 返回指定年月的最大日期，用于调整年月时夹住当前日。
pub(super) fn runtime_days_in_month(year: i32, month: u32) -> u32 {
    let (next_year, next_month) = if month == 12 {
        (year + 1, 1)
    } else {
        (year, month + 1)
    };
    let Some(next_month_start) = NaiveDate::from_ymd_opt(next_year, next_month, 1) else {
        return 28;
    };
    next_month_start
        .pred_opt()
        .map(|date| date.day())
        .unwrap_or(28)
}

/// 按年月调整 Runtime 时间，保留当前时分秒并处理月末越界。
pub(super) fn adjust_runtime_datetime_month(
    datetime: chrono::DateTime<Local>,
    delta: i32,
) -> chrono::DateTime<Local> {
    let mut year = datetime.year();
    let mut month = datetime.month() as i32;
    let month_index = year * 12 + (month - 1) + delta;
    year = month_index.div_euclid(12);
    month = month_index.rem_euclid(12) + 1;

    let month = month.clamp(1, 12) as u32;
    let day = datetime.day().min(runtime_days_in_month(year, month));
    let Some(date) = NaiveDate::from_ymd_opt(year, month, day) else {
        return datetime;
    };
    let Some(naive) = date.and_hms_opt(datetime.hour(), datetime.minute(), datetime.second())
    else {
        return datetime;
    };
    Local
        .from_local_datetime(&naive)
        .single()
        .unwrap_or(datetime)
}

/// 按指定部分调整 Runtime 时间过滤值。
pub(super) fn adjust_runtime_datetime_part(
    datetime: chrono::DateTime<Local>,
    part: RuntimeDateTimePart,
    delta: i32,
) -> chrono::DateTime<Local> {
    match part {
        RuntimeDateTimePart::Month => adjust_runtime_datetime_month(datetime, delta),
        RuntimeDateTimePart::Hour => datetime + chrono::Duration::hours(delta as i64),
        RuntimeDateTimePart::Minute => datetime + chrono::Duration::minutes(delta as i64),
        RuntimeDateTimePart::Second => datetime + chrono::Duration::seconds(delta as i64),
    }
}

/// 返回 Runtime 时间过滤输入框在空值时使用的默认时间。
pub(super) fn default_runtime_filter_datetime(is_end: bool) -> chrono::DateTime<Local> {
    let now = Local::now();
    if is_end {
        let Some(naive) = now.date_naive().and_hms_opt(23, 59, 59) else {
            return now;
        };
        Local.from_local_datetime(&naive).single().unwrap_or(now)
    } else {
        let Some(naive) = now.date_naive().and_hms_opt(0, 0, 0) else {
            return now;
        };
        Local.from_local_datetime(&naive).single().unwrap_or(now)
    }
}
