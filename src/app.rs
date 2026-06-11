//! 文件职责：维护 Argus 应用状态、来源加载状态和界面展示数据。
//! 创建日期：2026-06-09
//! 修改日期：2026-06-10
//! 作者：Argus 开发团队
//! 主要功能：提供工作区切换、真实来源树、未读取内容提示和保留的日志样例数据。

mod log_text;
mod placeholder_data;
mod source_search;

use std::borrow::{Borrow, Cow};
use std::collections::HashMap;
#[cfg(test)]
use std::path::PathBuf;

use crate::config::{AppConfig, ConfigManager};
use crate::highlight::HighlightCache;
use crate::loader::{LoadReport, LogSourceLoader, SourceId, SourceLocation, SourceRegistry};
#[cfg(test)]
use crate::loader::{SourceKind, SourceMetadata, SourceTreeNode};
use crate::reader::log_file_reader::{
    LogFileReader, LogOpenState, LogReaderHandle, OpenLogRequest,
};
use crate::reader::read_mode::ReadMode;
use crate::text_selection::TextSelectionGranularity;
#[cfg(test)]
use crate::text_selection::character_count;
use crate::theme::{AppTheme, ThemeManager};
use crate::ui::components::context_menu::{ActiveMenu, ActiveMenuKind, MenuAction, MenuEntry};
use crate::ui::main_window;
use gpui::{
    Context, IntoElement, Keystroke, PathPromptOptions, Pixels, Point, Render, SharedString, Window,
};
use gpui::{ScrollHandle, ScrollStrategy, UniformListScrollHandle};
#[cfg(test)]
use log_text::{log_text_range_for_granularity, merge_log_text_ranges};
#[cfg(test)]
use placeholder_data::{placeholder_logs, placeholder_source_registry};

/// 来源侧栏默认宽度。
pub const SOURCE_PANEL_DEFAULT_WIDTH: f32 = 300.0;
/// 来源侧栏最小宽度，需保证标题栏左侧 4 个操作按钮和固定右侧间距完整展示。
pub const SOURCE_PANEL_MIN_WIDTH: f32 = 244.0;
/// 来源侧栏最大宽度，避免占位界面被侧栏挤压。
pub const SOURCE_PANEL_MAX_WIDTH: f32 = 520.0;
/// 日志内容字号最小值，避免主阅读区文字过小影响可读性。
pub const LOG_CONTENT_FONT_SIZE_MIN: f32 = 12.0;
/// 日志内容字号最大值，避免大字号破坏当前日志行布局。
pub const LOG_CONTENT_FONT_SIZE_MAX: f32 = 20.0;
/// 日志内容默认字号，匹配设计文档要求的高密度 12px 阅读区。
pub const LOG_CONTENT_FONT_SIZE_DEFAULT: f32 = 12.0;
/// 日志正文左侧内边距；命中测试和渲染必须保持一致。
pub const LOG_VIEWER_TEXT_LEFT_PADDING: f32 = 16.0;
/// 日志正文右侧内边距；横向滚动范围和渲染必须保持一致。
pub const LOG_VIEWER_TEXT_RIGHT_PADDING: f32 = 16.0;
/// 日志正文固定行高；分页滚动和 UI 渲染必须保持一致。
pub const LOG_VIEWER_ROW_HEIGHT: f32 = 20.0;
/// 行号栏最小宽度，保证小文件也有稳定的视觉留白。
pub const LOG_VIEWER_LINE_NUMBER_MIN_WIDTH: f32 = 44.0;
/// 行号栏最大宽度，避免超大文件行号挤占正文区域。
pub const LOG_VIEWER_LINE_NUMBER_MAX_WIDTH: f32 = 96.0;
/// 行号栏单个数字的估算宽度，用于无布局测量时的稳定宽度计算。
pub const LOG_VIEWER_LINE_NUMBER_DIGIT_WIDTH: f32 = 7.0;
/// 行号栏左右留白总和，保证行号和正文之间有清晰间隔。
pub const LOG_VIEWER_LINE_NUMBER_PADDING: f32 = 18.0;
/// 日志正文中的制表符展示为空格时的固定宽度。
pub const LOG_VIEWER_TAB_DISPLAY_SPACES: &str = "    ";

/// 根据日志总行数计算行号栏宽度。
///
/// 参数说明：
/// - `line_count`：当前日志文档的总行数。
///
/// 返回值：可直接用于日志渲染和鼠标命中的行号栏像素宽度。
pub fn log_viewer_line_number_width(line_count: usize) -> f32 {
    let display_line_count = line_count.max(1);
    let digits = display_line_count.ilog10() as f32 + 1.0;
    (digits * LOG_VIEWER_LINE_NUMBER_DIGIT_WIDTH + LOG_VIEWER_LINE_NUMBER_PADDING).clamp(
        LOG_VIEWER_LINE_NUMBER_MIN_WIDTH,
        LOG_VIEWER_LINE_NUMBER_MAX_WIDTH,
    )
}

/// 将日志原文转换为阅读区展示文本。
///
/// 参数说明：
/// - `text`：读取器返回的原始单行日志文本。
///
/// 返回值：没有制表符时借用原文本；存在制表符时返回已展开为 4 个空格的新字符串。
pub fn log_viewer_display_text(text: &str) -> Cow<'_, str> {
    if text.contains('\t') {
        Cow::Owned(text.replace('\t', LOG_VIEWER_TAB_DISPLAY_SPACES))
    } else {
        Cow::Borrowed(text)
    }
}

/// 当前界面工作区，仅保留日志分析和设置两个入口。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Workspace {
    /// 日志分析工作区，用于展示来源侧栏和日志内容占位界面。
    LogAnalysis,
    /// 设置工作区，用于展示主题、编码、缓存、快捷键等占位配置。
    Settings,
}

/// 设置页主题选项，只影响本地 UI 状态说明。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ThemeMode {
    /// 跟随系统主题。
    System,
    /// 深色主题。
    Dark,
    /// 浅色主题。
    Light,
}

impl ThemeMode {
    /// 返回设置页展示文案。
    pub fn label(self) -> &'static str {
        match self {
            Self::System => "跟随系统",
            Self::Dark => "深色",
            Self::Light => "浅色",
        }
    }

    /// 返回持久化配置中使用的稳定字符串。
    pub fn config_value(self) -> &'static str {
        match self {
            Self::System => "system",
            Self::Dark => "dark",
            Self::Light => "light",
        }
    }

    /// 从持久化配置值恢复主题模式，未知值回退深色主题。
    pub fn from_config_value(value: &str) -> Self {
        match value.trim().to_ascii_lowercase().as_str() {
            "system" => Self::System,
            "light" => Self::Light,
            _ => Self::Dark,
        }
    }
}

/// 打开来源占位弹窗中的来源类型。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PlaceholderSourceKind {
    /// 本地日志文件。
    File,
    /// 本地目录。
    Directory,
    /// 压缩包来源。
    Archive,
}

impl PlaceholderSourceKind {
    /// 返回来源类型展示文案。
    pub fn label(self) -> &'static str {
        match self {
            Self::File => "日志文件",
            Self::Directory => "目录",
            Self::Archive => "压缩包",
        }
    }
}

/// 占位弹窗类型，当前仅用于打开来源。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PlaceholderDialog {
    /// 打开来源弹窗。
    OpenSource,
}

/// 顶部标签页类型，决定主内容区渲染哪个页面。
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TabKind {
    /// 空标签页，用于启动或关闭最后一个标签后的占位状态。
    Empty,
    /// 日志来源标签页；本轮只保存来源身份和展示路径，不读取正文。
    LogSource {
        /// 对应来源树节点 ID，用于去重和重新选中来源树。
        source_id: SourceId,
        /// 来源展示路径，可能是本地路径或压缩包内虚拟路径。
        path: String,
    },
    /// 设置标签页；全局唯一，可关闭后再次从标题栏打开。
    Settings,
}

/// 顶部标签页状态。
#[derive(Clone, Debug)]
pub struct ArgusTab {
    /// 标签唯一 ID，用于选中、关闭和渲染。
    pub id: usize,
    /// 标签标题。
    pub title: String,
    /// 标签内容类型。
    pub kind: TabKind,
}

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

/// 单行输入框拖拽选择状态，记录起始字符范围和当前拖拽粒度。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InputTextSelectionDrag {
    /// 鼠标按下时形成的基础字符范围。
    pub anchor_range: std::ops::Range<usize>,
    /// 当前拖拽粒度，决定移动时按字符、词或整行扩展。
    pub granularity: TextSelectionGranularity,
}

/// 分页日志滚动状态，使用 f64 避免超大行数下的像素精度丢失。
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct PagedLogScrollState {
    /// 纵向滚动像素。
    pub top_px: f64,
    /// 横向滚动像素。
    pub left_px: f64,
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
    /// 当前文本选区。
    pub selection: Option<LogTextSelection>,
    /// 鼠标拖拽选区状态；鼠标释放后清空。
    pub selection_drag: Option<LogTextSelectionDrag>,
    /// 当前 tab 日志正文是否接收键盘复制等快捷键。
    pub is_focused: bool,
    /// 当前 tab 的语法高亮缓存，避免滚动时重复扫描热点行。
    pub highlight_cache: HighlightCache,
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
            selection: None,
            selection_drag: None,
            is_focused: false,
            highlight_cache: HighlightCache::default(),
        }
    }
}

/// 来源树占位节点，用于模拟文件、目录和压缩包结构。
#[derive(Clone, Debug)]
pub struct SourceNode {
    /// 节点唯一 ID，用于本地选择与展开折叠。
    pub id: usize,
    /// 节点缩进层级，模拟目录树深度。
    pub depth: usize,
    /// 节点名称。
    pub label: String,
    /// 节点类型文案。
    pub kind: String,
    /// 是否为当前选中节点。
    pub selected: bool,
    /// 是否为已展开节点；叶子节点忽略该字段。
    pub expanded: bool,
}

/// 日志行占位数据，用于模拟 INFO/WARN/ERROR 等等级日志。
#[derive(Clone, Debug)]
pub struct LogLine {
    /// 行号。
    pub number: usize,
    /// 日志等级。
    pub level: String,
    /// 日志文本。
    pub message: String,
}

