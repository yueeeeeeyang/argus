//! 文件职责：提取 Jstack 线程日志分析页签状态类型与辅助函数
//! 创建日期：2026-07-08
//! 作者：Argus 开发团队
//! 主要功能：定义分析任务状态、频率矩阵状态、线程名选区和可见行筛选逻辑

use std::collections::BTreeSet;
use std::ops::Range;

use gpui::UniformListScrollHandle;

use crate::analysis::jstack::{
    JstackAnalysisResult, JstackFrequencyRow, JstackThreadFilter, JstackThreadStackOccurrence,
    JstackThreadState,
};
use crate::infra::text_selection::{TextSelectionGranularity, character_count, word_range_at};

/// Jstack 分析任务状态，供内容区页签展示加载、结果或失败。
#[derive(Clone, Debug)]
pub(crate) enum JstackAnalysisTaskState {
    /// 后台任务正在读取和聚合线程栈。
    Loading {
        /// 当前加载提示。
        message: String,
    },
    /// 分析完成，可渲染频率矩阵。
    Ready(JstackAnalysisResult),
}

/// 单个 Jstack 分析页签的持久状态。
#[derive(Clone, Debug)]
pub(crate) struct JstackAnalysisState {
    /// 后台任务 generation，避免旧任务覆盖新结果。
    pub generation: usize,
    /// 当前启用的线程状态筛选项；默认仅展示 RUNNABLE。
    pub active_states: BTreeSet<JstackThreadState>,
    /// 是否启用设置页配置的线程堆栈过滤；新分析页默认开启。
    pub is_thread_filter_enabled: bool,
    /// 当前在分析矩阵左侧线程名列中选中的文本范围。
    pub thread_name_selection: Option<JstackThreadNameSelection>,
    /// 当前线程名列拖拽选择状态，用于持续扩展选区。
    pub thread_name_selection_drag: Option<JstackThreadNameSelectionDrag>,
    /// 当前点击过的线程方块 key，用于在矩阵中高亮具体快照格子。
    pub selected_cell_key: Option<String>,
    /// 当前筛选条件下可见的结果行索引，避免矩阵滚动渲染时重复扫描全部线程。
    pub visible_row_indices: Vec<usize>,
    /// 当前线程堆栈配置过滤隐藏的线程数量，用于标题统计展示。
    pub filtered_row_count: usize,
    /// 线程频率矩阵行虚拟列表滚动句柄。
    pub row_scroll: UniformListScrollHandle,
    /// 当前任务状态。
    pub task_state: JstackAnalysisTaskState,
}

/// Jstack 分析矩阵左侧线程名的单行文本选区。
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct JstackThreadNameSelection {
    /// 线程身份 key，包含线程名和线程 ID，用于区分同名线程。
    pub thread_identity: String,
    /// 当前显示的线程名文本；复制时只复制该文本的选中片段。
    pub thread_name: String,
    /// 选区锚点字符列。
    pub anchor: usize,
    /// 选区焦点字符列。
    pub focus: usize,
}

impl JstackThreadNameSelection {
    /// 返回按字符顺序归一化后的非空选区。
    pub(crate) fn normalized_range(&self) -> Option<Range<usize>> {
        let text_length = character_count(&self.thread_name);
        let start = self.anchor.min(self.focus).min(text_length);
        let end = self.anchor.max(self.focus).min(text_length);
        (start < end).then_some(start..end)
    }
}

/// Jstack 分析矩阵左侧线程名拖拽选择状态。
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct JstackThreadNameSelectionDrag {
    /// 本次拖拽起始的线程身份 key。
    pub thread_identity: String,
    /// 本次拖拽起始的线程名文本。
    pub thread_name: String,
    /// 鼠标按下时按点击次数扩展后的基础字符范围。
    pub anchor_range: Range<usize>,
    /// 当前选择粒度，支持单击字符、双击词和三击整行。
    pub granularity: TextSelectionGranularity,
}

impl JstackAnalysisState {
    /// 根据当前状态筛选和配置过滤规则重建可见行缓存。
    ///
    /// 参数说明：
    /// - `thread_filter`：设置页当前配置的线程过滤器。
    ///
    /// 返回值：无；结果会写入 `visible_row_indices` 和 `filtered_row_count`。
    pub(crate) fn rebuild_visible_row_cache(&mut self, thread_filter: &JstackThreadFilter) {
        let JstackAnalysisTaskState::Ready(result) = &self.task_state else {
            self.visible_row_indices.clear();
            self.filtered_row_count = 0;
            return;
        };

        let should_filter_threads = self.is_thread_filter_enabled && !thread_filter.is_empty();
        self.filtered_row_count = if should_filter_threads {
            result
                .rows
                .iter()
                .filter(|row| thread_filter.matches_row(row))
                .count()
        } else {
            0
        };
        self.visible_row_indices = visible_jstack_row_indices(
            result,
            &self.active_states,
            should_filter_threads.then_some(thread_filter),
        );
    }
}

