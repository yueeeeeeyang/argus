//! 文件职责：维护 Argus 应用状态、来源加载状态和界面展示数据。
//! 创建日期：2026-06-09
//! 修改日期：2026-06-25
//! 作者：Argus 开发团队
//! 主要功能：提供工作区切换、真实来源树、Jstack 分析、升级状态、未读取内容提示和保留的日志样例数据。

mod log_search_actions;
mod log_text;
mod placeholder_data;
mod remote;
mod settings_actions;
mod source_picker_actions;
mod source_search_actions;
mod text_input_actions;

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
mod source_tree_actions;
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
mod tests;