/// 内容区显示状态；本阶段真实来源只显示未读取提示，不读取日志正文。
#[derive(Clone, Debug)]
pub enum ContentState {
    /// 初始样例预览，用于空项目首次启动时展示界面密度。
    PlaceholderPreview,
    /// 已接入真实来源树，但尚未选择日志候选节点。
    SourceNotSelected,
    /// 已选择真实来源节点，正文读取状态由 `log_read_states` 继续描述。
    SourceNotRead {
        /// 被选择的来源 ID。
        source_id: SourceId,
        /// 标签展示名称。
        label: String,
        /// 状态栏和内容区展示路径。
        path: String,
    },
}

/// Argus 根视图状态，驱动界面、真实来源加载和本地 UI 行为。
pub struct ArgusApp {
    /// 应用运行期配置。
    pub config: AppConfig,
    /// 应用配置管理器，负责读取和保存 `~/.argus/settings.toml`。
    pub config_manager: ConfigManager,
    /// 主题管理器，负责从内置和用户 TOML 主题生成运行时主题令牌。
    pub theme_manager: ThemeManager,
    /// 当前活动工作区。
    pub workspace: Workspace,
    /// 用户点击未实现操作后的占位提示。
    pub placeholder_notice: String,
    /// 深色主题令牌。
    pub theme: AppTheme,
    /// 真实来源注册表，维护节点、父子关系和可见索引。
    pub source_registry: SourceRegistry,
    /// 是否已经加载过真实来源；用于首次加载替换启动样例。
    pub has_loaded_real_sources: bool,
    /// 是否正在加载来源。
    pub is_source_loading: bool,
    /// 来源树虚拟列表滚动句柄。
    pub source_tree_scroll: UniformListScrollHandle,
    /// 来源树自定义滚动条拖拽时鼠标在 thumb 内的相对位置。
    pub source_scrollbar_drag_position: Option<Point<Pixels>>,
    /// 来源树搜索工具栏是否处于输入模式。
    pub is_source_tree_search_open: bool,
    /// 来源树搜索框输入内容，仅过滤已加载的日志候选节点。
    pub source_tree_search_query: String,
    /// 来源树搜索框光标位置，使用字符索引而非字节索引以兼容中文。
    pub source_tree_search_cursor: usize,
    /// 来源树搜索框选区锚点；与光标不一致时表示存在选区。
    pub source_tree_search_selection_anchor: Option<usize>,
    /// 来源树搜索框鼠标拖拽选择状态；鼠标释放后清空。
    pub source_tree_search_selection_drag: Option<InputTextSelectionDrag>,
    /// 来源树搜索框是否处于聚焦状态，用于展示光标和选区。
    pub is_source_tree_search_focused: bool,
    /// 来源树搜索框显隐动画序号，每次开关递增以重启动画。
    pub source_tree_search_animation_generation: usize,
    /// 来源树过滤后的可见节点 ID，包含命中日志和必要祖先目录。
    pub filtered_source_ids: Vec<SourceId>,
    /// 来源树子级懒加载 generation，用于丢弃过期后台结果。
    pub source_child_load_generations: HashMap<SourceId, usize>,
    /// 当前内容区状态。
    pub content_state: ContentState,
    /// 日志行占位数据。
    pub logs: Vec<LogLine>,
    /// 日志读取状态，以来源 ID 为键复用已打开的 reader。
    pub log_read_states: HashMap<SourceId, LogOpenState>,
    /// 日志读取 generation，用于丢弃后台任务返回的过期结果。
    pub log_reader_generations: HashMap<SourceId, usize>,
    /// 每个日志 tab 的滚动、选区和焦点状态。
    pub log_tab_view_states: HashMap<usize, LogTabViewState>,
    /// 日志正文滚动条拖拽状态。
    pub log_scrollbar_drag: Option<LogScrollbarDrag>,
    /// 当前打开的标签页。
    pub tabs: Vec<ArgusTab>,
    /// 当前激活的标签 ID。
    pub active_tab_id: usize,
    /// 下一个占位标签 ID。
    pub next_tab_id: usize,
    /// 当前鼠标悬停的标签 ID，用于控制未激活标签的边框和关闭按钮。
    pub hovered_tab_id: Option<usize>,
    /// 当前打开的上下文菜单或下拉菜单。
    pub active_menu: Option<ActiveMenu>,
    /// 标签菜单滚动句柄，用于多标签溢出菜单的固定高度滚动。
    pub tab_menu_scroll: UniformListScrollHandle,
    /// 来源侧栏是否折叠。
    pub is_source_panel_collapsed: bool,
    /// 来源侧栏当前宽度，标题栏左段与内容区侧栏共用。
    pub source_panel_width: f32,
    /// 是否正在拖拽来源侧栏分割线。
    pub is_source_panel_resizing: bool,
    /// 鼠标是否悬停在来源侧栏分割线命中区。
    pub is_source_resizer_hovered: bool,
    /// 开始拖拽时鼠标的窗口横坐标。
    pub source_resize_start_x: f32,
    /// 开始拖拽时来源侧栏宽度。
    pub source_resize_start_width: f32,
    /// 来源侧栏宽度动画序号，每次收起或展开递增以重启动画。
    pub source_panel_animation_generation: usize,
    /// 来源侧栏动画起始宽度。
    pub source_panel_animation_from_width: f32,
    /// 来源侧栏动画目标宽度。
    pub source_panel_animation_to_width: f32,
    /// 搜索面板是否打开。
    pub is_search_panel_open: bool,
    /// 搜索框本地输入内容。
    pub search_query: String,
    /// 是否启用大小写敏感搜索。
    pub is_case_sensitive: bool,
    /// 是否启用正则搜索。
    pub is_regex_enabled: bool,
    /// 是否启用全词匹配。
    pub is_whole_word_enabled: bool,
    /// 当前选中日志行。
    pub selected_log_line: Option<usize>,
    /// 当前弹出的占位弹窗。
    pub active_dialog: Option<PlaceholderDialog>,
    /// 打开来源弹窗中选中的来源类型。
    pub selected_placeholder_source: PlaceholderSourceKind,
    /// 设置页主题模式。
    pub theme_mode: ThemeMode,
    /// 日志内容区字号，仅影响主阅读区域。
    pub log_content_font_size: f32,
    /// 设置页编码选项。
    pub selected_encoding: String,
    /// 是否启用临时缓存。
    pub is_cache_enabled: bool,
    /// 缓存上限，单位 MB。
    pub cache_limit_mb: usize,
}

impl ArgusApp {
    /// 创建界面占位版应用状态。
    pub fn new() -> Self {
        Self::new_with_config_manager(ConfigManager::default())
    }

    /// 使用指定配置管理器创建应用状态，测试可借此隔离真实用户配置目录。
    pub fn new_with_config_manager(config_manager: ConfigManager) -> Self {
        let (mut config, config_warning) = config_manager.load_with_warning();
        let theme_manager = ThemeManager::load_default();
        let theme_mode = ThemeMode::from_config_value(&config.appearance.theme_mode);
        let theme = theme_manager.theme_for_mode(theme_mode.config_value());
        let log_content_font_size = config
            .appearance
            .log_content_font_size
            .clamp(LOG_CONTENT_FONT_SIZE_MIN, LOG_CONTENT_FONT_SIZE_MAX);
        let selected_encoding = config.encoding.selected.clone();
        let is_cache_enabled = config.cache.enabled;
        let cache_limit_mb = config.cache.limit_mb.clamp(128, 2048);
        config.appearance.theme_mode = theme_mode.config_value().to_string();
        config.appearance.log_content_font_size = log_content_font_size;
        config.cache.limit_mb = cache_limit_mb;
        Self {
            config,
            config_manager,
            theme_manager,
            workspace: Workspace::LogAnalysis,
            placeholder_notice: config_warning.unwrap_or_else(|| "请选择日志来源".to_string()),
            theme,
            source_registry: SourceRegistry::new(),
            has_loaded_real_sources: false,
            is_source_loading: false,
            source_tree_scroll: UniformListScrollHandle::new(),
            source_scrollbar_drag_position: None,
            is_source_tree_search_open: false,
            source_tree_search_query: String::new(),
            source_tree_search_cursor: 0,
            source_tree_search_selection_anchor: None,
            source_tree_search_selection_drag: None,
            is_source_tree_search_focused: false,
            source_tree_search_animation_generation: 0,
            filtered_source_ids: Vec::new(),
            source_child_load_generations: HashMap::new(),
            content_state: ContentState::SourceNotSelected,
            logs: Vec::new(),
            log_read_states: HashMap::new(),
            log_reader_generations: HashMap::new(),
            log_tab_view_states: HashMap::new(),
            log_scrollbar_drag: None,
            tabs: vec![ArgusTab {
                id: 1,
                title: "未选择日志".to_string(),
                kind: TabKind::Empty,
            }],
            active_tab_id: 1,
            next_tab_id: 2,
            hovered_tab_id: None,
            active_menu: None,
            tab_menu_scroll: UniformListScrollHandle::new(),
            is_source_panel_collapsed: false,
            source_panel_width: SOURCE_PANEL_DEFAULT_WIDTH,
            is_source_panel_resizing: false,
            is_source_resizer_hovered: false,
            source_resize_start_x: 0.0,
            source_resize_start_width: SOURCE_PANEL_DEFAULT_WIDTH,
            source_panel_animation_generation: 0,
            source_panel_animation_from_width: SOURCE_PANEL_DEFAULT_WIDTH,
            source_panel_animation_to_width: SOURCE_PANEL_DEFAULT_WIDTH,
            is_search_panel_open: false,
            search_query: String::new(),
            is_case_sensitive: false,
            is_regex_enabled: false,
            is_whole_word_enabled: false,
            selected_log_line: None,
            active_dialog: None,
            selected_placeholder_source: PlaceholderSourceKind::File,
            theme_mode,
            log_content_font_size,
            selected_encoding,
            is_cache_enabled,
            cache_limit_mb,
        }
    }

    /// 切换标题栏工作区入口，并更新状态提示。
    pub fn switch_workspace(&mut self, workspace: Workspace) {
        if workspace == Workspace::Settings {
            self.open_or_focus_settings_tab();
            return;
        }

        self.workspace = workspace;
        self.placeholder_notice = match workspace {
            Workspace::LogAnalysis => "已切换到日志分析占位工作区".to_string(),
            Workspace::Settings => "已切换到设置占位工作区".to_string(),
        };
    }

    /// 兼容旧入口：打开或聚焦设置标签页。
    pub fn open_settings_modal(&mut self) {
        self.open_or_focus_settings_tab();
    }

    /// 兼容旧入口：设置页现在作为标签页展示，因此关闭模态框不再改变 UI 树。
    pub fn close_settings_modal(&mut self) {
        self.placeholder_notice = "设置已作为标签页展示".to_string();
    }