/// 返回当前 Jstack 筛选条件下需要渲染的结果行索引。
pub(super) fn visible_jstack_row_indices(
    result: &JstackAnalysisResult,
    active_states: &BTreeSet<JstackThreadState>,
    thread_filter: Option<&JstackThreadFilter>,
) -> Vec<usize> {
    if active_states.is_empty() {
        return Vec::new();
    }

    let mut visible_rows = result
        .rows
        .iter()
        .enumerate()
        .filter_map(|(index, row)| {
            if thread_filter.is_some_and(|filter| filter.matches_row(row)) {
                return None;
            }

            // 按当前状态筛选后的实际命中次数排序，避免隐藏状态的历史出现次数把低命中线程顶到前面。
            let visible_hit_count = row
                .cells
                .iter()
                .filter(|cell| {
                    cell.count > 0
                        && cell
                            .state
                            .is_some_and(|state| active_states.contains(&state))
                })
                .map(|cell| cell.count)
                .sum::<usize>();

            (visible_hit_count > 0).then_some((index, visible_hit_count))
        })
        .collect::<Vec<_>>();

    visible_rows.sort_by(|(left_index, left_count), (right_index, right_count)| {
        right_count.cmp(left_count).then_with(|| {
            result.rows[*left_index]
                .thread_name
                .cmp(&result.rows[*right_index].thread_name)
                .then_with(|| {
                    result.rows[*left_index]
                        .thread_id
                        .cmp(&result.rows[*right_index].thread_id)
                })
        })
    });
    visible_rows.into_iter().map(|(index, _)| index).collect()
}

/// 为线程详情窗口收集当前可见状态下的代表堆栈记录。
///
/// 参数说明：
/// - `row`：频率矩阵中的线程行。
/// - `active_states`：当前启用的线程状态筛选。
/// - `active_snapshot_index`：点击方块所在快照序号。
/// - `active_occurrence_index`：点击方块在同一快照内选中的出现序号。
///
/// 返回值：按快照顺序排列的堆栈记录，每个快照最多一条。
pub(super) fn jstack_detail_occurrences_for_visible_cells(
    row: &JstackFrequencyRow,
    active_states: &BTreeSet<JstackThreadState>,
    active_snapshot_index: usize,
    active_occurrence_index: usize,
) -> Vec<JstackThreadStackOccurrence> {
    if active_states.is_empty() {
        return Vec::new();
    }

    row.cells
        .iter()
        .filter_map(|cell| {
            let cell_state = cell.state?;
            if cell.count == 0 || !active_states.contains(&cell_state) {
                return None;
            }

            if cell.snapshot_index == active_snapshot_index {
                return cell
                    .stack_occurrences
                    .iter()
                    .find(|occurrence| occurrence.occurrence_index == active_occurrence_index)
                    .or_else(|| {
                        cell.stack_occurrences
                            .iter()
                            .find(|occurrence| occurrence.state == cell_state)
                    })
                    .or_else(|| cell.stack_occurrences.first())
                    .cloned();
            }

            cell.stack_occurrences
                .iter()
                .find(|occurrence| occurrence.state == cell_state)
                .or_else(|| cell.stack_occurrences.first())
                .cloned()
        })
        .collect()
}

/// 生成 Jstack 频率矩阵方块的稳定选择 key。
///
/// 参数说明：
/// - `row_index`：分析结果中的线程行索引。
/// - `snapshot_index`：快照列索引。
///
/// 返回值：可在状态和 UI 之间共享的方块标识。
pub(crate) fn jstack_cell_selection_key(row_index: usize, snapshot_index: usize) -> String {
    format!("{row_index}:{snapshot_index}")
}

/// 根据点击次数返回 Jstack 线程名文本的字符选区。
///
/// 参数说明：
/// - `thread_name`：当前显示的线程名。
/// - `character_index`：命中的字符列。
/// - `granularity`：选择粒度。
///
/// 返回值：按字符索引表示的选区范围。
pub(super) fn jstack_thread_name_range_for_granularity(
    thread_name: &str,
    character_index: usize,
    granularity: TextSelectionGranularity,
) -> Range<usize> {
    let text_length = character_count(thread_name);
    let cursor = character_index.min(text_length);
    match granularity {
        TextSelectionGranularity::Character => cursor..cursor,
        TextSelectionGranularity::Word => {
            word_range_at(thread_name, cursor).unwrap_or(cursor..cursor)
        }
        TextSelectionGranularity::Line => 0..text_length,
    }
}
