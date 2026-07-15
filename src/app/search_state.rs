// 文件职责: 提取日志搜索运行期状态类型定义
// 创建日期: 2026-07-08
// 作者: Argus 开发团队
// 主要功能: 定义搜索任务状态、结果分组、滚动条拖拽和搜索面板状态

use std::collections::BTreeSet;
use std::sync::{Arc, atomic::AtomicBool};

use gpui::{Pixels, UniformListScrollHandle, WindowHandle};

use crate::loader::SourceId;
use crate::search::search_engine::{SearchProgress, SearchResult, SearchScope};
use crate::search::search_task::SearchTaskState;
use crate::ui::log_search_window::LogSearchWindow;

use super::constants::SEARCH_RESULT_PANEL_HEIGHT_DEFAULT;
use super::types::TextInputState;

/// 当前日志快速查找缓存键，避免关键字、选项或日志变化后复用过期结果。
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct QuickMatchKey {
    /// 当前日志来源节点。
    pub source_id: SourceId,
    /// 当前关键字。
    pub keyword: String,
    /// 是否区分大小写。
    pub case_sensitive: bool,
    /// 是否启用正则。
    pub regex_enabled: bool,
}

/// 搜索结果文件分组，记录结果在全量列表中的连续范围。
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct SearchResultGroup {
    /// 分组对应的来源节点。
    pub source_id: SourceId,
    /// 文件展示名称。
    pub label: String,
    /// 文件展示路径。
    pub path: String,
    /// 分组内第一条结果的全量索引。
    pub start_index: usize,
    /// 分组内最后一条结果之后的位置。
    pub end_index: usize,
}

/// 搜索结果面板虚拟列表中的可见行。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum SearchResultListItem {
    /// 文件分组标题行。
    Group(usize),
    /// 单条命中结果行。
    Result(usize),
}

/// 日志搜索任务来源，用于结果面板区分普通搜索和快搜。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum SearchRunKind {
    /// 搜索窗口关键字输入框发起的普通搜索。
    Normal,
    /// 设置中的快搜关键字集合发起的一键搜索。
    QuickKeywords,
}

/// 搜索结果面板自绘滚动条方向。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum SearchResultScrollbarAxis {
    /// 纵向结果滚动。
    Vertical,
    /// 横向预览滚动。
    Horizontal,
}

/// 搜索结果面板滚动条拖拽状态。
#[derive(Clone, Copy, Debug)]
pub(crate) struct SearchResultScrollbarDrag {
    /// 当前拖动方向。
    pub axis: SearchResultScrollbarAxis,
    /// 鼠标按下点在 thumb 内的相对偏移。
    pub cursor_offset: Pixels,
}

/// 搜索结果面板高度拖拽状态。
#[derive(Clone, Copy, Debug)]
pub(crate) struct SearchResultPanelResizeDrag {
    /// 鼠标按下时的窗口 y 坐标。
    pub start_y: Pixels,
    /// 鼠标按下时的面板高度。
    pub start_height: f32,
}

