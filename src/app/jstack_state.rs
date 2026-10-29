//! 文件职责：提取 Jstack 线程日志分析页签状态类型与辅助函数
//! 创建日期：2026-07-08
//! 修改日期：2026-09-24
//! 作者：Argus 开发团队
//! 主要功能：定义分析任务状态、频率矩阵状态、线程名选区和可见行筛选逻辑

use std::collections::{BTreeSet, HashMap};
use std::ops::Range;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use gpui::UniformListScrollHandle;

use crate::analysis::jstack::{
    JstackAnalysisResult, JstackFrequencyRow, JstackThreadFilter, JstackThreadStackOccurrence,
    JstackThreadState, JstackVisibleCell,
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
    /// 用户取消了后台分析任务；仅保留取消前的进度描述。
    Cancelled {
        /// 取消结果提示。
        message: String,
    },
}

/// 单个 Jstack 分析页签的持久状态。
#[derive(Debug)]
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
    /// 按原始行索引缓存筛选后的格子摘要；渲染不再重复扫描堆栈，原始样本保持不变。
    pub visible_cells: HashMap<usize, Vec<JstackVisibleCell>>,
    /// 配置规则完全排除的线程数量；仅部分样本被隐藏的线程不计入。
    pub filtered_row_count: usize,
    /// 线程频率矩阵行虚拟列表滚动句柄。
    pub row_scroll: UniformListScrollHandle,
    /// 当前任务状态。
    pub task_state: JstackAnalysisTaskState,
    /// 在途分析任务的取消标记；任务进入终态后清空。
    pub cancel_token: Option<Arc<AtomicBool>>,
}

impl Drop for JstackAnalysisState {
    /// 标签关闭、来源替换和窗口释放都必须通知工作线程停止。
    fn drop(&mut self) {
        if let Some(cancel_token) = self.cancel_token.take() {
            cancel_token.store(true, std::sync::atomic::Ordering::Relaxed);
        }
    }
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
    /// 返回值：无；重建可见行、格子摘要和完全过滤的线程数，清除旧的格子选择。
    pub(crate) fn rebuild_visible_row_cache(&mut self, thread_filter: &JstackThreadFilter) {
        self.selected_cell_key = None;
        self.thread_name_selection = None;
        self.thread_name_selection_drag = None;
        let JstackAnalysisTaskState::Ready(result) = &self.task_state else {
            self.visible_row_indices.clear();
            self.visible_cells.clear();
            self.filtered_row_count = 0;
            return;
        };

        let should_filter_threads = self.is_thread_filter_enabled && !thread_filter.is_empty();
        let visible = compute_visible_jstack_rows(
            result,
            &self.active_states,
            should_filter_threads.then_some(thread_filter),
        );
        self.filtered_row_count = visible.filtered_row_count;
        self.visible_row_indices = visible.indices;
        self.visible_cells = visible.cells;
    }
}

/// 一次筛选生成的可见矩阵，只存摘要，不复制完整堆栈。
struct JstackVisibleRows {
    indices: Vec<usize>,
    cells: HashMap<usize, Vec<JstackVisibleCell>>,
    filtered_row_count: usize,
}

/// 按样本应用状态与堆栈规则，用同一份摘要决定显示、计数和排序。
fn compute_visible_jstack_rows(
    result: &JstackAnalysisResult,
    active_states: &BTreeSet<JstackThreadState>,
    thread_filter: Option<&JstackThreadFilter>,
) -> JstackVisibleRows {
    // 不提前返回：状态全不选时可见行自然为空，但过滤统计仍按全部行计算，与历史口径一致。
    let mut matched_row_count = 0_usize;
    let mut visible_rows = Vec::new();
    let mut visible_cells = HashMap::new();
    for (index, row) in result.rows.iter().enumerate() {
        let is_filtered = thread_filter.is_some_and(|filter| filter.matches_row(row));
        if is_filtered {
            matched_row_count += 1;
            continue;
        }
        if active_states.is_empty() {
            continue;
        }

        // 一份样本被过滤不能抹掉同线程的其他执行记录；主状态也必须从剩余样本重算。
        let cells = row
            .cells
            .iter()
            .map(|cell| cell.visible_summary(active_states, thread_filter))
            .collect::<Vec<_>>();
        let visible_hit_count = cells.iter().map(|cell| cell.count).sum::<usize>();

        if visible_hit_count > 0 {
            visible_rows.push((index, visible_hit_count));
            visible_cells.insert(index, cells);
        }
    }

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
    JstackVisibleRows {
        filtered_row_count: matched_row_count,
        indices: visible_rows.into_iter().map(|(index, _)| index).collect(),
        cells: visible_cells,
    }
}

/// 返回当前 Jstack 筛选条件下需要渲染的结果行索引。
///
/// 说明：生产路径统一走 `compute_visible_jstack_rows` 单遍计算；该入口保留给测试直接断言可见行。
#[cfg(test)]
pub(super) fn visible_jstack_row_indices(
    result: &JstackAnalysisResult,
    active_states: &BTreeSet<JstackThreadState>,
    thread_filter: Option<&JstackThreadFilter>,
) -> Vec<usize> {
    compute_visible_jstack_rows(result, active_states, thread_filter).indices
}

/// 为线程详情窗口收集当前可见状态下的代表堆栈记录。
///
/// 参数说明：
/// - `row`：频率矩阵中的线程行。
/// - `active_states`：当前启用的线程状态筛选。
/// - `active_snapshot_index`：点击方块所在快照序号。
/// - `active_occurrence_index`：点击方块在同一快照内选中的出现序号。
/// - `thread_filter`：当前生效的配置过滤；关闭时传 `None`。
///
/// 返回值：按快照顺序排列的堆栈记录，每个快照最多一条。
pub(super) fn jstack_detail_occurrences_for_visible_cells(
    row: &JstackFrequencyRow,
    active_states: &BTreeSet<JstackThreadState>,
    active_snapshot_index: usize,
    active_occurrence_index: usize,
    thread_filter: Option<&JstackThreadFilter>,
) -> Vec<JstackThreadStackOccurrence> {
    if active_states.is_empty()
        || thread_filter.is_some_and(|filter| filter.matches_thread_name(&row.thread_name))
    {
        return Vec::new();
    }

    row.cells
        .iter()
        .filter_map(|cell| {
            let summary = cell.visible_summary(active_states, thread_filter);
            let representative = summary.representative_occurrence_index?;
            let selected = (cell.snapshot_index == active_snapshot_index)
                .then(|| {
                    cell.visible_occurrences(active_states, thread_filter)
                        .find(|occurrence| occurrence.occurrence_index == active_occurrence_index)
                })
                .flatten();
            selected
                .or_else(|| {
                    cell.visible_occurrences(active_states, thread_filter)
                        .find(|occurrence| occurrence.occurrence_index == representative)
                })
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
