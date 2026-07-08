//! 文件职责：维护 Argus 应用状态、来源加载状态和界面展示数据。
//! 创建日期：2026-06-09
//! 修改日期：2026-06-25
//! 作者：Argus 开发团队
//! 主要功能：提供工作区切换、真实来源树、Jstack 分析、升级状态、未读取内容提示和保留的日志样例数据。

mod log_search;
mod log_text;
mod placeholder_data;
mod remote;
mod settings_actions;
mod source_picker_actions;
mod source_search;
mod text_input;

mod constants;
mod types;
mod log_state;
mod search_state;
mod jstack_state;
mod runtime_state;
mod remote_state;
mod jstack_actions;
mod menu_actions;
mod runtime_actions;
mod source_tree;
mod upgrade_actions;

pub use constants::*;
pub use jstack_state::*;
pub use log_state::*;
pub use remote_state::*;
pub use runtime_state::*;
pub use search_state::*;
pub use types::*;

use std::borrow::{Borrow, Cow};
use std::cell::RefCell;
use std::collections::{BTreeSet, HashMap, VecDeque};
#[cfg(test)]
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use crate::config::{AppConfig, ConfigManager};
use crate::remote::connection::ConnectionNodeId;
use crate::highlight::HighlightLanguage;
use crate::analysis::jstack::{
    JstackAnalysisResult, JstackAnalysisTarget, JstackThreadDetail,
    JstackThreadFilter, JstackThreadState, analyze_jstack_targets,
};
#[cfg(test)]
use crate::loader::SourceMetadata;
use crate::loader::{
    LoadReport, LogSourceLoader, SourceArchiveProbeRequest, SourceArchiveProbeResult, SourceId,
    SourceKind, SourceLocation, SourceRegistry, SourceTreeNode,
};
use crate::infra::perf::PerfSpan;
use crate::platform::open_with_registration::RegistrationStatus;
use crate::reader::log_file_reader::{
    LogFileReader, LogOpenState, LogReaderHandle, OpenLogRequest,
};
use crate::reader::read_mode::ReadMode;
use crate::analysis::runtime::{
    RuntimeAnalysisFilterRows, RuntimeAnalysisResult,
    RuntimeAnalysisTarget, RuntimeAnalysisTargetKind,
    RuntimeSlowSqlSummaryRow, RuntimeSqlFrequencyAnalysisRow,
    analyze_runtime_targets, build_runtime_analysis_filter_rows,
    build_runtime_slow_sql_rows_for_filter, build_runtime_sql_frequency_rows_for_filter,
    parse_runtime_analysis_filter_criteria,
};
use crate::remote::sftp::SftpSessionState;
use crate::remote::terminal::TerminalSessionState;
use crate::infra::text_selection::{
    TextSelectionGranularity, character_count, replace_character_range, slice_character_range,
};
use crate::theme::{AppTheme, ThemeManager, ThemeOption};
use crate::ui::components::context_menu::{ActiveMenu, ActiveMenuKind, MenuAction, MenuEntry};
use crate::ui::connection_dialog::{ConnectionDirectoryWindow, ConnectionLinkWindow};
use crate::ui::file_preview_window::FilePreviewWindow;
use crate::ui::jstack_analysis_view::JstackCellHoverPreview;
use crate::ui::jstack_thread_detail_window::JstackThreadDetailWindow;
use crate::ui::main_window;
use crate::ui::settings_window::{JstackStackSegmentFilterEditorWindow, SettingsWindow};
use crate::infra::updater::{
    UpgradeCheckOutcome, UpgradeService, current_platform_arch,
    current_platform_os,
};
use chrono::{Local, NaiveDate, TimeZone, Timelike};
use gpui::{
    AppContext, Bounds, ClipboardItem, Context, Entity, IntoElement, Keystroke,
    Pixels, Point, Render, Subscription, Timer, TitlebarOptions, Window, WindowBounds,
    WindowHandle, WindowOptions, point, px, size,
};
use gpui::{ScrollHandle, ScrollStrategy, UniformListScrollHandle};
#[cfg(test)]
use log_text::{log_text_range_for_granularity, merge_log_text_ranges};
#[cfg(test)]
use placeholder_data::{placeholder_logs, placeholder_source_registry};
pub use source_picker_actions::{
    ExternalSourceTrigger, SourcePickerSortDirection, SourcePickerSortKey, SourcePickerState,
};

/// 兼容 UI 层既有命名：Runtime SQL 分析缓存使用的过滤快照。
pub use crate::analysis::runtime::RuntimeAnalysisFilterSnapshot as RuntimeSqlAnalysisFilterSnapshot;

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