    /// 持久化当前配置；失败时只更新提示，不回滚已经生效的 UI 状态。
    fn persist_config_or_report(&mut self) {
        if let Err(error) = self.config_manager.save(&self.config) {
            self.placeholder_notice = format!("{}；设置保存失败：{error}", self.placeholder_notice);
        }
    }

    /// 记录用户触发了尚未实现的操作。
    pub fn mark_placeholder_action(&mut self, action_name: &str) {
        self.placeholder_notice = format!("{action_name} 功能暂未实现，仅保留界面占位");
    }

    /// 返回当前激活标签页标题。
    pub fn active_tab_title(&self) -> &str {
        self.tabs
            .iter()
            .find(|tab| tab.id == self.active_tab_id)
            .map(|tab| tab.title.as_str())
            .unwrap_or("未命名日志")
    }

    /// 打开或关闭搜索面板。
    pub fn toggle_search_panel(&mut self) {
        self.is_search_panel_open = !self.is_search_panel_open;
        self.placeholder_notice = if self.is_search_panel_open {
            "已打开本地搜索面板，占位搜索不会扫描真实文件".to_string()
        } else {
            "已关闭本地搜索面板".to_string()
        };
    }

    /// 打开来源占位弹窗。
    pub fn open_source_dialog(&mut self) {
        self.active_dialog = Some(PlaceholderDialog::OpenSource);
        self.placeholder_notice = "请使用来源工具栏的加载日志按钮打开系统文件选择器".to_string();
    }