/// 独立日志搜索窗口和结果面板共享的运行期状态。
#[derive(Clone, Debug)]
pub(crate) struct LogSearchState {
    /// 搜索窗口是否已打开。
    pub is_window_open: bool,
    /// 搜索窗口句柄；再次打开时用于置前。
    pub window_handle: Option<WindowHandle<LogSearchWindow>>,
    /// 当前搜索范围。
    pub scope: SearchScope,
    /// 关键字输入框状态。
    pub keyword_input: TextInputState,
    /// 关键字历史下拉菜单是否展开。
    pub keyword_history_open: bool,
    /// 关键字历史下拉菜单当前高亮项索引。
    pub keyword_history_highlight: Option<usize>,
    /// 目录输入框状态。
    pub directory_input: TextInputState,
    /// 目录输入框对应的来源树目录节点。
    pub directory_source_id: Option<SourceId>,
    /// 是否区分大小写；同时影响普通关键字和正则搜索。
    pub case_sensitive: bool,
    /// 是否启用正则表达式搜索。
    pub regex_enabled: bool,
    /// 当前搜索进度。
    pub progress: SearchProgress,
    /// 当前任务状态。
    pub task_state: SearchTaskState,
    /// 当前搜索任务类型，用于结果面板文案和提示。
    pub run_kind: SearchRunKind,
    /// 搜索 generation，用于丢弃过期后台事件。
    pub generation: usize,
    /// 当前搜索取消令牌。
    pub cancel_token: Option<Arc<AtomicBool>>,
    /// 当前日志快速查找 generation，用于丢弃过期计数结果。
    pub quick_match_generation: usize,
    /// 当前日志快速查找缓存键。
    pub quick_match_key: Option<QuickMatchKey>,
    /// 当前日志快速查找取消令牌。
    pub quick_cancel_token: Option<Arc<AtomicBool>>,
    /// 当前日志按行缓存的快速查找结果。
    pub quick_matches: Vec<SearchResult>,
    /// 当前日志关键字出现总次数。
    pub quick_match_count: usize,
    /// 当前激活的快速查找命中序号，按出现次数计数。
    pub active_quick_match_index: Option<usize>,
    /// 当前日志快速查找提示。
    pub quick_match_message: Option<String>,
    /// 是否正在扫描当前日志用于计数或定位。
    pub is_quick_counting: bool,
    /// 全量搜索结果；不做数量截断，UI 通过虚拟列表渲染。
    pub results: Vec<SearchResult>,
    /// 搜索结果已命中的关键字集合，用于增量维护标题栏摘要。
    pub result_keywords: BTreeSet<String>,
    /// 搜索结果标题栏关键字摘要缓存，避免 UI 渲染期遍历全量结果。
    pub result_keyword_summary: Option<String>,
    /// 按文件聚合后的搜索结果分组。
    pub result_groups: Vec<SearchResultGroup>,
    /// 当前展开状态下虚拟列表需要渲染的行。
    pub visible_result_items: Vec<SearchResultListItem>,
    /// 已折叠的搜索结果文件分组。
    pub collapsed_result_groups: BTreeSet<SourceId>,
    /// 搜索结果列表估算内容宽度，用于横向滚动条。
    pub result_list_content_width: f32,
    /// 搜索结果面板当前高度。
    pub result_panel_height: f32,
    /// 搜索结果面板高度拖拽状态。
    pub result_panel_resize_drag: Option<SearchResultPanelResizeDrag>,
    /// 搜索结果面板滚动句柄。
    pub result_scroll: UniformListScrollHandle,
    /// 搜索结果面板自绘滚动条拖拽状态。
    pub result_scrollbar_drag: Option<SearchResultScrollbarDrag>,
    /// 当前激活的结果索引。
    pub active_result_index: Option<usize>,
    /// 点击结果但日志尚未读取完成时的待跳转结果。
    pub pending_activation: Option<SearchResult>,
    /// 最近一次搜索错误或提示。
    pub message: Option<String>,
}

impl Default for LogSearchState {
    /// 创建空闲搜索状态。
    fn default() -> Self {
        Self {
            is_window_open: false,
            window_handle: None,
            scope: SearchScope::CurrentFile,
            keyword_input: TextInputState::default(),
            keyword_history_open: false,
            keyword_history_highlight: None,
            directory_input: TextInputState::default(),
            directory_source_id: None,
            case_sensitive: false,
            regex_enabled: false,
            progress: SearchProgress::default(),
            task_state: SearchTaskState::Idle,
            run_kind: SearchRunKind::Normal,
            generation: 0,
            cancel_token: None,
            quick_match_generation: 0,
            quick_match_key: None,
            quick_cancel_token: None,
            quick_matches: Vec::new(),
            quick_match_count: 0,
            active_quick_match_index: None,
            quick_match_message: None,
            is_quick_counting: false,
            results: Vec::new(),
            result_keywords: BTreeSet::new(),
            result_keyword_summary: None,
            result_groups: Vec::new(),
            visible_result_items: Vec::new(),
            collapsed_result_groups: BTreeSet::new(),
            result_list_content_width: 0.0,
            result_panel_height: SEARCH_RESULT_PANEL_HEIGHT_DEFAULT,
            result_panel_resize_drag: None,
            result_scroll: UniformListScrollHandle::new(),
            result_scrollbar_drag: None,
            active_result_index: None,
            pending_activation: None,
            message: None,
        }
    }
}