/// 订阅主应用主题变化，主题切换时回调 `apply` 更新视图状态并刷新渲染。
///
/// 用于替代各独立窗口中重复的 `cx.observe(&app, |view, app_entity, cx| { view.theme = ...app.theme.clone(); cx.notify(); })` 样板。
///
/// 参数说明：
/// - `cx`：当前视图上下文。
/// - `app`：主应用实体。
/// - `apply`：主题变化时更新视图状态的回调，参数为视图、最新主题、上下文。
///
/// 返回值：主题订阅句柄，需由调用者持有以保持订阅存活。
pub fn observe_app_theme<V: 'static>(
    cx: &mut Context<V>,
    app: &Entity<ArgusApp>,
    apply: impl Fn(&mut V, &AppTheme, &mut Context<V>) + 'static,
) -> Subscription {
    let mut observed_theme = app.read(cx).theme.clone();
    cx.observe(app, move |view, app_entity, cx| {
        let theme = app_entity.read_with(cx, |app, _| app.theme.clone());
        if theme == observed_theme {
            return;
        }
        observed_theme = theme.clone();
        apply(view, &theme, cx);
        cx.notify();
    })
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

/// 构建无系统标题栏但保留可缩放能力的窗口标题栏选项。
///
/// GPUI 在 `titlebar: Some` 时会强制添加关闭/缩放按钮（红绿灯），而 `titlebar: None`
/// 又会忽略 `is_resizable` 导致窗口不可缩放。这里把红绿灯定位到窗口可视区外，既保留
/// 可缩放能力，又不显示系统按钮，关闭操作改由标题栏右侧的自定义关闭按钮承担。
/// 线程详情窗口与文件预览窗口共用此配置。
fn frameless_resizable_titlebar() -> TitlebarOptions {
    TitlebarOptions {
        title: None,
        appears_transparent: true,
        traffic_light_position: Some(point(px(-1000.0), px(0.0))),
    }
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
    /// 当前主题令牌。
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
    /// 来源树搜索框输入法 marked text 字符范围。
    pub source_tree_search_marked_range: Option<std::ops::Range<usize>>,
    /// 来源树搜索框鼠标拖拽选择状态；鼠标释放后清空。
    pub source_tree_search_selection_drag: Option<InputTextSelectionDrag>,
    /// 来源树搜索框是否处于聚焦状态，用于展示光标和选区。
    pub is_source_tree_search_focused: bool,
    /// 来源树搜索框显隐动画序号，每次开关递增以重启动画。
    pub source_tree_search_animation_generation: usize,
    /// 来源树过滤后的可见节点 ID，包含命中日志和必要祖先目录。
    pub filtered_source_ids: Vec<SourceId>,
    /// 链接树虚拟列表滚动句柄。
    pub connection_tree_scroll: UniformListScrollHandle,
    /// 当前选中的链接目录、SSH 链接或 SMB 链接节点。
    pub selected_connection_node_id: Option<ConnectionNodeId>,
    /// 链接树搜索工具栏是否处于输入模式。
    pub is_connection_tree_search_open: bool,
    /// 链接树过滤输入框状态。
    pub connection_tree_search_input: SettingsTextInputState,
    /// 当前打开的链接工作区弹窗。
    pub connection_dialog: Option<ConnectionDialogState>,
    /// 新增链接目录独立窗口是否处于打开状态。
    pub is_connection_directory_window_open: bool,
    /// 新增链接目录独立窗口句柄，用于重复点击时置前已有窗口。
    pub connection_directory_window_handle: Option<WindowHandle<ConnectionDirectoryWindow>>,
    /// 新增或编辑链接独立窗口是否处于打开状态。
    pub is_connection_link_window_open: bool,
    /// 新增或编辑链接独立窗口句柄，用于重复点击时置前已有窗口。
    pub connection_link_window_handle: Option<WindowHandle<ConnectionLinkWindow>>,
    /// 来源树子级懒加载 generation，用于丢弃过期后台结果。
    pub source_child_load_generations: HashMap<SourceId, usize>,
    /// 等待后台探测的压缩包节点队列。
    pub source_archive_probe_queue: VecDeque<SourceId>,
    /// 已在探测队列中的压缩包节点，避免重复入队。
    pub source_archive_probe_queued_ids: BTreeSet<SourceId>,
    /// 正在后台探测的压缩包节点，避免重复调度。
    pub source_archive_probe_inflight_ids: BTreeSet<SourceId>,
    /// 用户点击后独立触发的压缩包探测节点；不受批量队列阻塞。
    pub source_archive_probe_direct_inflight_ids: BTreeSet<SourceId>,
    /// 已经完成探测的压缩包节点，避免滚动时反复提交。
    pub source_archive_probe_completed_ids: BTreeSet<SourceId>,
    /// 用户点击后等待探测结果自动继续打开或展开的压缩包节点。
    pub source_archive_probe_click_intents: BTreeSet<SourceId>,
    /// 压缩包探测批次 generation，用于丢弃旧来源树返回的过期结果。
    pub source_archive_probe_generation: usize,
    /// 压缩包内目录子级加载完成后需要自动继续的分析动作。
    pub pending_source_analysis_after_load: Option<PendingSourceAnalysisAction>,
    /// 自定义日志来源选择器状态，用于替代系统路径选择器。
    pub source_picker: SourcePickerState,
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
    /// 独立日志搜索窗口、搜索任务和结果面板状态。
    pub log_search: LogSearchState,
    /// Jstack 线程日志分析页签状态表。
    pub jstack_analyses: HashMap<usize, JstackAnalysisState>,
    /// 下一个 Jstack 分析 ID。
    pub next_jstack_analysis_id: usize,
    /// Runtime 请求日志分析页签状态表。
    pub runtime_analyses: HashMap<usize, RuntimeAnalysisState>,
    /// 下一个 Runtime 分析 ID。
    pub next_runtime_analysis_id: usize,
    /// SSH 终端会话状态表。
    pub terminal_sessions: HashMap<usize, TerminalSessionState>,
    /// 下一个 SSH 终端会话 ID。
    pub next_terminal_session_id: usize,
    /// 远程文件管理会话状态表。
    pub sftp_sessions: HashMap<usize, SftpSessionState>,
    /// 下一个远程文件管理会话 ID。
    pub next_sftp_session_id: usize,
    /// 当前打开的远程文件管理弹窗。
    pub sftp_dialog: Option<SftpDialogState>,
    /// 当前 Jstack 频率方块的内部悬浮气泡数据。
    pub jstack_cell_hover_preview: Option<JstackCellHoverPreview>,
    /// 来源树中用于“选中文件搜索”的多选日志节点。
    pub selected_search_source_ids: BTreeSet<SourceId>,
    /// 来源树 Shift 范围选择锚点。
    pub last_source_selection_anchor: Option<SourceId>,
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
    /// 日志来源侧栏当前宽度，标题栏左段与内容区侧栏共用。
    pub source_panel_width: f32,
    /// 链接工作区侧栏当前宽度；与日志来源侧栏独立，默认使用最小可用宽度。
    pub connection_source_panel_width: f32,
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
    /// 当前选择的主题 ID；内置和用户主题都使用 TOML 文件名。
    pub selected_theme_id: String,
    /// 设置窗口主题下拉框是否展开。
    pub is_theme_dropdown_open: bool,
    /// 设置窗口“快搜关键字”输入框状态。
    pub settings_quick_keywords_input: SettingsTextInputState,
    /// 设置窗口“Jstack 线程名过滤”输入框状态。
    pub settings_jstack_thread_name_filter_input: SettingsTextInputState,
    /// 设置窗口“Jstack 线程段过滤”输入框状态。
    pub settings_jstack_stack_segment_filter_input: SettingsTextInputState,
    /// 设置窗口“升级服务器”输入框状态。
    pub settings_upgrade_server_input: SettingsTextInputState,
    /// 设置窗口“升级验签公钥”输入框状态。
    pub settings_upgrade_public_key_input: SettingsTextInputState,
    /// 设置窗口是否处于打开状态。
    pub is_settings_window_open: bool,
    /// 设置窗口句柄，用于重复点击设置按钮时置前已有窗口。
    pub settings_window_handle: Option<WindowHandle<SettingsWindow>>,
    /// Jstack 线程段过滤编辑器是否处于打开状态。
    pub is_jstack_stack_segment_filter_editor_open: bool,
    /// Jstack 线程段过滤编辑器窗口句柄，用于从设置页重复点击时置前已有编辑器。
    pub jstack_stack_segment_filter_editor_handle:
        Option<WindowHandle<JstackStackSegmentFilterEditorWindow>>,
    /// 系统“用 Argus 打开”右键菜单注册状态。
    pub open_with_registration_status: RegistrationStatus,
    /// 是否正在执行系统右键菜单注册或卸载任务。
    pub is_open_with_registration_busy: bool,
    /// 系统右键菜单注册最近一次操作提示。
    pub open_with_registration_message: Option<String>,
    /// 日志内容区字号，仅影响主阅读区域。
    pub log_content_font_size: f32,
    /// 设置页编码选项。
    pub selected_encoding: String,
    /// 是否启用临时缓存。
    pub is_cache_enabled: bool,
    /// 缓存上限，单位 MB。
    pub cache_limit_mb: usize,
    /// 是否正在后台检查升级。
    pub is_upgrade_checking: bool,
    /// 是否正在下载、替换或重启升级版本。
    pub is_upgrade_installing: bool,
    /// 最近一次升级检查或安装提示。
    pub upgrade_message: Option<String>,
    /// 当前升级弹窗状态。
    pub upgrade_dialog: Option<UpgradeDialogState>,
    /// 主窗口输入框真实焦点句柄；首次渲染时创建，测试环境可保持为空。
    pub input_focus_handles: Option<AppInputFocusHandles>,
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
        let selected_theme_id = theme_manager.resolve_theme_id(&config.appearance.theme_mode);
        let theme = theme_manager.theme_for_id(&selected_theme_id);
        let log_content_font_size = config
            .appearance
            .log_content_font_size
            .clamp(LOG_CONTENT_FONT_SIZE_MIN, LOG_CONTENT_FONT_SIZE_MAX);
        let selected_encoding = config.encoding.selected.clone();
        let is_cache_enabled = config.cache.enabled;
        let cache_limit_mb = config.cache.limit_mb.clamp(128, 2048);
        let quick_keywords_input_value = config.log_search.quick_keywords.clone();
        let jstack_thread_name_filter_input_value =
            config.log_display.jstack_thread_name_filters.clone();
        let jstack_stack_segment_filter_input_value =
            config.log_display.jstack_stack_segment_filters.clone();
        let upgrade_server_input_value = config.upgrade.server_url.clone();
        let upgrade_public_key_input_value = config.upgrade.public_key_base64.clone();
        config.appearance.theme_mode = selected_theme_id.clone();
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
            source_tree_search_marked_range: None,
            source_tree_search_selection_drag: None,
            is_source_tree_search_focused: false,
            source_tree_search_animation_generation: 0,
            filtered_source_ids: Vec::new(),
            connection_tree_scroll: UniformListScrollHandle::new(),
            selected_connection_node_id: None,
            is_connection_tree_search_open: false,
            connection_tree_search_input: SettingsTextInputState::default(),
            connection_dialog: None,
            is_connection_directory_window_open: false,
            connection_directory_window_handle: None,
            is_connection_link_window_open: false,
            connection_link_window_handle: None,
            source_child_load_generations: HashMap::new(),
            source_archive_probe_queue: VecDeque::new(),
            source_archive_probe_queued_ids: BTreeSet::new(),
            source_archive_probe_inflight_ids: BTreeSet::new(),
            source_archive_probe_direct_inflight_ids: BTreeSet::new(),
            source_archive_probe_completed_ids: BTreeSet::new(),
            source_archive_probe_click_intents: BTreeSet::new(),
            source_archive_probe_generation: 0,
            pending_source_analysis_after_load: None,
            source_picker: SourcePickerState::default(),
            content_state: ContentState::SourceNotSelected,
            logs: Vec::new(),
            log_read_states: HashMap::new(),
            log_reader_generations: HashMap::new(),
            log_tab_view_states: HashMap::new(),
            log_scrollbar_drag: None,
            log_search: LogSearchState::default(),
            jstack_analyses: HashMap::new(),
            next_jstack_analysis_id: 1,
            runtime_analyses: HashMap::new(),
            next_runtime_analysis_id: 1,
            terminal_sessions: HashMap::new(),
            next_terminal_session_id: 1,
            sftp_sessions: HashMap::new(),
            next_sftp_session_id: 1,
            sftp_dialog: None,
            jstack_cell_hover_preview: None,
            selected_search_source_ids: BTreeSet::new(),
            last_source_selection_anchor: None,
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
            connection_source_panel_width: SOURCE_PANEL_MIN_WIDTH,
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
            selected_theme_id,
            is_theme_dropdown_open: false,
            settings_quick_keywords_input: SettingsTextInputState::from_value(
                quick_keywords_input_value,
            ),
            settings_jstack_thread_name_filter_input: SettingsTextInputState::from_value(
                jstack_thread_name_filter_input_value,
            ),
            settings_jstack_stack_segment_filter_input: SettingsTextInputState::from_value(
                jstack_stack_segment_filter_input_value,
            ),
            settings_upgrade_server_input: SettingsTextInputState::from_value(
                upgrade_server_input_value,
            ),
            settings_upgrade_public_key_input: SettingsTextInputState::from_value(
                upgrade_public_key_input_value,
            ),
            is_settings_window_open: false,
            settings_window_handle: None,
            is_jstack_stack_segment_filter_editor_open: false,
            jstack_stack_segment_filter_editor_handle: None,
            open_with_registration_status: RegistrationStatus::Unknown("尚未检查".to_string()),
            is_open_with_registration_busy: false,
            open_with_registration_message: None,
            log_content_font_size,
            selected_encoding,
            is_cache_enabled,
            cache_limit_mb,
            is_upgrade_checking: false,
            is_upgrade_installing: false,
            upgrade_message: None,
            upgrade_dialog: None,
            input_focus_handles: None,
        }
    }

    /// 确保主窗口输入框焦点句柄已创建，并返回可复制的句柄集合。
    pub fn ensure_input_focus_handles(&mut self, cx: &mut Context<Self>) -> AppInputFocusHandles {
        if self.input_focus_handles.is_none() {
            self.input_focus_handles = Some(AppInputFocusHandles {
                root: cx.focus_handle(),
                source_tree_search: cx.focus_handle(),
                connection_tree_search: cx.focus_handle(),
                connection_directory_name: cx.focus_handle(),
                connection_link_name: cx.focus_handle(),
                connection_link_host: cx.focus_handle(),
                connection_link_port: cx.focus_handle(),
                connection_link_username: cx.focus_handle(),
                connection_link_password: cx.focus_handle(),
                connection_link_private_key_path: cx.focus_handle(),
                connection_link_private_key_passphrase: cx.focus_handle(),
                sftp_address: cx.focus_handle(),
                sftp_rename_name: cx.focus_handle(),
                terminal: cx.focus_handle(),
                jstack_analysis: cx.focus_handle(),
                runtime_analysis: cx.focus_handle(),
                runtime_filter_keyword: cx.focus_handle(),
                runtime_filter_username: cx.focus_handle(),
                runtime_filter_start_time: cx.focus_handle(),
                runtime_filter_end_time: cx.focus_handle(),
            });
        }
        self.input_focus_handles
            .as_ref()
            .expect("主窗口输入框焦点句柄应已初始化")
            .clone()
    }

    /// 切换标题栏工作区入口，并更新状态提示。
    pub fn switch_workspace(&mut self, workspace: Workspace, cx: &mut Context<Self>) {
        if workspace == Workspace::Settings {
            self.open_settings_window(cx);
            return;
        }

        self.workspace = workspace;
        self.sync_source_panel_animation_to_current_width();
        if workspace == Workspace::Connections && self.is_source_panel_collapsed {
            self.toggle_source_panel();
        }
        self.placeholder_notice = match workspace {
            Workspace::LogAnalysis => "已切换到日志分析占位工作区".to_string(),
            Workspace::Connections => "已切换到链接工作区".to_string(),
            Workspace::Settings => "已切换到设置占位工作区".to_string(),
        };
    }

    /// 兼容旧入口：打开或聚焦独立设置窗口。
    pub fn open_settings_modal(&mut self, cx: &mut Context<Self>) {
        self.open_settings_window(cx);
    }

    /// 兼容旧入口：关闭独立设置窗口状态。
    pub fn close_settings_modal(&mut self) {
        self.close_settings_window();
    }

    /// 持久化当前配置；失败时只更新提示，不回滚已经生效的 UI 状态。
    fn persist_config_or_report(&mut self) {
        if let Err(error) = self.config_manager.save(&self.config) {
            self.placeholder_notice = format!("{}；设置保存失败：{error}", self.placeholder_notice);
        }
    }

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
        self.placeholder_notice = "请使用来源工具栏的加载日志按钮打开自定义来源选择器".to_string();
    }

    /// 打开自定义跨平台来源选择器，后续由选择器确认按钮触发真实加载。
    pub fn request_load_sources(&mut self, cx: &mut Context<Self>) {
        self.open_source_picker(cx);
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
        let current_width = self.current_source_panel_width();
        self.is_source_panel_collapsed = !self.is_source_panel_collapsed;
        self.is_source_panel_resizing = false;
        self.is_source_resizer_hovered = false;
        self.source_panel_animation_generation =
            self.source_panel_animation_generation.wrapping_add(1);
        self.source_panel_animation_from_width = if was_collapsed { 0.0 } else { current_width };
        self.source_panel_animation_to_width = if self.is_source_panel_collapsed {
            0.0
        } else {
            current_width
        };
        self.placeholder_notice = if self.is_source_panel_collapsed {
            "已折叠来源侧栏".to_string()
        } else {
            "已展开来源侧栏".to_string()
        };
    }

    /// 返回当前工作区对应的侧栏宽度，日志与链接互不影响。
    pub fn current_source_panel_width(&self) -> f32 {
        match self.workspace {
            Workspace::Connections => self.connection_source_panel_width,
            Workspace::LogAnalysis | Workspace::Settings => self.source_panel_width,
        }
    }

    /// 更新当前工作区对应的侧栏宽度。
    fn set_current_source_panel_width(&mut self, width: f32) {
        match self.workspace {
            Workspace::Connections => self.connection_source_panel_width = width,
            Workspace::LogAnalysis | Workspace::Settings => self.source_panel_width = width,
        }
    }

    /// 同步侧栏动画端点，避免切换工作区时沿用另一个功能的宽度。
    fn sync_source_panel_animation_to_current_width(&mut self) {
        let current_width = if self.is_source_panel_collapsed {
            0.0
        } else {
            self.current_source_panel_width()
        };
        self.source_panel_animation_from_width = current_width;
        self.source_panel_animation_to_width = current_width;
    }

    /// 开始拖拽来源侧栏分割线，记录初始鼠标位置和宽度。
    pub fn begin_source_panel_resize(&mut self, pointer_x: f32) {
        self.is_source_panel_resizing = true;
        self.is_source_resizer_hovered = true;
        self.source_resize_start_x = pointer_x;
        self.source_resize_start_width = self.current_source_panel_width();
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
        if (next_width - self.current_source_panel_width()).abs() < 0.5 {
            return false;
        }

        self.set_current_source_panel_width(next_width);
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
        self.placeholder_notice = format!(
            "来源侧栏宽度已调整为 {:.0}px",
            self.current_source_panel_width()
        );
        true
    }

    /// 兼容旧测试入口：设置页已迁移为独立窗口，标签页路径不再由 UI 触发。
    pub fn open_or_focus_settings_tab(&mut self) {
        self.placeholder_notice = "设置已迁移到独立窗口，请从标题栏设置按钮打开".to_string();
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
            self.log_read_states
                .entry(source_id)
                .or_insert(LogOpenState::Idle);
            self.sync_source_tree_selection_from_active_tab();
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
        self.log_read_states
            .entry(source_id)
            .or_insert(LogOpenState::Idle);
        self.sync_source_tree_selection_from_active_tab();
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
                self.finish_pending_search_activation(source_id);
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

    /// 释放某个标签页对应的运行期资源，避免关闭 tab 后继续占用内存。
    fn release_resources_for_tab_kind(&mut self, kind: &TabKind) {
        match kind {
            TabKind::LogSource { source_id, .. } => {
                self.log_read_states.remove(source_id);
                self.log_reader_generations.remove(source_id);
            }
            TabKind::JstackAnalysis { analysis_id } => {
                self.jstack_analyses.remove(analysis_id);
            }
            TabKind::RuntimeAnalysis { analysis_id } => {
                self.runtime_analyses.remove(analysis_id);
            }
            TabKind::SshTerminal { session_id } => {
                self.disconnect_terminal_session(*session_id);
            }
            TabKind::SftpFileManager { session_id } => {
                self.disconnect_sftp_session(*session_id);
            }
            TabKind::Empty | TabKind::Settings => {}
        }
    }

    /// 只保留指定来源的日志读取状态；非日志 tab 会清空全部读取结果。
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

    /// 只保留指定 Jstack 分析 tab 的结果；非分析 tab 会清空全部分析状态。
    fn retain_jstack_analysis_for_tab_kind(&mut self, kept_kind: &TabKind) {
        match kept_kind {
            TabKind::JstackAnalysis { analysis_id } => {
                self.jstack_analyses
                    .retain(|existing_id, _| *existing_id == *analysis_id);
            }
            TabKind::Empty
            | TabKind::LogSource { .. }
            | TabKind::SshTerminal { .. }
            | TabKind::SftpFileManager { .. }
            | TabKind::RuntimeAnalysis { .. }
            | TabKind::Settings => {
                self.jstack_analyses.clear();
            }
        }
    }

    /// 只保留指定 Runtime 分析 tab 的结果；非 Runtime 分析 tab 会清空全部 Runtime 状态。
    fn retain_runtime_analysis_for_tab_kind(&mut self, kept_kind: &TabKind) {
        match kept_kind {
            TabKind::RuntimeAnalysis { analysis_id } => {
                self.runtime_analyses
                    .retain(|existing_id, _| *existing_id == *analysis_id);
            }
            TabKind::Empty
            | TabKind::LogSource { .. }
            | TabKind::JstackAnalysis { .. }
            | TabKind::SshTerminal { .. }
            | TabKind::SftpFileManager { .. }
            | TabKind::Settings => {
                self.runtime_analyses.clear();
            }
        }
    }

    /// 只保留指定 SSH 终端 tab 的会话；非终端 tab 会断开全部终端。
    fn retain_terminal_session_for_tab_kind(&mut self, kept_kind: &TabKind) {
        let kept_session_id = match kept_kind {
            TabKind::SshTerminal { session_id } => Some(*session_id),
            TabKind::Empty
            | TabKind::LogSource { .. }
            | TabKind::JstackAnalysis { .. }
            | TabKind::SftpFileManager { .. }
            | TabKind::RuntimeAnalysis { .. }
            | TabKind::Settings => None,
        };
        let sessions_to_disconnect = self
            .terminal_sessions
            .keys()
            .copied()
            .filter(|session_id| Some(*session_id) != kept_session_id)
            .collect::<Vec<_>>();
        for session_id in sessions_to_disconnect {
            self.disconnect_terminal_session(session_id);
        }
    }

    /// 只保留指定远程文件管理 tab 的会话；非远程文件 tab 会断开全部文件管理会话。
    fn retain_sftp_session_for_tab_kind(&mut self, kept_kind: &TabKind) {
        let kept_session_id = match kept_kind {
            TabKind::SftpFileManager { session_id } => Some(*session_id),
            TabKind::Empty
            | TabKind::LogSource { .. }
            | TabKind::JstackAnalysis { .. }
            | TabKind::RuntimeAnalysis { .. }
            | TabKind::SshTerminal { .. }
            | TabKind::Settings => None,
        };
        let sessions_to_disconnect = self
            .sftp_sessions
            .keys()
            .copied()
            .filter(|session_id| Some(*session_id) != kept_session_id)
            .collect::<Vec<_>>();
        for session_id in sessions_to_disconnect {
            self.disconnect_sftp_session(session_id);
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

    /// 为分页日志当前视口安排后台预取；UI 渲染期只登记请求，不同步读取文件。
    ///
    /// 参数说明：
    /// - `tab_id`：当前日志标签页 ID。
    /// - `source_id`：日志来源节点 ID，用于避免旧来源预取覆盖新状态。
    /// - `handle`：日志读取句柄；内部分页缓存通过 `Arc` 共享给后台任务。
    /// - `first_line_index`：当前视口首行 0 基行号。
    /// - `visible_rows`：当前视口行容量。
    /// - `cx`：应用上下文，用于调度后台读取并通知 UI 刷新。
    pub fn request_paged_log_prefetch(
        &mut self,
        tab_id: usize,
        source_id: SourceId,
        handle: LogReaderHandle,
        first_line_index: usize,
        visible_rows: usize,
        cx: &mut Context<Self>,
    ) {
        let _span = PerfSpan::new("request_paged_log_prefetch");
        if !matches!(
            handle.document(),
            crate::reader::log_file_reader::LogDocument::Paged(_)
        ) || visible_rows == 0
        {
            return;
        }

        let line_count = handle.line_count();
        if first_line_index >= line_count {
            return;
        }

        let prefetch_start = first_line_index.saturating_sub(visible_rows);
        let prefetch_end = first_line_index
            .saturating_add(visible_rows.saturating_mul(2))
            .min(line_count);
        let prefetch_count = prefetch_end.saturating_sub(prefetch_start);
        if prefetch_count == 0 || handle.has_cached_lines(prefetch_start, prefetch_count) {
            if let Some(state) = self.log_tab_view_states.get_mut(&tab_id) {
                state.pending_paged_prefetch = None;
            }
            return;
        }

        let Some(state) = self.log_tab_view_states.get_mut(&tab_id) else {
            return;
        };
        let requested_end = prefetch_start.saturating_add(prefetch_count);
        if state
            .pending_paged_prefetch
            .as_ref()
            .is_some_and(|pending| {
                pending.source_id == source_id
                    && pending.start_line <= prefetch_start
                    && pending.start_line.saturating_add(pending.max_lines) >= requested_end
            })
        {
            return;
        }

        state.pending_paged_prefetch = Some(PagedLogPrefetchRequest {
            source_id,
            start_line: prefetch_start,
            max_lines: prefetch_count,
        });

        cx.spawn(async move |view, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { handle.lines(prefetch_start, prefetch_count).map(|_| ()) })
                .await;

            view.update(cx, |app, cx| {
                if let Some(state) = app.log_tab_view_states.get_mut(&tab_id)
                    && state
                        .pending_paged_prefetch
                        .as_ref()
                        .is_some_and(|pending| {
                            pending.source_id == source_id
                                && pending.start_line == prefetch_start
                                && pending.max_lines == prefetch_count
                        })
                {
                    state.pending_paged_prefetch = None;
                }
                if let Err(error) = result {
                    app.placeholder_notice = format!("分页日志预取失败：{error}");
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// 为分页日志可见文本安排后台语法高亮预取；UI 首帧只显示已有高亮缓存。
    ///
    /// 参数说明：
    /// - `tab_id`：当前日志标签页 ID。
    /// - `source_id`：日志来源节点 ID。
    /// - `language`：当前日志识别出的语法高亮语言。
    /// - `lines`：需要补算高亮的可见文本切片。
    /// - `cx`：应用上下文，用于调度后台高亮并通知 UI 刷新。
    pub fn request_log_highlight_prefetch(
        &mut self,
        tab_id: usize,
        source_id: SourceId,
        language: HighlightLanguage,
        lines: Vec<(usize, String)>,
        cx: &mut Context<Self>,
    ) {
        if lines.is_empty() || language == HighlightLanguage::Plain {
            return;
        }

        let Some(first_line) = lines.first().map(|(line_number, _)| *line_number) else {
            return;
        };
        let Some(last_line) = lines.last().map(|(line_number, _)| *line_number) else {
            return;
        };
        let request = LogHighlightPrefetchRequest {
            source_id,
            language,
            start_line: first_line,
            max_lines: last_line.saturating_sub(first_line).saturating_add(1),
        };

        let Some(state) = self.log_tab_view_states.get_mut(&tab_id) else {
            return;
        };
        let requested_end = request.start_line.saturating_add(request.max_lines);
        if state
            .pending_highlight_prefetch
            .as_ref()
            .is_some_and(|pending| {
                pending.source_id == request.source_id
                    && pending.language == request.language
                    && pending.start_line <= request.start_line
                    && pending.start_line.saturating_add(pending.max_lines) >= requested_end
            })
        {
            return;
        }

        state.pending_highlight_prefetch = Some(request.clone());
        let highlight_cache = state.highlight_cache.clone();

        cx.spawn(async move |view, cx| {
            cx.background_executor()
                .spawn(async move {
                    for (line_number, text) in lines {
                        highlight_cache.highlight_line(line_number, language, &text);
                    }
                })
                .await;

            view.update(cx, |app, cx| {
                if let Some(state) = app.log_tab_view_states.get_mut(&tab_id)
                    && state
                        .pending_highlight_prefetch
                        .as_ref()
                        .is_some_and(|pending| pending == &request)
                {
                    state.pending_highlight_prefetch = None;
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// 切换到指定标签页。
    pub fn activate_tab(&mut self, tab_id: usize) {
        let _span = PerfSpan::new("activate_tab");
        if self.active_tab_id == tab_id {
            return;
        }
        if self.tabs.iter().any(|tab| tab.id == tab_id) {
            self.active_tab_id = tab_id;
            self.sync_source_tree_selection_from_active_tab();
            self.placeholder_notice = format!("已切换到 {}", self.active_tab_title());
        }
    }

    /// 在 UI 事件中切换标签页，并同步清理 Jstack 方块悬浮气泡。
    pub fn activate_tab_with_context(&mut self, tab_id: usize, _cx: &mut Context<Self>) {
        self.clear_jstack_cell_hover_preview();
        self.activate_tab(tab_id);
    }

    /// 让来源树视觉选中跟随当前日志标签，并确保当前文件所在目录可见。
    ///
    /// 说明：这里是 UI 视图同步，不触发日志读取或目录懒加载；普通 tab 切换不是主动多选。
    /// 若用户已经主动多选多个搜索文件，仅更新强激活态，避免悄悄改写搜索范围。
    fn sync_source_tree_selection_from_active_tab(&mut self) {
        let TabKind::LogSource { source_id, .. } = self.active_tab_kind() else {
            return;
        };

        let was_selected = self.source_registry.selected_id() == Some(source_id);
        let expanded = self.source_registry.expand_ancestors(source_id);
        let selected = if was_selected {
            true
        } else {
            self.source_registry.select(source_id).is_some()
        };
        if selected {
            if self.selected_search_source_ids.len() <= 1 {
                self.selected_search_source_ids.clear();
                self.selected_search_source_ids.insert(source_id);
                self.last_source_selection_anchor = Some(source_id);
            }
            if !was_selected || expanded {
                self.scroll_source_into_view(source_id);
            }
        }
    }

    pub fn set_hovered_tab(&mut self, tab_id: usize, is_hovered: bool) -> bool {
        if is_hovered {
            if self.hovered_tab_id == Some(tab_id) {
                return false;
            }
            self.hovered_tab_id = Some(tab_id);
            true
        } else if self.hovered_tab_id == Some(tab_id) {
            self.hovered_tab_id = None;
            true
        } else {
            false
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
            self.jstack_analyses.clear();
            self.next_jstack_analysis_id = 1;
            self.runtime_analyses.clear();
            self.next_runtime_analysis_id = 1;
            self.disconnect_all_terminal_sessions();
            self.disconnect_all_sftp_sessions();
            self.ensure_log_tab_view_state(self.active_tab_id);
            self.reset_log_text_selection();
            self.log_scrollbar_drag = None;
            self.reset_log_search_runtime_state();
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
            self.release_resources_for_tab_kind(&kind);
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
            self.sync_source_tree_selection_from_active_tab();
        }
        self.placeholder_notice = "已关闭标签页".to_string();
    }

    /// 在 UI 事件中关闭指定标签页，并同步清理 Jstack 方块悬浮气泡。
    pub fn close_tab_with_context(&mut self, tab_id: usize, _cx: &mut Context<Self>) {
        self.clear_jstack_cell_hover_preview();
        self.close_tab(tab_id);
    }

    /// 关闭指定标签之外的其他标签，并激活保留标签。
    pub fn close_other_tabs(&mut self, tab_id: usize) {
        let Some(kept_tab) = self.tabs.iter().find(|tab| tab.id == tab_id).cloned() else {
            self.placeholder_notice = "未找到需要保留的标签页".to_string();
            return;
        };

        let removed_count = self.tabs.len().saturating_sub(1);
        let kept_source_id = source_id_for_tab_kind(&kept_tab.kind);
        let kept_kind = kept_tab.kind.clone();
        self.tabs = vec![kept_tab];
        self.log_tab_view_states
            .retain(|existing_tab_id, _| *existing_tab_id == tab_id);
        self.ensure_log_tab_view_state(tab_id);
        self.retain_reader_for_source(kept_source_id);
        self.retain_jstack_analysis_for_tab_kind(&kept_kind);
        self.retain_runtime_analysis_for_tab_kind(&kept_kind);
        self.retain_terminal_session_for_tab_kind(&kept_kind);
        self.retain_sftp_session_for_tab_kind(&kept_kind);
        self.active_tab_id = tab_id;
        self.sync_source_tree_selection_from_active_tab();
        self.hovered_tab_id = None;
        self.log_scrollbar_drag = None;
        self.placeholder_notice = if removed_count == 0 {
            "没有其他标签可关闭".to_string()
        } else {
            format!("已关闭 {removed_count} 个其他标签")
        };
    }

    /// 在 UI 事件中关闭其他标签页，并同步清理 Jstack 方块悬浮气泡。
    pub fn close_other_tabs_with_context(&mut self, tab_id: usize, _cx: &mut Context<Self>) {
        self.clear_jstack_cell_hover_preview();
        self.close_other_tabs(tab_id);
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
        self.jstack_analyses.clear();
        self.next_jstack_analysis_id = 1;
        self.runtime_analyses.clear();
        self.next_runtime_analysis_id = 1;
        self.disconnect_all_terminal_sessions();
        self.disconnect_all_sftp_sessions();
        self.ensure_log_tab_view_state(empty_tab_id);
        self.active_tab_id = empty_tab_id;
        self.hovered_tab_id = None;
        self.reset_log_text_selection();
        self.log_scrollbar_drag = None;
        self.reset_log_search_runtime_state();
        self.content_state = ContentState::SourceNotSelected;
        self.placeholder_notice = "已关闭全部标签".to_string();
    }

    /// 在 UI 事件中关闭全部标签页，并同步清理 Jstack 方块悬浮气泡。
    pub fn close_all_tabs_with_context(&mut self, _cx: &mut Context<Self>) {
        self.clear_jstack_cell_hover_preview();
        self.close_all_tabs();
    }

    pub fn active_tab(&self) -> Option<&ArgusTab> {
        self.tabs.iter().find(|tab| tab.id == self.active_tab_id)
    }

    /// 返回当前激活标签类型；缺失时按空标签兜底。
    pub fn active_tab_kind(&self) -> TabKind {
        self.active_tab()
            .map(|tab| tab.kind.clone())
            .unwrap_or(TabKind::Empty)
    }

    /// 返回内容区路径文案，优先展示真实选中来源。
    pub fn content_path_label(&self) -> String {
        match self.active_tab_kind() {
            TabKind::LogSource { path, .. } => path,
            TabKind::JstackAnalysis { analysis_id } => self
                .jstack_analyses
                .get(&analysis_id)
                .map(|state| format!("Argus / {}", state.title))
                .unwrap_or_else(|| "Argus / Jstack分析".to_string()),
            TabKind::RuntimeAnalysis { analysis_id } => self
                .runtime_analyses
                .get(&analysis_id)
                .map(|state| format!("Argus / {}", state.title))
                .unwrap_or_else(|| "Argus / Runtime分析".to_string()),
            TabKind::SshTerminal { session_id } => self
                .terminal_sessions
                .get(&session_id)
                .map(|state| format!("SSH / {}", state.address))
                .unwrap_or_else(|| "SSH / 终端".to_string()),
            TabKind::SftpFileManager { session_id } => self
                .sftp_sessions
                .get(&session_id)
                .map(|state| format!("SFTP / {}:{}", state.address, state.current_dir))
                .unwrap_or_else(|| "SFTP / 文件管理".to_string()),
            TabKind::Settings => "Argus / 设置".to_string(),
            TabKind::Empty if self.has_loaded_real_sources => "请选择日志来源".to_string(),
            TabKind::Empty => "未选择来源".to_string(),
        }
    }

    /// 请求来源树滚动到指定可见节点。
    pub fn scroll_source_into_view(&mut self, source_id: SourceId) {
        let index = if self.is_source_tree_filtering() {
            self.filtered_source_ids
                .iter()
                .position(|visible_id| *visible_id == source_id)
        } else {
            self.source_registry.visible_index_of(source_id)
        };

        if let Some(index) = index {
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

    /// 在主窗口渲染前保留主题同步入口；当前仅支持暗色主题，因此不随系统外观切换。
    pub fn sync_window_appearance_theme(&mut self, _window: &Window) {}

    /// 返回设置下拉框中的主题选项。
    pub fn theme_options(&self) -> Vec<ThemeOption> {
        self.theme_manager.theme_options()
    }

    /// 返回当前主题在下拉框中的展示文案。
    pub fn selected_theme_label(&self) -> String {
        self.theme_manager
            .label_for_theme_id(&self.selected_theme_id)
    }

    /// 切换设置窗口中的主题下拉框展开状态。
    pub fn toggle_theme_dropdown(&mut self) {
        if self.is_theme_dropdown_open {
            self.close_theme_dropdown();
            return;
        }

        self.is_theme_dropdown_open = true;
    }

    /// 关闭设置窗口中的主题下拉框。
    pub fn close_theme_dropdown(&mut self) {
        self.is_theme_dropdown_open = false;
    }

    /// 按主题 TOML 文件名选择主题，并立即持久化设置。
    pub fn select_theme(&mut self, theme_id: String) {
        let resolved_theme_id = self.theme_manager.resolve_theme_id(&theme_id);
        self.selected_theme_id = resolved_theme_id.clone();
        self.theme = self.theme_manager.theme_for_id(&resolved_theme_id);
        self.config.appearance.theme_mode = resolved_theme_id.clone();
        self.is_theme_dropdown_open = false;
        self.placeholder_notice = format!("主题已切换为 {resolved_theme_id}");
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

    /// 调整当前目录层单文件压缩包探测并发数，设置会影响后续来源加载任务。
    pub fn adjust_archive_probe_concurrency(&mut self, delta: isize) {
        self.config.loader.archive_probe_concurrency = self
            .config
            .loader
            .archive_probe_concurrency
            .saturating_add_signed(delta)
            .clamp(1, 16);
        self.placeholder_notice = format!(
            "单文件压缩包探测并发已调整为 {}",
            self.config.loader.archive_probe_concurrency
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
        TabKind::Empty
        | TabKind::JstackAnalysis { .. }
        | TabKind::RuntimeAnalysis { .. }
        | TabKind::SshTerminal { .. }
        | TabKind::SftpFileManager { .. }
        | TabKind::Settings => None,
    }
}

/// 返回 Runtime 总览排序字段的默认方向。
fn default_runtime_summary_sort_direction(sort_key: RuntimeSummarySortKey) -> RuntimeSortDirection {
    match sort_key {
        RuntimeSummarySortKey::RequestPath => RuntimeSortDirection::Ascending,
        RuntimeSummarySortKey::RequestCount
        | RuntimeSummarySortKey::AverageDuration
        | RuntimeSummarySortKey::SlowSqlRatio => RuntimeSortDirection::Descending,
    }
}

/// 返回 Runtime 请求明细排序字段的默认方向。
fn default_runtime_request_sort_direction(sort_key: RuntimeRequestSortKey) -> RuntimeSortDirection {
    match sort_key {
        RuntimeRequestSortKey::Username | RuntimeRequestSortKey::RequestPath => {
            RuntimeSortDirection::Ascending
        }
        RuntimeRequestSortKey::RequestTime | RuntimeRequestSortKey::RequestDuration => {
            RuntimeSortDirection::Descending
        }
    }
}

/// 返回 Runtime SQL 明细排序字段的默认方向。
fn default_runtime_sql_sort_direction(sort_key: RuntimeSqlSortKey) -> RuntimeSortDirection {
    match sort_key {
        RuntimeSqlSortKey::SqlText => RuntimeSortDirection::Ascending,
        RuntimeSqlSortKey::ExecuteDuration
        | RuntimeSqlSortKey::AcquireConnectionDuration
        | RuntimeSqlSortKey::CommitDuration
        | RuntimeSqlSortKey::ReleaseConnectionDuration
        | RuntimeSqlSortKey::ParseResultDuration => RuntimeSortDirection::Descending,
    }
}

/// 生成 Runtime SQL 行展开状态使用的稳定 key。
pub fn runtime_sql_row_key(request_index: usize, sql_index: usize) -> String {
    format!("{request_index}:{sql_index}")
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

    /// 在配置中创建一个测试 SSH 链接。
    fn add_test_ssh_link(app: &mut ArgusApp) -> ConnectionNodeId {
        app.config
            .connections
            .add_ssh_link(
                None,
                "测试服务器",
                crate::remote::connection::SshLinkConfig {
                    host: "127.0.0.1".to_string(),
                    port: 22,
                    username: "tester".to_string(),
                    password: "secret".to_string(),
                    private_key_path: None,
                    private_key_passphrase: None,
                },
            )
            .expect("应能创建测试 SSH 链接")
    }

    /// 插入不连接真实服务器的终端会话。
    fn insert_test_terminal_session(
        app: &mut ArgusApp,
        session_id: usize,
        link_id: ConnectionNodeId,
    ) {
        let link = app
            .config
            .connections
            .link(link_id)
            .expect("应存在测试链接")
            .clone();
        let (sender, _) = std::sync::mpsc::channel();
        let mut session =
            crate::remote::terminal::TerminalSessionState::connecting(session_id, &link, sender);
        session.status = crate::remote::terminal::TerminalStatus::Connected;
        app.terminal_sessions.insert(session_id, session);
    }

    /// 插入不连接真实服务器的 SFTP 会话，并返回命令接收端。
    fn insert_test_sftp_session(
        app: &mut ArgusApp,
        session_id: usize,
        link_id: ConnectionNodeId,
    ) -> std::sync::mpsc::Receiver<crate::remote::sftp::SftpCommand> {
        let link = app
            .config
            .connections
            .link(link_id)
            .expect("应存在测试链接")
            .clone();
        let (sender, receiver) = std::sync::mpsc::channel();
        let mut session = crate::remote::sftp::SftpSessionState::connecting(
            session_id,
            &link,
            crate::remote::sftp::RemoteFileBackend::Sftp,
            sender,
        );
        session.status = crate::remote::sftp::SftpStatus::Connected;
        session.current_dir = "/home/tester".to_string();
        session.address_input = SettingsTextInputState::from_value(session.current_dir.clone());
        app.sftp_sessions.insert(session_id, session);
        receiver
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

    /// 验证链接工作区侧栏默认使用最小宽度，且拖拽不会污染日志来源侧栏宽度。
    #[test]
    fn connection_source_panel_width_is_independent_from_log_width() {
        let mut app = test_app();

        assert_eq!(app.current_source_panel_width(), SOURCE_PANEL_DEFAULT_WIDTH);

        app.workspace = Workspace::Connections;
        assert_eq!(app.current_source_panel_width(), SOURCE_PANEL_MIN_WIDTH);

        app.begin_source_panel_resize(0.0);
        assert!(app.resize_source_panel(100.0));
        assert_eq!(
            app.connection_source_panel_width,
            SOURCE_PANEL_MIN_WIDTH + 100.0
        );
        assert_eq!(app.source_panel_width, SOURCE_PANEL_DEFAULT_WIDTH);

        app.workspace = Workspace::LogAnalysis;
        assert_eq!(app.current_source_panel_width(), SOURCE_PANEL_DEFAULT_WIDTH);
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

    /// 构造一个已加载的压缩包内目录，模拟用户在压缩包树上直接右键目录。
    fn app_with_loaded_archive_directory() -> (ArgusApp, SourceId, SourceId, SourceId) {
        let mut app = test_app();
        let mut registry = SourceRegistry::new();
        let archive_format = crate::loader::archive::ArchiveFormat::Zip;
        let archive_path = PathBuf::from("runtime.zip");
        let dir_id = registry.allocate_id();
        registry.insert_node(SourceTreeNode {
            id: dir_id,
            parent_id: None,
            depth: 0,
            label: "runtime".to_string(),
            kind: SourceKind::ArchiveDirectory,
            location: SourceLocation::ArchiveEntry {
                archive_path: archive_path.clone(),
                root_format: archive_format,
                container_entries: Vec::new(),
                entry_path: "runtime".to_string(),
                format: archive_format,
                archive_depth: 0,
            },
            metadata: SourceMetadata {
                size: None,
                children_loaded: true,
                is_loading: false,
                message: None,
            },
            selected: false,
            expanded: true,
        });

        let first_log_id = registry.allocate_id();
        registry.insert_node(SourceTreeNode {
            id: first_log_id,
            parent_id: Some(dir_id),
            depth: 1,
            label: "thread0100.log".to_string(),
            kind: SourceKind::ArchiveFile,
            location: SourceLocation::ArchiveEntry {
                archive_path: archive_path.clone(),
                root_format: archive_format,
                container_entries: Vec::new(),
                entry_path: "runtime/thread0100.log".to_string(),
                format: archive_format,
                archive_depth: 0,
            },
            metadata: SourceMetadata {
                size: Some(128),
                children_loaded: true,
                is_loading: false,
                message: None,
            },
            selected: false,
            expanded: false,
        });

        let second_log_id = registry.allocate_id();
        registry.insert_node(SourceTreeNode {
            id: second_log_id,
            parent_id: Some(dir_id),
            depth: 1,
            label: "thread0200.log".to_string(),
            kind: SourceKind::ArchiveFile,
            location: SourceLocation::ArchiveEntry {
                archive_path,
                root_format: archive_format,
                container_entries: Vec::new(),
                entry_path: "runtime/thread0200.log".to_string(),
                format: archive_format,
                archive_depth: 0,
            },
            metadata: SourceMetadata {
                size: Some(256),
                children_loaded: true,
                is_loading: false,
                message: None,
            },
            selected: false,
            expanded: false,
        });

        registry.rebuild_all_indices();
        app.source_registry = registry;
        (app, dir_id, first_log_id, second_log_id)
    }

    /// 验证来源树右键菜单对日志候选和本地目录节点展示 Jstack 与 Runtime 分析入口。
    #[test]
    fn source_tree_context_menu_shows_analysis_actions_for_supported_sources() {
        let mut app = app_with_placeholder_sources();
        let app_log_id = source_id_by_label(&app, "app.log");
        let logs_dir_id = source_id_by_label(&app, "logs");

        app.open_source_tree_context_menu(app_log_id, gpui::point(gpui::px(1.0), gpui::px(1.0)));

        assert!(matches!(
            app.active_menu.as_ref().map(|menu| &menu.kind),
            Some(ActiveMenuKind::SourceTree { source_id }) if *source_id == app_log_id
        ));
        assert_eq!(app.active_menu_entries().len(), 2);
        assert!(matches!(
            app.active_menu_entries()[0].action,
            MenuAction::OpenJstackAnalysis { source_id } if source_id == app_log_id
        ));
        assert!(matches!(
            app.active_menu_entries()[1].action,
            MenuAction::OpenRuntimeAnalysis { source_id } if source_id == app_log_id
        ));

        app.close_active_menu();
        app.open_source_tree_context_menu(logs_dir_id, gpui::point(gpui::px(1.0), gpui::px(1.0)));

        assert!(matches!(
            app.active_menu.as_ref().map(|menu| &menu.kind),
            Some(ActiveMenuKind::SourceTree { source_id }) if *source_id == logs_dir_id
        ));
        assert_eq!(app.active_menu_entries().len(), 2);
        assert!(matches!(
            app.active_menu_entries()[0].action,
            MenuAction::OpenJstackAnalysis { source_id } if source_id == logs_dir_id
        ));
        assert!(matches!(
            app.active_menu_entries()[1].action,
            MenuAction::OpenRuntimeAnalysis { source_id } if source_id == logs_dir_id
        ));
    }

    /// 验证 SSH 终端正文右键菜单展示文件管理入口。
    #[test]
    fn terminal_context_menu_shows_file_manager_action() {
        let mut app = test_app();
        let link_id = add_test_ssh_link(&mut app);
        insert_test_terminal_session(&mut app, 7, link_id);

        app.open_terminal_context_menu(7, gpui::point(gpui::px(1.0), gpui::px(1.0)));

        assert!(matches!(
            app.active_menu.as_ref().map(|menu| &menu.kind),
            Some(ActiveMenuKind::TerminalContext { session_id }) if *session_id == 7
        ));
        let entries = app.active_menu_entries();
        assert_eq!(entries.len(), 1);
        assert!(matches!(
            entries[0].action,
            MenuAction::OpenSftpFileManager {
                terminal_session_id
            } if terminal_session_id == 7
        ));
    }

    /// 验证 SFTP 文件行右键菜单展示下载、重命名和删除动作。
    #[test]
    fn sftp_entry_context_menu_shows_file_actions() {
        let mut app = test_app();
        let link_id = add_test_ssh_link(&mut app);
        let _receiver = insert_test_sftp_session(&mut app, 1, link_id);
        let remote_path = "/home/tester/app.log".to_string();
        if let Some(session) = app.sftp_sessions.get_mut(&1) {
            session.entries = vec![crate::remote::sftp::SftpEntry {
                name: "app.log".to_string(),
                path: remote_path.clone(),
                kind: crate::remote::sftp::SftpEntryKind::RegularFile,
                size: Some(128),
                mtime: None,
                permissions: Some(0o100644),
            }];
        }

        app.open_sftp_entry_context_menu(
            1,
            remote_path.clone(),
            gpui::point(gpui::px(2.0), gpui::px(3.0)),
        );

        assert!(matches!(
            app.active_menu.as_ref().map(|menu| &menu.kind),
            Some(ActiveMenuKind::SftpEntry { session_id }) if *session_id == 1
        ));
        assert_eq!(
            app.sftp_sessions
                .get(&1)
                .expect("应存在 SFTP 会话")
                .selected_paths
                .iter()
                .cloned()
                .collect::<Vec<_>>(),
            vec![remote_path]
        );
        let entries = app.active_menu_entries();
        assert_eq!(entries.len(), 4);
        assert!(matches!(
            entries[0].action,
            MenuAction::PreviewSftpSelection { session_id } if session_id == 1
        ));
        assert!(matches!(
            entries[1].action,
            MenuAction::DownloadSftpSelection { session_id } if session_id == 1
        ));
        assert!(matches!(
            entries[2].action,
            MenuAction::RenameSftpSelection { session_id } if session_id == 1
        ));
        assert!(matches!(
            entries[3].action,
            MenuAction::DeleteSftpSelection { session_id } if session_id == 1
        ));
        assert!(entries[3].is_danger);
    }

    /// 验证右键已选集合内文件时保留多选，方便从菜单下载多个文件。
    #[test]
    fn sftp_entry_context_menu_preserves_existing_multi_selection() {
        let mut app = test_app();
        let link_id = add_test_ssh_link(&mut app);
        let _receiver = insert_test_sftp_session(&mut app, 1, link_id);
        let first_path = "/home/tester/app.log".to_string();
        let second_path = "/home/tester/error.log".to_string();
        if let Some(session) = app.sftp_sessions.get_mut(&1) {
            session.entries = vec![
                crate::remote::sftp::SftpEntry {
                    name: "app.log".to_string(),
                    path: first_path.clone(),
                    kind: crate::remote::sftp::SftpEntryKind::RegularFile,
                    size: Some(128),
                    mtime: None,
                    permissions: Some(0o100644),
                },
                crate::remote::sftp::SftpEntry {
                    name: "error.log".to_string(),
                    path: second_path.clone(),
                    kind: crate::remote::sftp::SftpEntryKind::RegularFile,
                    size: Some(256),
                    mtime: None,
                    permissions: Some(0o100644),
                },
            ];
            session.selected_paths.insert(first_path.clone());
            session.selected_paths.insert(second_path.clone());
        }

        app.open_sftp_entry_context_menu(1, second_path, gpui::point(gpui::px(2.0), gpui::px(3.0)));

        let selected_paths = &app
            .sftp_sessions
            .get(&1)
            .expect("应存在 SFTP 会话")
            .selected_paths;
        assert_eq!(selected_paths.len(), 2);
        assert!(selected_paths.contains(&first_path));
        assert!(selected_paths.contains("/home/tester/error.log"));
    }

    /// 验证同一个 SSH 链接可以打开多个独立 SFTP 文件管理标签。
    #[test]
    fn sftp_file_manager_tabs_allow_multiple_same_link() {
        let mut app = test_app();
        let link_id = add_test_ssh_link(&mut app);
        let _first_receiver = insert_test_sftp_session(&mut app, 1, link_id);
        let _second_receiver = insert_test_sftp_session(&mut app, 2, link_id);

        app.create_sftp_tab_for_session(1);
        app.create_sftp_tab_for_session(2);

        assert_eq!(app.tabs.len(), 2);
        assert!(matches!(
            app.tabs[0].kind,
            TabKind::SftpFileManager { session_id } if session_id == 1
        ));
        assert!(matches!(
            app.tabs[1].kind,
            TabKind::SftpFileManager { session_id } if session_id == 2
        ));
        assert_eq!(app.active_tab_id, app.tabs[1].id);
    }

    /// 验证关闭 SFTP 文件管理标签会断开并清理对应会话。
    #[test]
    fn close_sftp_tab_disconnects_session() {
        let mut app = test_app();
        let link_id = add_test_ssh_link(&mut app);
        let receiver = insert_test_sftp_session(&mut app, 1, link_id);
        app.tabs[0].title = "文件管理 - 测试服务器".to_string();
        app.tabs[0].kind = TabKind::SftpFileManager { session_id: 1 };
        app.active_tab_id = app.tabs[0].id;

        app.close_tab(app.tabs[0].id);

        assert!(app.sftp_sessions.is_empty());
        assert!(matches!(
            receiver.try_recv(),
            Ok(crate::remote::sftp::SftpCommand::Disconnect)
        ));
        assert!(matches!(app.tabs[0].kind, TabKind::Empty));
    }

    /// 验证 SFTP 删除入口只允许普通文件和目录，避免误删符号链接等特殊条目。
    #[test]
    fn sftp_delete_selection_rejects_special_entries() {
        let mut app = test_app();
        let link_id = add_test_ssh_link(&mut app);
        let _receiver = insert_test_sftp_session(&mut app, 1, link_id);
        let remote_path = "/home/tester/current".to_string();
        if let Some(session) = app.sftp_sessions.get_mut(&1) {
            session.entries = vec![crate::remote::sftp::SftpEntry {
                name: "current".to_string(),
                path: remote_path.clone(),
                kind: crate::remote::sftp::SftpEntryKind::Symlink,
                size: None,
                mtime: None,
                permissions: None,
            }];
            session.selected_paths.insert(remote_path);
        }

        assert!(!app.can_delete_sftp_selection(1));
        app.request_delete_sftp_entry(1);

        assert!(app.sftp_dialog.is_none());
        assert!(
            app.placeholder_notice
                .contains("仅支持删除普通文件或空目录")
        );
    }

    /// 验证 SFTP 忙碌状态下不会继续启用文件操作按钮。
    #[test]
    fn sftp_file_actions_are_disabled_while_busy() {
        let mut app = test_app();
        let link_id = add_test_ssh_link(&mut app);
        let _receiver = insert_test_sftp_session(&mut app, 1, link_id);
        let remote_path = "/home/tester/app.log".to_string();
        if let Some(session) = app.sftp_sessions.get_mut(&1) {
            session.entries = vec![crate::remote::sftp::SftpEntry {
                name: "app.log".to_string(),
                path: remote_path.clone(),
                kind: crate::remote::sftp::SftpEntryKind::RegularFile,
                size: Some(128),
                mtime: None,
                permissions: Some(0o100644),
            }];
            session.selected_paths.insert(remote_path);
            session.status = crate::remote::sftp::SftpStatus::Transferring;
        }

        assert!(!app.can_download_sftp_selection(1));
        assert!(!app.can_rename_sftp_selection(1));
        assert!(!app.can_delete_sftp_selection(1));
    }

    /// 验证单文件探测未完成的压缩包已被选中时，也能立即打开 Jstack 分析右键菜单。
    #[test]
    fn source_tree_context_menu_shows_jstack_action_for_pending_archive_probe() {
        let mut app = test_app();
        let mut registry = SourceRegistry::new();
        let archive_id = registry.allocate_id();
        registry.insert_node(SourceTreeNode {
            id: archive_id,
            parent_id: None,
            depth: 0,
            label: "thread.zip".to_string(),
            kind: SourceKind::Archive(crate::loader::archive::ArchiveFormat::Zip),
            location: SourceLocation::LocalPath(PathBuf::from("thread.zip")),
            metadata: SourceMetadata {
                size: Some(1024),
                children_loaded: false,
                is_loading: true,
                message: None,
            },
            selected: false,
            expanded: false,
        });
        registry.rebuild_all_indices();
        app.source_registry = registry;
        app.selected_search_source_ids.insert(archive_id);

        app.open_source_tree_context_menu(archive_id, gpui::point(gpui::px(1.0), gpui::px(1.0)));

        assert!(matches!(
            app.active_menu.as_ref().map(|menu| &menu.kind),
            Some(ActiveMenuKind::SourceTree { source_id }) if *source_id == archive_id
        ));
        assert!(matches!(
            app.active_menu_entries()[0].action,
            MenuAction::OpenJstackAnalysis { source_id } if source_id == archive_id
        ));
    }

    /// 验证压缩包内目录也能显示 Jstack 与 Runtime 分析入口。
    #[test]
    fn source_tree_context_menu_shows_analysis_actions_for_archive_directory() {
        let (mut app, archive_dir_id, _, _) = app_with_loaded_archive_directory();

        app.open_source_tree_context_menu(
            archive_dir_id,
            gpui::point(gpui::px(1.0), gpui::px(1.0)),
        );

        assert!(matches!(
            app.active_menu.as_ref().map(|menu| &menu.kind),
            Some(ActiveMenuKind::SourceTree { source_id }) if *source_id == archive_dir_id
        ));
        assert_eq!(app.active_menu_entries().len(), 2);
        assert!(matches!(
            app.active_menu_entries()[0].action,
            MenuAction::OpenJstackAnalysis { source_id } if source_id == archive_dir_id
        ));
        assert!(matches!(
            app.active_menu_entries()[1].action,
            MenuAction::OpenRuntimeAnalysis { source_id } if source_id == archive_dir_id
        ));
    }

    /// 验证右键未选中文件时会把分析输入切换为该文件。
    #[test]
    fn jstack_context_selection_switches_to_right_clicked_file() {
        let mut app = app_with_placeholder_sources();
        let app_log_id = source_id_by_label(&app, "app.log");
        let error_log_id = source_id_by_label(&app, "error.log");
        app.selected_search_source_ids.insert(app_log_id);

        let source_ids = app.jstack_source_ids_for_context(error_log_id);

        assert_eq!(source_ids, vec![error_log_id]);
        assert_eq!(
            app.selected_search_source_ids,
            BTreeSet::from([error_log_id])
        );
        assert_eq!(app.last_source_selection_anchor, Some(error_log_id));
    }

    /// 验证本地目录右键触发 Jstack 分析时会把目录作为独立目标交给后台递归展开。
    #[test]
    fn jstack_context_accepts_local_directory_target() {
        let mut app = app_with_placeholder_sources();
        let logs_dir_id = source_id_by_label(&app, "logs");

        let source_ids = app.jstack_source_ids_for_context(logs_dir_id);
        let targets = app.jstack_targets_from_source_ids(&source_ids);

        assert_eq!(source_ids, vec![logs_dir_id]);
        assert_eq!(targets.len(), 1);
        assert!(matches!(targets[0].location, SourceLocation::LocalPath(_)));
        assert_eq!(targets[0].label, "logs");
    }

    /// 验证 Jstack 右键压缩包内目录时，会按来源树顺序收集已加载的后代日志文件。
    #[test]
    fn jstack_context_archive_directory_collects_loaded_descendants() {
        let (mut app, archive_dir_id, first_log_id, second_log_id) =
            app_with_loaded_archive_directory();

        let source_ids = app.jstack_source_ids_for_context(archive_dir_id);
        let targets = app.jstack_targets_from_source_ids(&source_ids);

        assert_eq!(source_ids, vec![first_log_id, second_log_id]);
        assert_eq!(targets.len(), 2);
        assert!(
            targets
                .iter()
                .all(|target| matches!(target.location, SourceLocation::ArchiveEntry { .. }))
        );
    }

    /// 验证右键已在多选集合中时，会按来源树可见顺序保留多选输入。
    #[test]
    fn jstack_context_selection_keeps_multi_selection_in_tree_order() {
        let mut app = app_with_placeholder_sources();
        let app_log_id = source_id_by_label(&app, "app.log");
        let error_log_id = source_id_by_label(&app, "error.log");
        let nested_log_id = source_id_by_label(&app, "nested.log");
        app.selected_search_source_ids = BTreeSet::from([nested_log_id, error_log_id, app_log_id]);

        let source_ids = app.jstack_source_ids_for_context(error_log_id);

        assert_eq!(source_ids, vec![app_log_id, error_log_id, nested_log_id]);
    }

    /// 验证创建 Jstack 分析 tab 会复用空 tab 并写入加载状态。
    #[test]
    fn creating_jstack_analysis_tab_reuses_empty_tab() {
        let mut app = app_with_placeholder_sources();
        let app_log_id = source_id_by_label(&app, "app.log");
        let error_log_id = source_id_by_label(&app, "error.log");
        let targets = app.jstack_targets_from_source_ids(&[app_log_id, error_log_id]);

        let (analysis_id, generation) = app
            .create_jstack_analysis_tab_state(targets)
            .expect("应能创建 Jstack 分析 tab");

        assert_eq!(generation, 1);
        assert_eq!(app.tabs.len(), 1);
        assert!(matches!(
            app.active_tab_kind(),
            TabKind::JstackAnalysis { analysis_id: active_id } if active_id == analysis_id
        ));
        let state = app
            .jstack_analysis_state(analysis_id)
            .expect("应保存分析状态");
        assert_eq!(state.targets.len(), 2);
        assert_eq!(
            state.active_states,
            BTreeSet::from([JstackThreadState::Runnable])
        );
        assert!(state.is_thread_filter_enabled);
        assert!(matches!(
            state.task_state,
            JstackAnalysisTaskState::Loading { .. }
        ));
        assert_eq!(app.active_tab_title(), "Jstack分析(2)");
    }

    /// 验证 Jstack 配置过滤开关默认开启，并可在分析页内临时关闭。
    #[test]
    fn toggling_jstack_thread_filter_updates_analysis_state() {
        let mut app = app_with_placeholder_sources();
        let app_log_id = source_id_by_label(&app, "app.log");
        let targets = app.jstack_targets_from_source_ids(&[app_log_id]);
        let (analysis_id, _) = app
            .create_jstack_analysis_tab_state(targets)
            .expect("应能创建 Jstack 分析 tab");

        assert!(
            app.jstack_analysis_state(analysis_id)
                .expect("应保存分析状态")
                .is_thread_filter_enabled
        );

        app.toggle_jstack_thread_filter(analysis_id);

        assert!(
            !app.jstack_analysis_state(analysis_id)
                .expect("应保存分析状态")
                .is_thread_filter_enabled
        );
        assert_eq!(app.placeholder_notice, "已关闭 Jstack 配置过滤");
    }

    /// 验证 Runtime 右键已在多选集合中时，会按来源树可见顺序保留多选输入。
    #[test]
    fn runtime_context_selection_keeps_multi_selection_in_tree_order() {
        let mut app = app_with_placeholder_sources();
        let app_log_id = source_id_by_label(&app, "app.log");
        let error_log_id = source_id_by_label(&app, "error.log");
        let nested_log_id = source_id_by_label(&app, "nested.log");
        app.selected_search_source_ids = BTreeSet::from([nested_log_id, error_log_id, app_log_id]);

        let targets = app.runtime_targets_for_context(error_log_id);

        assert_eq!(targets.len(), 3);
        assert_eq!(targets[0].source_id, app_log_id);
        assert_eq!(targets[1].source_id, error_log_id);
        assert_eq!(targets[2].source_id, nested_log_id);
        assert!(
            targets
                .iter()
                .all(|target| target.kind == RuntimeAnalysisTargetKind::File)
        );
    }

    /// 验证 Runtime 右键本地目录会生成目录目标，由后台递归展开。
    #[test]
    fn runtime_context_accepts_local_directory_target() {
        let mut app = app_with_placeholder_sources();
        let logs_dir_id = source_id_by_label(&app, "logs");

        let targets = app.runtime_targets_for_context(logs_dir_id);

        assert_eq!(targets.len(), 1);
        assert_eq!(targets[0].source_id, logs_dir_id);
        assert_eq!(targets[0].kind, RuntimeAnalysisTargetKind::Directory);
    }

    /// 验证 Runtime 右键压缩包内目录时，会把已加载的后代日志条目作为文件目标解析。
    #[test]
    fn runtime_context_archive_directory_collects_loaded_descendant_files() {
        let (mut app, archive_dir_id, first_log_id, second_log_id) =
            app_with_loaded_archive_directory();

        let targets = app.runtime_targets_for_context(archive_dir_id);

        assert_eq!(targets.len(), 2);
        assert_eq!(targets[0].source_id, first_log_id);
        assert_eq!(targets[1].source_id, second_log_id);
        assert!(
            targets
                .iter()
                .all(|target| target.kind == RuntimeAnalysisTargetKind::File)
        );
    }

    /// 验证创建 Runtime 分析 tab 会复用空 tab 并写入加载状态。
    #[test]
    fn creating_runtime_analysis_tab_reuses_empty_tab() {
        let mut app = app_with_placeholder_sources();
        let app_log_id = source_id_by_label(&app, "app.log");
        let error_log_id = source_id_by_label(&app, "error.log");
        let targets = app.runtime_targets_from_source_ids(&[app_log_id, error_log_id]);

        let (analysis_id, generation) = app
            .create_runtime_analysis_tab_state(targets)
            .expect("应能创建 Runtime 分析 tab");

        assert_eq!(generation, 1);
        assert_eq!(app.tabs.len(), 1);
        assert!(matches!(
            app.active_tab_kind(),
            TabKind::RuntimeAnalysis { analysis_id: active_id } if active_id == analysis_id
        ));
        let state = app
            .runtime_analysis_state(analysis_id)
            .expect("应保存 Runtime 分析状态");
        assert_eq!(state.targets.len(), 2);
        assert_eq!(state.result_type, RuntimeAnalysisResultType::Statistics);
        assert_eq!(state.summary_sort_key, RuntimeSummarySortKey::RequestCount);
        assert_eq!(
            state.summary_sort_direction,
            RuntimeSortDirection::Descending
        );
        assert!(matches!(
            state.task_state,
            RuntimeAnalysisTaskState::Loading { .. }
        ));
        assert_eq!(app.active_tab_title(), "Runtime分析(2)");
    }

    /// 验证切换 Runtime 结果类型会清理旧表格交互态，统计下钻会回到统计分析。
    #[test]
    fn switching_runtime_result_type_clears_transient_state() {
        let mut app = app_with_placeholder_sources();
        let app_log_id = source_id_by_label(&app, "app.log");
        let targets = app.runtime_targets_from_source_ids(&[app_log_id]);
        let (analysis_id, _) = app
            .create_runtime_analysis_tab_state(targets)
            .expect("应能创建 Runtime 分析 tab");
        {
            let state = app
                .runtime_analysis_state_mut(analysis_id)
                .expect("应存在 Runtime 分析状态");
            state.cell_selection = Some(RuntimeTableCellSelection {
                cell_key: "summary:0:path".to_string(),
                text: "/api/test".to_string(),
                anchor: 0,
                focus: 4,
            });
            state.cell_selection_drag = Some(RuntimeTableCellSelectionDrag {
                cell_key: "summary:0:path".to_string(),
                text: "/api/test".to_string(),
                anchor_range: 0..4,
                granularity: TextSelectionGranularity::Character,
            });
            state.hovered_sql_cell = Some(RuntimeSqlCellKey::Record {
                request_index: 0,
                sql_index: 0,
            });
            state.sql_text_dialog = Some(RuntimeSqlTextDialog {
                request_path: "/api/test".to_string(),
                request_time_label: "2026-06-25 14:25:03".to_string(),
                username: "tester".to_string(),
                sql_text: "select 1".to_string(),
                selection: None,
                selection_drag: None,
            });
        }

        app.set_runtime_result_type(analysis_id, RuntimeAnalysisResultType::SqlFrequency, None);

        let state = app
            .runtime_analysis_state(analysis_id)
            .expect("应存在 Runtime 分析状态");
        assert_eq!(state.result_type, RuntimeAnalysisResultType::SqlFrequency);
        assert!(state.cell_selection.is_none());
        assert!(state.cell_selection_drag.is_none());
        assert!(state.hovered_sql_cell.is_none());
        assert!(state.sql_text_dialog.is_none());

        app.open_runtime_request_details(analysis_id, "/api/test".to_string());

        let state = app
            .runtime_analysis_state(analysis_id)
            .expect("应存在 Runtime 分析状态");
        assert_eq!(state.result_type, RuntimeAnalysisResultType::Statistics);
        assert!(matches!(
            state.view,
            RuntimeAnalysisView::RequestDetails { .. }
        ));
    }

    /// 验证切回统计分析不会清空 SQL 频率和慢 SQL 的懒计算缓存。
    #[test]
    fn switching_runtime_statistics_preserves_sql_analysis_caches() {
        let mut app = app_with_placeholder_sources();
        let app_log_id = source_id_by_label(&app, "app.log");
        let targets = app.runtime_targets_from_source_ids(&[app_log_id]);
        let (analysis_id, _) = app
            .create_runtime_analysis_tab_state(targets)
            .expect("应能创建 Runtime 分析 tab");
        let filter = RuntimeSqlAnalysisFilterSnapshot::default();
        {
            let state = app
                .runtime_analysis_state_mut(analysis_id)
                .expect("应存在 Runtime 分析状态");
            state.result_type = RuntimeAnalysisResultType::SqlFrequency;
            state
                .sql_frequency_rows_cache
                .borrow_mut()
                .replace(RuntimeSqlFrequencyRowsCache {
                    filter: filter.clone(),
                    rows: Arc::new(vec![RuntimeSqlFrequencyAnalysisRow {
                        normalized_sql: "select ?".to_string(),
                        total_execute_ms: 12,
                        execute_count: 1,
                    }]),
                });
            state
                .slow_sql_rows_cache
                .borrow_mut()
                .replace(RuntimeSlowSqlRowsCache {
                    filter,
                    rows: Arc::new(vec![RuntimeSlowSqlSummaryRow {
                        normalized_sql: "select ?".to_string(),
                        total_execute_ms: 12,
                        execute_count: 1,
                    }]),
                });
        }

        app.set_runtime_result_type(analysis_id, RuntimeAnalysisResultType::Statistics, None);
        app.set_runtime_result_type(analysis_id, RuntimeAnalysisResultType::SqlFrequency, None);

        let state = app
            .runtime_analysis_state(analysis_id)
            .expect("应存在 Runtime 分析状态");
        assert!(state.sql_frequency_rows_cache.borrow().is_some());
        assert!(state.slow_sql_rows_cache.borrow().is_some());
    }

    /// 验证 SQL 频率详情动作会进入详情页，并可返回频率列表。
    #[test]
    fn runtime_sql_frequency_detail_open_and_back_updates_state() {
        let mut app = app_with_placeholder_sources();
        let app_log_id = source_id_by_label(&app, "app.log");
        let targets = app.runtime_targets_from_source_ids(&[app_log_id]);
        let (analysis_id, _) = app
            .create_runtime_analysis_tab_state(targets)
            .expect("应能创建 Runtime 分析 tab");

        app.open_runtime_sql_frequency_detail(
            analysis_id,
            "select * from users where id = ?".to_string(),
        );

        let state = app
            .runtime_analysis_state(analysis_id)
            .expect("应存在 Runtime 分析状态");
        assert_eq!(state.result_type, RuntimeAnalysisResultType::SqlFrequency);
        assert_eq!(
            state.sql_frequency_detail_sql.as_deref(),
            Some("select * from users where id = ?")
        );

        app.show_runtime_sql_frequency_summary(analysis_id);

        let state = app
            .runtime_analysis_state(analysis_id)
            .expect("应存在 Runtime 分析状态");
        assert_eq!(state.result_type, RuntimeAnalysisResultType::SqlFrequency);
        assert!(state.sql_frequency_detail_sql.is_none());
    }

    /// 验证 Runtime 时间选择器点选日期时保留原时分秒，并保持浮层打开以便继续调时间。
    #[test]
    fn runtime_time_picker_date_selection_preserves_time() {
        let mut app = app_with_placeholder_sources();
        let app_log_id = source_id_by_label(&app, "app.log");
        let targets = app.runtime_targets_from_source_ids(&[app_log_id]);
        let (analysis_id, _) = app
            .create_runtime_analysis_tab_state(targets)
            .expect("应能创建 Runtime 分析 tab");
        app.runtime_analysis_state_mut(analysis_id)
            .expect("应存在 Runtime 分析状态")
            .filter_start_time_input
            .value = "2026-06-25 14:25:03".to_string();

        app.open_runtime_time_picker(analysis_id, RuntimeFilterInputKind::StartTime);
        app.set_runtime_filter_date(
            analysis_id,
            RuntimeFilterInputKind::StartTime,
            2026,
            7,
            2,
            None,
        );

        let state = app
            .runtime_analysis_state(analysis_id)
            .expect("应存在 Runtime 分析状态");
        assert_eq!(state.filter_start_time_input.value, "2026-07-02 14:25:03");
        assert_eq!(
            state.open_time_picker,
            Some(RuntimeFilterInputKind::StartTime)
        );
    }

    /// 验证 Runtime 时间选择器可以通过页面主体点击对应的状态方法关闭。
    #[test]
    fn closing_runtime_time_picker_clears_open_panel() {
        let mut app = app_with_placeholder_sources();
        let app_log_id = source_id_by_label(&app, "app.log");
        let targets = app.runtime_targets_from_source_ids(&[app_log_id]);
        let (analysis_id, _) = app
            .create_runtime_analysis_tab_state(targets)
            .expect("应能创建 Runtime 分析 tab");

        app.open_runtime_time_picker(analysis_id, RuntimeFilterInputKind::EndTime);

        assert!(app.close_runtime_time_picker(analysis_id));
        assert_eq!(
            app.runtime_analysis_state(analysis_id)
                .expect("应存在 Runtime 分析状态")
                .open_time_picker,
            None
        );
    }

    /// 验证 Runtime SQL 完整文本弹窗可以正常打开和关闭。
    #[test]
    fn runtime_sql_text_dialog_opens_and_closes() {
        let mut app = app_with_placeholder_sources();
        let app_log_id = source_id_by_label(&app, "app.log");
        let targets = app.runtime_targets_from_source_ids(&[app_log_id]);
        let (analysis_id, _) = app
            .create_runtime_analysis_tab_state(targets)
            .expect("应能创建 Runtime 分析 tab");

        app.open_runtime_sql_text_dialog(
            analysis_id,
            RuntimeSqlTextDialog {
                request_path: "/api/test".to_string(),
                request_time_label: "2026-06-25 14:25:03".to_string(),
                username: "tester".to_string(),
                sql_text: "select *\nfrom test_table".to_string(),
                selection: None,
                selection_drag: None,
            },
        );

        assert_eq!(
            app.runtime_analysis_state(analysis_id)
                .expect("应存在 Runtime 分析状态")
                .sql_text_dialog
                .as_ref()
                .map(|dialog| dialog.sql_text.as_str()),
            Some("select *\nfrom test_table")
        );
        assert!(app.close_runtime_sql_text_dialog(analysis_id));
        assert!(
            app.runtime_analysis_state(analysis_id)
                .expect("应存在 Runtime 分析状态")
                .sql_text_dialog
                .is_none()
        );
    }

    /// 验证 Runtime SQL 弹窗正文选区跨行提取时保留换行和缩进。
    #[test]
    fn runtime_sql_text_selection_extracts_multiline_text() {
        let lines = runtime_sql_text_lines("select *\n  from test_table\nwhere id = 1");
        let selection = RuntimeSqlTextSelection {
            anchor: RuntimeSqlTextPosition {
                line_index: 0,
                column: 7,
            },
            focus: RuntimeSqlTextPosition {
                line_index: 2,
                column: 5,
            },
        };

        let selected = selected_runtime_sql_text_from_lines(&lines, &selection)
            .expect("应能提取跨行 SQL 选区");

        assert_eq!(selected, "*\n  from test_table\nwhere");
    }

    /// 验证点击 SQL 弹窗其他位置时只清理正文选区，不关闭弹窗。
    #[test]
    fn clearing_runtime_sql_text_selection_keeps_dialog_open() {
        let mut app = app_with_placeholder_sources();
        let app_log_id = source_id_by_label(&app, "app.log");
        let targets = app.runtime_targets_from_source_ids(&[app_log_id]);
        let (analysis_id, _) = app
            .create_runtime_analysis_tab_state(targets)
            .expect("应能创建 Runtime 分析 tab");

        app.open_runtime_sql_text_dialog(
            analysis_id,
            RuntimeSqlTextDialog {
                request_path: "/api/test".to_string(),
                request_time_label: "2026-06-25 14:25:03".to_string(),
                username: "tester".to_string(),
                sql_text: "select *\nfrom test_table".to_string(),
                selection: None,
                selection_drag: None,
            },
        );
        app.begin_runtime_sql_text_selection(
            analysis_id,
            0,
            "select *".to_string(),
            0,
            TextSelectionGranularity::Character,
        );
        assert!(app.update_runtime_sql_text_selection(analysis_id, 0, "select *".to_string(), 6));
        assert!(app.finish_runtime_sql_text_selection(analysis_id));

        assert!(app.clear_runtime_sql_text_selection(analysis_id));
        let dialog = app
            .runtime_analysis_state(analysis_id)
            .expect("应存在 Runtime 分析状态")
            .sql_text_dialog
            .as_ref()
            .expect("清理选区不应关闭 SQL 弹窗");
        assert!(dialog.selection.is_none());
        assert!(dialog.selection_drag.is_none());
    }

    /// 验证 Runtime 表格单元格拖拽只保留用户选择的局部文本范围。
    #[test]
    fn runtime_cell_selection_keeps_character_range() {
        let mut app = app_with_placeholder_sources();
        let app_log_id = source_id_by_label(&app, "app.log");
        let targets = app.runtime_targets_from_source_ids(&[app_log_id]);
        let (analysis_id, _) = app
            .create_runtime_analysis_tab_state(targets)
            .expect("应能创建 Runtime 分析 tab");

        app.begin_runtime_cell_selection(
            analysis_id,
            "summary:0:path".to_string(),
            "/api/runtime/example".to_string(),
            5,
            TextSelectionGranularity::Character,
        );
        assert!(app.update_runtime_cell_selection(analysis_id, "summary:0:path", 12));
        assert!(app.finish_runtime_cell_selection(analysis_id));

        let selection = app
            .runtime_analysis_state(analysis_id)
            .and_then(|state| state.cell_selection.as_ref())
            .expect("应存在 Runtime 单元格选区");
        let range = selection.normalized_range().expect("应存在非空选区");
        assert_eq!(slice_character_range(&selection.text, range), "runtime");
    }

    /// 验证 Runtime 表格单元格双击会选中整个单元格内容。
    #[test]
    fn runtime_cell_double_click_selects_whole_cell() {
        let mut app = app_with_placeholder_sources();
        let app_log_id = source_id_by_label(&app, "app.log");
        let targets = app.runtime_targets_from_source_ids(&[app_log_id]);
        let (analysis_id, _) = app
            .create_runtime_analysis_tab_state(targets)
            .expect("应能创建 Runtime 分析 tab");

        app.begin_runtime_cell_selection(
            analysis_id,
            "request:1:username".to_string(),
            "youyj".to_string(),
            2,
            TextSelectionGranularity::Line,
        );
        assert!(app.finish_runtime_cell_selection(analysis_id));

        let selection = app
            .runtime_analysis_state(analysis_id)
            .and_then(|state| state.cell_selection.as_ref())
            .expect("应存在 Runtime 单元格选区");
        let range = selection.normalized_range().expect("应存在非空选区");
        assert_eq!(slice_character_range(&selection.text, range), "youyj");
    }

    /// 验证点击 Runtime 单元格以外区域时可以清理已有单元格选区。
    #[test]
    fn clearing_runtime_cell_selection_removes_active_selection() {
        let mut app = app_with_placeholder_sources();
        let app_log_id = source_id_by_label(&app, "app.log");
        let targets = app.runtime_targets_from_source_ids(&[app_log_id]);
        let (analysis_id, _) = app
            .create_runtime_analysis_tab_state(targets)
            .expect("应能创建 Runtime 分析 tab");

        app.begin_runtime_cell_selection(
            analysis_id,
            "summary:0:path".to_string(),
            "/api/runtime/example".to_string(),
            0,
            TextSelectionGranularity::Line,
        );
        assert!(app.finish_runtime_cell_selection(analysis_id));

        assert!(app.clear_runtime_cell_selection());
        assert!(
            app.runtime_analysis_state(analysis_id)
                .and_then(|state| state.cell_selection.as_ref())
                .is_none()
        );
        assert!(!app.clear_runtime_cell_selection());
    }

    /// 验证关闭 Runtime 分析 tab 会清理对应分析状态。
    #[test]
    fn closing_runtime_analysis_tab_clears_analysis_state() {
        let mut app = app_with_placeholder_sources();
        let app_log_id = source_id_by_label(&app, "app.log");
        let targets = app.runtime_targets_from_source_ids(&[app_log_id]);
        let (analysis_id, _) = app
            .create_runtime_analysis_tab_state(targets)
            .expect("应能创建 Runtime 分析 tab");
        let tab_id = app.active_tab_id;

        app.close_tab(tab_id);

        assert!(app.runtime_analysis_state(analysis_id).is_none());
        assert!(matches!(app.active_tab_kind(), TabKind::Empty));
    }

    /// 验证 Jstack 线程名复制入口只记录用户拖选的局部文本范围。
    #[test]
    fn jstack_thread_name_selection_keeps_character_range() {
        let mut app = app_with_placeholder_sources();
        let app_log_id = source_id_by_label(&app, "app.log");
        let targets = app.jstack_targets_from_source_ids(&[app_log_id]);
        let (analysis_id, _) = app
            .create_jstack_analysis_tab_state(targets)
            .expect("应能创建 Jstack 分析 tab");

        app.begin_jstack_thread_name_selection(
            analysis_id,
            "worker-1#123".to_string(),
            "worker-1".to_string(),
            0,
            TextSelectionGranularity::Character,
        );
        assert!(app.update_jstack_thread_name_selection(analysis_id, "worker-1#123", 4));
        assert!(app.finish_jstack_thread_name_selection(analysis_id));

        let selection = app
            .jstack_analysis_state(analysis_id)
            .and_then(|state| state.thread_name_selection.as_ref())
            .expect("应保留非空线程名选区");
        let range = selection.normalized_range().expect("应存在非空选区");
        assert_eq!(slice_character_range(&selection.thread_name, range), "work");
    }

    /// 验证 Jstack 状态筛选开关可以增删状态并重置滚动句柄。
    #[test]
    fn toggling_jstack_state_filter_updates_active_states() {
        let mut app = app_with_placeholder_sources();
        let app_log_id = source_id_by_label(&app, "app.log");
        let targets = app.jstack_targets_from_source_ids(&[app_log_id]);
        let (analysis_id, _) = app
            .create_jstack_analysis_tab_state(targets)
            .expect("应能创建 Jstack 分析 tab");

        app.toggle_jstack_state_filter(analysis_id, JstackThreadState::Blocked);

        let state = app
            .jstack_analysis_state(analysis_id)
            .expect("应保存分析状态");
        assert!(state.active_states.contains(&JstackThreadState::Runnable));
        assert!(state.active_states.contains(&JstackThreadState::Blocked));

        app.toggle_jstack_state_filter(analysis_id, JstackThreadState::Runnable);

        let state = app
            .jstack_analysis_state(analysis_id)
            .expect("应保存分析状态");
        assert!(!state.active_states.contains(&JstackThreadState::Runnable));
        assert!(state.active_states.contains(&JstackThreadState::Blocked));
    }

    /// 验证 Jstack 可见行按当前状态筛选后的命中数量排序，而不是按隐藏状态参与的总频率排序。
    #[test]
    fn visible_jstack_rows_sort_by_filtered_hit_count() {
        let first = crate::analysis::jstack::parse_jstack_snapshot(
            SourceId(1),
            "001.log",
            "/tmp/001.log",
            r#""mostly-hidden" #1
   java.lang.Thread.State: RUNNABLE
"alpha-runnable" #2
   java.lang.Thread.State: RUNNABLE
"always-runnable" #3
   java.lang.Thread.State: RUNNABLE
"#,
        );
        let second = crate::analysis::jstack::parse_jstack_snapshot(
            SourceId(2),
            "002.log",
            "/tmp/002.log",
            r#""mostly-hidden" #1
   java.lang.Thread.State: WAITING (parking)
"mostly-hidden" #1
   java.lang.Thread.State: WAITING (parking)
"mostly-hidden" #1
   java.lang.Thread.State: WAITING (parking)
"mostly-hidden" #1
   java.lang.Thread.State: WAITING (parking)
"alpha-runnable" #2
   java.lang.Thread.State: RUNNABLE
"always-runnable" #3
   java.lang.Thread.State: RUNNABLE
"#,
        );
        let result =
            crate::analysis::jstack::build_analysis_result(vec![first, second], Vec::new(), 2);
        let active_states = BTreeSet::from([JstackThreadState::Runnable]);

        let row_names = visible_jstack_row_indices(&result, &active_states, None)
            .into_iter()
            .map(|index| result.rows[index].thread_name.as_str())
            .collect::<Vec<_>>();

        assert_eq!(
            row_names,
            vec!["alpha-runnable", "always-runnable", "mostly-hidden"]
        );
    }

    /// 验证线程详情按可见快照收集代表堆栈，不把同一文件内重复出现展开成多条同源记录。
    #[test]
    fn jstack_detail_occurrences_keep_one_stack_per_visible_snapshot() {
        let first = crate::analysis::jstack::parse_jstack_snapshot(
            SourceId(1),
            "001.log",
            "/tmp/001.log",
            r#""same-thread" #7 tid=0x7
   java.lang.Thread.State: RUNNABLE
        at app.First.one(First.java:1)
"same-thread" #7 tid=0x7
   java.lang.Thread.State: RUNNABLE
        at app.First.two(First.java:2)
"#,
        );
        let second = crate::analysis::jstack::parse_jstack_snapshot(
            SourceId(2),
            "002.log",
            "/tmp/002.log",
            r#""same-thread" #7 tid=0x7
   java.lang.Thread.State: RUNNABLE
        at app.Second.one(Second.java:1)
"#,
        );
        let result =
            crate::analysis::jstack::build_analysis_result(vec![first, second], Vec::new(), 2);
        let row = result
            .rows
            .iter()
            .find(|row| row.thread_name == "same-thread")
            .expect("应存在同一线程行");
        let active_states = BTreeSet::from([JstackThreadState::Runnable]);

        let occurrences = jstack_detail_occurrences_for_visible_cells(row, &active_states, 0, 2);

        assert_eq!(occurrences.len(), 2);
        assert_eq!(occurrences[0].snapshot_label, "001.log");
        assert_eq!(occurrences[0].occurrence_index, 2);
        assert_eq!(occurrences[1].snapshot_label, "002.log");
        assert_eq!(occurrences[1].occurrence_index, 1);
    }

    /// 验证单文件探测未完成的压缩包不会打断来源树 Shift 范围多选。
    #[test]
    fn shift_range_selection_includes_pending_single_file_archive_probe() {
        let mut app = test_app();
        let mut registry = SourceRegistry::new();
        let root_id = registry.allocate_id();
        registry.insert_node(SourceTreeNode {
            id: root_id,
            parent_id: None,
            depth: 0,
            label: "logs".to_string(),
            kind: SourceKind::Directory,
            location: SourceLocation::LocalPath(PathBuf::from("logs")),
            metadata: SourceMetadata {
                size: None,
                children_loaded: true,
                is_loading: false,
                message: None,
            },
            selected: false,
            expanded: true,
        });
        let first_id = registry.allocate_id();
        registry.insert_node(SourceTreeNode {
            id: first_id,
            parent_id: Some(root_id),
            depth: 1,
            label: "001.log".to_string(),
            kind: SourceKind::LogFile,
            location: SourceLocation::LocalPath(PathBuf::from("logs/001.log")),
            metadata: SourceMetadata {
                size: Some(10),
                children_loaded: true,
                is_loading: false,
                message: None,
            },
            selected: false,
            expanded: false,
        });
        let pending_archive_id = registry.allocate_id();
        registry.insert_node(SourceTreeNode {
            id: pending_archive_id,
            parent_id: Some(root_id),
            depth: 1,
            label: "002.zip".to_string(),
            kind: SourceKind::Archive(crate::loader::archive::ArchiveFormat::Zip),
            location: SourceLocation::LocalPath(PathBuf::from("logs/002.zip")),
            metadata: SourceMetadata {
                size: Some(20),
                children_loaded: false,
                is_loading: false,
                message: None,
            },
            selected: false,
            expanded: false,
        });
        let last_id = registry.allocate_id();
        registry.insert_node(SourceTreeNode {
            id: last_id,
            parent_id: Some(root_id),
            depth: 1,
            label: "003.log".to_string(),
            kind: SourceKind::LogFile,
            location: SourceLocation::LocalPath(PathBuf::from("logs/003.log")),
            metadata: SourceMetadata {
                size: Some(30),
                children_loaded: true,
                is_loading: false,
                message: None,
            },
            selected: false,
            expanded: false,
        });
        registry.rebuild_all_indices();
        app.source_registry = registry;
        app.selected_search_source_ids.insert(first_id);
        app.last_source_selection_anchor = Some(first_id);

        app.select_source_tree_range_for_search(last_id);

        assert_eq!(
            app.selected_search_source_ids,
            BTreeSet::from([first_id, pending_archive_id, last_id])
        );
    }

    /// 验证来源树过滤态下，未完成单文件探测的压缩包仍参与 Shift 范围多选。
    #[test]
    fn source_tree_filter_keeps_pending_archive_for_shift_range_selection() {
        let mut app = test_app();
        let mut registry = SourceRegistry::new();
        let root_id = registry.allocate_id();
        registry.insert_node(SourceTreeNode {
            id: root_id,
            parent_id: None,
            depth: 0,
            label: "logs".to_string(),
            kind: SourceKind::Directory,
            location: SourceLocation::LocalPath(PathBuf::from("logs")),
            metadata: SourceMetadata {
                children_loaded: true,
                ..SourceMetadata::default()
            },
            selected: false,
            expanded: true,
        });

        let source_specs = [
            ("thread001.log", SourceKind::LogFile, true),
            (
                "thread002.zip",
                SourceKind::Archive(crate::loader::archive::ArchiveFormat::Zip),
                false,
            ),
            ("thread003.log", SourceKind::LogFile, true),
        ];
        let mut ids = Vec::new();
        for (label, kind, children_loaded) in source_specs {
            let source_id = registry.allocate_id();
            registry.insert_node(SourceTreeNode {
                id: source_id,
                parent_id: Some(root_id),
                depth: 1,
                label: label.to_string(),
                kind,
                location: SourceLocation::LocalPath(PathBuf::from(format!("logs/{label}"))),
                metadata: SourceMetadata {
                    size: Some(10),
                    children_loaded,
                    is_loading: !children_loaded,
                    message: None,
                },
                selected: false,
                expanded: false,
            });
            ids.push(source_id);
        }
        registry.rebuild_all_indices();
        app.source_registry = registry;

        app.open_source_tree_search();
        app.update_source_tree_search_query("thread".to_string());
        app.selected_search_source_ids.insert(ids[0]);
        app.last_source_selection_anchor = Some(ids[0]);

        app.select_source_tree_range_for_search(ids[2]);

        assert_eq!(app.selected_search_source_ids, BTreeSet::from_iter(ids));
    }

    /// 验证探测期间可见列表短暂缺少中间节点时，Shift 范围选择会用稳定树序补齐。
    #[test]
    fn shift_range_selection_fills_pending_archives_from_tree_order_during_probe() {
        let mut app = test_app();
        let mut registry = SourceRegistry::new();
        let root_id = registry.allocate_id();
        registry.insert_node(SourceTreeNode {
            id: root_id,
            parent_id: None,
            depth: 0,
            label: "logs".to_string(),
            kind: SourceKind::Directory,
            location: SourceLocation::LocalPath(PathBuf::from("logs")),
            metadata: SourceMetadata {
                children_loaded: true,
                ..SourceMetadata::default()
            },
            selected: false,
            expanded: true,
        });

        let mut source_ids = Vec::new();
        for index in 0..5 {
            let source_id = registry.allocate_id();
            registry.insert_node(SourceTreeNode {
                id: source_id,
                parent_id: Some(root_id),
                depth: 1,
                label: format!("thread{index:03}.zip"),
                kind: SourceKind::Archive(crate::loader::archive::ArchiveFormat::Zip),
                location: SourceLocation::LocalPath(PathBuf::from(format!(
                    "logs/thread{index:03}.zip"
                ))),
                metadata: SourceMetadata {
                    size: Some(1024),
                    children_loaded: false,
                    is_loading: true,
                    message: None,
                },
                selected: false,
                expanded: false,
            });
            source_ids.push(source_id);
        }
        registry.rebuild_all_indices();
        app.source_registry = registry;
        app.is_source_tree_search_open = true;
        app.source_tree_search_query = "thread".to_string();
        app.filtered_source_ids = vec![root_id, source_ids[0], source_ids[4]];
        app.source_archive_probe_queue
            .extend(source_ids.iter().copied());
        app.source_archive_probe_queued_ids
            .extend(source_ids.iter().copied());
        assert!(app.select_pending_archive_probe_for_search_anchor(source_ids[0]));

        app.select_source_tree_range_for_search(source_ids[4]);

        assert_eq!(
            app.selected_search_source_ids,
            BTreeSet::from_iter(source_ids)
        );
    }

    /// 验证关闭 Jstack 分析 tab 会清理对应分析状态。
    #[test]
    fn closing_jstack_analysis_tab_clears_analysis_state() {
        let mut app = app_with_placeholder_sources();
        let app_log_id = source_id_by_label(&app, "app.log");
        let targets = app.jstack_targets_from_source_ids(&[app_log_id]);
        let (analysis_id, _) = app
            .create_jstack_analysis_tab_state(targets)
            .expect("应能创建 Jstack 分析 tab");
        let tab_id = app.active_tab_id;

        app.close_tab(tab_id);

        assert!(app.jstack_analysis_state(analysis_id).is_none());
        assert!(matches!(app.active_tab_kind(), TabKind::Empty));
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

    /// 验证旧设置标签入口不再创建设置标签页。
    #[test]
    fn legacy_settings_tab_entry_does_not_create_tab() {
        let mut app = test_app();

        app.open_or_focus_settings_tab();

        assert_eq!(app.tabs.len(), 1);
        assert!(matches!(app.active_tab_kind(), TabKind::Empty));
        assert!(app.placeholder_notice.contains("独立窗口"));
    }

    /// 验证旧设置标签入口不会影响当前日志标签。
    #[test]
    fn legacy_settings_tab_entry_keeps_active_log_tab() {
        let mut app = app_with_placeholder_sources();
        let app_log_id = source_id_by_label(&app, "app.log");

        app.select_source(app_log_id);
        let app_tab_id = app.active_tab_id;
        app.open_or_focus_settings_tab();

        assert_eq!(app.active_tab_id, app_tab_id);
        assert_eq!(
            app.tabs
                .iter()
                .filter(|tab| matches!(tab.kind, TabKind::Settings))
                .count(),
            0
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

    /// 验证日志搜索快捷键只在日志正文拥有业务焦点时允许触发。
    #[test]
    fn log_search_shortcut_requires_log_text_focus() {
        let mut app = app_with_placeholder_sources();
        let app_log_id = source_id_by_label(&app, "app.log");

        assert!(!app.is_active_log_view_focused());

        app.select_source(app_log_id);
        assert!(!app.is_active_log_view_focused());

        app.focus_log_text_view(app.active_tab_id);
        assert!(app.is_active_log_view_focused());

        app.close_tab(app.active_tab_id);
        assert!(!app.is_active_log_view_focused());
    }

    /// 验证日志行号打点在同一行重复点击时会添加再移除。
    #[test]
    fn toggling_log_line_marker_adds_and_removes_line() {
        let mut app = test_app();
        let tab_id = app.active_tab_id;

        app.toggle_log_line_marker(tab_id, 9);
        assert!(
            app.log_tab_view_state(tab_id)
                .is_some_and(|state| state.line_markers.contains(&9))
        );
        assert!(app.placeholder_notice.contains("已添加第 10 行"));

        app.toggle_log_line_marker(tab_id, 9);
        assert!(
            app.log_tab_view_state(tab_id)
                .is_some_and(|state| state.line_markers.is_empty())
        );
        assert!(app.placeholder_notice.contains("已移除第 10 行"));
    }

    /// 验证手动切换打点会清除上一轮 F2 跳转缓存，下一次跳转应从当前视口重新计算。
    #[test]
    fn toggling_log_line_marker_clears_last_jump_cache() {
        let mut app = test_app();
        let tab_id = app.active_tab_id;
        app.toggle_log_line_marker(tab_id, 9);
        app.log_tab_view_state_mut(tab_id)
            .expect("测试应用应存在默认日志视图状态")
            .last_line_marker_jump = Some(9);

        app.toggle_log_line_marker(tab_id, 12);

        assert!(
            app.log_tab_view_state(tab_id)
                .is_some_and(|state| state.last_line_marker_jump.is_none())
        );
    }

    /// 验证关闭日志标签页时会释放该标签的打点状态。
    #[test]
    fn closing_tab_clears_line_markers_for_that_tab() {
        let mut app = app_with_placeholder_sources();
        let app_log_id = source_id_by_label(&app, "app.log");

        app.select_source(app_log_id);
        let tab_id = app.active_tab_id;
        app.toggle_log_line_marker(tab_id, 2);
        app.close_tab(tab_id);

        assert!(
            app.log_tab_view_state(tab_id)
                .is_some_and(|state| state.line_markers.is_empty())
        );
        assert!(matches!(app.active_tab_kind(), TabKind::Empty));
    }

    /// 验证激活日志标签页会展开来源树路径，并把非主动单选收束到当前文件。
    #[test]
    fn activating_log_tab_syncs_single_source_tree_selection() {
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

        assert_eq!(app.active_tab_id, app_tab_id);
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
                .unwrap_or(false)
        );
        assert!(
            app.source_registry
                .node(logs_id)
                .map(|source| source.expanded)
                .unwrap_or(false)
        );
        assert!(app.visible_source_ids().contains(&app_log_id));
        assert_eq!(app.selected_search_source_ids, BTreeSet::from([app_log_id]));
    }

    /// 验证激活日志标签页不会破坏用户主动多选的搜索文件范围。
    #[test]
    fn activating_log_tab_preserves_multi_source_search_selection() {
        let mut app = app_with_placeholder_sources();
        let app_log_id = source_id_by_label(&app, "app.log");
        let error_log_id = source_id_by_label(&app, "error.log");
        let nested_log_id = source_id_by_label(&app, "nested.log");

        app.select_source(app_log_id);
        let app_tab_id = app.active_tab_id;
        app.select_source(error_log_id);
        app.selected_search_source_ids = BTreeSet::from([error_log_id, nested_log_id]);

        app.activate_tab(app_tab_id);

        assert_eq!(app.active_tab_id, app_tab_id);
        assert!(
            app.source_registry
                .node(app_log_id)
                .map(|source| source.selected)
                .unwrap_or(false)
        );
        assert_eq!(
            app.selected_search_source_ids,
            BTreeSet::from([error_log_id, nested_log_id])
        );
    }

    /// 验证关闭当前标签后会激活相邻标签，关闭最后一个标签会回到空标签。
    #[test]
    fn close_tab_activates_neighbor_and_keeps_one_empty_tab() {
        let mut app = app_with_placeholder_sources();
        let app_log_id = source_id_by_label(&app, "app.log");
        let error_log_id = source_id_by_label(&app, "error.log");
        app.select_source(app_log_id);
        app.select_source(error_log_id);
        let error_tab_id = app.active_tab_id;

        app.close_tab(error_tab_id);
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

    /// 验证外观主题文件切换会立即替换运行时主题令牌。
    #[test]
    fn select_theme_updates_runtime_theme_tokens() {
        let mut app = test_app();

        app.select_theme("dark.toml".to_string());
        assert_eq!(app.selected_theme_id, "dark.toml");
        assert_eq!(
            app.theme.content,
            app.theme_manager.theme_for_id("dark.toml").content
        );
    }

    /// 验证旧版 light/system 配置会迁移到内置暗色主题文件。
    #[test]
    fn legacy_theme_modes_resolve_to_dark_theme_file() {
        let mut app = test_app();

        app.select_theme("light".to_string());

        assert_eq!(app.selected_theme_id, "dark.toml");
        assert_eq!(app.config.appearance.theme_mode, "dark.toml");
    }

    /// 验证外观和加载设置修改后会立即写入配置文件。
    #[test]
    fn settings_changes_are_persisted_to_config_file() {
        let config_manager = isolated_config_manager();
        let settings_path = config_manager.settings_path().to_path_buf();
        let mut app = ArgusApp::new_with_config_manager(config_manager);

        app.select_theme("dark.toml".to_string());
        app.adjust_log_content_font_size(2.0);
        app.adjust_max_archive_depth(1);
        app.adjust_archive_probe_concurrency(2);
        app.toggle_follow_symlinks();
        app.update_settings_quick_keywords("ERROR,WARN,timeout".to_string());
        app.update_settings_jstack_thread_name_filter("Attach Listener".to_string());
        app.update_settings_jstack_stack_segment_filter("Unsafe.park\n\nSocket\nread".to_string());

        let saved =
            ConfigManager::load_from_path(&settings_path).expect("设置变更后应写入配置文件");

        assert_eq!(saved.appearance.theme_mode, "dark.toml");
        assert_eq!(saved.appearance.log_content_font_size, 14.0);
        assert_eq!(saved.loader.max_archive_depth, 3);
        assert_eq!(saved.loader.archive_probe_concurrency, 6);
        assert!(saved.loader.follow_symlinks);
        assert_eq!(saved.log_search.quick_keywords, "ERROR,WARN,timeout");
        assert_eq!(
            saved.log_display.jstack_thread_name_filters,
            "Attach Listener"
        );
        assert_eq!(
            saved.log_display.jstack_stack_segment_filters,
            "Unsafe.park\n\nSocket\nread"
        );
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

    /// 验证切换到被当前过滤条件隐藏的日志标签时，只同步选中态，不修改过滤状态。
    #[test]
    fn activating_hidden_log_tab_keeps_source_tree_filter_and_updates_selection() {
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

        assert_eq!(app.active_tab_id, app_tab_id);
        assert!(app.is_source_tree_search_open);
        assert_eq!(app.source_tree_search_query, "error");
        assert!(!app.visible_source_ids().contains(&app_log_id));
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