    /// 请求系统路径选择器并在后台加载真实日志来源结构。
    pub fn request_load_sources(&mut self, cx: &mut Context<Self>) {
        if self.is_source_loading {
            self.placeholder_notice = "日志来源正在加载中，请稍候".to_string();
            return;
        }

        self.is_source_loading = true;
        self.placeholder_notice = "正在打开系统路径选择器".to_string();

        let app_context: &gpui::App = (&*cx).borrow();
        let can_select_mixed = app_context.can_select_mixed_files_and_dirs();
        let paths_rx = app_context.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: can_select_mixed,
            multiple: true,
            prompt: Some(SharedString::from(if can_select_mixed {
                "选择日志文件、目录或压缩包"
            } else {
                "选择日志文件或压缩包"
            })),
        });
        let loader_config = self.config.loader.clone();

        cx.spawn(async move |view, cx| {
            let paths = match paths_rx.await {
                Ok(Ok(Some(paths))) if !paths.is_empty() => paths,
                Ok(Ok(_)) => {
                    view.update(cx, |app, cx| {
                        app.is_source_loading = false;
                        app.placeholder_notice = "已取消加载日志来源".to_string();
                        cx.notify();
                    })
                    .ok();
                    return;
                }
                Ok(Err(error)) => {
                    view.update(cx, |app, cx| {
                        app.is_source_loading = false;
                        app.placeholder_notice = format!("打开系统路径选择器失败：{error}");
                        cx.notify();
                    })
                    .ok();
                    return;
                }
                Err(error) => {
                    view.update(cx, |app, cx| {
                        app.is_source_loading = false;
                        app.placeholder_notice = format!("路径选择器未返回结果：{error}");
                        cx.notify();
                    })
                    .ok();
                    return;
                }
            };

            let report = cx
                .background_executor()
                .spawn(async move { LogSourceLoader::new(loader_config).load_paths(paths) })
                .await;

            view.update(cx, |app, cx| {
                app.apply_load_report(report);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// 关闭当前占位弹窗。
    pub fn close_dialog(&mut self) {
        self.active_dialog = None;
        self.placeholder_notice = "已关闭占位弹窗".to_string();
    }

    /// 选择打开来源弹窗中的来源类型。
    pub fn select_placeholder_source(&mut self, source_kind: PlaceholderSourceKind) {
        self.selected_placeholder_source = source_kind;
        self.placeholder_notice = format!("已选择{}占位来源", source_kind.label());
    }

    /// 切换来源侧栏折叠状态。
    pub fn toggle_source_panel(&mut self) {
        let was_collapsed = self.is_source_panel_collapsed;
        self.is_source_panel_collapsed = !self.is_source_panel_collapsed;
        self.is_source_panel_resizing = false;
        self.is_source_resizer_hovered = false;
        self.source_panel_animation_generation =
            self.source_panel_animation_generation.wrapping_add(1);
        self.source_panel_animation_from_width = if was_collapsed {
            0.0
        } else {
            self.source_panel_width
        };
        self.source_panel_animation_to_width = if self.is_source_panel_collapsed {
            0.0
        } else {
            self.source_panel_width
        };
        self.placeholder_notice = if self.is_source_panel_collapsed {
            "已折叠来源侧栏".to_string()
        } else {
            "已展开来源侧栏".to_string()
        };
    }

    /// 开始拖拽来源侧栏分割线，记录初始鼠标位置和宽度。
    pub fn begin_source_panel_resize(&mut self, pointer_x: f32) {
        self.is_source_panel_resizing = true;
        self.is_source_resizer_hovered = true;
        self.source_resize_start_x = pointer_x;
        self.source_resize_start_width = self.source_panel_width;
    }

    /// 更新来源侧栏分割线悬停状态。
    pub fn set_source_resizer_hovered(&mut self, is_hovered: bool) -> bool {
        if self.is_source_resizer_hovered == is_hovered {
            return false;
        }

        self.is_source_resizer_hovered = is_hovered;
        true
    }

    /// 根据当前鼠标位置更新来源侧栏宽度。
    pub fn resize_source_panel(&mut self, pointer_x: f32) -> bool {
        if !self.is_source_panel_resizing {
            return false;
        }

        let delta = pointer_x - self.source_resize_start_x;
        let next_width = (self.source_resize_start_width + delta)
            .clamp(SOURCE_PANEL_MIN_WIDTH, SOURCE_PANEL_MAX_WIDTH);
        if (next_width - self.source_panel_width).abs() < 0.5 {
            return false;
        }

        self.source_panel_width = next_width;
        self.source_panel_animation_from_width = next_width;
        self.source_panel_animation_to_width = next_width;
        true
    }

    /// 结束来源侧栏宽度拖拽，并写入占位状态提示。
    pub fn finish_source_panel_resize(&mut self) -> bool {
        if !self.is_source_panel_resizing {
            return false;
        }

        self.is_source_panel_resizing = false;
        self.placeholder_notice = format!("来源侧栏宽度已调整为 {:.0}px", self.source_panel_width);
        true
    }

    /// 打开或聚焦唯一设置标签页。
    pub fn open_or_focus_settings_tab(&mut self) {
        self.workspace = Workspace::LogAnalysis;

        if let Some(tab_id) = self
            .tabs
            .iter()
            .find(|tab| matches!(tab.kind, TabKind::Settings))
            .map(|tab| tab.id)
        {
            self.active_tab_id = tab_id;
            self.placeholder_notice = "已切换到设置标签页".to_string();
            return;
        }

        let tab_id = self.next_tab_id;
        self.next_tab_id += 1;
        self.tabs.push(ArgusTab {
            id: tab_id,
            title: "设置".to_string(),
            kind: TabKind::Settings,
        });
        self.active_tab_id = tab_id;
        self.placeholder_notice = "已打开设置标签页".to_string();
    }

    /// 打开或聚焦指定日志来源标签页；读取正文由 UI 入口随后触发后台任务。
    pub fn open_or_focus_log_tab(&mut self, source_id: SourceId) {
        let Some(selected_node) = self.source_registry.node(source_id).cloned() else {
            self.placeholder_notice = "未找到来源节点".to_string();
            return;
        };
        if !selected_node.kind.is_log_candidate() {
            self.placeholder_notice = format!("{} 不是可打开的日志候选", selected_node.label);
            return;
        }

        let path = selected_node.location.display_path();
        if let Some(tab_id) = self
            .tabs
            .iter()
            .find(|tab| {
                matches!(
                    tab.kind,
                    TabKind::LogSource {
                        source_id: existing_id,
                        ..
                    } if existing_id == source_id
                )
            })
            .map(|tab| tab.id)
        {
            self.active_tab_id = tab_id;
            self.ensure_log_tab_view_state(tab_id);
            self.sync_content_state_from_active_tab();
            self.log_read_states
                .entry(source_id)
                .or_insert(LogOpenState::Idle);
            self.placeholder_notice = format!("已切换到 {path}");
            return;
        }

        let tab_id = if self.tabs.len() == 1 && matches!(self.tabs[0].kind, TabKind::Empty) {
            self.tabs[0].id
        } else {
            let next_id = self.next_tab_id;
            self.next_tab_id += 1;
            self.tabs.push(ArgusTab {
                id: next_id,
                title: String::new(),
                kind: TabKind::Empty,
            });
            next_id
        };

        if let Some(tab) = self.tabs.iter_mut().find(|tab| tab.id == tab_id) {
            tab.title = selected_node.label.clone();
            tab.kind = TabKind::LogSource {
                source_id,
                path: path.clone(),
            };
        }
        self.active_tab_id = tab_id;
        self.ensure_log_tab_view_state(tab_id);
        self.sync_content_state_from_active_tab();
        self.log_read_states
            .entry(source_id)
            .or_insert(LogOpenState::Idle);
        self.placeholder_notice = format!("已打开 {path}，准备读取日志内容");
    }

    /// 为指定日志来源启动后台读取任务；同一来源处于读取中或已就绪时直接复用。
    ///
    /// 参数说明：
    /// - `source_id`：来源树中的日志候选节点 ID。
    /// - `cx`：GPUI 上下文，用于把耗时读取派发到后台线程。
    pub fn request_open_log_content(&mut self, source_id: SourceId, cx: &mut Context<Self>) {
        if matches!(
            self.log_read_states.get(&source_id),
            Some(LogOpenState::Loading { .. } | LogOpenState::Ready(_))
        ) {
            return;
        }

        let Some(source_node) = self.source_registry.node(source_id).cloned() else {
            self.log_read_states.insert(
                source_id,
                LogOpenState::Failed {
                    mode: None,
                    message: "未找到来源节点".to_string(),
                },
            );
            return;
        };
        if !source_node.kind.is_log_candidate() {
            self.log_read_states.insert(
                source_id,
                LogOpenState::Failed {
                    mode: None,
                    message: format!("{} 不是可读取的日志候选", source_node.label),
                },
            );
            return;
        }

        let read_mode = read_mode_for_location(&source_node.location);
        let request = OpenLogRequest {
            source_id,
            location: source_node.location.clone(),
            label: source_node.label.clone(),
            default_encoding: self.selected_encoding.clone(),
        };
        let generation = self.next_log_reader_generation(source_id);
        self.log_read_states.insert(
            source_id,
            LogOpenState::Loading {
                mode: read_mode,
                message: format!("正在读取 {}", source_node.location.display_path()),
            },
        );
        self.placeholder_notice = format!("正在读取 {}", source_node.label);

        cx.spawn(async move |view, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { LogFileReader::open(request) })
                .await;

            view.update(cx, |app, cx| {
                app.apply_log_open_result(source_id, generation, result);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// 应用后台日志读取结果，过期 generation 会被丢弃。
    fn apply_log_open_result(
        &mut self,
        source_id: SourceId,
        generation: usize,
        result: anyhow::Result<LogReaderHandle>,
    ) {
        if self.log_reader_generations.get(&source_id).copied() != Some(generation) {
            return;
        }

        match result {
            Ok(handle) => {
                let line_count = handle.line_count();
                let label = handle.label.clone();
                self.log_read_states
                    .insert(source_id, LogOpenState::Ready(handle));
                self.placeholder_notice = format!("已读取 {label}，共 {line_count} 行");
            }
            Err(error) => {
                let read_mode = self
                    .source_registry
                    .node(source_id)
                    .map(|node| read_mode_for_location(&node.location));
                self.log_read_states.insert(
                    source_id,
                    LogOpenState::Failed {
                        mode: read_mode,
                        message: error.to_string(),
                    },
                );
                self.placeholder_notice = format!("日志读取失败：{error}");
            }
        }
    }

    /// 为指定日志来源生成下一次读取 generation。
    fn next_log_reader_generation(&mut self, source_id: SourceId) -> usize {
        let generation = self.log_reader_generations.entry(source_id).or_insert(0);
        *generation = generation.wrapping_add(1);
        *generation
    }

    /// 释放某个标签页对应的日志读取状态，避免关闭 tab 后继续占用内存。
    fn release_reader_for_tab_kind(&mut self, kind: &TabKind) {
        if let Some(source_id) = source_id_for_tab_kind(kind) {
            self.log_read_states.remove(&source_id);
            self.log_reader_generations.remove(&source_id);
        }
    }

    /// 只保留指定来源的日志读取状态；设置或空 tab 会清空全部读取结果。
    fn retain_reader_for_source(&mut self, kept_source_id: Option<SourceId>) {
        match kept_source_id {
            Some(source_id) => {
                self.log_read_states.retain(|id, _| *id == source_id);
                self.log_reader_generations.retain(|id, _| *id == source_id);
            }
            None => {
                self.log_read_states.clear();
                self.log_reader_generations.clear();
            }
        }
    }

    /// 返回指定来源的日志读取状态。
    pub fn log_read_state(&self, source_id: SourceId) -> Option<&LogOpenState> {
        self.log_read_states.get(&source_id)
    }

    /// 返回当前激活日志标签的读取状态。
    pub fn active_log_read_state(&self) -> Option<&LogOpenState> {
        let TabKind::LogSource { source_id, .. } = self.active_tab_kind() else {
            return None;
        };

        self.log_read_state(source_id)
    }

    /// 返回当前激活日志标签页的读取句柄。
    pub fn active_log_handle(&self) -> Option<&LogReaderHandle> {
        let TabKind::LogSource { source_id, .. } = self.active_tab_kind() else {
            return None;
        };

        match self.log_read_state(source_id)? {
            LogOpenState::Ready(handle) => Some(handle),
            LogOpenState::Idle | LogOpenState::Loading { .. } | LogOpenState::Failed { .. } => None,
        }
    }

    /// 确保指定 tab 拥有日志阅读区视图状态。
    pub fn ensure_log_tab_view_state(&mut self, tab_id: usize) {
        self.log_tab_view_states.entry(tab_id).or_default();
    }

    /// 返回指定 tab 的阅读区视图状态。
    pub fn log_tab_view_state(&self, tab_id: usize) -> Option<&LogTabViewState> {
        self.log_tab_view_states.get(&tab_id)
    }

    /// 返回指定 tab 的可变阅读区视图状态。
    pub fn log_tab_view_state_mut(&mut self, tab_id: usize) -> Option<&mut LogTabViewState> {
        self.log_tab_view_states.get_mut(&tab_id)
    }

    /// 切换到指定标签页。
    pub fn activate_tab(&mut self, tab_id: usize) {
        if self.tabs.iter().any(|tab| tab.id == tab_id) {
            self.active_tab_id = tab_id;
            self.sync_content_state_from_active_tab();
            self.placeholder_notice = format!("已切换到 {}", self.active_tab_title());
        }
    }

    /// 在指定窗口坐标打开标签页右键菜单。
    pub fn open_tab_context_menu(&mut self, tab_id: usize, anchor: Point<Pixels>) {
        if !self.tabs.iter().any(|tab| tab.id == tab_id) {
            self.placeholder_notice = "未找到可操作的标签页".to_string();
            return;
        }

        self.tab_menu_scroll = UniformListScrollHandle::new();
        self.active_menu = Some(ActiveMenu {
            kind: ActiveMenuKind::TabContext { tab_id },
            anchor,
        });
    }

    /// 在指定窗口坐标打开全部标签页溢出菜单。
    pub fn open_tab_overflow_menu(&mut self, anchor: Point<Pixels>) {
        self.tab_menu_scroll = UniformListScrollHandle::new();
        self.active_menu = Some(ActiveMenu {
            kind: ActiveMenuKind::TabOverflow,
            anchor,
        });
    }

    /// 关闭当前活动菜单。
    pub fn close_active_menu(&mut self) {
        self.active_menu = None;
    }

    /// 返回当前活动菜单应展示的菜单项。
    pub fn active_menu_entries(&self) -> Vec<MenuEntry> {
        let Some(active_menu) = &self.active_menu else {
            return Vec::new();
        };

        match active_menu.kind {
            ActiveMenuKind::TabContext { tab_id } => vec![
                MenuEntry::new("关闭当前", MenuAction::CloseTab { tab_id }),
                MenuEntry::new("关闭其他", MenuAction::CloseOtherTabs { tab_id }),
                MenuEntry::new("关闭全部", MenuAction::CloseAllTabs).danger(),
            ],
            ActiveMenuKind::TabOverflow => self
                .tabs
                .iter()
                .map(|tab| {
                    MenuEntry::new(
                        tab.title.clone(),
                        MenuAction::ActivateTab { tab_id: tab.id },
                    )
                    .selected(tab.id == self.active_tab_id)
                })
                .collect(),
        }
    }

    /// 执行通用菜单动作，并在动作完成后关闭菜单。
    pub fn handle_menu_action(&mut self, action: MenuAction) {
        match action {
            MenuAction::ActivateTab { tab_id } => self.activate_tab(tab_id),
            MenuAction::CloseTab { tab_id } => self.close_tab(tab_id),
            MenuAction::CloseOtherTabs { tab_id } => self.close_other_tabs(tab_id),
            MenuAction::CloseAllTabs => self.close_all_tabs(),
        }

        self.close_active_menu();
    }

    /// 更新鼠标悬停标签，仅影响标题栏标签视觉状态。
    pub fn set_hovered_tab(&mut self, tab_id: usize, is_hovered: bool) {
        if is_hovered {
            self.hovered_tab_id = Some(tab_id);
        } else if self.hovered_tab_id == Some(tab_id) {
            self.hovered_tab_id = None;
        }
    }

    /// 关闭指定标签页，至少保留一个空标签。
    pub fn close_tab(&mut self, tab_id: usize) {
        self.close_active_menu();

        if self.tabs.len() == 1 {
            if let Some(tab) = self.tabs.first_mut() {
                tab.title = "未选择日志".to_string();
                tab.kind = TabKind::Empty;
            }
            self.active_tab_id = self.tabs[0].id;
            self.content_state = ContentState::SourceNotSelected;
            self.logs.clear();
            self.log_read_states.clear();
            self.log_reader_generations.clear();
            self.log_tab_view_states.clear();
            self.ensure_log_tab_view_state(self.active_tab_id);
            self.reset_log_text_selection();
            self.log_scrollbar_drag = None;
            self.placeholder_notice = "已清空最后一个标签".to_string();
            return;
        }

        let closed_index = self
            .tabs
            .iter()
            .position(|tab| tab.id == tab_id)
            .unwrap_or(0);
        let closed_tab_kind = self
            .tabs
            .iter()
            .find(|tab| tab.id == tab_id)
            .map(|tab| tab.kind.clone());
        self.tabs.retain(|tab| tab.id != tab_id);
        self.log_tab_view_states.remove(&tab_id);
        if let Some(kind) = closed_tab_kind {
            self.release_reader_for_tab_kind(&kind);
        }
        if self.hovered_tab_id == Some(tab_id) {
            self.hovered_tab_id = None;
        }
        if self
            .log_scrollbar_drag
            .is_some_and(|drag| drag.tab_id == tab_id)
        {
            self.log_scrollbar_drag = None;
        }
        if self.active_tab_id == tab_id {
            let next_index = closed_index.min(self.tabs.len().saturating_sub(1));
            self.active_tab_id = self.tabs[next_index].id;
            self.sync_content_state_from_active_tab();
        }
        self.placeholder_notice = "已关闭标签页".to_string();
    }

    /// 关闭指定标签之外的其他标签，并激活保留标签。
    pub fn close_other_tabs(&mut self, tab_id: usize) {
        let Some(kept_tab) = self.tabs.iter().find(|tab| tab.id == tab_id).cloned() else {
            self.placeholder_notice = "未找到需要保留的标签页".to_string();
            return;
        };

        let removed_count = self.tabs.len().saturating_sub(1);
        let kept_source_id = source_id_for_tab_kind(&kept_tab.kind);
        self.tabs = vec![kept_tab];
        self.log_tab_view_states
            .retain(|existing_tab_id, _| *existing_tab_id == tab_id);
        self.ensure_log_tab_view_state(tab_id);
        self.retain_reader_for_source(kept_source_id);
        self.active_tab_id = tab_id;
        self.hovered_tab_id = None;
        self.log_scrollbar_drag = None;
        self.sync_content_state_from_active_tab();
        self.placeholder_notice = if removed_count == 0 {
            "没有其他标签可关闭".to_string()
        } else {
            format!("已关闭 {removed_count} 个其他标签")
        };
    }

    /// 关闭全部标签，并创建一个新的空标签保持界面可用。
    pub fn close_all_tabs(&mut self) {
        let empty_tab_id = self.next_tab_id;
        self.next_tab_id += 1;
        self.tabs = vec![ArgusTab {
            id: empty_tab_id,
            title: "未选择日志".to_string(),
            kind: TabKind::Empty,
        }];
        self.log_read_states.clear();
        self.log_reader_generations.clear();
        self.log_tab_view_states.clear();
        self.ensure_log_tab_view_state(empty_tab_id);
        self.active_tab_id = empty_tab_id;
        self.hovered_tab_id = None;
        self.reset_log_text_selection();
        self.log_scrollbar_drag = None;
        self.sync_content_state_from_active_tab();
        self.placeholder_notice = "已关闭全部标签".to_string();
    }

    /// 根据节点 ID 选择来源树节点。
    pub fn select_source(&mut self, source_id: SourceId) {
        let Some(selected_node) = self.source_registry.select(source_id) else {
            self.placeholder_notice = "未找到来源节点".to_string();
            return;
        };

        self.selected_log_line = None;
        if selected_node.kind.is_log_candidate() {
            self.logs.clear();
            self.open_or_focus_log_tab(source_id);
        } else {
            self.placeholder_notice = format!("已选择来源节点 {}", selected_node.label);
        }
    }

    /// 展开或折叠目录、压缩包等来源节点。
    pub fn toggle_source_expanded(&mut self, source_id: SourceId, cx: &mut Context<Self>) {
        let Some(node) = self.source_registry.node(source_id).cloned() else {
            self.placeholder_notice = "未找到可展开来源节点".to_string();
            return;
        };

        if !node.kind.can_expand() {
            self.placeholder_notice = format!("{} 没有可展开的子级", node.label);
            return;
        }

        if node.metadata.is_loading {
            if let Some(node) = self.source_registry.node_mut(source_id) {
                node.expanded = !node.expanded;
            }
            self.source_registry.rebuild_visible_index();
            self.rebuild_filtered_source_ids();
            self.placeholder_notice = if node.expanded {
                format!("已折叠 {}，后台加载完成后保持收起", node.label)
            } else {
                format!("已展开 {}，正在等待后台加载完成", node.label)
            };
            return;
        }

        if node.expanded {
            self.source_registry.toggle_expanded(source_id);
            self.rebuild_filtered_source_ids();
            self.placeholder_notice = format!("已折叠 {}", node.label);
            return;
        }

        if node.metadata.children_loaded {
            self.source_registry.toggle_expanded(source_id);
            self.rebuild_filtered_source_ids();
            self.placeholder_notice = format!("已展开 {}", node.label);
            return;
        }

        if let Some(node) = self.source_registry.node_mut(source_id) {
            node.expanded = true;
            node.metadata.is_loading = true;
        }
        self.source_registry.rebuild_visible_index();
        self.rebuild_filtered_source_ids();
        self.placeholder_notice = format!("正在加载 {} 的子级", node.label);

        let loader_config = self.config.loader.clone();
        let load_generation = self.next_source_child_load_generation(source_id);
        cx.spawn(async move |view, cx| {
            let report = cx
                .background_executor()
                .spawn(async move { LogSourceLoader::new(loader_config).load_children(&node) })
                .await;

            view.update(cx, |app, cx| {
                app.apply_child_load_report(source_id, load_generation, report);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// 收起来源目录树中的所有可展开节点。
    pub fn collapse_all_sources(&mut self) {
        let collapsed_count = self.source_registry.collapse_all();
        self.rebuild_filtered_source_ids();

        self.placeholder_notice = if collapsed_count == 0 {
            "目录树已处于全部收起状态".to_string()
        } else {
            format!("已收起 {collapsed_count} 个目录树节点")
        };
    }

    /// 返回当前应渲染的来源节点 ID 列表。
    pub fn visible_source_ids(&self) -> &[SourceId] {
        if self.is_source_tree_filtering() {
            &self.filtered_source_ids
        } else {
            self.source_registry.visible_source_ids()
        }
    }

    /// 清理旧日志工作区状态，确保新来源不会继承旧日志的标签、筛选和内容选择。
    fn reset_log_workspace_after_source_replace(&mut self) {
        self.content_state = ContentState::SourceNotSelected;
        self.logs.clear();
        self.log_read_states.clear();
        self.log_reader_generations.clear();
        self.log_tab_view_states.clear();
        self.reset_log_text_selection();
        self.log_scrollbar_drag = None;
        self.selected_log_line = None;
        self.is_search_panel_open = false;
        self.search_query.clear();
        self.hovered_tab_id = None;
        self.active_menu = None;
        self.log_scrollbar_drag = None;
        self.tab_menu_scroll = UniformListScrollHandle::new();

        self.tabs = vec![ArgusTab {
            id: 1,
            title: "未选择日志".to_string(),
            kind: TabKind::Empty,
        }];
        self.active_tab_id = 1;
        self.next_tab_id = 2;
        self.ensure_log_tab_view_state(1);

        self.is_source_tree_search_open = false;
        self.source_tree_search_query.clear();
        self.source_tree_search_cursor = 0;
        self.source_tree_search_selection_anchor = None;
        self.source_tree_search_selection_drag = None;
        self.is_source_tree_search_focused = false;
        self.filtered_source_ids.clear();
        self.source_tree_scroll
            .scroll_to_item(0, ScrollStrategy::Top);
    }

    /// 应用根来源加载报告。
    ///
    /// 每次成功加载真实来源都会替换旧来源，避免不同批次日志结构混在同一棵树中。
    pub fn apply_load_report(&mut self, report: LoadReport) {
        self.is_source_loading = false;
        let added_count = report.added_count;

        if report.registry.is_empty() {
            self.placeholder_notice = if report.errors.is_empty() {
                "未加载任何日志来源".to_string()
            } else {
                format!("来源加载失败：{}", report.errors.join("；"))
            };
            return;
        }

        self.source_registry = report.registry;
        self.has_loaded_real_sources = true;
        self.source_child_load_generations.clear();
        self.reset_log_workspace_after_source_replace();

        self.placeholder_notice = if report.errors.is_empty() {
            format!("已加载 {added_count} 个来源节点，请选择日志")
        } else {
            format!(
                "已加载 {added_count} 个来源节点，{} 项失败：{}",
                report.errors.len(),
                report.errors.join("；")
            )
        };
    }

    /// 应用懒加载子级报告，并挂回指定父节点。
    pub fn apply_child_load_report(
        &mut self,
        parent_id: SourceId,
        load_generation: usize,
        report: LoadReport,
    ) {
        if self.source_child_load_generations.get(&parent_id).copied() != Some(load_generation) {
            return;
        }
        self.source_child_load_generations.remove(&parent_id);

        if report.registry.is_empty() && !report.errors.is_empty() {
            let message = report.errors.join("；");
            self.source_registry
                .mark_children_load_failed(parent_id, message.clone());
            self.rebuild_filtered_source_ids();
            self.placeholder_notice = format!("子级加载失败：{message}");
            return;
        }

        let should_keep_expanded = self
            .source_registry
            .node(parent_id)
            .map(|node| node.expanded)
            .unwrap_or(false);
        let added_count = self.source_registry.append_children_registry(
            parent_id,
            report.registry,
            should_keep_expanded,
        );

        if let Some(parent) = self.source_registry.node_mut(parent_id)
            && !report.errors.is_empty()
        {
            parent.metadata.message = Some(report.errors.join("；"));
        }
        self.rebuild_filtered_source_ids();

        self.placeholder_notice = if report.errors.is_empty() {
            format!("已加载 {added_count} 个子节点")
        } else if added_count == 0 {
            format!("子级加载失败：{}", report.errors.join("；"))
        } else {
            format!(
                "已加载 {added_count} 个子节点，{} 项失败：{}",
                report.errors.len(),
                report.errors.join("；")
            )
        };
    }

    /// 为指定来源节点生成下一次子级懒加载 generation。
    fn next_source_child_load_generation(&mut self, source_id: SourceId) -> usize {
        let generation = self
            .source_child_load_generations
            .entry(source_id)
            .or_insert(0);
        *generation = generation.wrapping_add(1);
        *generation
    }

    /// 返回当前激活标签页。
    pub fn active_tab(&self) -> Option<&ArgusTab> {
        self.tabs.iter().find(|tab| tab.id == self.active_tab_id)
    }

    /// 返回当前激活标签类型；缺失时按空标签兜底。
    pub fn active_tab_kind(&self) -> TabKind {
        self.active_tab()
            .map(|tab| tab.kind.clone())
            .unwrap_or(TabKind::Empty)
    }

    /// 根据当前激活标签同步过渡期内容状态，供状态栏和旧测试继续使用。
    fn sync_content_state_from_active_tab(&mut self) {
        match self.active_tab_kind() {
            TabKind::Empty => {
                self.content_state = ContentState::SourceNotSelected;
                self.logs.clear();
                self.selected_log_line = None;
                self.reset_log_text_selection_for_tab(self.active_tab_id);
            }
            TabKind::Settings => {
                self.content_state = ContentState::SourceNotSelected;
                self.logs.clear();
                self.selected_log_line = None;
            }
            TabKind::LogSource { source_id, path } => {
                self.ensure_log_tab_view_state(self.active_tab_id);
                let label = self
                    .source_registry
                    .node(source_id)
                    .map(|node| node.label.clone())
                    .unwrap_or_else(|| self.active_tab_title().to_string());
                self.content_state = ContentState::SourceNotRead {
                    source_id,
                    label,
                    path,
                };
                self.logs.clear();
                self.selected_log_line = None;
                self.sync_source_tree_selection_from_active_log(source_id);
            }
        }
    }

    /// 根据当前日志标签同步左侧来源树的选中态，并尽量滚动到对应节点。
    fn sync_source_tree_selection_from_active_log(&mut self, source_id: SourceId) {
        if self.source_registry.select(source_id).is_none() {
            return;
        }

        self.source_registry.expand_ancestors(source_id);
        self.rebuild_filtered_source_ids();
        if self.is_source_tree_filtering() && !self.filtered_source_ids.contains(&source_id) {
            self.close_source_tree_search();
        }
        self.scroll_source_into_view(source_id);
    }

    /// 返回内容区路径文案，优先展示真实选中来源。
    pub fn content_path_label(&self) -> String {
        match self.active_tab_kind() {
            TabKind::LogSource { path, .. } => path,
            TabKind::Settings => "Argus / 设置".to_string(),
            TabKind::Empty if self.has_loaded_real_sources => "请选择日志来源".to_string(),
            TabKind::Empty => "未选择来源".to_string(),
        }
    }

    /// 请求来源树滚动到指定可见节点。
    pub fn scroll_source_into_view(&mut self, source_id: SourceId) {
        if let Some(index) = self
            .visible_source_ids()
            .iter()
            .position(|visible_id| *visible_id == source_id)
        {
            self.source_tree_scroll
                .scroll_to_item(index, ScrollStrategy::Center);
        }
    }

    /// 选择日志行，仅更新本地高亮状态。
    pub fn select_log_line(&mut self, line_number: usize) {
        self.selected_log_line = Some(line_number);
        self.placeholder_notice = format!("已选择第 {line_number} 行日志");
    }

    /// 切换大小写、正则或全词匹配等搜索开关。
    pub fn toggle_search_option(&mut self, option_name: &str) {
        match option_name {
            "case" => {
                self.is_case_sensitive = !self.is_case_sensitive;
                self.placeholder_notice = "已切换大小写匹配选项".to_string();
            }
            "regex" => {
                self.is_regex_enabled = !self.is_regex_enabled;
                self.placeholder_notice = "已切换正则搜索选项".to_string();
            }
            "whole" => {
                self.is_whole_word_enabled = !self.is_whole_word_enabled;
                self.placeholder_notice = "已切换全词匹配选项".to_string();
            }
            _ => self.mark_placeholder_action("搜索选项"),
        }
    }

    /// 处理搜索框按键输入，当前只维护本地字符串。
    pub fn handle_search_key(&mut self, keystroke: &Keystroke) {
        match keystroke.key.as_str() {
            "backspace" => {
                self.search_query.pop();
            }
            "escape" => {
                self.is_search_panel_open = false;
                self.placeholder_notice = "已关闭本地搜索面板".to_string();
                return;
            }
            "enter" => {
                self.placeholder_notice =
                    format!("搜索「{}」为占位操作，未扫描真实日志", self.search_query);
                return;
            }
            _ => {
                if let Some(key_char) = keystroke.key_char.as_ref()
                    && !keystroke.modifiers.platform
                    && !key_char.chars().any(char::is_control)
                {
                    self.search_query.push_str(key_char);
                }
            }
        }

        if self.search_query.is_empty() {
            self.placeholder_notice = "搜索框为空，占位搜索未执行".to_string();
        } else {
            self.placeholder_notice = format!("已输入搜索关键字：{}", self.search_query);
        }
    }

    /// 清空搜索关键字。
    pub fn clear_search_query(&mut self) {
        self.search_query.clear();
        self.placeholder_notice = "已清空搜索关键字".to_string();
    }

    /// 更新主题模式设置。
    pub fn set_theme_mode(&mut self, theme_mode: ThemeMode) {
        self.theme_mode = theme_mode;
        self.theme = self.theme_manager.theme_for_mode(theme_mode.config_value());
        self.config.appearance.theme_mode = theme_mode.config_value().to_string();
        self.placeholder_notice = match theme_mode {
            ThemeMode::System => {
                "主题已切换为跟随系统；当前系统监听未接入，暂按深色主题展示".to_string()
            }
            _ => format!("主题已切换为{}", theme_mode.label()),
        };
        self.persist_config_or_report();
    }

    /// 调整日志内容字号，并限制在外观设置允许的可读范围内。
    pub fn adjust_log_content_font_size(&mut self, delta: f32) {
        self.log_content_font_size = (self.log_content_font_size + delta)
            .clamp(LOG_CONTENT_FONT_SIZE_MIN, LOG_CONTENT_FONT_SIZE_MAX);
        self.config.appearance.log_content_font_size = self.log_content_font_size;
        self.placeholder_notice =
            format!("日志内容字号已调整为 {:.0}px", self.log_content_font_size);
        self.persist_config_or_report();
    }

    /// 切换编码设置。
    pub fn cycle_encoding(&mut self) {
        self.selected_encoding = match self.selected_encoding.as_str() {
            "UTF-8" => "GBK".to_string(),
            "GBK" => "Latin-1".to_string(),
            _ => "UTF-8".to_string(),
        };
        self.config.encoding.selected = self.selected_encoding.clone();
        self.placeholder_notice = format!("编码设置已切换为 {}", self.selected_encoding);
        self.persist_config_or_report();
    }

    /// 切换临时缓存开关。
    pub fn toggle_cache_enabled(&mut self) {
        self.is_cache_enabled = !self.is_cache_enabled;
        self.config.cache.enabled = self.is_cache_enabled;
        self.placeholder_notice = if self.is_cache_enabled {
            "已启用临时缓存占位设置".to_string()
        } else {
            "已关闭临时缓存占位设置".to_string()
        };
        self.persist_config_or_report();
    }

    /// 调整缓存上限，始终限制在占位设置页可展示范围内。
    pub fn adjust_cache_limit(&mut self, delta: isize) {
        self.cache_limit_mb = self
            .cache_limit_mb
            .saturating_add_signed(delta)
            .clamp(128, 2048);
        self.config.cache.limit_mb = self.cache_limit_mb;
        self.placeholder_notice = format!("缓存上限已调整为 {} MB", self.cache_limit_mb);
        self.persist_config_or_report();
    }

    /// 调整嵌套压缩包最大展开深度，设置会影响后续来源加载任务。
    pub fn adjust_max_archive_depth(&mut self, delta: isize) {
        self.config.loader.max_archive_depth = self
            .config
            .loader
            .max_archive_depth
            .saturating_add_signed(delta)
            .clamp(0, 8);
        self.placeholder_notice = format!(
            "嵌套压缩包深度已调整为 {} 层",
            self.config.loader.max_archive_depth
        );
        self.persist_config_or_report();
    }

    /// 切换符号链接跟随策略，设置会影响后续目录来源加载任务。
    pub fn toggle_follow_symlinks(&mut self) {
        self.config.loader.follow_symlinks = !self.config.loader.follow_symlinks;
        self.placeholder_notice = if self.config.loader.follow_symlinks {
            "日志加载已允许跟随符号链接".to_string()
        } else {
            "日志加载已禁止跟随符号链接".to_string()
        };
        self.persist_config_or_report();
    }
}

/// 从标签类型中提取日志来源 ID；非日志标签返回 `None`。
fn source_id_for_tab_kind(kind: &TabKind) -> Option<SourceId> {
    match kind {
        TabKind::LogSource { source_id, .. } => Some(*source_id),
        TabKind::Empty | TabKind::Settings => None,
    }
}

/// 根据来源位置选择读取模式，避免 UI 或状态栏分散判断来源类型。
fn read_mode_for_location(location: &SourceLocation) -> ReadMode {
    match location {
        SourceLocation::LocalPath(_) => ReadMode::MmapPaged,
        SourceLocation::ArchiveEntry { .. } => ReadMode::ArchiveStreaming,
    }
}

impl Default for ArgusApp {
    /// 构造默认应用状态，保持与显式 `new` 入口完全一致。
    fn default() -> Self {
        Self::new()
    }
}

impl Render for ArgusApp {
    /// 渲染 Argus 主界面，所有真实业务能力均保持占位。
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        main_window::render(self, window, cx)
    }
}

/// 判断日志文本位置是否按文档顺序小于等于另一个位置。
fn log_text_position_le(left: LogTextPosition, right: LogTextPosition) -> bool {
    left.line_index < right.line_index
        || (left.line_index == right.line_index && left.column <= right.column)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// 测试配置路径计数器，保证每个应用状态使用独立 settings.toml。
    static NEXT_TEST_CONFIG_ID: AtomicUsize = AtomicUsize::new(0);

    /// 构造隔离真实用户目录的配置管理器。
    fn isolated_config_manager() -> ConfigManager {
        let id = NEXT_TEST_CONFIG_ID.fetch_add(1, Ordering::Relaxed);
        let config_dir =
            std::env::temp_dir().join(format!("argus-app-test-{}-{id}", std::process::id()));
        let _ = std::fs::remove_dir_all(&config_dir);
        ConfigManager::new(config_dir.join("settings.toml"))
    }

    /// 构造使用临时配置路径的应用状态，避免测试污染 `~/.argus`。
    fn test_app() -> ArgusApp {
        ArgusApp::new_with_config_manager(isolated_config_manager())
    }

    /// 日志阅读区展示文本会把制表符固定展开为 4 个空格。
    #[test]
    fn log_display_text_expands_tab_to_four_spaces() {
        assert_eq!(log_viewer_display_text("a\tb").as_ref(), "a    b");
        assert_eq!(
            log_viewer_display_text("\tlevel\tmessage").as_ref(),
            "    level    message"
        );
    }

    /// 构造带样例来源树的应用状态，避免单元测试依赖正式启动空态。
    fn app_with_placeholder_sources() -> ArgusApp {
        let mut app = test_app();
        app.source_registry = placeholder_source_registry();
        app
    }

    /// 按当前可见索引返回节点名称，便于验证来源树过滤结果。
    fn visible_labels(app: &ArgusApp) -> Vec<String> {
        app.visible_source_ids()
            .iter()
            .filter_map(|source_id| app.source_registry.node(*source_id))
            .map(|source| source.label.clone())
            .collect()
    }

    /// 按名称查找测试来源 ID，避免测试依赖硬编码数字 ID。
    fn source_id_by_label(app: &ArgusApp, label: &str) -> SourceId {
        app.source_registry
            .tree_order_source_ids()
            .iter()
            .copied()
            .find(|source_id| {
                app.source_registry
                    .node(*source_id)
                    .map(|source| source.label == label)
                    .unwrap_or(false)
            })
            .expect("测试样例来源应存在")
    }

    /// 验证正式启动时来源树为空，左侧由空态图标承接而非展示样例数据。
    #[test]
    fn new_app_starts_with_empty_source_tree() {
        let app = test_app();

        assert!(app.source_registry.is_empty());
        assert!(app.visible_source_ids().is_empty());
    }

    /// 验证正式启动时内容区只展示提示信息，不渲染样例日志行。
    #[test]
    fn new_app_starts_without_placeholder_log_rows() {
        let app = test_app();

        assert!(app.logs.is_empty());
        assert!(app.selected_log_line.is_none());
        assert_eq!(app.active_tab_title(), "未选择日志");
        assert!(matches!(app.active_tab_kind(), TabKind::Empty));
        assert!(matches!(app.content_state, ContentState::SourceNotSelected));
    }

    /// 验证打开设置入口时进入唯一设置标签页。
    #[test]
    fn open_settings_tab_creates_single_settings_tab() {
        let mut app = test_app();

        app.open_or_focus_settings_tab();

        assert_eq!(app.tabs.len(), 2);
        assert!(matches!(app.active_tab_kind(), TabKind::Settings));
    }

    /// 验证重复点击设置入口会复用同一个设置标签页。
    #[test]
    fn settings_tab_is_reused() {
        let mut app = app_with_placeholder_sources();
        let app_log_id = source_id_by_label(&app, "app.log");

        app.open_or_focus_settings_tab();
        let settings_tab_id = app.active_tab_id;
        app.select_source(app_log_id);
        app.open_or_focus_settings_tab();

        assert_eq!(app.active_tab_id, settings_tab_id);
        assert_eq!(
            app.tabs
                .iter()
                .filter(|tab| matches!(tab.kind, TabKind::Settings))
                .count(),
            1
        );
    }

    /// 验证同一日志来源重复点击时复用已有标签页。
    #[test]
    fn selecting_same_log_reuses_existing_tab() {
        let mut app = app_with_placeholder_sources();
        let app_log_id = source_id_by_label(&app, "app.log");

        app.select_source(app_log_id);
        let tab_id = app.active_tab_id;
        app.select_source(app_log_id);

        assert_eq!(app.active_tab_id, tab_id);
        assert_eq!(app.tabs.len(), 1);
        assert!(matches!(
            app.active_tab_kind(),
            TabKind::LogSource {
                source_id,
                ..
            } if source_id == app_log_id
        ));
    }

    /// 验证不同日志来源会打开独立标签页。
    #[test]
    fn selecting_different_logs_opens_different_tabs() {
        let mut app = app_with_placeholder_sources();
        let app_log_id = source_id_by_label(&app, "app.log");
        let error_log_id = source_id_by_label(&app, "error.log");

        app.select_source(app_log_id);
        app.select_source(error_log_id);

        assert_eq!(app.tabs.len(), 2);
        assert_eq!(app.active_tab_title(), "error.log");
        assert!(app.tabs.iter().any(|tab| tab.title == "app.log"));
        assert!(app.tabs.iter().any(|tab| tab.title == "error.log"));
    }

    /// 验证激活日志标签页时，左侧来源树会选中同一个日志并展开父级路径。
    #[test]
    fn activating_log_tab_selects_matching_source_tree_node() {
        let mut app = app_with_placeholder_sources();
        let logs_id = source_id_by_label(&app, "logs");
        let app_log_id = source_id_by_label(&app, "app.log");
        let error_log_id = source_id_by_label(&app, "error.log");

        app.select_source(app_log_id);
        let app_tab_id = app.active_tab_id;
        app.select_source(error_log_id);
        if app
            .source_registry
            .node(logs_id)
            .map(|source| source.expanded)
            .unwrap_or(false)
        {
            app.source_registry.toggle_expanded(logs_id);
        }

        assert!(!app.visible_source_ids().contains(&app_log_id));

        app.activate_tab(app_tab_id);

        assert!(
            app.source_registry
                .node(app_log_id)
                .map(|source| source.selected)
                .unwrap_or(false)
        );
        assert!(
            !app.source_registry
                .node(error_log_id)
                .map(|source| source.selected)
                .unwrap_or(true)
        );
        assert!(
            app.source_registry
                .node(logs_id)
                .map(|source| source.expanded)
                .unwrap_or(false)
        );
        assert!(app.visible_source_ids().contains(&app_log_id));
    }

    /// 验证关闭当前标签后会激活相邻标签，关闭最后一个标签会回到空标签。
    #[test]
    fn close_tab_activates_neighbor_and_keeps_one_empty_tab() {
        let mut app = app_with_placeholder_sources();
        let app_log_id = source_id_by_label(&app, "app.log");
        app.select_source(app_log_id);
        app.open_or_focus_settings_tab();
        let settings_tab_id = app.active_tab_id;

        app.close_tab(settings_tab_id);
        assert_eq!(app.tabs.len(), 1);
        assert_eq!(app.active_tab_title(), "app.log");

        let last_tab_id = app.active_tab_id;
        app.close_tab(last_tab_id);
        assert_eq!(app.tabs.len(), 1);
        assert!(matches!(app.active_tab_kind(), TabKind::Empty));
        assert_eq!(app.active_tab_title(), "未选择日志");
    }

    /// 验证标签右键菜单会记录目标标签和窗口锚点。
    #[test]
    fn tab_context_menu_records_target_tab_and_anchor() {
        let mut app = test_app();
        let target_tab_id = app.active_tab_id;
        let anchor = gpui::point(gpui::px(120.0), gpui::px(40.0));

        app.open_tab_context_menu(target_tab_id, anchor);

        let Some(active_menu) = app.active_menu.as_ref() else {
            panic!("右键标签后应打开活动菜单");
        };
        assert!(matches!(
            active_menu.kind,
            ActiveMenuKind::TabContext { tab_id } if tab_id == target_tab_id
        ));
        assert_eq!(active_menu.anchor, anchor);
    }

    /// 验证关闭其他标签只保留目标标签并激活它。
    #[test]
    fn close_other_tabs_keeps_target_tab_active() {
        let mut app = app_with_placeholder_sources();
        let app_log_id = source_id_by_label(&app, "app.log");
        let error_log_id = source_id_by_label(&app, "error.log");

        app.select_source(app_log_id);
        let app_tab_id = app.active_tab_id;
        app.select_source(error_log_id);
        app.open_or_focus_settings_tab();
        app.close_other_tabs(app_tab_id);

        assert_eq!(app.tabs.len(), 1);
        assert_eq!(app.active_tab_id, app_tab_id);
        assert_eq!(app.active_tab_title(), "app.log");
    }

    /// 验证关闭全部标签后仍保留一个空标签承接界面。
    #[test]
    fn close_all_tabs_keeps_single_empty_tab() {
        let mut app = app_with_placeholder_sources();
        let app_log_id = source_id_by_label(&app, "app.log");

        app.select_source(app_log_id);
        app.open_or_focus_settings_tab();
        app.close_all_tabs();

        assert_eq!(app.tabs.len(), 1);
        assert_eq!(app.active_tab_title(), "未选择日志");
        assert!(matches!(app.active_tab_kind(), TabKind::Empty));
    }

    /// 验证标签溢出菜单项点击后会激活目标标签并关闭菜单。
    #[test]
    fn overflow_menu_action_activates_tab_and_closes_menu() {
        let mut app = app_with_placeholder_sources();
        let app_log_id = source_id_by_label(&app, "app.log");
        let error_log_id = source_id_by_label(&app, "error.log");

        app.select_source(app_log_id);
        let app_tab_id = app.active_tab_id;
        app.select_source(error_log_id);
        app.open_tab_overflow_menu(gpui::point(gpui::px(200.0), gpui::px(40.0)));
        app.handle_menu_action(MenuAction::ActivateTab { tab_id: app_tab_id });

        assert_eq!(app.active_tab_id, app_tab_id);
        assert_eq!(app.active_tab_title(), "app.log");
        assert!(app.active_menu.is_none());
    }

    /// 验证日志内容字号会被限制在外观设置允许范围内。
    #[test]
    fn adjust_log_content_font_size_clamps_to_range() {
        let mut app = test_app();

        app.adjust_log_content_font_size(100.0);
        assert_eq!(app.log_content_font_size, LOG_CONTENT_FONT_SIZE_MAX);

        app.adjust_log_content_font_size(-100.0);
        assert_eq!(app.log_content_font_size, LOG_CONTENT_FONT_SIZE_MIN);
    }

    /// 验证外观主题切换会立即替换运行时主题令牌。
    #[test]
    fn set_theme_mode_updates_runtime_theme_tokens() {
        let mut app = test_app();

        app.set_theme_mode(ThemeMode::Light);
        assert_eq!(app.theme_mode, ThemeMode::Light);
        assert_eq!(
            app.theme.content,
            app.theme_manager.theme_for_mode("light").content
        );

        app.set_theme_mode(ThemeMode::Dark);
        assert_eq!(app.theme_mode, ThemeMode::Dark);
        assert_eq!(
            app.theme.content,
            app.theme_manager.theme_for_mode("dark").content
        );
    }

    /// 验证外观和加载设置修改后会立即写入配置文件。
    #[test]
    fn settings_changes_are_persisted_to_config_file() {
        let config_manager = isolated_config_manager();
        let settings_path = config_manager.settings_path().to_path_buf();
        let mut app = ArgusApp::new_with_config_manager(config_manager);

        app.set_theme_mode(ThemeMode::Light);
        app.adjust_log_content_font_size(2.0);
        app.adjust_max_archive_depth(1);
        app.toggle_follow_symlinks();

        let saved =
            ConfigManager::load_from_path(&settings_path).expect("设置变更后应写入配置文件");

        assert_eq!(saved.appearance.theme_mode, "light");
        assert_eq!(saved.appearance.log_content_font_size, 14.0);
        assert_eq!(saved.loader.max_archive_depth, 3);
        assert!(saved.loader.follow_symlinks);
    }

    /// 验证新日志来源加载成功后会替换旧来源，并清理旧日志相关界面状态。
    #[test]
    fn applying_new_load_report_replaces_old_log_workspace() {
        let mut app = app_with_placeholder_sources();
        app.has_loaded_real_sources = true;
        app.logs = placeholder_logs();
        app.tabs.push(ArgusTab {
            id: 2,
            title: "old.log".to_string(),
            kind: TabKind::LogSource {
                source_id: SourceId(999),
                path: "old.log".to_string(),
            },
        });
        app.active_tab_id = 2;
        app.next_tab_id = 3;
        app.open_source_tree_search();
        app.update_source_tree_search_query("old".to_string());

        let mut registry = SourceRegistry::new();
        let new_id = registry.allocate_id();
        registry.insert_node(SourceTreeNode {
            id: new_id,
            parent_id: None,
            depth: 0,
            label: "new.log".to_string(),
            kind: SourceKind::LogFile,
            location: SourceLocation::LocalPath(PathBuf::from("new.log")),
            metadata: SourceMetadata {
                size: Some(128),
                children_loaded: true,
                is_loading: false,
                message: None,
            },
            selected: false,
            expanded: false,
        });
        registry.rebuild_all_indices();

        app.apply_load_report(LoadReport {
            registry,
            added_count: 1,
            skipped_count: 0,
            errors: Vec::new(),
        });

        assert_eq!(visible_labels(&app), vec!["new.log"]);
        assert!(app.logs.is_empty());
        assert_eq!(app.tabs.len(), 1);
        assert_eq!(app.active_tab_title(), "未选择日志");
        assert!(matches!(app.active_tab_kind(), TabKind::Empty));
        assert_eq!(app.next_tab_id, 2);
        assert!(matches!(app.content_state, ContentState::SourceNotSelected));
        assert!(!app.is_source_tree_search_open);
        assert!(app.source_tree_search_query.is_empty());
        assert!(app.filtered_source_ids.is_empty());
    }

    /// 验证来源树搜索只匹配日志候选节点，并保留其祖先目录上下文。
    #[test]
    fn source_tree_filter_matches_logs_and_keeps_ancestors() {
        let mut app = app_with_placeholder_sources();

        app.open_source_tree_search();
        app.update_source_tree_search_query("APP".to_string());

        assert_eq!(visible_labels(&app), vec!["logs", "app.log"]);
    }

    /// 验证来源树过滤不会改变真实目录树的展开状态。
    #[test]
    fn source_tree_filter_does_not_mutate_expansion_state() {
        let mut app = app_with_placeholder_sources();
        let logs_id = source_id_by_label(&app, "logs");

        app.source_registry.toggle_expanded(logs_id);
        app.open_source_tree_search();
        app.update_source_tree_search_query("app".to_string());

        assert!(!app.source_registry.node(logs_id).unwrap().expanded);
        assert_eq!(visible_labels(&app), vec!["logs", "app.log"]);
    }

    /// 验证切换到被当前过滤条件隐藏的日志标签时，会退出过滤并显示对应来源节点。
    #[test]
    fn activating_hidden_log_tab_clears_source_tree_filter() {
        let mut app = app_with_placeholder_sources();
        let app_log_id = source_id_by_label(&app, "app.log");
        let error_log_id = source_id_by_label(&app, "error.log");

        app.select_source(app_log_id);
        let app_tab_id = app.active_tab_id;
        app.select_source(error_log_id);
        app.open_source_tree_search();
        app.update_source_tree_search_query("error".to_string());

        assert_eq!(visible_labels(&app), vec!["logs", "error.log"]);

        app.activate_tab(app_tab_id);

        assert!(!app.is_source_tree_search_open);
        assert!(app.source_tree_search_query.is_empty());
        assert!(app.visible_source_ids().contains(&app_log_id));
        assert!(
            app.source_registry
                .node(app_log_id)
                .map(|source| source.selected)
                .unwrap_or(false)
        );
    }

    /// 验证输入框编辑状态按字符索引移动，避免中文被按字节截断。
    #[test]
    fn source_tree_search_editing_uses_character_indices() {
        let mut app = test_app();

        app.insert_source_tree_search_text("日a志");
        app.move_source_tree_search_left(false);
        app.move_source_tree_search_left(true);

        assert_eq!(app.source_tree_search_cursor, 1);
        assert_eq!(app.source_tree_search_selection_range(), Some(1..2));

        app.insert_source_tree_search_text("b");
        assert_eq!(app.source_tree_search_query, "日b志");
        assert_eq!(app.source_tree_search_cursor, 2);
    }

    /// 验证全选和删除操作会同步维护光标和选区。
    #[test]
    fn source_tree_search_selection_delete_updates_cursor() {
        let mut app = test_app();

        app.update_source_tree_search_query("archive.log".to_string());
        app.select_all_source_tree_search();
        app.delete_source_tree_search_backward();

        assert!(app.source_tree_search_query.is_empty());
        assert_eq!(app.source_tree_search_cursor, 0);
        assert!(app.source_tree_search_selection_range().is_none());
    }

    /// 验证输入框鼠标拖拽按字符索引生成选区，中文不会被截断。
    #[test]
    fn source_tree_search_pointer_drag_selects_character_range() {
        let mut app = test_app();

        app.update_source_tree_search_query("日a志".to_string());
        app.begin_source_tree_search_pointer_selection(0, TextSelectionGranularity::Character);
        app.update_source_tree_search_pointer_selection(2);
        app.finish_source_tree_search_pointer_selection();

        assert_eq!(app.source_tree_search_selection_range(), Some(0..2));
        assert_eq!(
            app.selected_source_tree_search_text(),
            Some("日a".to_string())
        );
    }

    /// 验证输入框双击按词选择常见日志令牌，点号会作为分隔符。
    #[test]
    fn source_tree_search_double_click_selects_word() {
        let mut app = test_app();

        app.update_source_tree_search_query("中文 thread_001.zip java.lang.Class".to_string());
        app.begin_source_tree_search_pointer_selection(4, TextSelectionGranularity::Word);
        app.finish_source_tree_search_pointer_selection();

        assert_eq!(
            app.selected_source_tree_search_text(),
            Some("thread_001".to_string())
        );
    }

    /// 验证输入框三击会选中整个单行输入值。
    #[test]
    fn source_tree_search_triple_click_selects_whole_line() {
        let mut app = test_app();

        app.update_source_tree_search_query("archive.log".to_string());
        app.begin_source_tree_search_pointer_selection(3, TextSelectionGranularity::Line);
        app.finish_source_tree_search_pointer_selection();

        assert_eq!(
            app.selected_source_tree_search_text(),
            Some("archive.log".to_string())
        );
    }

    /// 验证日志双击选词支持中文、下划线令牌和点号分隔的 Java 类名片段。
    #[test]
    fn log_word_selection_supports_common_log_tokens() {
        let mut app = test_app();
        let tab_id = app.active_tab_id;
        let line = "中文 thread_001.zip java.lang.Class";

        app.select_log_word_at(tab_id, 0, line, 0);
        let selection = app
            .log_tab_view_state(tab_id)
            .and_then(|state| state.selection.as_ref())
            .unwrap();
        assert_eq!(selection.normalized().0.column, 0);
        assert_eq!(selection.normalized().1.column, 2);

        app.select_log_word_at(tab_id, 0, line, 4);
        let selection = app
            .log_tab_view_state(tab_id)
            .and_then(|state| state.selection.as_ref())
            .unwrap();
        assert_eq!(selection.normalized().0.column, 3);
        assert_eq!(selection.normalized().1.column, 13);

        app.select_log_word_at(tab_id, 0, line, 20);
        let selection = app
            .log_tab_view_state(tab_id)
            .and_then(|state| state.selection.as_ref())
            .unwrap();
        assert_eq!(selection.normalized().0.column, 18);
        assert_eq!(selection.normalized().1.column, 22);
    }

    /// 验证日志三击会选中整行展示文本，包含制表符展开后的列宽。
    #[test]
    fn log_triple_click_selects_whole_display_line() {
        let mut app = test_app();
        let tab_id = app.active_tab_id;

        app.select_log_text_line(tab_id, 7, "abc\tdef");
        let selection = app
            .log_tab_view_state(tab_id)
            .and_then(|state| state.selection.as_ref())
            .unwrap();
        let (start, end) = selection.normalized();

        assert_eq!(
            start,
            LogTextPosition {
                line_index: 7,
                column: 0
            }
        );
        assert_eq!(end.line_index, 7);
        assert_eq!(end.column, character_count("abc    def"));
    }

    /// 验证日志词级和行级拖拽会完整合并起始范围与当前范围。
    #[test]
    fn log_range_merge_expands_word_and_line_selection() {
        let word_anchor =
            log_text_range_for_granularity(0, "one two three", 1, TextSelectionGranularity::Word);
        let word_focus =
            log_text_range_for_granularity(0, "one two three", 5, TextSelectionGranularity::Word);
        let (word_start, word_end) = merge_log_text_ranges(&word_anchor, &word_focus).normalized();
        assert_eq!(
            word_start,
            LogTextPosition {
                line_index: 0,
                column: 0
            }
        );
        assert_eq!(
            word_end,
            LogTextPosition {
                line_index: 0,
                column: 7
            }
        );

        let line_anchor =
            log_text_range_for_granularity(1, "first", 2, TextSelectionGranularity::Line);
        let line_focus =
            log_text_range_for_granularity(3, "third line", 4, TextSelectionGranularity::Line);
        let (line_start, line_end) = merge_log_text_ranges(&line_anchor, &line_focus).normalized();
        assert_eq!(
            line_start,
            LogTextPosition {
                line_index: 1,
                column: 0
            }
        );
        assert_eq!(
            line_end,
            LogTextPosition {
                line_index: 3,
                column: 10
            }
        );
    }

    /// 构造只有一个未加载目录的应用状态，用于验证懒加载状态机。
    fn app_with_loading_directory() -> (ArgusApp, SourceId) {
        let mut app = test_app();
        let mut registry = SourceRegistry::new();
        let id = registry.allocate_id();
        registry.insert_node(SourceTreeNode {
            id,
            parent_id: None,
            depth: 0,
            label: "logs".to_string(),
            kind: SourceKind::Directory,
            location: SourceLocation::LocalPath(PathBuf::from("logs")),
            metadata: SourceMetadata {
                size: None,
                children_loaded: false,
                is_loading: true,
                message: None,
            },
            selected: false,
            expanded: true,
        });
        registry.rebuild_all_indices();
        app.source_registry = registry;
        app.source_child_load_generations.insert(id, 1);
        (app, id)
    }

    /// 验证子级加载失败后不会标记为已加载，用户后续点击仍可重试。
    #[test]
    fn child_load_failure_keeps_node_retryable() {
        let (mut app, source_id) = app_with_loading_directory();
        let report = LoadReport {
            registry: SourceRegistry::new(),
            added_count: 0,
            skipped_count: 1,
            errors: vec!["权限不足".to_string()],
        };

        app.apply_child_load_report(source_id, 1, report);

        let node = app.source_registry.node(source_id).unwrap();
        assert!(!node.metadata.children_loaded);
        assert!(!node.metadata.is_loading);
        assert!(!node.expanded);
        assert_eq!(node.metadata.message.as_deref(), Some("权限不足"));
        assert!(!app.source_child_load_generations.contains_key(&source_id));
    }

    /// 验证过期的后台懒加载结果不会覆盖当前节点状态。
    #[test]
    fn stale_child_load_report_is_ignored() {
        let (mut app, source_id) = app_with_loading_directory();
        app.source_child_load_generations.insert(source_id, 2);
        let report = LoadReport {
            registry: SourceRegistry::new(),
            added_count: 0,
            skipped_count: 1,
            errors: vec!["旧结果".to_string()],
        };

        app.apply_child_load_report(source_id, 1, report);

        let node = app.source_registry.node(source_id).unwrap();
        assert!(node.metadata.is_loading);
        assert!(node.expanded);
        assert_eq!(
            app.source_child_load_generations.get(&source_id).copied(),
            Some(2)
        );
    }
}
