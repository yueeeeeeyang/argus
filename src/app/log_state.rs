// 文件职责: 提取日志阅读区 UI 状态类型定义
// 创建日期: 2026-07-08
// 作者: Argus 开发团队
// 主要功能: 定义日志文本选区、分页滚动、高亮预取和行标记等日志查看状态类型

use std::collections::BTreeSet;
use std::ops::Range;

use gpui::{Pixels, ScrollHandle, UniformListScrollHandle};

use crate::highlight::{HighlightCache, HighlightLanguage};
use crate::infra::text_selection::TextSelectionGranularity;
use crate::loader::SourceId;

/// 日志正文中的文本位置，使用行号和字符列表达。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LogTextPosition {
    /// 0 基日志行号。
    pub line_index: usize,
    /// 行内字符列，按 Unicode 标量值计数，避免中文被字节下标截断。
    pub column: usize,
}

/// 日志正文选区，支持跨行复制。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LogTextSelection {
    /// 鼠标按下时的选区锚点。
    pub anchor: LogTextPosition,
    /// 当前拖拽或键盘扩展到的焦点位置。
    pub focus: LogTextPosition,
}

impl LogTextSelection {
    /// 返回选区是否为空。
    pub fn is_empty(&self) -> bool {
        self.anchor == self.focus
    }

    /// 返回按文档顺序排列后的起止位置。
    pub fn normalized(&self) -> (LogTextPosition, LogTextPosition) {
        if log_text_position_le(self.anchor, self.focus) {
            (self.anchor, self.focus)
        } else {
            (self.focus, self.anchor)
        }
    }
}

/// 日志正文拖拽选择状态，记录起始选区和当前拖拽粒度。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LogTextSelectionDrag {
    /// 鼠标按下时形成的基础选区；双击为词，三击为整行。
    pub anchor_range: LogTextSelection,
    /// 当前拖拽粒度，决定后续移动时如何扩展选区。
    pub granularity: TextSelectionGranularity,
}

/// 日志正文中当前被搜索结果激活的命中位置。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ActiveSearchMatch {
    /// 命中所在来源节点。
    pub source_id: SourceId,
    /// 0 基行号。
    pub line_number: usize,
    /// 命中关键字的字节范围。
    pub match_ranges: Vec<Range<usize>>,
    /// 当前通过上/下一个定位到的单个命中范围；为空时高亮整行所有命中。
    pub active_range: Option<Range<usize>>,
}

/// 分页日志滚动状态，使用 f64 避免超大行数下的像素精度丢失。
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct PagedLogScrollState {
    /// 纵向滚动像素。
    pub top_px: f64,
    /// 横向滚动像素。
    pub left_px: f64,
}

/// 分页日志后台预取请求标记，避免 UI 重绘期间重复启动同一范围读取。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PagedLogPrefetchRequest {
    /// 预取所属的来源节点 ID。
    pub source_id: SourceId,
    /// 起始 0 基行号。
    pub start_line: usize,
    /// 预取行数。
    pub max_lines: usize,
}

/// 分页日志可见行高亮后台预取请求标记。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LogHighlightPrefetchRequest {
    /// 预取所属的来源节点 ID。
    pub source_id: SourceId,
    /// 高亮语言。
    pub language: HighlightLanguage,
    /// 起始 0 基行号。
    pub start_line: usize,
    /// 预取行数。
    pub max_lines: usize,
}

/// 单个日志 tab 的阅读区 UI 状态。
#[derive(Clone, Debug)]
pub struct LogTabViewState {
    /// 小日志 uniform_list 滚动句柄。
    pub scroll_handle: UniformListScrollHandle,
    /// 大日志分页视口测量句柄。
    pub paged_viewport_handle: ScrollHandle,
    /// 大日志分页滚动状态。
    pub paged_scroll: PagedLogScrollState,
    /// 当前正在后台预取的分页行范围；完成后清空。
    pub pending_paged_prefetch: Option<PagedLogPrefetchRequest>,
    /// 当前文本选区。
    pub selection: Option<LogTextSelection>,
    /// 鼠标拖拽选区状态；鼠标释放后清空。
    pub selection_drag: Option<LogTextSelectionDrag>,
    /// 当前 tab 日志正文是否接收键盘复制等快捷键。
    pub is_focused: bool,
    /// 当前 tab 的语法高亮缓存，避免滚动时重复扫描热点行。
    pub highlight_cache: HighlightCache,
    /// 当前正在后台预取的分页日志高亮范围；完成后清空。
    pub pending_highlight_prefetch: Option<LogHighlightPrefetchRequest>,
    /// 当前搜索结果激活后需要在正文中强调的命中行和片段。
    pub active_search_match: Option<ActiveSearchMatch>,
    /// 当前日志页签的行号打点集合，使用 0 基行号并按行号有序保存，便于 F2 循环跳转。
    pub line_markers: BTreeSet<usize>,
    /// 上一次 F2 跳转到的打点行，避免系统按键重复在同一渲染帧内反复命中同一打点。
    pub last_line_marker_jump: Option<usize>,
}

/// 日志正文滚动条方向。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LogScrollbarAxis {
    /// 纵向滚动条。
    Vertical,
    /// 横向滚动条。
    Horizontal,
}

/// 日志正文滚动条拖拽状态。
#[derive(Clone, Copy, Debug)]
pub struct LogScrollbarDrag {
    /// 被拖动的标签页 ID。
    pub tab_id: usize,
    /// 当前拖动方向。
    pub axis: LogScrollbarAxis,
    /// 鼠标按下点在 thumb 内的相对偏移。
    pub cursor_offset: Pixels,
}

impl Default for LogTabViewState {
    /// 创建默认阅读区状态。
    fn default() -> Self {
        Self {
            scroll_handle: UniformListScrollHandle::new(),
            paged_viewport_handle: ScrollHandle::new(),
            paged_scroll: PagedLogScrollState::default(),
            pending_paged_prefetch: None,
            selection: None,
            selection_drag: None,
            is_focused: false,
            highlight_cache: HighlightCache::default(),
            pending_highlight_prefetch: None,
            active_search_match: None,
            line_markers: BTreeSet::new(),
            last_line_marker_jump: None,
        }
    }
}

/// 判断日志文本位置是否按文档顺序小于等于另一个位置。
pub(super) fn log_text_position_le(left: LogTextPosition, right: LogTextPosition) -> bool {
    left.line_index < right.line_index
        || (left.line_index == right.line_index && left.column <= right.column)
}
