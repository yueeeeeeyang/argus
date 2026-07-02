//! 文件职责：维护 Argus 应用状态、来源加载状态和界面展示数据。
//! 创建日期：2026-06-09
//! 修改日期：2026-06-25
//! 作者：Argus 开发团队
//! 主要功能：提供工作区切换、真实来源树、Jstack 分析、升级状态、未读取内容提示和保留的日志样例数据。

mod connection_actions;
mod log_search;
mod log_text;
mod placeholder_data;
mod settings_window;
mod sftp_actions;
mod source_picker;
mod source_search;
mod terminal_actions;
mod text_input;

use std::borrow::{Borrow, Cow};
use std::cell::RefCell;
use std::collections::{BTreeSet, HashMap, VecDeque};
use std::ops::Range;
#[cfg(test)]
use std::path::PathBuf;
use std::sync::{Arc, atomic::AtomicBool};
use std::time::Duration;

use crate::config::{AppConfig, ConfigManager};
use crate::connections::ConnectionNodeId;
use crate::highlight::HighlightCache;
use crate::jstack_analysis::{
    JstackAnalysisResult, JstackAnalysisTarget, JstackFrequencyRow, JstackThreadDetail,
    JstackThreadFilter, JstackThreadStackOccurrence, JstackThreadState, analyze_jstack_targets,
};
#[cfg(test)]
use crate::loader::SourceMetadata;
use crate::loader::{
    LoadReport, LogSourceLoader, SourceArchiveProbeRequest, SourceArchiveProbeResult, SourceId,
    SourceKind, SourceLocation, SourceRegistry, SourceTreeNode,
};
use crate::platform::open_with_registration::RegistrationStatus;
use crate::reader::log_file_reader::{
    LogFileReader, LogOpenState, LogReaderHandle, OpenLogRequest,
};
use crate::reader::read_mode::ReadMode;
use crate::runtime_analysis::{
    RuntimeAnalysisFilterRows, RuntimeAnalysisFilterSnapshot, RuntimeAnalysisResult,
    RuntimeAnalysisTarget, RuntimeAnalysisTargetKind, RuntimeSlowSqlSummaryRow,
    RuntimeSqlFrequencyAnalysisRow, RuntimeSqlFrequencyDetailRow, analyze_runtime_targets,
    build_runtime_analysis_filter_rows, build_runtime_slow_sql_rows_for_filter,
    build_runtime_sql_frequency_rows_for_filter, parse_runtime_analysis_filter_criteria,
};
use crate::search::search_engine::{SearchProgress, SearchResult, SearchScope};
use crate::search::search_task::SearchTaskState;
use crate::sftp::SftpSessionState;
use crate::terminal::TerminalSessionState;
use crate::text_selection::{
    TextSelectionGranularity, character_count, replace_character_range, slice_character_range,
    word_range_at,
};
use crate::theme::{AppTheme, ThemeManager, ThemeOption};
use crate::ui::components::context_menu::{ActiveMenu, ActiveMenuKind, MenuAction, MenuEntry};
use crate::ui::connection_dialog::{ConnectionDirectoryWindow, ConnectionLinkWindow};
use crate::ui::jstack_analysis_view::JstackCellHoverPreview;
use crate::ui::jstack_thread_detail_window::JstackThreadDetailWindow;
use crate::ui::log_search_window::LogSearchWindow;
use crate::ui::main_window;
use crate::ui::settings_window::{JstackStackSegmentFilterEditorWindow, SettingsWindow};
use crate::updater::{
    AvailableUpgrade, UpgradeCheckOutcome, UpgradeService, current_platform_arch,
    current_platform_os,
};
use chrono::{Datelike, Local, NaiveDate, NaiveDateTime, TimeZone, Timelike};
use gpui::{
    AppContext, Bounds, ClipboardItem, Context, FocusHandle, IntoElement, Keystroke, Pixels, Point,
    Render, Timer, Window, WindowBounds, WindowHandle, WindowOptions, px, size,
};
use gpui::{ScrollHandle, ScrollStrategy, UniformListScrollHandle};
#[cfg(test)]
use log_text::{log_text_range_for_granularity, merge_log_text_ranges};
#[cfg(test)]
use placeholder_data::{placeholder_logs, placeholder_source_registry};
pub use source_picker::{
    ExternalSourceTrigger, SourcePickerSortDirection, SourcePickerSortKey, SourcePickerState,
};

/// 兼容 UI 层既有命名：Runtime SQL 分析缓存使用的过滤快照。
pub use crate::runtime_analysis::RuntimeAnalysisFilterSnapshot as RuntimeSqlAnalysisFilterSnapshot;

/// 来源侧栏默认宽度；主窗口默认宽度同步增加，避免挤占右侧日志阅读区。
pub const SOURCE_PANEL_DEFAULT_WIDTH: f32 = 350.0;
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
/// 搜索结果面板默认高度。
pub const SEARCH_RESULT_PANEL_HEIGHT_DEFAULT: f32 = 220.0;
/// 搜索结果面板最小高度，保证标题和至少几行结果可见。
pub const SEARCH_RESULT_PANEL_HEIGHT_MIN: f32 = 140.0;
/// 搜索结果面板最大高度，避免拖拽时挤掉主要日志阅读区。
pub const SEARCH_RESULT_PANEL_HEIGHT_MAX: f32 = 520.0;
/// 日志正文左侧内边距；命中测试和渲染必须保持一致。
pub const LOG_VIEWER_TEXT_LEFT_PADDING: f32 = 16.0;
/// 日志正文右侧内边距；横向滚动范围和渲染必须保持一致。
pub const LOG_VIEWER_TEXT_RIGHT_PADDING: f32 = 16.0;
/// Jstack 线程详情窗口默认宽度。
const JSTACK_THREAD_DETAIL_WINDOW_WIDTH: f32 = 900.0;
/// Jstack 线程详情窗口默认高度。
const JSTACK_THREAD_DETAIL_WINDOW_HEIGHT: f32 = 640.0;
/// Jstack 线程详情窗口最小宽度。
const JSTACK_THREAD_DETAIL_WINDOW_MIN_WIDTH: f32 = 600.0;
/// Jstack 线程详情窗口最小高度。
const JSTACK_THREAD_DETAIL_WINDOW_MIN_HEIGHT: f32 = 420.0;
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
/// 后台压缩包探测每批最多处理 `并发数 * 该系数` 个节点，避免频繁重绘。
const SOURCE_ARCHIVE_PROBE_BATCH_FACTOR: usize = 16;
/// Runtime 过滤输入防抖时长，避免每个字符都触发大结果集重新过滤。
const RUNTIME_FILTER_DEBOUNCE_MS: u64 = 260;

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

/// 当前界面工作区，驱动标题栏入口和左侧侧栏内容。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Workspace {
    /// 日志分析工作区，用于展示来源侧栏和日志内容占位界面。
    LogAnalysis,
    /// 链接工作区，用于展示 SSH 链接目录树和终端标签页。
    Connections,
    /// 设置工作区，用于展示主题、编码、缓存、快捷键等占位配置。
    Settings,
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
    /// Jstack 线程日志分析标签页。
    JstackAnalysis {
        /// 分析状态 ID，用于从应用状态表中读取结果。
        analysis_id: usize,
    },
    /// Runtime 请求日志分析标签页。
    RuntimeAnalysis {
        /// 分析状态 ID，用于从应用状态表中读取结果。
        analysis_id: usize,
    },
    /// SSH 终端标签页。
    SshTerminal {
        /// 终端会话 ID，用于从应用状态表中读取终端输出和连接状态。
        session_id: usize,
    },
    /// SFTP 文件管理标签页。
    SftpFileManager {
        /// SFTP 会话 ID，用于从应用状态表中读取远程文件列表和操作状态。
        session_id: usize,
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

/// 子级懒加载完成后需要自动续做的来源树分析动作。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PendingSourceAnalysisAction {
    /// 加载完成后打开 Jstack 线程日志分析。
    Jstack {
        /// 触发右键菜单的来源目录 ID。
        source_id: SourceId,
    },
    /// 加载完成后打开 Runtime 日志解析。
    Runtime {
        /// 触发右键菜单的来源目录 ID。
        source_id: SourceId,
    },
}

impl PendingSourceAnalysisAction {
    /// 返回等待加载的来源目录 ID，便于子级加载回调精确匹配。
    fn source_id(self) -> SourceId {
        match self {
            Self::Jstack { source_id } | Self::Runtime { source_id } => source_id,
        }
    }
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

/// 日志搜索窗口输入框类型，用于复用同一套输入状态处理。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LogSearchInputKind {
    /// 关键字输入框。
    Keyword,
    /// 来源树目录输入框。
    Directory,
}

/// Runtime 分析页过滤输入框类型。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RuntimeFilterInputKind {
    /// 表格任意关键字过滤。
    Keyword,
    /// 用户名模糊过滤。
    Username,
    /// 请求开始时间过滤。
    StartTime,
    /// 请求结束时间过滤。
    EndTime,
}

/// Runtime 日期时间选择器可调整的时间部分。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RuntimeDateTimePart {
    /// 年。
    Year,
    /// 月。
    Month,
    /// 日。
    Day,
    /// 时。
    Hour,
    /// 分。
    Minute,
    /// 秒。
    Second,
}

/// Runtime 日期时间选择器快捷动作。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RuntimeDateTimeQuickAction {
    /// 设置为今天 00:00:00。
    TodayStart,
    /// 设置为当前本地时间。
    Now,
    /// 设置为今天 23:59:59。
    TodayEnd,
    /// 清空当前时间过滤条件。
    Clear,
}

/// 应用内所有自绘单行输入框的原生文本输入目标。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AppTextInputTarget {
    /// 来源树过滤输入框。
    SourceTreeSearch,
    /// 链接树过滤输入框。
    ConnectionTreeSearch,
    /// 新增目录表单中的目录名称输入框。
    ConnectionDirectoryName,
    /// 新增 SSH 链接表单中的链接名称输入框。
    ConnectionLinkName,
    /// 新增 SSH 链接表单中的主机输入框。
    ConnectionLinkHost,
    /// 新增 SSH 链接表单中的端口输入框。
    ConnectionLinkPort,
    /// 新增 SSH 链接表单中的用户名输入框。
    ConnectionLinkUsername,
    /// 新增 SSH 链接表单中的密码输入框。
    ConnectionLinkPassword,
    /// 新增 SSH 链接表单中的私钥路径输入框。
    ConnectionLinkPrivateKeyPath,
    /// 新增 SSH 链接表单中的私钥口令输入框。
    ConnectionLinkPrivateKeyPassphrase,
    /// SFTP 文件管理地址栏输入框。
    SftpAddress {
        /// SFTP 会话 ID。
        session_id: usize,
    },
    /// SFTP 重命名弹窗名称输入框。
    SftpRenameName,
    /// 来源选择器路径输入框。
    SourcePickerPath,
    /// 独立日志搜索窗口输入框。
    LogSearch(LogSearchInputKind),
    /// Runtime 分析页过滤输入框。
    RuntimeFilter {
        /// Runtime 分析页 ID。
        analysis_id: usize,
        /// 过滤输入框类型。
        input_kind: RuntimeFilterInputKind,
    },
    /// 设置窗口快搜关键字输入框。
    SettingsQuickKeywords,
    /// 设置窗口 Jstack 线程名过滤输入框。
    SettingsJstackThreadNameFilter,
    /// 设置窗口 Jstack 完整线程段过滤输入框。
    SettingsJstackStackSegmentFilter,
    /// 设置窗口升级服务器输入框。
    SettingsUpgradeServer,
    /// 设置窗口升级验签公钥输入框。
    SettingsUpgradePublicKey,
}

/// 日志搜索窗口中的单行输入框状态。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LogSearchInputState {
    /// 输入框当前文本。
    pub value: String,
    /// 光标字符位置。
    pub cursor: usize,
    /// 选区锚点；与光标不一致时表示存在选区。
    pub selection_anchor: Option<usize>,
    /// 输入法 marked text 字符范围，候选态替换时使用。
    pub marked_range: Option<std::ops::Range<usize>>,
    /// 鼠标拖拽选区状态。
    pub selection_drag: Option<InputTextSelectionDrag>,
    /// 是否处于焦点状态。
    pub is_focused: bool,
}

impl Default for LogSearchInputState {
    /// 创建空输入框状态。
    fn default() -> Self {
        Self {
            value: String::new(),
            cursor: 0,
            selection_anchor: None,
            marked_range: None,
            selection_drag: None,
            is_focused: false,
        }
    }
}

/// 设置窗口中的单行输入框状态；用于保存持久化设置项的编辑光标和选区。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SettingsTextInputState {
    /// 输入框当前文本。
    pub value: String,
    /// 光标字符位置。
    pub cursor: usize,
    /// 选区锚点；与光标不一致时表示存在选区。
    pub selection_anchor: Option<usize>,
    /// 输入法 marked text 字符范围，候选态替换时使用。
    pub marked_range: Option<std::ops::Range<usize>>,
    /// 鼠标拖拽选区状态。
    pub selection_drag: Option<InputTextSelectionDrag>,
    /// 是否处于焦点状态。
    pub is_focused: bool,
}

impl SettingsTextInputState {
    /// 根据已有配置值构造设置输入框状态，光标默认位于文本末尾。
    pub fn from_value(value: String) -> Self {
        let cursor = character_count(&value);
        Self {
            value,
            cursor,
            selection_anchor: None,
            marked_range: None,
            selection_drag: None,
            is_focused: false,
        }
    }
}

/// 主窗口内输入框真实焦点句柄集合。
#[derive(Clone)]
pub struct AppInputFocusHandles {
    /// 主窗口根区域焦点，用于点击非输入区域时承接真实键盘焦点。
    pub root: FocusHandle,
    /// 来源树过滤输入框焦点。
    pub source_tree_search: FocusHandle,
    /// 链接树过滤输入框焦点。
    pub connection_tree_search: FocusHandle,
    /// 新增目录名称输入框焦点。
    pub connection_directory_name: FocusHandle,
    /// 新增 SSH 链接名称输入框焦点。
    pub connection_link_name: FocusHandle,
    /// 新增 SSH 链接主机输入框焦点。
    pub connection_link_host: FocusHandle,
    /// 新增 SSH 链接端口输入框焦点。
    pub connection_link_port: FocusHandle,
    /// 新增 SSH 链接用户名输入框焦点。
    pub connection_link_username: FocusHandle,
    /// 新增 SSH 链接密码输入框焦点。
    pub connection_link_password: FocusHandle,
    /// 新增 SSH 链接私钥路径输入框焦点。
    pub connection_link_private_key_path: FocusHandle,
    /// 新增 SSH 链接私钥口令输入框焦点。
    pub connection_link_private_key_passphrase: FocusHandle,
    /// SFTP 文件管理地址栏焦点。
    pub sftp_address: FocusHandle,
    /// SFTP 重命名弹窗输入框焦点。
    pub sftp_rename_name: FocusHandle,
    /// 右侧终端面板焦点。
    pub terminal: FocusHandle,
    /// Jstack 分析页焦点，用于线程名拖选后稳定接收复制快捷键。
    pub jstack_analysis: FocusHandle,
    /// Runtime 分析页焦点，用于表格单元格拖选后稳定接收复制快捷键。
    pub runtime_analysis: FocusHandle,
    /// Runtime 关键字过滤输入框焦点。
    pub runtime_filter_keyword: FocusHandle,
    /// Runtime 用户名过滤输入框焦点。
    pub runtime_filter_username: FocusHandle,
    /// Runtime 开始时间过滤输入框焦点。
    pub runtime_filter_start_time: FocusHandle,
    /// Runtime 结束时间过滤输入框焦点。
    pub runtime_filter_end_time: FocusHandle,
}

impl Default for SettingsTextInputState {
    /// 创建空设置输入框状态。
    fn default() -> Self {
        Self::from_value(String::new())
    }
}

/// 链接工作区当前打开的弹窗。
#[derive(Clone, Debug)]
pub enum ConnectionDialogState {
    /// 新增目录表单。
    NewDirectory(ConnectionDirectoryFormState),
    /// 新增 SSH 链接表单。
    NewSshLink(ConnectionLinkFormState),
    /// SSH 首次连接未知主机时的指纹确认弹窗。
    ConfirmHostKey(ConnectionHostKeyPromptState),
    /// 删除链接目录或 SSH 链接前的二次确认弹窗。
    ConfirmDelete(ConnectionDeletePromptState),
}

/// 新增目录表单状态。
#[derive(Clone, Debug)]
pub struct ConnectionDirectoryFormState {
    /// 新目录的父目录 ID；为空表示创建在根层级。
    pub parent_id: Option<ConnectionNodeId>,
    /// 目录名称输入框。
    pub name_input: SettingsTextInputState,
    /// 最近一次校验错误。
    pub error_message: Option<String>,
}

/// 新增 SSH 链接表单状态。
#[derive(Clone, Debug)]
pub struct ConnectionLinkFormState {
    /// 新链接的父目录 ID；为空表示创建在根层级。
    pub parent_id: Option<ConnectionNodeId>,
    /// 链接名称输入框。
    pub name_input: SettingsTextInputState,
    /// SSH 主机输入框。
    pub host_input: SettingsTextInputState,
    /// SSH 端口输入框。
    pub port_input: SettingsTextInputState,
    /// SSH 用户名输入框。
    pub username_input: SettingsTextInputState,
    /// SSH 密码输入框。
    pub password_input: SettingsTextInputState,
    /// SSH 私钥路径输入框。
    pub private_key_path_input: SettingsTextInputState,
    /// SSH 私钥口令输入框。
    pub private_key_passphrase_input: SettingsTextInputState,
    /// 最近一次校验错误。
    pub error_message: Option<String>,
}

/// SSH 主机指纹确认弹窗状态。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HostKeyPromptOwner {
    /// 终端会话触发的主机指纹确认。
    Terminal {
        /// 终端会话 ID。
        session_id: usize,
    },
    /// SFTP 文件管理会话触发的主机指纹确认。
    Sftp {
        /// SFTP 会话 ID。
        session_id: usize,
    },
}

/// SSH 主机指纹确认弹窗状态。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ConnectionHostKeyPromptState {
    /// 等待确认的会话 ID；具体类型由 `owner` 区分。
    pub session_id: usize,
    /// 触发确认的会话类型。
    pub owner: HostKeyPromptOwner,
    /// 关联链接节点 ID。
    pub link_id: ConnectionNodeId,
    /// 远程主机。
    pub host: String,
    /// 远程端口。
    pub port: u16,
    /// 待确认的 SHA256 指纹。
    pub fingerprint: String,
}

/// 删除链接节点二次确认弹窗状态。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ConnectionDeletePromptState {
    /// 待删除的连接节点 ID。
    pub node_id: ConnectionNodeId,
    /// 待删除节点展示名称。
    pub label: String,
    /// 是否为目录；目录删除前会额外要求为空。
    pub is_directory: bool,
}

/// SFTP 文件管理内的应用弹窗。
#[derive(Clone, Debug)]
pub enum SftpDialogState {
    /// 重命名远程文件或目录。
    Rename(SftpRenameDialogState),
    /// 删除远程普通文件或空目录前的二次确认。
    ConfirmDelete(SftpDeletePromptState),
}

/// SFTP 重命名弹窗状态。
#[derive(Clone, Debug)]
pub struct SftpRenameDialogState {
    /// SFTP 会话 ID。
    pub session_id: usize,
    /// 原始远程路径。
    pub remote_path: String,
    /// 原始名称。
    pub original_name: String,
    /// 名称输入框。
    pub name_input: SettingsTextInputState,
    /// 最近一次校验错误。
    pub error_message: Option<String>,
}

/// SFTP 删除二次确认弹窗状态。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SftpDeletePromptState {
    /// SFTP 会话 ID。
    pub session_id: usize,
    /// 待删除远程路径。
    pub remote_path: String,
    /// 待删除文件或目录名称。
    pub name: String,
    /// 是否为目录。
    pub is_directory: bool,
}

/// 升级弹窗状态，覆盖发现版本、安装进度和失败提示三类用户可见流程。
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum UpgradeDialogState {
    /// 发现可安装版本，等待用户确认升级、跳过或稍后。
    Available {
        /// 待安装的新版本信息。
        upgrade: AvailableUpgrade,
    },
    /// 正在下载、校验、替换或重启。
    Progress {
        /// 正在处理的新版本号。
        version: String,
        /// 当前阶段说明。
        message: String,
    },
    /// 升级失败，等待用户关闭后继续使用旧版本。
    Failed {
        /// 失败关联版本；手动检查失败时可能没有版本号。
        version: Option<String>,
        /// 失败原因。
        message: String,
    },
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

/// 当前日志快速查找缓存键，避免关键字、选项或日志变化后复用过期结果。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct QuickMatchKey {
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
pub struct SearchResultGroup {
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
pub enum SearchResultListItem {
    /// 文件分组标题行。
    Group(usize),
    /// 单条命中结果行。
    Result(usize),
}

/// 日志搜索任务来源，用于结果面板区分普通搜索和快搜。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SearchRunKind {
    /// 搜索窗口关键字输入框发起的普通搜索。
    Normal,
    /// 设置中的快搜关键字集合发起的一键搜索。
    QuickKeywords,
}

/// 搜索结果面板自绘滚动条方向。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SearchResultScrollbarAxis {
    /// 纵向结果滚动。
    Vertical,
    /// 横向预览滚动。
    Horizontal,
}

/// 搜索结果面板滚动条拖拽状态。
#[derive(Clone, Copy, Debug)]
pub struct SearchResultScrollbarDrag {
    /// 当前拖动方向。
    pub axis: SearchResultScrollbarAxis,
    /// 鼠标按下点在 thumb 内的相对偏移。
    pub cursor_offset: Pixels,
}

/// 搜索结果面板高度拖拽状态。
#[derive(Clone, Copy, Debug)]
pub struct SearchResultPanelResizeDrag {
    /// 鼠标按下时的窗口 y 坐标。
    pub start_y: Pixels,
    /// 鼠标按下时的面板高度。
    pub start_height: f32,
}

/// 独立日志搜索窗口和结果面板共享的运行期状态。
#[derive(Clone, Debug)]
pub struct LogSearchState {
    /// 搜索窗口是否已打开。
    pub is_window_open: bool,
    /// 搜索窗口句柄；再次打开时用于置前。
    pub window_handle: Option<WindowHandle<LogSearchWindow>>,
    /// 当前搜索范围。
    pub scope: SearchScope,
    /// 关键字输入框状态。
    pub keyword_input: LogSearchInputState,
    /// 目录输入框状态。
    pub directory_input: LogSearchInputState,
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
            keyword_input: LogSearchInputState::default(),
            directory_input: LogSearchInputState::default(),
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

/// Jstack 分析任务状态，供内容区页签展示加载、结果或失败。
#[derive(Clone, Debug)]
pub enum JstackAnalysisTaskState {
    /// 后台任务正在读取和聚合线程栈。
    Loading {
        /// 当前加载提示。
        message: String,
    },
    /// 分析完成，可渲染频率矩阵。
    Ready(JstackAnalysisResult),
    /// 分析任务启动或后台执行失败。
    Failed {
        /// 用户可读失败原因。
        message: String,
    },
}

/// 单个 Jstack 分析页签的持久状态。
#[derive(Clone, Debug)]
pub struct JstackAnalysisState {
    /// 分析 ID，与 `TabKind::JstackAnalysis` 对应。
    pub id: usize,
    /// 页签标题。
    pub title: String,
    /// 本次分析的来源目标快照，保持创建页签时的来源树顺序。
    pub targets: Vec<JstackAnalysisTarget>,
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

/// Runtime 分析任务状态，供内容区页签展示加载、结果或失败。
#[derive(Clone, Debug)]
pub enum RuntimeAnalysisTaskState {
    /// 后台任务正在读取和聚合 Runtime 日志。
    Loading {
        /// 当前加载提示。
        message: String,
    },
    /// 分析完成，可渲染三层统计表格。
    Ready(Arc<RuntimeAnalysisResult>),
    /// 分析任务启动或后台执行失败。
    Failed {
        /// 用户可读失败原因。
        message: String,
    },
}

/// Runtime 分析页当前显示层级。
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RuntimeAnalysisView {
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
pub enum RuntimeAnalysisResultType {
    /// 当前请求统计总览和下钻表格。
    Statistics,
    /// 按 SQL 结构聚合后的执行频率分析。
    SqlFrequency,
    /// 按 SQL 结构聚合后的平均执行耗时分析。
    SlowSql,
}

/// Runtime SQL 频率分析缓存。
#[derive(Clone, Debug)]
pub struct RuntimeSqlFrequencyRowsCache {
    /// 生成缓存时使用的过滤输入快照。
    pub filter: RuntimeSqlAnalysisFilterSnapshot,
    /// 已过滤和排序的 SQL 频率行。
    pub rows: Arc<Vec<RuntimeSqlFrequencyAnalysisRow>>,
}

/// Runtime SQL 频率详情缓存。
#[derive(Clone, Debug)]
pub struct RuntimeSqlFrequencyDetailRowsCache {
    /// 生成缓存时使用的过滤输入快照。
    pub filter: RuntimeSqlAnalysisFilterSnapshot,
    /// 当前详情页对应的 SQL 结构文本。
    pub normalized_sql: String,
    /// 已过滤和排序的 SQL 执行详情行。
    pub rows: Arc<Vec<RuntimeSqlFrequencyDetailRow>>,
}

/// Runtime 慢 SQL 分析缓存。
#[derive(Clone, Debug)]
pub struct RuntimeSlowSqlRowsCache {
    /// 生成缓存时使用的过滤输入快照。
    pub filter: RuntimeSqlAnalysisFilterSnapshot,
    /// 已过滤和排序的慢 SQL 聚合行。
    pub rows: Arc<Vec<RuntimeSlowSqlSummaryRow>>,
}

/// Runtime 表格排序方向。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RuntimeSortDirection {
    /// 升序。
    Ascending,
    /// 降序。
    Descending,
}

/// Runtime SQL 明细收起态固定行高。
///
/// 说明：Runtime SQL 表格已固定为单行展示，长 SQL 由单元格内部横向滚动承载；
/// UI 层的 SQL 行高必须与这里保持一致，才能让虚拟列表滚动条稳定计算范围。
pub const RUNTIME_SQL_COLLAPSED_ROW_HEIGHT: f32 = 36.0;

/// Runtime 表格滚动条所属表格。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RuntimeScrollbarTable {
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
}

/// Runtime 表格滚动条拖拽状态。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RuntimeScrollbarDrag {
    /// 当前被拖拽的表格。
    pub table: RuntimeScrollbarTable,
    /// 鼠标按下位置相对滑块顶部的偏移。
    pub cursor_offset: Pixels,
}

impl RuntimeSortDirection {
    /// 返回切换后的排序方向。
    pub fn toggled(self) -> Self {
        match self {
            Self::Ascending => Self::Descending,
            Self::Descending => Self::Ascending,
        }
    }

    /// 返回表头展示箭头。
    pub fn indicator(self) -> &'static str {
        match self {
            Self::Ascending => " ↑",
            Self::Descending => " ↓",
        }
    }
}

/// Runtime 总览表排序字段。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RuntimeSummarySortKey {
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
pub enum RuntimeRequestSortKey {
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
pub enum RuntimeSqlSortKey {
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
pub struct RuntimeAnalysisState {
    /// 分析 ID，与 `TabKind::RuntimeAnalysis` 对应。
    pub id: usize,
    /// 页签标题。
    pub title: String,
    /// 本次分析的来源目标快照，保持创建页签时的来源树顺序。
    pub targets: Vec<RuntimeAnalysisTarget>,
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
    pub filter_keyword_input: SettingsTextInputState,
    /// 用户名过滤输入框状态。
    pub filter_username_input: SettingsTextInputState,
    /// 请求开始时间过滤输入框状态。
    pub filter_start_time_input: SettingsTextInputState,
    /// 请求结束时间过滤输入框状态。
    pub filter_end_time_input: SettingsTextInputState,
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
    /// 当前 Runtime 表格滚动条拖拽状态。
    pub scrollbar_drag: Option<RuntimeScrollbarDrag>,
    /// 当前任务状态。
    pub task_state: RuntimeAnalysisTaskState,
}

/// Runtime SQL 文本单元格身份。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RuntimeSqlCellKey {
    /// 请求记录在分析结果中的稳定索引。
    pub request_index: usize,
    /// SQL 记录在当前请求中的稳定索引。
    pub sql_index: usize,
}

/// Runtime SQL 完整文本弹窗状态。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RuntimeSqlTextDialog {
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
pub struct RuntimeSqlTextPosition {
    /// 0 基 SQL 行号。
    pub line_index: usize,
    /// 行内字符列，按 Unicode 标量值计数。
    pub column: usize,
}

/// Runtime SQL 弹窗正文选区，支持跨行复制。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RuntimeSqlTextSelection {
    /// 鼠标按下时的选区锚点。
    pub anchor: RuntimeSqlTextPosition,
    /// 当前拖拽到的焦点位置。
    pub focus: RuntimeSqlTextPosition,
}

impl RuntimeSqlTextSelection {
    /// 返回选区是否为空。
    pub fn is_empty(&self) -> bool {
        self.anchor == self.focus
    }

    /// 返回按文档顺序排列后的起止位置。
    pub fn normalized(&self) -> (RuntimeSqlTextPosition, RuntimeSqlTextPosition) {
        if runtime_sql_text_position_le(self.anchor, self.focus) {
            (self.anchor, self.focus)
        } else {
            (self.focus, self.anchor)
        }
    }
}

/// Runtime SQL 弹窗正文拖拽选择状态。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RuntimeSqlTextSelectionDrag {
    /// 鼠标按下时形成的基础选区。
    pub anchor_range: RuntimeSqlTextSelection,
    /// 当前拖拽粒度，决定后续移动时按字符、词或整行扩展。
    pub granularity: TextSelectionGranularity,
}

/// Runtime 表格单元格的单行文本选区。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RuntimeTableCellSelection {
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
    pub fn normalized_range(&self) -> Option<Range<usize>> {
        let text_length = character_count(&self.text);
        let start = self.anchor.min(self.focus).min(text_length);
        let end = self.anchor.max(self.focus).min(text_length);
        (start < end).then_some(start..end)
    }
}

/// Runtime 表格单元格拖拽选择状态。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RuntimeTableCellSelectionDrag {
    /// 本次拖拽起始的单元格 key。
    pub cell_key: String,
    /// 本次拖拽起始的单元格完整文本。
    pub text: String,
    /// 鼠标按下时按点击次数扩展后的基础字符范围。
    pub anchor_range: Range<usize>,
    /// 当前选择粒度，单击按字符，双击及以上按整格内容。
    pub granularity: TextSelectionGranularity,
}

/// Jstack 分析矩阵左侧线程名的单行文本选区。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct JstackThreadNameSelection {
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
    pub fn normalized_range(&self) -> Option<Range<usize>> {
        let text_length = character_count(&self.thread_name);
        let start = self.anchor.min(self.focus).min(text_length);
        let end = self.anchor.max(self.focus).min(text_length);
        (start < end).then_some(start..end)
    }
}

/// Jstack 分析矩阵左侧线程名拖拽选择状态。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct JstackThreadNameSelectionDrag {
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
    pub fn rebuild_visible_row_cache(&mut self, thread_filter: &JstackThreadFilter) {
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
            active_search_match: None,
            line_markers: BTreeSet::new(),
            last_line_marker_jump: None,
        }
    }
}

/// 返回当前 Jstack 筛选条件下需要渲染的结果行索引。
fn visible_jstack_row_indices(
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
fn jstack_detail_occurrences_for_visible_cells(
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
pub fn jstack_cell_selection_key(row_index: usize, snapshot_index: usize) -> String {
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
fn jstack_thread_name_range_for_granularity(
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

/// 根据点击次数返回 Runtime 表格单元格文本的字符选区。
///
/// 参数说明：
/// - `text`：当前单元格完整文本。
/// - `character_index`：命中的字符列。
/// - `granularity`：选择粒度；Runtime 表格要求双击选中整格内容。
///
/// 返回值：按字符索引表示的选区范围。
fn runtime_cell_range_for_granularity(
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
fn runtime_sql_text_position_le(
    left: RuntimeSqlTextPosition,
    right: RuntimeSqlTextPosition,
) -> bool {
    left.line_index < right.line_index
        || (left.line_index == right.line_index && left.column <= right.column)
}

/// 按点击粒度生成 Runtime SQL 弹窗正文选区。
fn runtime_sql_text_range_for_granularity(
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
fn runtime_sql_text_lines(sql_text: &str) -> Vec<String> {
    sql_text
        .replace("\r\n", "\n")
        .replace('\r', "\n")
        .split('\n')
        .map(ToString::to_string)
        .collect()
}

/// 从 Runtime SQL 弹窗行集合中提取当前选区文本，保留跨行换行符。
fn selected_runtime_sql_text_from_lines(
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
    for line_index in start.line_index..=end_line {
        if line_index > start.line_index {
            selected.push('\n');
        }
        let line = &lines[line_index];
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
fn clear_runtime_filter_inputs_focus(state: &mut RuntimeAnalysisState) {
    clear_runtime_filter_input_focus(&mut state.filter_keyword_input);
    clear_runtime_filter_input_focus(&mut state.filter_username_input);
    clear_runtime_filter_input_focus(&mut state.filter_start_time_input);
    clear_runtime_filter_input_focus(&mut state.filter_end_time_input);
    state.open_time_picker = None;
}

/// 从 Runtime 过滤输入框状态生成原始输入快照。
fn runtime_filter_input_snapshot_from_state(
    state: &RuntimeAnalysisState,
) -> RuntimeAnalysisFilterSnapshot {
    RuntimeAnalysisFilterSnapshot {
        keyword: state.filter_keyword_input.value.clone(),
        username: state.filter_username_input.value.clone(),
        start_time: state.filter_start_time_input.value.clone(),
        end_time: state.filter_end_time_input.value.clone(),
    }
}

/// 从 Runtime 已应用过滤值生成快照。
fn runtime_filter_applied_snapshot_from_state(
    state: &RuntimeAnalysisState,
) -> RuntimeAnalysisFilterSnapshot {
    RuntimeAnalysisFilterSnapshot {
        keyword: state.applied_filter_keyword.clone(),
        username: state.applied_filter_username.clone(),
        start_time: state.applied_filter_start_time.clone(),
        end_time: state.applied_filter_end_time.clone(),
    }
}

/// 将过滤快照写入 Runtime 已应用状态。
fn apply_runtime_filter_snapshot_to_state(
    state: &mut RuntimeAnalysisState,
    filter: &RuntimeAnalysisFilterSnapshot,
) {
    state.applied_filter_keyword = filter.keyword.clone();
    state.applied_filter_username = filter.username.clone();
    state.applied_filter_start_time = filter.start_time.clone();
    state.applied_filter_end_time = filter.end_time.clone();
}

/// 过滤结果真正生效后清理表格滚动、选区和旧的局部缓存。
fn reset_runtime_filter_result_view_state(state: &mut RuntimeAnalysisState) {
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
    state.sql_frequency_rows_cache.borrow_mut().take();
    state.sql_frequency_detail_rows_cache.borrow_mut().take();
    state.slow_sql_rows_cache.borrow_mut().take();
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
fn clear_runtime_filter_input_focus(input: &mut SettingsTextInputState) {
    input.is_focused = false;
    input.selection_anchor = None;
    input.marked_range = None;
    input.selection_drag = None;
}

/// 返回 Runtime 过滤输入框规范化后的非空选区。
fn normalized_runtime_filter_input_selection_range(
    input: &SettingsTextInputState,
) -> Option<Range<usize>> {
    let anchor = input.selection_anchor?;
    if anchor == input.cursor {
        return None;
    }
    Some(anchor.min(input.cursor)..anchor.max(input.cursor))
}

/// 根据鼠标选择粒度返回 Runtime 过滤输入框目标字符范围。
fn runtime_filter_input_range_for_granularity(
    input: &SettingsTextInputState,
    character_index: usize,
    granularity: TextSelectionGranularity,
) -> Range<usize> {
    let text_length = character_count(&input.value);
    let cursor = character_index.min(text_length);
    match granularity {
        TextSelectionGranularity::Character => cursor..cursor,
        TextSelectionGranularity::Word => {
            word_range_at(&input.value, cursor).unwrap_or(cursor..cursor)
        }
        TextSelectionGranularity::Line => 0..text_length,
    }
}

/// 解析 Runtime 时间过滤输入，支持毫秒时间戳和常见本地日期时间格式。
fn parse_runtime_filter_datetime_value(raw: &str, is_end: bool) -> Option<chrono::DateTime<Local>> {
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
fn format_runtime_filter_datetime_value(datetime: chrono::DateTime<Local>) -> String {
    datetime.format("%Y-%m-%d %H:%M:%S").to_string()
}

/// 返回指定年月的最大日期，用于调整年月时夹住当前日。
fn runtime_days_in_month(year: i32, month: u32) -> u32 {
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
fn adjust_runtime_datetime_year_month(
    datetime: chrono::DateTime<Local>,
    part: RuntimeDateTimePart,
    delta: i32,
) -> chrono::DateTime<Local> {
    let mut year = datetime.year();
    let mut month = datetime.month() as i32;
    match part {
        RuntimeDateTimePart::Year => year += delta,
        RuntimeDateTimePart::Month => {
            let month_index = year * 12 + (month - 1) + delta;
            year = month_index.div_euclid(12);
            month = month_index.rem_euclid(12) + 1;
        }
        RuntimeDateTimePart::Day
        | RuntimeDateTimePart::Hour
        | RuntimeDateTimePart::Minute
        | RuntimeDateTimePart::Second => return datetime,
    }

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
fn adjust_runtime_datetime_part(
    datetime: chrono::DateTime<Local>,
    part: RuntimeDateTimePart,
    delta: i32,
) -> chrono::DateTime<Local> {
    match part {
        RuntimeDateTimePart::Year | RuntimeDateTimePart::Month => {
            adjust_runtime_datetime_year_month(datetime, part, delta)
        }
        RuntimeDateTimePart::Day => datetime + chrono::Duration::days(delta as i64),
        RuntimeDateTimePart::Hour => datetime + chrono::Duration::hours(delta as i64),
        RuntimeDateTimePart::Minute => datetime + chrono::Duration::minutes(delta as i64),
        RuntimeDateTimePart::Second => datetime + chrono::Duration::seconds(delta as i64),
    }
}

/// 返回 Runtime 时间过滤输入框在空值时使用的默认时间。
fn default_runtime_filter_datetime(is_end: bool) -> chrono::DateTime<Local> {
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
    /// 当前选中的链接目录或 SSH 链接节点。
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
    /// 新增 SSH 链接独立窗口是否处于打开状态。
    pub is_connection_link_window_open: bool,
    /// 新增 SSH 链接独立窗口句柄，用于重复点击时置前已有窗口。
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
    /// SFTP 文件管理会话状态表。
    pub sftp_sessions: HashMap<usize, SftpSessionState>,
    /// 下一个 SFTP 文件管理会话 ID。
    pub next_sftp_session_id: usize,
    /// 当前打开的 SFTP 文件管理弹窗。
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

    /// 启动升级检查任务。
    ///
    /// 参数说明：
    /// - `is_manual`：是否由用户在设置页手动触发；手动检查会忽略“已跳过版本”并显示失败提示。
    /// - `cx`：应用上下文，用于调度后台网络任务并在完成后刷新 UI。
    pub fn start_upgrade_check(&mut self, is_manual: bool, cx: &mut Context<Self>) {
        if self.is_upgrade_checking {
            self.upgrade_message = Some("升级检查正在进行".to_string());
            self.placeholder_notice = "升级检查正在进行".to_string();
            return;
        }
        if !is_manual
            && (!self.config.upgrade.enabled
                || self.config.upgrade.server_url.is_empty()
                || self.config.upgrade.public_key_base64.is_empty())
        {
            return;
        }
        if is_manual && self.config.upgrade.server_url.is_empty() {
            self.upgrade_message = Some("请先配置升级服务器地址".to_string());
            self.upgrade_dialog = Some(UpgradeDialogState::Failed {
                version: None,
                message: "请先配置升级服务器地址".to_string(),
            });
            self.placeholder_notice = "请先配置升级服务器地址".to_string();
            return;
        }
        if is_manual && self.config.upgrade.public_key_base64.is_empty() {
            self.upgrade_message = Some("请先配置升级验签公钥".to_string());
            self.upgrade_dialog = Some(UpgradeDialogState::Failed {
                version: None,
                message: "请先配置升级验签公钥".to_string(),
            });
            self.placeholder_notice = "请先配置升级验签公钥".to_string();
            return;
        }

        self.is_upgrade_checking = true;
        self.upgrade_message = Some("正在检查新版本...".to_string());
        self.placeholder_notice = if is_manual {
            "正在手动检查新版本".to_string()
        } else {
            "正在后台检查新版本".to_string()
        };
        let mut upgrade_config = self.config.upgrade.clone();
        if is_manual {
            upgrade_config.enabled = true;
        }

        cx.spawn(async move |view, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    UpgradeService::runtime(&upgrade_config).check_for_update(
                        &upgrade_config,
                        env!("CARGO_PKG_VERSION"),
                        !is_manual,
                    )
                })
                .await;

            view.update(cx, |app, cx| {
                app.is_upgrade_checking = false;
                app.config.upgrade.last_check_at = Some(chrono::Utc::now().to_rfc3339());
                match result {
                    Ok(UpgradeCheckOutcome::Disabled) => {
                        app.upgrade_message = Some("自动升级未启用或未配置服务器".to_string());
                        if is_manual {
                            app.upgrade_dialog = Some(UpgradeDialogState::Failed {
                                version: None,
                                message: "自动升级未启用或未配置服务器".to_string(),
                            });
                        }
                    }
                    Ok(UpgradeCheckOutcome::UpToDate) => {
                        app.upgrade_message = Some("当前已是最新版本".to_string());
                        if is_manual {
                            app.placeholder_notice = "当前已是最新版本".to_string();
                        }
                    }
                    Ok(UpgradeCheckOutcome::Skipped(version)) => {
                        app.upgrade_message = Some(format!("已跳过版本 {version}"));
                    }
                    Ok(UpgradeCheckOutcome::Available(upgrade)) => {
                        let version = upgrade.version.clone();
                        app.upgrade_message = Some(format!("发现新版本 {version}"));
                        app.placeholder_notice = format!("发现新版本 {version}");
                        app.upgrade_dialog = Some(UpgradeDialogState::Available { upgrade });
                    }
                    Err(error) => {
                        let message = error.to_string();
                        app.upgrade_message = Some(message.clone());
                        app.placeholder_notice = format!("升级检查失败：{message}");
                        if is_manual {
                            app.upgrade_dialog = Some(UpgradeDialogState::Failed {
                                version: None,
                                message,
                            });
                        }
                    }
                }
                app.persist_config_or_report();
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// 关闭升级弹窗，保留已经记录的升级消息。
    pub fn dismiss_upgrade_dialog(&mut self) {
        self.upgrade_dialog = None;
        self.placeholder_notice = "已关闭升级提示".to_string();
    }

    /// 跳过当前弹窗中的升级版本，并持久化到配置。
    pub fn skip_available_upgrade(&mut self) {
        let Some(UpgradeDialogState::Available { upgrade }) = self.upgrade_dialog.clone() else {
            return;
        };
        self.config.upgrade.skipped_version = Some(upgrade.version.clone());
        self.upgrade_message = Some(format!("已跳过版本 {}", upgrade.version));
        self.placeholder_notice = format!("已跳过版本 {}", upgrade.version);
        self.upgrade_dialog = None;
        self.persist_config_or_report();
    }

    /// 下载、校验并安装当前弹窗中的升级版本，成功后自动重启 Argus。
    pub fn install_available_upgrade(&mut self, cx: &mut Context<Self>) {
        if self.is_upgrade_installing {
            self.upgrade_message = Some("升级安装正在进行".to_string());
            return;
        }
        let Some(UpgradeDialogState::Available { upgrade }) = self.upgrade_dialog.clone() else {
            self.upgrade_dialog = Some(UpgradeDialogState::Failed {
                version: None,
                message: "没有可安装的新版本".to_string(),
            });
            return;
        };

        let version = upgrade.version.clone();
        self.is_upgrade_installing = true;
        self.upgrade_message = Some(format!("正在下载版本 {version}..."));
        self.placeholder_notice = format!("正在下载版本 {version}");
        self.upgrade_dialog = Some(UpgradeDialogState::Progress {
            version: version.clone(),
            message: "正在下载并校验升级包...".to_string(),
        });
        let upgrade_config = self.config.upgrade.clone();

        cx.spawn(async move |view, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    let service = UpgradeService::runtime(&upgrade_config);
                    let prepared = service.download_and_prepare(&upgrade)?;
                    service.install_prepared_upgrade(&prepared)
                })
                .await;

            view.update(cx, |app, cx| {
                app.is_upgrade_installing = false;
                match result {
                    Ok(()) => {
                        app.upgrade_message = Some(format!("版本 {version} 已安装，正在重启"));
                        app.placeholder_notice = format!("版本 {version} 已安装，正在重启");
                        app.upgrade_dialog = Some(UpgradeDialogState::Progress {
                            version: version.clone(),
                            message: "已启动新版本，正在退出旧进程...".to_string(),
                        });
                        cx.notify();
                        cx.quit();
                    }
                    Err(error) => {
                        let message = error.to_string();
                        app.upgrade_message = Some(message.clone());
                        app.placeholder_notice = format!("升级安装失败：{message}");
                        app.upgrade_dialog = Some(UpgradeDialogState::Failed {
                            version: Some(version),
                            message,
                        });
                        cx.notify();
                    }
                }
            })
            .ok();
        })
        .detach();
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

    /// 只保留指定 SFTP 文件管理 tab 的会话；非 SFTP tab 会断开全部文件管理会话。
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

    /// 切换到指定标签页。
    pub fn activate_tab(&mut self, tab_id: usize) {
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

    /// 让来源树视觉选中跟随当前日志标签，不执行展开、过滤清理或多选清理等业务动作。
    ///
    /// 说明：这里是 UI 视图同步，不应触发日志读取、目录懒加载或来源树结构变更。
    fn sync_source_tree_selection_from_active_tab(&mut self) {
        let TabKind::LogSource { source_id, .. } = self.active_tab_kind() else {
            return;
        };

        let selected = self.source_registry.select(source_id).is_some();
        if selected {
            self.scroll_source_into_view(source_id);
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

    /// 在搜索结果面板指定窗口坐标打开批量操作右键菜单。
    pub fn open_search_results_context_menu(&mut self, anchor: Point<Pixels>) {
        if self.log_search.result_groups.is_empty() {
            self.placeholder_notice = "暂无可操作的搜索结果分组".to_string();
            return;
        }

        self.tab_menu_scroll = UniformListScrollHandle::new();
        self.active_menu = Some(ActiveMenu {
            kind: ActiveMenuKind::SearchResultsPanel,
            anchor,
        });
    }

    /// 在来源树指定窗口坐标打开日志候选或 Runtime 目录节点右键菜单。
    pub fn open_source_tree_context_menu(&mut self, source_id: SourceId, anchor: Point<Pixels>) {
        let Some(source) = self.source_registry.node(source_id) else {
            self.placeholder_notice = "未找到可操作的来源节点".to_string();
            return;
        };
        if !self.source_supports_any_analysis_context_menu(source_id) {
            self.placeholder_notice = format!("{} 不是可分析的日志候选", source.label);
            return;
        }

        self.tab_menu_scroll = UniformListScrollHandle::new();
        self.active_menu = Some(ActiveMenu {
            kind: ActiveMenuKind::SourceTree { source_id },
            anchor,
        });
    }

    /// 在链接树指定窗口坐标打开目录或 SSH 链接右键菜单。
    pub fn open_connection_tree_context_menu(
        &mut self,
        node_id: ConnectionNodeId,
        anchor: Point<Pixels>,
    ) {
        if !self.config.connections.is_directory(node_id)
            && !self.config.connections.is_link(node_id)
        {
            self.placeholder_notice = "未找到可操作的连接节点".to_string();
            return;
        }

        self.selected_connection_node_id = Some(node_id);
        self.tab_menu_scroll = UniformListScrollHandle::new();
        self.active_menu = Some(ActiveMenu {
            kind: ActiveMenuKind::ConnectionTree { node_id },
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
            ActiveMenuKind::SearchResultsPanel => vec![
                MenuEntry::new("全部展开", MenuAction::ExpandAllSearchResults),
                MenuEntry::new("全部收起", MenuAction::CollapseAllSearchResults),
            ],
            ActiveMenuKind::SourceTree { source_id } => {
                let mut entries = Vec::new();
                if self.source_supports_jstack_analysis(source_id) {
                    entries.push(MenuEntry::new(
                        "Jstack线程日志分析",
                        MenuAction::OpenJstackAnalysis { source_id },
                    ));
                }
                if self.source_supports_runtime_analysis(source_id) {
                    entries.push(MenuEntry::new(
                        "Runtime日志解析",
                        MenuAction::OpenRuntimeAnalysis { source_id },
                    ));
                }
                entries
            }
            ActiveMenuKind::ConnectionTree { node_id } => {
                let (edit_label, delete_label) = if self.config.connections.is_directory(node_id) {
                    ("编辑目录", "删除目录")
                } else {
                    ("编辑链接", "删除链接")
                };
                vec![
                    MenuEntry::new(edit_label, MenuAction::EditConnectionNode { node_id }),
                    MenuEntry::new(delete_label, MenuAction::DeleteConnectionNode { node_id })
                        .danger(),
                ]
            }
            ActiveMenuKind::TerminalContext { session_id } => vec![MenuEntry::new(
                "文件管理",
                MenuAction::OpenSftpFileManager {
                    terminal_session_id: session_id,
                },
            )],
            ActiveMenuKind::SftpEntry { session_id } => vec![
                MenuEntry::new("下载", MenuAction::DownloadSftpSelection { session_id }),
                MenuEntry::new("重命名", MenuAction::RenameSftpSelection { session_id }),
                MenuEntry::new("删除", MenuAction::DeleteSftpSelection { session_id }).danger(),
            ],
        }
    }

    /// 执行通用菜单动作，并在动作完成后关闭菜单。
    pub fn handle_menu_action(&mut self, action: MenuAction) {
        match action {
            MenuAction::ActivateTab { tab_id } => self.activate_tab(tab_id),
            MenuAction::CloseTab { tab_id } => self.close_tab(tab_id),
            MenuAction::CloseOtherTabs { tab_id } => self.close_other_tabs(tab_id),
            MenuAction::CloseAllTabs => self.close_all_tabs(),
            MenuAction::ExpandAllSearchResults => self.expand_all_search_result_groups(),
            MenuAction::CollapseAllSearchResults => self.collapse_all_search_result_groups(),
            MenuAction::OpenJstackAnalysis { .. } => {
                self.placeholder_notice = "Jstack 分析需要从界面菜单触发".to_string();
            }
            MenuAction::OpenRuntimeAnalysis { .. } => {
                self.placeholder_notice = "Runtime 分析需要从界面菜单触发".to_string();
            }
            MenuAction::EditConnectionNode { .. } => {
                self.placeholder_notice = "连接编辑需要从界面菜单触发".to_string();
            }
            MenuAction::DeleteConnectionNode { node_id } => {
                self.request_delete_connection_node(node_id);
            }
            MenuAction::OpenSftpFileManager { .. } => {
                self.placeholder_notice = "文件管理需要从界面菜单触发".to_string();
            }
            MenuAction::DownloadSftpSelection { .. } => {
                self.placeholder_notice = "SFTP 下载需要从界面菜单触发".to_string();
            }
            MenuAction::RenameSftpSelection { session_id } => {
                self.open_sftp_rename_dialog(session_id);
            }
            MenuAction::DeleteSftpSelection { session_id } => {
                self.request_delete_sftp_entry(session_id);
            }
        }

        self.close_active_menu();
    }

    /// 执行需要 GPUI 上下文的菜单动作；普通动作复用无上下文分发。
    pub fn handle_menu_action_with_context(&mut self, action: MenuAction, cx: &mut Context<Self>) {
        match action {
            MenuAction::ActivateTab { tab_id } => {
                self.activate_tab_with_context(tab_id, cx);
                self.close_active_menu();
            }
            MenuAction::CloseTab { tab_id } => {
                self.close_tab_with_context(tab_id, cx);
                self.close_active_menu();
            }
            MenuAction::CloseOtherTabs { tab_id } => {
                self.close_other_tabs_with_context(tab_id, cx);
                self.close_active_menu();
            }
            MenuAction::CloseAllTabs => {
                self.close_all_tabs_with_context(cx);
                self.close_active_menu();
            }
            MenuAction::OpenJstackAnalysis { source_id } => {
                self.open_jstack_analysis_tab(source_id, cx);
                self.close_active_menu();
            }
            MenuAction::OpenRuntimeAnalysis { source_id } => {
                self.open_runtime_analysis_tab(source_id, cx);
                self.close_active_menu();
            }
            MenuAction::EditConnectionNode { node_id } => {
                self.open_edit_connection_node_window(node_id, cx);
                self.close_active_menu();
            }
            MenuAction::DeleteConnectionNode { node_id } => {
                self.request_delete_connection_node(node_id);
                self.close_active_menu();
            }
            MenuAction::OpenSftpFileManager {
                terminal_session_id,
            } => {
                self.open_sftp_file_manager_from_terminal(terminal_session_id, cx);
                self.close_active_menu();
            }
            MenuAction::DownloadSftpSelection { session_id } => {
                self.choose_sftp_download_target(session_id, cx);
                self.close_active_menu();
            }
            MenuAction::RenameSftpSelection { session_id } => {
                self.open_sftp_rename_dialog(session_id);
                self.close_active_menu();
            }
            MenuAction::DeleteSftpSelection { session_id } => {
                self.request_delete_sftp_entry(session_id);
                self.close_active_menu();
            }
            other => self.handle_menu_action(other),
        }
    }

    /// 判断来源节点是否至少支持一种右键分析动作。
    fn source_supports_any_analysis_context_menu(&self, source_id: SourceId) -> bool {
        self.source_supports_jstack_analysis(source_id)
            || self.source_supports_runtime_analysis(source_id)
    }

    /// 判断来源节点是否是分析功能可以展开收集的目录。
    fn source_is_analysis_directory(&self, source_id: SourceId) -> bool {
        self.source_registry.node(source_id).is_some_and(|node| {
            matches!(
                node.kind,
                SourceKind::Directory | SourceKind::ArchiveDirectory
            )
        })
    }

    /// 判断来源节点是否是本地真实目录；本地目录可以直接交给后台递归文件系统。
    fn source_is_local_directory(&self, source_id: SourceId) -> bool {
        self.source_registry.node(source_id).is_some_and(|node| {
            matches!(node.kind, SourceKind::Directory)
                && matches!(node.location, SourceLocation::LocalPath(_))
        })
    }

    /// 判断来源节点是否是压缩包内目录；需要先加载子级再收集已加载后代文件。
    fn source_is_archive_directory(&self, source_id: SourceId) -> bool {
        self.source_registry
            .node(source_id)
            .is_some_and(|node| matches!(node.kind, SourceKind::ArchiveDirectory))
    }

    /// 判断来源节点是否支持 Jstack 线程日志分析入口。
    fn source_supports_jstack_analysis(&self, source_id: SourceId) -> bool {
        self.is_source_selectable_for_search_selection(source_id)
            || self.source_is_analysis_directory(source_id)
    }

    /// 判断来源节点是否支持 Runtime 日志解析入口。
    fn source_supports_runtime_analysis(&self, source_id: SourceId) -> bool {
        self.source_registry.node(source_id).is_some_and(|node| {
            node.kind.is_log_candidate()
                || self.is_source_selectable_for_search_selection(source_id)
                || self.source_is_analysis_directory(source_id)
        })
    }

    /// 确保压缩包内目录子级已经加载；未加载时先触发加载并记录待续做动作。
    fn ensure_source_directory_ready_for_analysis(
        &mut self,
        source_id: SourceId,
        pending_action: PendingSourceAnalysisAction,
        cx: &mut Context<Self>,
    ) -> bool {
        if !self.source_is_archive_directory(source_id) {
            return true;
        }

        let Some(node) = self.source_registry.node(source_id).cloned() else {
            self.placeholder_notice = "未找到可分析的来源目录".to_string();
            return false;
        };
        if node.metadata.children_loaded {
            return true;
        }

        self.pending_source_analysis_after_load = Some(pending_action);
        if node.metadata.is_loading {
            self.placeholder_notice = format!("正在加载 {} 的子级，完成后自动开始分析", node.label);
            return false;
        }

        self.start_source_child_load(source_id, node.clone(), cx);
        self.placeholder_notice = format!("正在加载 {} 的子级，完成后自动开始分析", node.label);
        false
    }

    /// 创建 Jstack 分析标签页，并启动后台读取与聚合任务。
    pub fn open_jstack_analysis_tab(&mut self, source_id: SourceId, cx: &mut Context<Self>) {
        if !self.ensure_source_directory_ready_for_analysis(
            source_id,
            PendingSourceAnalysisAction::Jstack { source_id },
            cx,
        ) {
            return;
        }

        let target_ids = self.jstack_source_ids_for_context(source_id);
        if target_ids.is_empty() {
            self.placeholder_notice = "请选择至少一个可分析的 Jstack 日志文件".to_string();
            return;
        }

        let targets = self.jstack_targets_from_source_ids(&target_ids);
        if targets.is_empty() {
            self.placeholder_notice = "未找到可读取的 Jstack 日志来源".to_string();
            return;
        }

        let background_targets = targets.clone();
        let Some((analysis_id, generation)) = self.create_jstack_analysis_tab_state(targets) else {
            self.placeholder_notice = "未找到可读取的 Jstack 日志来源".to_string();
            return;
        };

        let default_encoding = self.selected_encoding.clone();
        let loader_config = self.config.loader.clone();
        cx.spawn(async move |view, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    analyze_jstack_targets(background_targets, default_encoding, loader_config)
                })
                .await;

            view.update(cx, |app, cx| {
                app.apply_jstack_analysis_result(analysis_id, generation, result);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// 返回指定 Jstack 分析状态。
    pub fn jstack_analysis_state(&self, analysis_id: usize) -> Option<&JstackAnalysisState> {
        self.jstack_analyses.get(&analysis_id)
    }

    /// 返回当前设置页配置的 Jstack 线程过滤器。
    pub fn jstack_thread_filter(&self) -> JstackThreadFilter {
        JstackThreadFilter::from_raw(
            &self.config.log_display.jstack_thread_name_filters,
            &self.config.log_display.jstack_stack_segment_filters,
        )
    }

    /// 根据当前配置重建所有 Jstack 分析页的可见行缓存。
    pub fn rebuild_all_jstack_visible_row_caches(&mut self) {
        let thread_filter = self.jstack_thread_filter();
        for state in self.jstack_analyses.values_mut() {
            state.rebuild_visible_row_cache(&thread_filter);
        }
    }

    /// 根据频率矩阵格子定位线程详情，并延迟组装完整堆栈记录。
    ///
    /// 参数说明：
    /// - `analysis_id`：Jstack 分析页签 ID。
    /// - `row_index`：频率矩阵中的线程行索引。
    /// - `active_snapshot_index`：用户点击的快照列索引。
    /// - `active_occurrence_index`：同快照内重复线程名的出现序号。
    /// - `cx`：主应用上下文，用于继续打开详情窗口。
    pub fn open_jstack_thread_detail_for_cell(
        &mut self,
        analysis_id: usize,
        row_index: usize,
        active_snapshot_index: usize,
        active_occurrence_index: usize,
        cx: &mut Context<Self>,
    ) {
        let detail_result: std::result::Result<(JstackThreadDetail, String), String> = (|| {
            let Some(state) = self.jstack_analyses.get(&analysis_id) else {
                return Err("未找到 Jstack 分析结果".to_string());
            };
            let JstackAnalysisTaskState::Ready(result) = &state.task_state else {
                return Err("Jstack 分析尚未完成".to_string());
            };
            let Some(row) = result.rows.get(row_index) else {
                return Err("未找到当前线程行".to_string());
            };
            let thread_name = row.thread_name.clone();
            let selected_cell_key = jstack_cell_selection_key(row_index, active_snapshot_index);

            // 详情窗口用于对比同一线程在不同日志快照中的表现；同一快照内重复出现时只取
            // 一个代表堆栈，避免单个文件里的多段采样被误看成多个日志文件。
            let occurrences = jstack_detail_occurrences_for_visible_cells(
                row,
                &state.active_states,
                active_snapshot_index,
                active_occurrence_index,
            );
            if occurrences.is_empty() {
                return Err("当前线程没有可展示的堆栈详情".to_string());
            }

            Ok((
                JstackThreadDetail {
                    thread_name,
                    thread_id: row.thread_id.clone(),
                    occurrences,
                },
                selected_cell_key,
            ))
        })();

        match detail_result {
            Ok((detail, selected_cell_key)) => {
                if let Some(state) = self.jstack_analyses.get_mut(&analysis_id) {
                    state.selected_cell_key = Some(selected_cell_key);
                    state.thread_name_selection = None;
                    state.thread_name_selection_drag = None;
                }
                self.open_jstack_thread_detail_window(
                    detail,
                    active_snapshot_index,
                    active_occurrence_index,
                    cx,
                );
            }
            Err(message) => {
                self.placeholder_notice = message;
            }
        }
    }

    /// 打开 Jstack 线程详情独立窗口。
    ///
    /// 参数说明：
    /// - `detail`：当前线程跨快照的完整堆栈记录。
    /// - `active_snapshot_index`：用户点击的快照序号，用于定位详情初始页。
    /// - `active_occurrence_index`：同快照内线程出现序号，用于重复线程名时定位具体堆栈。
    /// - `cx`：主应用上下文，用于创建无系统标题栏窗口。
    pub fn open_jstack_thread_detail_window(
        &mut self,
        detail: JstackThreadDetail,
        active_snapshot_index: usize,
        active_occurrence_index: usize,
        cx: &mut Context<Self>,
    ) {
        if detail.occurrences.is_empty() {
            self.placeholder_notice = "当前线程没有可展示的堆栈详情".to_string();
            return;
        }

        let initial_theme = self.theme.clone();
        let bounds = Bounds::centered(
            None,
            size(
                px(JSTACK_THREAD_DETAIL_WINDOW_WIDTH),
                px(JSTACK_THREAD_DETAIL_WINDOW_HEIGHT),
            ),
            cx,
        );
        let window_options = WindowOptions {
            titlebar: None,
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            window_min_size: Some(size(
                px(JSTACK_THREAD_DETAIL_WINDOW_MIN_WIDTH),
                px(JSTACK_THREAD_DETAIL_WINDOW_MIN_HEIGHT),
            )),
            ..Default::default()
        };

        let thread_name = detail.display_label();
        match cx.open_window(window_options, move |_, cx| {
            cx.new(|cx| {
                JstackThreadDetailWindow::new(
                    initial_theme,
                    detail,
                    active_snapshot_index,
                    active_occurrence_index,
                    cx,
                )
            })
        }) {
            Ok(_) => {
                self.placeholder_notice = format!("已打开线程详情：{thread_name}");
            }
            Err(error) => {
                self.placeholder_notice = format!("打开线程详情失败：{error}");
            }
        }
    }

    /// 打开或更新 Jstack 方块内部悬浮气泡。
    ///
    /// 参数说明：
    /// - `preview`：当前方块的稳定 key、位置和预览内容。
    pub fn show_jstack_cell_hover_preview(&mut self, preview: JstackCellHoverPreview) {
        self.jstack_cell_hover_preview = Some(preview);
    }

    /// 清理 Jstack 方块内部悬浮气泡。
    pub fn clear_jstack_cell_hover_preview(&mut self) {
        self.jstack_cell_hover_preview = None;
    }

    /// 创建 Jstack 分析 tab 和加载状态；后台任务由调用方负责启动。
    fn create_jstack_analysis_tab_state(
        &mut self,
        targets: Vec<JstackAnalysisTarget>,
    ) -> Option<(usize, usize)> {
        if targets.is_empty() {
            return None;
        }

        let analysis_id = self.next_jstack_analysis_id;
        self.next_jstack_analysis_id = self.next_jstack_analysis_id.saturating_add(1);
        let title = if targets.len() > 1 {
            format!("Jstack分析({})", targets.len())
        } else {
            "Jstack分析".to_string()
        };
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
            tab.title = title.clone();
            tab.kind = TabKind::JstackAnalysis { analysis_id };
        }
        self.active_tab_id = tab_id;
        self.log_tab_view_states.remove(&tab_id);

        let generation = 1;
        self.jstack_analyses.insert(
            analysis_id,
            JstackAnalysisState {
                id: analysis_id,
                title: title.clone(),
                targets,
                generation,
                active_states: BTreeSet::from([JstackThreadState::Runnable]),
                is_thread_filter_enabled: true,
                thread_name_selection: None,
                thread_name_selection_drag: None,
                selected_cell_key: None,
                visible_row_indices: Vec::new(),
                filtered_row_count: 0,
                row_scroll: UniformListScrollHandle::new(),
                task_state: JstackAnalysisTaskState::Loading {
                    message: "正在分析 Jstack 日志文件".to_string(),
                },
            },
        );
        self.placeholder_notice = format!("已创建 {title} 页签");

        Some((analysis_id, generation))
    }

    /// 切换 Jstack 分析结果中的线程状态筛选项。
    pub fn toggle_jstack_state_filter(
        &mut self,
        analysis_id: usize,
        thread_state: JstackThreadState,
    ) {
        let thread_filter = self.jstack_thread_filter();
        let Some(state) = self.jstack_analyses.get_mut(&analysis_id) else {
            self.placeholder_notice = "未找到 Jstack 分析结果".to_string();
            return;
        };

        if !state.active_states.insert(thread_state) {
            state.active_states.remove(&thread_state);
        }
        state.rebuild_visible_row_cache(&thread_filter);
        state.row_scroll = UniformListScrollHandle::new();
        self.placeholder_notice = if state.active_states.is_empty() {
            "已隐藏全部 Jstack 线程状态".to_string()
        } else {
            let labels = state
                .active_states
                .iter()
                .map(|state| state.label())
                .collect::<Vec<_>>()
                .join(", ");
            format!("Jstack 状态筛选：{labels}")
        };
    }

    /// 开始在 Jstack 分析矩阵左侧线程名列中选择文本。
    ///
    /// 参数说明：
    /// - `analysis_id`：分析页 ID。
    /// - `thread_identity`：内部稳定线程身份，包含线程名和线程 ID。
    /// - `thread_name`：当前行显示的线程名文本。
    /// - `character_index`：鼠标按下位置命中的字符列。
    /// - `granularity`：按点击次数决定的选择粒度。
    pub fn begin_jstack_thread_name_selection(
        &mut self,
        analysis_id: usize,
        thread_identity: String,
        thread_name: String,
        character_index: usize,
        granularity: TextSelectionGranularity,
    ) {
        let Some(state) = self.jstack_analyses.get_mut(&analysis_id) else {
            self.placeholder_notice = "未找到 Jstack 分析结果".to_string();
            return;
        };

        let anchor_range =
            jstack_thread_name_range_for_granularity(&thread_name, character_index, granularity);
        state.thread_name_selection = Some(JstackThreadNameSelection {
            thread_identity: thread_identity.clone(),
            thread_name: thread_name.clone(),
            anchor: anchor_range.start,
            focus: anchor_range.end,
        });
        state.thread_name_selection_drag = Some(JstackThreadNameSelectionDrag {
            thread_identity,
            thread_name,
            anchor_range,
            granularity,
        });
        state.selected_cell_key = None;
    }

    /// 拖拽更新 Jstack 分析矩阵左侧线程名列中的文本选区。
    ///
    /// 返回值：本次拖拽是否命中当前分析页和当前线程行。
    pub fn update_jstack_thread_name_selection(
        &mut self,
        analysis_id: usize,
        thread_identity: &str,
        character_index: usize,
    ) -> bool {
        let Some(state) = self.jstack_analyses.get_mut(&analysis_id) else {
            return false;
        };
        let Some(drag) = state.thread_name_selection_drag.clone() else {
            return false;
        };
        if drag.thread_identity != thread_identity {
            return false;
        }

        let focus_range = jstack_thread_name_range_for_granularity(
            &drag.thread_name,
            character_index,
            drag.granularity,
        );
        state.thread_name_selection = Some(JstackThreadNameSelection {
            thread_identity: drag.thread_identity,
            thread_name: drag.thread_name,
            anchor: drag.anchor_range.start.min(focus_range.start),
            focus: drag.anchor_range.end.max(focus_range.end),
        });
        true
    }

    /// 结束 Jstack 线程名文本选择；如果没有选中字符则清理空选区。
    ///
    /// 返回值：当前分析页是否存在需要结束的拖拽状态。
    pub fn finish_jstack_thread_name_selection(&mut self, analysis_id: usize) -> bool {
        let Some(state) = self.jstack_analyses.get_mut(&analysis_id) else {
            return false;
        };
        let had_drag = state.thread_name_selection_drag.take().is_some();
        if state
            .thread_name_selection
            .as_ref()
            .and_then(JstackThreadNameSelection::normalized_range)
            .is_none()
        {
            state.thread_name_selection = None;
        }
        had_drag
    }

    /// 复制当前 Jstack 分析页左侧线程名列中拖选的文本。
    pub fn copy_selected_jstack_thread_name(&mut self, analysis_id: usize, cx: &mut Context<Self>) {
        let Some((thread_name, range)) = self
            .jstack_analyses
            .get(&analysis_id)
            .and_then(|state| state.thread_name_selection.as_ref())
            .and_then(|selection| {
                selection
                    .normalized_range()
                    .map(|range| (selection.thread_name.clone(), range))
            })
        else {
            self.placeholder_notice = "请先拖选一个 Jstack 线程名片段".to_string();
            return;
        };

        let selected_text = slice_character_range(&thread_name, range);
        let app_context: &gpui::App = (&*cx).borrow();
        app_context.write_to_clipboard(ClipboardItem::new_string(selected_text.clone()));
        self.placeholder_notice = format!("已复制线程名片段：{selected_text}");
    }

    /// 切换 Jstack 分析页是否应用设置页中的线程堆栈过滤规则。
    pub fn toggle_jstack_thread_filter(&mut self, analysis_id: usize) {
        let thread_filter = self.jstack_thread_filter();
        let Some(state) = self.jstack_analyses.get_mut(&analysis_id) else {
            self.placeholder_notice = "未找到 Jstack 分析结果".to_string();
            return;
        };

        state.is_thread_filter_enabled = !state.is_thread_filter_enabled;
        state.rebuild_visible_row_cache(&thread_filter);
        state.row_scroll = UniformListScrollHandle::new();
        self.placeholder_notice = if state.is_thread_filter_enabled {
            "已启用 Jstack 配置过滤".to_string()
        } else {
            "已关闭 Jstack 配置过滤".to_string()
        };
    }

    /// 根据右键来源节点生成分析输入；多选命中时沿用多选，否则切换到单文件。
    fn jstack_source_ids_for_context(&mut self, source_id: SourceId) -> Vec<SourceId> {
        if self.source_registry.node(source_id).is_none() {
            return Vec::new();
        }
        if self.source_is_local_directory(source_id) {
            return vec![source_id];
        }
        if self.source_is_archive_directory(source_id) {
            return self.loaded_descendant_analysis_source_ids(source_id);
        }
        if !self.source_supports_jstack_analysis(source_id) {
            return Vec::new();
        }

        if !self.selected_search_source_ids.contains(&source_id) {
            self.selected_search_source_ids.clear();
            self.selected_search_source_ids.insert(source_id);
            self.last_source_selection_anchor = Some(source_id);
            return vec![source_id];
        }

        let selected_ids = self.selected_search_source_ids.clone();
        let mut ordered_ids = self
            .visible_source_ids()
            .iter()
            .filter(|visible_id| selected_ids.contains(visible_id))
            .filter(|visible_id| self.is_source_selectable_for_search_selection(**visible_id))
            .copied()
            .collect::<Vec<_>>();
        for selected_id in selected_ids {
            if !ordered_ids.contains(&selected_id)
                && self.is_source_selectable_for_search_selection(selected_id)
            {
                ordered_ids.push(selected_id);
            }
        }

        ordered_ids
    }

    /// 收集已加载目录下所有可分析文件来源，保持来源树展示顺序。
    fn loaded_descendant_analysis_source_ids(&self, parent_id: SourceId) -> Vec<SourceId> {
        let mut source_ids = Vec::new();
        self.collect_loaded_descendant_analysis_source_ids(parent_id, &mut source_ids);
        source_ids
    }

    /// 递归收集已加载后代文件；未加载目录不主动展开，避免在纯读取阶段阻塞 UI。
    fn collect_loaded_descendant_analysis_source_ids(
        &self,
        parent_id: SourceId,
        source_ids: &mut Vec<SourceId>,
    ) {
        for child_id in self.source_registry.child_ids(parent_id).iter().copied() {
            let Some(child) = self.source_registry.node(child_id) else {
                continue;
            };

            if child.kind.is_log_candidate()
                || self.is_source_selectable_for_search_selection(child_id)
            {
                source_ids.push(child_id);
            }

            if child.kind.can_expand() && child.metadata.children_loaded {
                self.collect_loaded_descendant_analysis_source_ids(child_id, source_ids);
            }
        }
    }

    /// 将来源树节点转换为 Jstack 分析目标。
    fn jstack_targets_from_source_ids(&self, source_ids: &[SourceId]) -> Vec<JstackAnalysisTarget> {
        source_ids
            .iter()
            .filter_map(|source_id| {
                let node = self.source_registry.node(*source_id)?;
                if !self.source_supports_jstack_analysis(*source_id) {
                    return None;
                }
                Some(JstackAnalysisTarget {
                    source_id: *source_id,
                    location: node.location.clone(),
                    archive_probe_node: self.jstack_archive_probe_node(*source_id),
                    label: node.label.clone(),
                    path: node.location.display_path(),
                })
            })
            .collect()
    }

    /// 为 Jstack 分析生成待探测压缩包快照；已识别日志节点不需要额外探测。
    fn jstack_archive_probe_node(&self, source_id: SourceId) -> Option<SourceTreeNode> {
        if !self.is_source_selectable_for_search_selection(source_id) {
            return None;
        }

        let node = self.source_registry.node(source_id)?;
        (!node.kind.is_log_candidate()).then(|| node.clone())
    }

    /// 应用后台 Jstack 分析结果，过期 generation 会被忽略。
    fn apply_jstack_analysis_result(
        &mut self,
        analysis_id: usize,
        generation: usize,
        result: JstackAnalysisResult,
    ) {
        let thread_filter = self.jstack_thread_filter();
        let Some(state) = self.jstack_analyses.get_mut(&analysis_id) else {
            return;
        };
        if state.generation != generation {
            return;
        }

        let thread_count = result.thread_count();
        let snapshot_count = result.snapshot_count();
        let skipped_count = result.skipped_count();
        state.task_state = JstackAnalysisTaskState::Ready(result);
        state.rebuild_visible_row_cache(&thread_filter);
        self.placeholder_notice = format!(
            "Jstack 分析完成：{snapshot_count} 个快照，{thread_count} 个线程，跳过 {skipped_count} 个文件"
        );
    }

    /// 创建 Runtime 分析标签页，并启动后台读取与聚合任务。
    pub fn open_runtime_analysis_tab(&mut self, source_id: SourceId, cx: &mut Context<Self>) {
        if !self.ensure_source_directory_ready_for_analysis(
            source_id,
            PendingSourceAnalysisAction::Runtime { source_id },
            cx,
        ) {
            return;
        }

        let targets = self.runtime_targets_for_context(source_id);
        if targets.is_empty() {
            self.placeholder_notice = "请选择至少一个 Runtime 日志文件或本地目录".to_string();
            return;
        }

        let background_targets = targets.clone();
        let Some((analysis_id, generation)) = self.create_runtime_analysis_tab_state(targets)
        else {
            self.placeholder_notice = "未找到可读取的 Runtime 日志来源".to_string();
            return;
        };

        let default_encoding = self.selected_encoding.clone();
        let loader_config = self.config.loader.clone();
        cx.spawn(async move |view, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    analyze_runtime_targets(background_targets, default_encoding, loader_config)
                })
                .await;

            view.update(cx, |app, cx| {
                app.apply_runtime_analysis_result(analysis_id, generation, result, cx);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// 返回指定 Runtime 分析状态。
    pub fn runtime_analysis_state(&self, analysis_id: usize) -> Option<&RuntimeAnalysisState> {
        self.runtime_analyses.get(&analysis_id)
    }

    /// 返回指定 Runtime 分析状态的可变引用。
    pub fn runtime_analysis_state_mut(
        &mut self,
        analysis_id: usize,
    ) -> Option<&mut RuntimeAnalysisState> {
        self.runtime_analyses.get_mut(&analysis_id)
    }

    /// 创建 Runtime 分析 tab 和加载状态；后台任务由调用方负责启动。
    fn create_runtime_analysis_tab_state(
        &mut self,
        targets: Vec<RuntimeAnalysisTarget>,
    ) -> Option<(usize, usize)> {
        if targets.is_empty() {
            return None;
        }

        let analysis_id = self.next_runtime_analysis_id;
        self.next_runtime_analysis_id = self.next_runtime_analysis_id.saturating_add(1);
        let title = if targets.len() > 1 {
            format!("Runtime分析({})", targets.len())
        } else {
            "Runtime分析".to_string()
        };
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
            tab.title = title.clone();
            tab.kind = TabKind::RuntimeAnalysis { analysis_id };
        }
        self.active_tab_id = tab_id;
        self.log_tab_view_states.remove(&tab_id);

        let generation = 1;
        self.runtime_analyses.insert(
            analysis_id,
            RuntimeAnalysisState {
                id: analysis_id,
                title: title.clone(),
                targets,
                generation,
                view: RuntimeAnalysisView::Summary,
                result_type: RuntimeAnalysisResultType::Statistics,
                summary_sort_key: RuntimeSummarySortKey::RequestCount,
                summary_sort_direction: RuntimeSortDirection::Descending,
                request_sort_key: RuntimeRequestSortKey::RequestTime,
                request_sort_direction: RuntimeSortDirection::Descending,
                sql_sort_key: RuntimeSqlSortKey::ExecuteDuration,
                sql_sort_direction: RuntimeSortDirection::Descending,
                filter_keyword_input: SettingsTextInputState::default(),
                filter_username_input: SettingsTextInputState::default(),
                filter_start_time_input: SettingsTextInputState::default(),
                filter_end_time_input: SettingsTextInputState::default(),
                applied_filter_keyword: String::new(),
                applied_filter_username: String::new(),
                applied_filter_start_time: String::new(),
                applied_filter_end_time: String::new(),
                filter_input_generation: 0,
                filter_task_generation: 0,
                is_filter_pending: false,
                is_filter_computing: false,
                open_time_picker: None,
                cell_selection: None,
                cell_selection_drag: None,
                hovered_sql_cell: None,
                sql_text_dialog: None,
                summary_scroll: UniformListScrollHandle::new(),
                request_scroll: UniformListScrollHandle::new(),
                sql_scroll: UniformListScrollHandle::new(),
                sql_frequency_scroll: UniformListScrollHandle::new(),
                sql_frequency_detail_scroll: UniformListScrollHandle::new(),
                slow_sql_scroll: UniformListScrollHandle::new(),
                sql_frequency_detail_sql: None,
                slow_sql_detail_sql: None,
                runtime_filter_rows_cache: None,
                sql_frequency_rows_task_generation: 0,
                slow_sql_rows_task_generation: 0,
                is_sql_frequency_rows_computing: false,
                is_slow_sql_rows_computing: false,
                sql_frequency_rows_computing_filter: None,
                slow_sql_rows_computing_filter: None,
                sql_frequency_rows_cache: RefCell::new(None),
                sql_frequency_detail_rows_cache: RefCell::new(None),
                slow_sql_rows_cache: RefCell::new(None),
                scrollbar_drag: None,
                task_state: RuntimeAnalysisTaskState::Loading {
                    message: "正在分析 Runtime 日志文件".to_string(),
                },
            },
        );
        self.placeholder_notice = format!("已创建 {title} 页签");

        Some((analysis_id, generation))
    }

    /// 根据右键来源节点生成 Runtime 分析输入；文件多选命中时沿用多选，目录直接递归解析。
    fn runtime_targets_for_context(&mut self, source_id: SourceId) -> Vec<RuntimeAnalysisTarget> {
        let Some(node) = self.source_registry.node(source_id) else {
            return Vec::new();
        };

        if self.source_is_local_directory(source_id) {
            return self.runtime_targets_from_source_ids(&[source_id]);
        }
        if self.source_is_archive_directory(source_id) {
            let source_ids = self.loaded_descendant_analysis_source_ids(source_id);
            return self.runtime_targets_from_source_ids(&source_ids);
        }

        if !node.kind.is_log_candidate()
            && !self.is_source_selectable_for_search_selection(source_id)
        {
            return Vec::new();
        }

        if !self.selected_search_source_ids.contains(&source_id) {
            self.selected_search_source_ids.clear();
            self.selected_search_source_ids.insert(source_id);
            self.last_source_selection_anchor = Some(source_id);
            return self.runtime_targets_from_source_ids(&[source_id]);
        }

        let selected_ids = self.selected_search_source_ids.clone();
        let mut ordered_ids = self
            .visible_source_ids()
            .iter()
            .filter(|visible_id| selected_ids.contains(visible_id))
            .filter(|visible_id| {
                self.source_registry
                    .node(**visible_id)
                    .is_some_and(|node| node.kind.is_log_candidate())
                    || self.is_source_selectable_for_search_selection(**visible_id)
            })
            .copied()
            .collect::<Vec<_>>();
        for selected_id in selected_ids {
            if !ordered_ids.contains(&selected_id)
                && (self
                    .source_registry
                    .node(selected_id)
                    .is_some_and(|node| node.kind.is_log_candidate())
                    || self.is_source_selectable_for_search_selection(selected_id))
            {
                ordered_ids.push(selected_id);
            }
        }

        self.runtime_targets_from_source_ids(&ordered_ids)
    }

    /// 将来源树节点转换为 Runtime 分析目标。
    fn runtime_targets_from_source_ids(
        &self,
        source_ids: &[SourceId],
    ) -> Vec<RuntimeAnalysisTarget> {
        source_ids
            .iter()
            .filter_map(|source_id| {
                let node = self.source_registry.node(*source_id)?;
                let kind = if matches!(node.kind, SourceKind::Directory) {
                    if !matches!(node.location, SourceLocation::LocalPath(_)) {
                        return None;
                    }
                    RuntimeAnalysisTargetKind::Directory
                } else if node.kind.is_log_candidate()
                    || self.is_source_selectable_for_search_selection(*source_id)
                {
                    RuntimeAnalysisTargetKind::File
                } else {
                    return None;
                };

                Some(RuntimeAnalysisTarget {
                    source_id: *source_id,
                    location: node.location.clone(),
                    archive_probe_node: self.runtime_archive_probe_node(*source_id),
                    label: node.label.clone(),
                    path: node.location.display_path(),
                    kind,
                })
            })
            .collect()
    }

    /// 为 Runtime 分析生成待探测压缩包快照；已识别日志节点不需要额外探测。
    fn runtime_archive_probe_node(&self, source_id: SourceId) -> Option<SourceTreeNode> {
        let node = self.source_registry.node(source_id)?;
        (!node.kind.is_log_candidate() && self.is_source_selectable_for_search_selection(source_id))
            .then(|| node.clone())
    }

    /// 应用后台 Runtime 分析结果，过期 generation 会被忽略。
    fn apply_runtime_analysis_result(
        &mut self,
        analysis_id: usize,
        generation: usize,
        result: RuntimeAnalysisResult,
        cx: &mut Context<Self>,
    ) {
        let Some(state) = self.runtime_analyses.get_mut(&analysis_id) else {
            return;
        };
        if state.generation != generation {
            return;
        }

        let file_count = result.total_files;
        let request_count = result.request_count();
        let sql_count = result.total_sql_records;
        let skipped_count = result.skipped_count();
        state.task_state = RuntimeAnalysisTaskState::Ready(Arc::new(result));
        let pending_generation = state
            .is_filter_pending
            .then_some(state.filter_input_generation);
        self.placeholder_notice = format!(
            "Runtime 分析完成：{file_count} 个文件，{request_count} 个请求，{sql_count} 条 SQL，跳过 {skipped_count} 个文件"
        );
        if let Some(input_generation) = pending_generation {
            self.schedule_runtime_filter_apply(analysis_id, input_generation, cx);
        } else {
            self.ensure_runtime_sql_analysis_rows_for_current_type(analysis_id, cx);
        }
    }

    /// 标记 Runtime 过滤输入发生变化，并通过防抖任务延后应用。
    fn queue_runtime_filter_refresh(&mut self, analysis_id: usize, cx: &mut Context<Self>) {
        self.trigger_runtime_filter_refresh(analysis_id, Some(cx));
    }

    /// 标记 Runtime 过滤输入变化；有 UI 上下文时同时安排防抖任务。
    fn trigger_runtime_filter_refresh(
        &mut self,
        analysis_id: usize,
        cx: Option<&mut Context<Self>>,
    ) {
        if let Some(input_generation) = self.after_runtime_filter_changed(analysis_id)
            && let Some(cx) = cx
        {
            self.schedule_runtime_filter_apply(analysis_id, input_generation, cx);
        }
    }

    /// 启动 Runtime 过滤防抖任务；过期 generation 会在真正计算前被丢弃。
    fn schedule_runtime_filter_apply(
        &mut self,
        analysis_id: usize,
        input_generation: usize,
        cx: &mut Context<Self>,
    ) {
        cx.spawn(async move |view, cx| {
            Timer::after(Duration::from_millis(RUNTIME_FILTER_DEBOUNCE_MS)).await;
            view.update(cx, |app, cx| {
                app.start_runtime_filter_apply_if_current(analysis_id, input_generation, cx);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// 在防抖完成后启动真正的 Runtime 过滤后台计算。
    fn start_runtime_filter_apply_if_current(
        &mut self,
        analysis_id: usize,
        input_generation: usize,
        cx: &mut Context<Self>,
    ) {
        let Some(state) = self.runtime_analyses.get_mut(&analysis_id) else {
            return;
        };
        if state.filter_input_generation != input_generation {
            return;
        }

        let filter = runtime_filter_input_snapshot_from_state(state);
        let criteria = parse_runtime_analysis_filter_criteria(&filter);
        let current_filter = runtime_filter_applied_snapshot_from_state(state);
        if current_filter == filter && !state.is_filter_pending {
            return;
        }

        state.filter_task_generation = state.filter_task_generation.saturating_add(1);
        let task_generation = state.filter_task_generation;
        state.is_filter_pending = false;

        if !criteria.is_active() {
            apply_runtime_filter_snapshot_to_state(state, &filter);
            state.runtime_filter_rows_cache = None;
            state.is_filter_computing = false;
            reset_runtime_filter_result_view_state(state);
            self.placeholder_notice = "Runtime 过滤条件已更新".to_string();
            self.ensure_runtime_sql_analysis_rows_for_current_type(analysis_id, cx);
            return;
        }

        let RuntimeAnalysisTaskState::Ready(result) = &state.task_state else {
            state.is_filter_pending = true;
            state.is_filter_computing = false;
            return;
        };
        let result = result.clone();
        state.is_filter_computing = true;
        self.placeholder_notice = "正在后台应用 Runtime 过滤条件".to_string();

        cx.spawn(async move |view, cx| {
            let rows = cx
                .background_executor()
                .spawn(async move { build_runtime_analysis_filter_rows(result.as_ref(), filter) })
                .await;

            view.update(cx, |app, cx| {
                app.apply_runtime_filter_rows(
                    analysis_id,
                    input_generation,
                    task_generation,
                    rows,
                    cx,
                );
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// 应用后台构建好的 Runtime 过滤行缓存。
    fn apply_runtime_filter_rows(
        &mut self,
        analysis_id: usize,
        input_generation: usize,
        task_generation: usize,
        rows: RuntimeAnalysisFilterRows,
        cx: &mut Context<Self>,
    ) {
        let Some(state) = self.runtime_analyses.get_mut(&analysis_id) else {
            return;
        };
        if state.filter_input_generation != input_generation
            || state.filter_task_generation != task_generation
        {
            return;
        }

        apply_runtime_filter_snapshot_to_state(state, &rows.filter);
        state.runtime_filter_rows_cache = Some(rows);
        state.is_filter_pending = false;
        state.is_filter_computing = false;
        reset_runtime_filter_result_view_state(state);
        self.placeholder_notice = "Runtime 过滤条件已更新".to_string();
        self.ensure_runtime_sql_analysis_rows_for_current_type(analysis_id, cx);
    }

    /// 当前页签是 SQL 分析类结果时，确保对应行数据已经进入后台懒计算。
    fn ensure_runtime_sql_analysis_rows_for_current_type(
        &mut self,
        analysis_id: usize,
        cx: &mut Context<Self>,
    ) {
        let Some(result_type) = self
            .runtime_analyses
            .get(&analysis_id)
            .map(|state| state.result_type)
        else {
            return;
        };

        match result_type {
            RuntimeAnalysisResultType::SqlFrequency => {
                self.ensure_runtime_sql_frequency_rows(analysis_id, cx)
            }
            RuntimeAnalysisResultType::SlowSql => {
                self.ensure_runtime_slow_sql_rows(analysis_id, cx)
            }
            RuntimeAnalysisResultType::Statistics => {}
        }
    }

    /// 启动 SQL 频率分析行数据的后台懒计算。
    fn ensure_runtime_sql_frequency_rows(&mut self, analysis_id: usize, cx: &mut Context<Self>) {
        let Some(state) = self.runtime_analyses.get_mut(&analysis_id) else {
            return;
        };
        let filter = runtime_filter_applied_snapshot_from_state(state);
        if state
            .sql_frequency_rows_cache
            .borrow()
            .as_ref()
            .is_some_and(|cache| cache.filter == filter)
        {
            return;
        }
        if state.is_sql_frequency_rows_computing
            && state.sql_frequency_rows_computing_filter.as_ref() == Some(&filter)
        {
            return;
        }
        let RuntimeAnalysisTaskState::Ready(result) = &state.task_state else {
            return;
        };

        let result = result.clone();
        state.sql_frequency_rows_task_generation =
            state.sql_frequency_rows_task_generation.saturating_add(1);
        let task_generation = state.sql_frequency_rows_task_generation;
        state.is_sql_frequency_rows_computing = true;
        state.sql_frequency_rows_computing_filter = Some(filter.clone());
        state.sql_frequency_detail_rows_cache.borrow_mut().take();
        self.placeholder_notice = "正在计算 SQL 频率分析".to_string();

        cx.spawn(async move |view, cx| {
            let filter_for_task = filter.clone();
            let rows = cx
                .background_executor()
                .spawn(async move {
                    Arc::new(build_runtime_sql_frequency_rows_for_filter(
                        result.as_ref(),
                        &filter_for_task,
                    ))
                })
                .await;

            view.update(cx, |app, cx| {
                app.apply_runtime_sql_frequency_rows(analysis_id, task_generation, filter, rows);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// 应用后台构建好的 SQL 频率分析行。
    fn apply_runtime_sql_frequency_rows(
        &mut self,
        analysis_id: usize,
        task_generation: usize,
        filter: RuntimeSqlAnalysisFilterSnapshot,
        rows: Arc<Vec<RuntimeSqlFrequencyAnalysisRow>>,
    ) {
        let Some(state) = self.runtime_analyses.get_mut(&analysis_id) else {
            return;
        };
        if state.sql_frequency_rows_task_generation != task_generation
            || runtime_filter_applied_snapshot_from_state(state) != filter
        {
            return;
        }

        state.is_sql_frequency_rows_computing = false;
        state.sql_frequency_rows_computing_filter = None;
        state
            .sql_frequency_rows_cache
            .borrow_mut()
            .replace(RuntimeSqlFrequencyRowsCache { filter, rows });
        self.placeholder_notice = "SQL 频率分析计算完成".to_string();
    }

    /// 启动慢 SQL 分析行数据的后台懒计算。
    fn ensure_runtime_slow_sql_rows(&mut self, analysis_id: usize, cx: &mut Context<Self>) {
        let Some(state) = self.runtime_analyses.get_mut(&analysis_id) else {
            return;
        };
        let filter = runtime_filter_applied_snapshot_from_state(state);
        if state
            .slow_sql_rows_cache
            .borrow()
            .as_ref()
            .is_some_and(|cache| cache.filter == filter)
        {
            return;
        }
        if state.is_slow_sql_rows_computing
            && state.slow_sql_rows_computing_filter.as_ref() == Some(&filter)
        {
            return;
        }
        let RuntimeAnalysisTaskState::Ready(result) = &state.task_state else {
            return;
        };

        let result = result.clone();
        state.slow_sql_rows_task_generation = state.slow_sql_rows_task_generation.saturating_add(1);
        let task_generation = state.slow_sql_rows_task_generation;
        state.is_slow_sql_rows_computing = true;
        state.slow_sql_rows_computing_filter = Some(filter.clone());
        self.placeholder_notice = "正在计算慢 SQL 分析".to_string();

        cx.spawn(async move |view, cx| {
            let filter_for_task = filter.clone();
            let rows = cx
                .background_executor()
                .spawn(async move {
                    Arc::new(build_runtime_slow_sql_rows_for_filter(
                        result.as_ref(),
                        &filter_for_task,
                    ))
                })
                .await;

            view.update(cx, |app, cx| {
                app.apply_runtime_slow_sql_rows(analysis_id, task_generation, filter, rows);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// 应用后台构建好的慢 SQL 分析行。
    fn apply_runtime_slow_sql_rows(
        &mut self,
        analysis_id: usize,
        task_generation: usize,
        filter: RuntimeSqlAnalysisFilterSnapshot,
        rows: Arc<Vec<RuntimeSlowSqlSummaryRow>>,
    ) {
        let Some(state) = self.runtime_analyses.get_mut(&analysis_id) else {
            return;
        };
        if state.slow_sql_rows_task_generation != task_generation
            || runtime_filter_applied_snapshot_from_state(state) != filter
        {
            return;
        }

        state.is_slow_sql_rows_computing = false;
        state.slow_sql_rows_computing_filter = None;
        state
            .slow_sql_rows_cache
            .borrow_mut()
            .replace(RuntimeSlowSqlRowsCache { filter, rows });
        self.placeholder_notice = "慢 SQL 分析计算完成".to_string();
    }

    /// 切换 Runtime 分析结果类型，并清理旧表格残留的交互状态。
    pub fn set_runtime_result_type(
        &mut self,
        analysis_id: usize,
        result_type: RuntimeAnalysisResultType,
        cx: Option<&mut Context<Self>>,
    ) {
        let Some(state) = self.runtime_analyses.get_mut(&analysis_id) else {
            self.placeholder_notice = "未找到 Runtime 分析结果".to_string();
            return;
        };
        if state.result_type == result_type {
            return;
        }

        state.result_type = result_type;
        state.cell_selection = None;
        state.cell_selection_drag = None;
        state.hovered_sql_cell = None;
        state.sql_text_dialog = None;
        state.scrollbar_drag = None;
        if result_type == RuntimeAnalysisResultType::Statistics {
            state.sql_frequency_detail_sql = None;
            state.slow_sql_detail_sql = None;
            // 切回统计分析只是页面切换，不代表 SQL 分析数据失效；
            // 频率/慢 SQL 列表缓存要保留，避免用户返回 SQL 页时重复后台计算。
            state.sql_frequency_detail_rows_cache.borrow_mut().take();
        }
        match result_type {
            RuntimeAnalysisResultType::Statistics => match state.view {
                RuntimeAnalysisView::Summary => {
                    state.summary_scroll = UniformListScrollHandle::new()
                }
                RuntimeAnalysisView::RequestDetails { .. } => {
                    state.request_scroll = UniformListScrollHandle::new()
                }
                RuntimeAnalysisView::SqlList { .. } => {
                    state.sql_scroll = UniformListScrollHandle::new()
                }
            },
            RuntimeAnalysisResultType::SqlFrequency => {
                state.sql_frequency_detail_sql = None;
                state.slow_sql_detail_sql = None;
                state.sql_frequency_scroll = UniformListScrollHandle::new();
                state.sql_frequency_detail_scroll = UniformListScrollHandle::new();
                state.sql_frequency_detail_rows_cache.borrow_mut().take();
            }
            RuntimeAnalysisResultType::SlowSql => {
                state.sql_frequency_detail_sql = None;
                state.slow_sql_detail_sql = None;
                state.slow_sql_scroll = UniformListScrollHandle::new()
            }
        }
        if let Some(cx) = cx {
            self.ensure_runtime_sql_analysis_rows_for_current_type(analysis_id, cx);
        }
    }

    /// 打开 SQL 频率分析中指定 SQL 结构的执行详情页。
    pub fn open_runtime_sql_frequency_detail(
        &mut self,
        analysis_id: usize,
        normalized_sql: String,
    ) {
        let Some(state) = self.runtime_analyses.get_mut(&analysis_id) else {
            self.placeholder_notice = "未找到 Runtime 分析结果".to_string();
            return;
        };
        state.result_type = RuntimeAnalysisResultType::SqlFrequency;
        state.sql_frequency_detail_sql = Some(normalized_sql);
        state.sql_frequency_detail_scroll = UniformListScrollHandle::new();
        state.cell_selection = None;
        state.cell_selection_drag = None;
        state.hovered_sql_cell = None;
        state.sql_text_dialog = None;
        state.scrollbar_drag = None;
        state.sql_frequency_detail_rows_cache.borrow_mut().take();
        self.placeholder_notice = "查看 SQL 频率详情".to_string();
    }

    /// 从 SQL 频率详情页返回 SQL 频率列表。
    pub fn show_runtime_sql_frequency_summary(&mut self, analysis_id: usize) {
        let Some(state) = self.runtime_analyses.get_mut(&analysis_id) else {
            self.placeholder_notice = "未找到 Runtime 分析结果".to_string();
            return;
        };
        state.result_type = RuntimeAnalysisResultType::SqlFrequency;
        state.sql_frequency_detail_sql = None;
        state.sql_frequency_scroll = UniformListScrollHandle::new();
        state.cell_selection = None;
        state.cell_selection_drag = None;
        state.hovered_sql_cell = None;
        state.sql_text_dialog = None;
        state.scrollbar_drag = None;
    }

    /// 打开慢 SQL 分析中指定 SQL 结构的执行详情页。
    pub fn open_runtime_slow_sql_detail(&mut self, analysis_id: usize, normalized_sql: String) {
        let Some(state) = self.runtime_analyses.get_mut(&analysis_id) else {
            self.placeholder_notice = "未找到 Runtime 分析结果".to_string();
            return;
        };
        state.result_type = RuntimeAnalysisResultType::SlowSql;
        state.slow_sql_detail_sql = Some(normalized_sql);
        state.slow_sql_scroll = UniformListScrollHandle::new();
        state.cell_selection = None;
        state.cell_selection_drag = None;
        state.hovered_sql_cell = None;
        state.sql_text_dialog = None;
        state.scrollbar_drag = None;
        state.sql_frequency_detail_rows_cache.borrow_mut().take();
        self.placeholder_notice = "查看慢 SQL 执行详情".to_string();
    }

    /// 从慢 SQL 详情页返回慢 SQL 聚合列表。
    pub fn show_runtime_slow_sql_summary(&mut self, analysis_id: usize) {
        let Some(state) = self.runtime_analyses.get_mut(&analysis_id) else {
            self.placeholder_notice = "未找到 Runtime 分析结果".to_string();
            return;
        };
        state.result_type = RuntimeAnalysisResultType::SlowSql;
        state.slow_sql_detail_sql = None;
        state.slow_sql_scroll = UniformListScrollHandle::new();
        state.cell_selection = None;
        state.cell_selection_drag = None;
        state.hovered_sql_cell = None;
        state.sql_text_dialog = None;
        state.scrollbar_drag = None;
    }

    /// 切换 Runtime 分析总览表排序字段。
    pub fn set_runtime_summary_sort(
        &mut self,
        analysis_id: usize,
        sort_key: RuntimeSummarySortKey,
    ) {
        let Some(state) = self.runtime_analyses.get_mut(&analysis_id) else {
            self.placeholder_notice = "未找到 Runtime 分析结果".to_string();
            return;
        };
        if state.summary_sort_key == sort_key {
            state.summary_sort_direction = state.summary_sort_direction.toggled();
        } else {
            state.summary_sort_key = sort_key;
            state.summary_sort_direction = default_runtime_summary_sort_direction(sort_key);
        }
        state.cell_selection = None;
        state.cell_selection_drag = None;
        state.hovered_sql_cell = None;
        state.sql_text_dialog = None;
        state.summary_scroll = UniformListScrollHandle::new();
    }

    /// 切换 Runtime 请求明细表排序字段。
    pub fn set_runtime_request_sort(
        &mut self,
        analysis_id: usize,
        sort_key: RuntimeRequestSortKey,
    ) {
        let Some(state) = self.runtime_analyses.get_mut(&analysis_id) else {
            self.placeholder_notice = "未找到 Runtime 分析结果".to_string();
            return;
        };
        if state.request_sort_key == sort_key {
            state.request_sort_direction = state.request_sort_direction.toggled();
        } else {
            state.request_sort_key = sort_key;
            state.request_sort_direction = default_runtime_request_sort_direction(sort_key);
        }
        state.cell_selection = None;
        state.cell_selection_drag = None;
        state.hovered_sql_cell = None;
        state.sql_text_dialog = None;
        state.request_scroll = UniformListScrollHandle::new();
    }

    /// 切换 Runtime SQL 明细表排序字段。
    pub fn set_runtime_sql_sort(&mut self, analysis_id: usize, sort_key: RuntimeSqlSortKey) {
        let Some(state) = self.runtime_analyses.get_mut(&analysis_id) else {
            self.placeholder_notice = "未找到 Runtime 分析结果".to_string();
            return;
        };
        if state.sql_sort_key == sort_key {
            state.sql_sort_direction = state.sql_sort_direction.toggled();
        } else {
            state.sql_sort_key = sort_key;
            state.sql_sort_direction = default_runtime_sql_sort_direction(sort_key);
        }
        state.cell_selection = None;
        state.cell_selection_drag = None;
        state.hovered_sql_cell = None;
        state.sql_scroll = UniformListScrollHandle::new();
    }

    /// 从 Runtime 总览进入指定请求地址的详情页。
    pub fn open_runtime_request_details(&mut self, analysis_id: usize, request_path: String) {
        let Some(state) = self.runtime_analyses.get_mut(&analysis_id) else {
            self.placeholder_notice = "未找到 Runtime 分析结果".to_string();
            return;
        };
        state.view = RuntimeAnalysisView::RequestDetails {
            request_path: request_path.clone(),
        };
        state.result_type = RuntimeAnalysisResultType::Statistics;
        state.request_scroll = UniformListScrollHandle::new();
        state.cell_selection = None;
        state.cell_selection_drag = None;
        state.hovered_sql_cell = None;
        state.sql_text_dialog = None;
        self.placeholder_notice = format!("查看 Runtime 请求详情：{request_path}");
    }

    /// 从 Runtime 请求详情进入指定请求日志的 SQL 列表。
    pub fn open_runtime_sql_list(
        &mut self,
        analysis_id: usize,
        request_path: String,
        request_index: usize,
    ) {
        let Some(state) = self.runtime_analyses.get_mut(&analysis_id) else {
            self.placeholder_notice = "未找到 Runtime 分析结果".to_string();
            return;
        };
        state.view = RuntimeAnalysisView::SqlList {
            request_path,
            request_index,
        };
        state.result_type = RuntimeAnalysisResultType::Statistics;
        state.sql_scroll = UniformListScrollHandle::new();
        state.cell_selection = None;
        state.cell_selection_drag = None;
        state.hovered_sql_cell = None;
        state.sql_text_dialog = None;
        self.placeholder_notice = "查看 Runtime SQL 列表".to_string();
    }

    /// 返回 Runtime 总览页。
    pub fn show_runtime_summary(&mut self, analysis_id: usize) {
        let Some(state) = self.runtime_analyses.get_mut(&analysis_id) else {
            self.placeholder_notice = "未找到 Runtime 分析结果".to_string();
            return;
        };
        state.view = RuntimeAnalysisView::Summary;
        state.result_type = RuntimeAnalysisResultType::Statistics;
        state.summary_scroll = UniformListScrollHandle::new();
        state.cell_selection = None;
        state.cell_selection_drag = None;
        state.hovered_sql_cell = None;
        state.sql_text_dialog = None;
    }

    /// 从 Runtime SQL 列表返回请求详情页。
    pub fn show_runtime_request_details(&mut self, analysis_id: usize, request_path: String) {
        let Some(state) = self.runtime_analyses.get_mut(&analysis_id) else {
            self.placeholder_notice = "未找到 Runtime 分析结果".to_string();
            return;
        };
        state.view = RuntimeAnalysisView::RequestDetails { request_path };
        state.result_type = RuntimeAnalysisResultType::Statistics;
        state.request_scroll = UniformListScrollHandle::new();
        state.cell_selection = None;
        state.cell_selection_drag = None;
        state.hovered_sql_cell = None;
        state.sql_text_dialog = None;
    }

    /// 更新 Runtime SQL 文本单元格悬浮状态。
    ///
    /// 返回值：状态是否发生变化，需要触发界面刷新。
    pub fn set_runtime_sql_cell_hovered(
        &mut self,
        analysis_id: usize,
        request_index: usize,
        sql_index: usize,
        is_hovered: bool,
    ) -> bool {
        let Some(state) = self.runtime_analyses.get_mut(&analysis_id) else {
            return false;
        };
        let cell_key = RuntimeSqlCellKey {
            request_index,
            sql_index,
        };
        if is_hovered {
            if state.hovered_sql_cell == Some(cell_key) {
                return false;
            }
            state.hovered_sql_cell = Some(cell_key);
            return true;
        }
        if state.hovered_sql_cell == Some(cell_key) {
            state.hovered_sql_cell = None;
            return true;
        }
        false
    }

    /// 打开 Runtime SQL 完整文本弹窗，弹窗内容保留 SQL 原始换行和缩进。
    pub fn open_runtime_sql_text_dialog(
        &mut self,
        analysis_id: usize,
        mut dialog: RuntimeSqlTextDialog,
    ) {
        let Some(state) = self.runtime_analyses.get_mut(&analysis_id) else {
            self.placeholder_notice = "未找到 Runtime 分析结果".to_string();
            return;
        };
        dialog.selection = None;
        dialog.selection_drag = None;
        state.sql_text_dialog = Some(dialog);
        state.cell_selection = None;
        state.cell_selection_drag = None;
    }

    /// 关闭 Runtime SQL 完整文本弹窗。
    pub fn close_runtime_sql_text_dialog(&mut self, analysis_id: usize) -> bool {
        let Some(state) = self.runtime_analyses.get_mut(&analysis_id) else {
            return false;
        };
        state.sql_text_dialog.take().is_some()
    }

    /// 清理 Runtime SQL 完整文本弹窗中的正文选区。
    pub fn clear_runtime_sql_text_selection(&mut self, analysis_id: usize) -> bool {
        let Some(dialog) = self
            .runtime_analyses
            .get_mut(&analysis_id)
            .and_then(|state| state.sql_text_dialog.as_mut())
        else {
            return false;
        };
        let had_selection = dialog.selection.take().is_some();
        let had_drag = dialog.selection_drag.take().is_some();
        had_selection || had_drag
    }

    /// 开始在 Runtime SQL 完整文本弹窗中选择文本。
    pub fn begin_runtime_sql_text_selection(
        &mut self,
        analysis_id: usize,
        line_index: usize,
        line: String,
        character_index: usize,
        granularity: TextSelectionGranularity,
    ) {
        let Some(dialog) = self
            .runtime_analyses
            .get_mut(&analysis_id)
            .and_then(|state| state.sql_text_dialog.as_mut())
        else {
            return;
        };

        let anchor_range =
            runtime_sql_text_range_for_granularity(line_index, &line, character_index, granularity);
        dialog.selection = Some(anchor_range.clone());
        dialog.selection_drag = Some(RuntimeSqlTextSelectionDrag {
            anchor_range,
            granularity,
        });
    }

    /// 拖拽更新 Runtime SQL 完整文本弹窗中的文本选区。
    pub fn update_runtime_sql_text_selection(
        &mut self,
        analysis_id: usize,
        line_index: usize,
        line: String,
        character_index: usize,
    ) -> bool {
        let Some(dialog) = self
            .runtime_analyses
            .get_mut(&analysis_id)
            .and_then(|state| state.sql_text_dialog.as_mut())
        else {
            return false;
        };
        let Some(drag) = dialog.selection_drag.clone() else {
            return false;
        };

        let focus_range = runtime_sql_text_range_for_granularity(
            line_index,
            &line,
            character_index,
            drag.granularity,
        );
        let anchor_start = drag.anchor_range.anchor;
        let anchor_end = drag.anchor_range.focus;
        let focus_start = focus_range.anchor;
        let focus_end = focus_range.focus;
        dialog.selection = Some(RuntimeSqlTextSelection {
            anchor: if runtime_sql_text_position_le(anchor_start, focus_start) {
                anchor_start
            } else {
                focus_start
            },
            focus: if runtime_sql_text_position_le(anchor_end, focus_end) {
                focus_end
            } else {
                anchor_end
            },
        });
        true
    }

    /// 结束 Runtime SQL 完整文本弹窗文本选择；空选区会被清理。
    pub fn finish_runtime_sql_text_selection(&mut self, analysis_id: usize) -> bool {
        let Some(dialog) = self
            .runtime_analyses
            .get_mut(&analysis_id)
            .and_then(|state| state.sql_text_dialog.as_mut())
        else {
            return false;
        };
        let had_drag = dialog.selection_drag.take().is_some();
        if dialog
            .selection
            .as_ref()
            .map_or(true, RuntimeSqlTextSelection::is_empty)
        {
            dialog.selection = None;
        }
        had_drag
    }

    /// 复制 Runtime SQL 完整文本弹窗中的当前选区。
    ///
    /// 返回值：是否存在可复制的 SQL 弹窗选区。
    pub fn copy_runtime_sql_text_selection(
        &mut self,
        analysis_id: usize,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(selected_text) = self
            .runtime_analyses
            .get(&analysis_id)
            .and_then(|state| state.sql_text_dialog.as_ref())
            .and_then(|dialog| {
                let selection = dialog.selection.as_ref()?;
                let lines = runtime_sql_text_lines(&dialog.sql_text);
                selected_runtime_sql_text_from_lines(&lines, selection)
            })
        else {
            return false;
        };

        let app_context: &gpui::App = (&*cx).borrow();
        app_context.write_to_clipboard(ClipboardItem::new_string(selected_text.clone()));
        self.placeholder_notice = format!("已复制 SQL 文本：{selected_text}");
        true
    }

    /// 开始在 Runtime 表格单元格中选择文本。
    ///
    /// 参数说明：
    /// - `analysis_id`：Runtime 分析页 ID。
    /// - `cell_key`：单元格稳定 key。
    /// - `text`：单元格完整文本。
    /// - `character_index`：鼠标按下位置命中的字符列。
    /// - `granularity`：按点击次数决定的选择粒度。
    pub fn begin_runtime_cell_selection(
        &mut self,
        analysis_id: usize,
        cell_key: String,
        text: String,
        character_index: usize,
        granularity: TextSelectionGranularity,
    ) {
        let Some(state) = self.runtime_analyses.get_mut(&analysis_id) else {
            self.placeholder_notice = "未找到 Runtime 分析结果".to_string();
            return;
        };

        let anchor_range = runtime_cell_range_for_granularity(&text, character_index, granularity);
        state.cell_selection = Some(RuntimeTableCellSelection {
            cell_key: cell_key.clone(),
            text: text.clone(),
            anchor: anchor_range.start,
            focus: anchor_range.end,
        });
        state.cell_selection_drag = Some(RuntimeTableCellSelectionDrag {
            cell_key,
            text,
            anchor_range,
            granularity,
        });
    }

    /// 拖拽更新 Runtime 表格单元格中的文本选区。
    ///
    /// 返回值：本次拖拽是否命中当前分析页和当前单元格。
    pub fn update_runtime_cell_selection(
        &mut self,
        analysis_id: usize,
        cell_key: &str,
        character_index: usize,
    ) -> bool {
        let Some(state) = self.runtime_analyses.get_mut(&analysis_id) else {
            return false;
        };
        let Some(drag) = state.cell_selection_drag.clone() else {
            return false;
        };
        if drag.cell_key != cell_key {
            return false;
        }

        let focus_range =
            runtime_cell_range_for_granularity(&drag.text, character_index, drag.granularity);
        state.cell_selection = Some(RuntimeTableCellSelection {
            cell_key: drag.cell_key,
            text: drag.text,
            anchor: drag.anchor_range.start.min(focus_range.start),
            focus: drag.anchor_range.end.max(focus_range.end),
        });
        true
    }

    /// 结束 Runtime 单元格文本选择；如果没有选中字符则清理空选区。
    ///
    /// 返回值：当前分析页是否存在需要结束的拖拽状态。
    pub fn finish_runtime_cell_selection(&mut self, analysis_id: usize) -> bool {
        let Some(state) = self.runtime_analyses.get_mut(&analysis_id) else {
            return false;
        };
        let had_drag = state.cell_selection_drag.take().is_some();
        if state
            .cell_selection
            .as_ref()
            .and_then(RuntimeTableCellSelection::normalized_range)
            .is_none()
        {
            state.cell_selection = None;
        }
        had_drag
    }

    /// 复制当前 Runtime 分析页表格单元格中拖选的文本。
    pub fn copy_selected_runtime_cell(&mut self, analysis_id: usize, cx: &mut Context<Self>) {
        let Some((cell_text, range)) = self
            .runtime_analyses
            .get(&analysis_id)
            .and_then(|state| state.cell_selection.as_ref())
            .and_then(|selection| {
                selection
                    .normalized_range()
                    .map(|range| (selection.text.clone(), range))
            })
        else {
            self.placeholder_notice = "请先拖选一个 Runtime 表格单元格内容".to_string();
            return;
        };

        let selected_text = slice_character_range(&cell_text, range);
        let app_context: &gpui::App = (&*cx).borrow();
        app_context.write_to_clipboard(ClipboardItem::new_string(selected_text.clone()));
        self.placeholder_notice = format!("已复制 Runtime 单元格内容：{selected_text}");
    }

    /// 清理全部 Runtime 分析页的表格单元格文本选区。
    ///
    /// 返回值：是否确实清理了已有选区或拖拽状态。
    pub fn clear_runtime_cell_selection(&mut self) -> bool {
        let mut changed = false;
        for state in self.runtime_analyses.values_mut() {
            if state.cell_selection.take().is_some() {
                changed = true;
            }
            if state.cell_selection_drag.take().is_some() {
                changed = true;
            }
        }
        changed
    }

    /// 打开 Runtime 时间选择器，并关闭其它 Runtime 页签中已展开的时间面板。
    pub fn open_runtime_time_picker(
        &mut self,
        analysis_id: usize,
        input_kind: RuntimeFilterInputKind,
    ) {
        if !matches!(
            input_kind,
            RuntimeFilterInputKind::StartTime | RuntimeFilterInputKind::EndTime
        ) {
            return;
        }
        for state in self.runtime_analyses.values_mut() {
            state.open_time_picker = None;
        }
        if let Some(state) = self.runtime_analyses.get_mut(&analysis_id) {
            state.open_time_picker = Some(input_kind);
        }
    }

    /// 关闭指定 Runtime 分析页的时间选择器。
    ///
    /// 返回值：是否关闭了一个已打开的面板。
    pub fn close_runtime_time_picker(&mut self, analysis_id: usize) -> bool {
        self.runtime_analyses
            .get_mut(&analysis_id)
            .and_then(|state| state.open_time_picker.take())
            .is_some()
    }

    /// 使用快捷动作设置 Runtime 时间过滤输入框。
    pub fn apply_runtime_time_picker_quick_action(
        &mut self,
        analysis_id: usize,
        input_kind: RuntimeFilterInputKind,
        action: RuntimeDateTimeQuickAction,
        cx: Option<&mut Context<Self>>,
    ) {
        let is_end = input_kind == RuntimeFilterInputKind::EndTime;
        if action == RuntimeDateTimeQuickAction::Clear {
            self.clear_runtime_filter_input(analysis_id, input_kind, cx);
            return;
        }

        let now = Local::now();
        let datetime = match action {
            RuntimeDateTimeQuickAction::TodayStart => now
                .date_naive()
                .and_hms_opt(0, 0, 0)
                .and_then(|datetime| Local.from_local_datetime(&datetime).single())
                .unwrap_or(now),
            RuntimeDateTimeQuickAction::Now => now,
            RuntimeDateTimeQuickAction::TodayEnd => now
                .date_naive()
                .and_hms_opt(23, 59, 59)
                .and_then(|datetime| Local.from_local_datetime(&datetime).single())
                .unwrap_or(now),
            RuntimeDateTimeQuickAction::Clear => now,
        };
        self.set_runtime_filter_time_value(analysis_id, input_kind, datetime, is_end, cx);
    }

    /// 按日期时间组件的步进按钮调整 Runtime 时间过滤输入框。
    pub fn adjust_runtime_filter_time(
        &mut self,
        analysis_id: usize,
        input_kind: RuntimeFilterInputKind,
        part: RuntimeDateTimePart,
        delta: i32,
        cx: Option<&mut Context<Self>>,
    ) {
        let is_end = input_kind == RuntimeFilterInputKind::EndTime;
        let current = self
            .runtime_filter_input(analysis_id, input_kind)
            .and_then(|input| parse_runtime_filter_datetime_value(&input.value, is_end))
            .unwrap_or_else(|| default_runtime_filter_datetime(is_end));
        let adjusted = adjust_runtime_datetime_part(current, part, delta);
        self.set_runtime_filter_time_value(analysis_id, input_kind, adjusted, is_end, cx);
    }

    /// 设置 Runtime 时间过滤输入框的日期部分，并保留当前时分秒。
    ///
    /// 参数说明：
    /// - `analysis_id`：Runtime 分析页 ID。
    /// - `input_kind`：开始时间或结束时间输入框。
    /// - `year`、`month`、`day`：日历面板中选中的本地日期。
    ///
    /// 说明：常见 Web 日期时间选择器在点选日期后仍保留时间选择能力；
    /// 因此这里只更新日期，不关闭浮层，方便用户继续微调时分秒。
    pub fn set_runtime_filter_date(
        &mut self,
        analysis_id: usize,
        input_kind: RuntimeFilterInputKind,
        year: i32,
        month: u32,
        day: u32,
        cx: Option<&mut Context<Self>>,
    ) {
        if !matches!(
            input_kind,
            RuntimeFilterInputKind::StartTime | RuntimeFilterInputKind::EndTime
        ) {
            return;
        }
        let is_end = input_kind == RuntimeFilterInputKind::EndTime;
        let current = self
            .runtime_filter_input(analysis_id, input_kind)
            .and_then(|input| parse_runtime_filter_datetime_value(&input.value, is_end))
            .unwrap_or_else(|| default_runtime_filter_datetime(is_end));
        let Some(date) = NaiveDate::from_ymd_opt(year, month, day) else {
            return;
        };
        let Some(naive) = date.and_hms_opt(current.hour(), current.minute(), current.second())
        else {
            return;
        };
        let datetime = Local
            .from_local_datetime(&naive)
            .single()
            .unwrap_or(current);
        self.set_runtime_filter_time_value(analysis_id, input_kind, datetime, is_end, cx);
    }

    /// 写入 Runtime 时间过滤输入框，并触发过滤结果刷新。
    fn set_runtime_filter_time_value(
        &mut self,
        analysis_id: usize,
        input_kind: RuntimeFilterInputKind,
        datetime: chrono::DateTime<Local>,
        is_end: bool,
        cx: Option<&mut Context<Self>>,
    ) {
        let Some(input) = self.runtime_filter_input_mut(analysis_id, input_kind) else {
            return;
        };
        input.value = format_runtime_filter_datetime_value(datetime);
        input.cursor = character_count(&input.value);
        input.selection_anchor = None;
        input.marked_range = None;
        input.selection_drag = None;
        input.is_focused = true;
        if let Some(state) = self.runtime_analyses.get_mut(&analysis_id) {
            state.open_time_picker = Some(if is_end {
                RuntimeFilterInputKind::EndTime
            } else {
                RuntimeFilterInputKind::StartTime
            });
        }
        self.trigger_runtime_filter_refresh(analysis_id, cx);
    }

    /// 聚焦 Runtime 过滤输入框，并清理其它 Runtime 过滤输入框的临时输入法状态。
    pub fn focus_runtime_filter_input(
        &mut self,
        analysis_id: usize,
        input_kind: RuntimeFilterInputKind,
    ) {
        for state in self.runtime_analyses.values_mut() {
            clear_runtime_filter_inputs_focus(state);
        }
        if let Some(input) = self.runtime_filter_input_mut(analysis_id, input_kind) {
            input.is_focused = true;
            input.marked_range = None;
        }
    }

    /// 清空 Runtime 过滤输入框，并立即刷新当前分析页过滤结果。
    pub fn clear_runtime_filter_input(
        &mut self,
        analysis_id: usize,
        input_kind: RuntimeFilterInputKind,
        cx: Option<&mut Context<Self>>,
    ) {
        let Some(input) = self.runtime_filter_input_mut(analysis_id, input_kind) else {
            return;
        };
        input.value.clear();
        input.cursor = 0;
        input.selection_anchor = None;
        input.marked_range = None;
        input.selection_drag = None;
        input.is_focused = true;
        self.trigger_runtime_filter_refresh(analysis_id, cx);
    }

    /// 处理 Runtime 过滤输入框键盘事件。
    pub fn handle_runtime_filter_input_key(
        &mut self,
        analysis_id: usize,
        input_kind: RuntimeFilterInputKind,
        keystroke: &Keystroke,
        cx: &mut Context<Self>,
    ) {
        let key = keystroke.key.to_lowercase();
        if keystroke.modifiers.secondary() {
            match key.as_str() {
                "a" => self.select_all_runtime_filter_input(analysis_id, input_kind),
                "c" => self.copy_runtime_filter_input_selection(analysis_id, input_kind, cx),
                "x" => self.cut_runtime_filter_input_selection(analysis_id, input_kind, cx),
                "v" => self.paste_runtime_filter_input_clipboard(analysis_id, input_kind, cx),
                "left" | "arrowleft" => self.move_runtime_filter_input_cursor(
                    analysis_id,
                    input_kind,
                    0,
                    keystroke.modifiers.shift,
                ),
                "right" | "arrowright" => {
                    let end = self
                        .runtime_filter_input(analysis_id, input_kind)
                        .map(|input| character_count(&input.value))
                        .unwrap_or_default();
                    self.move_runtime_filter_input_cursor(
                        analysis_id,
                        input_kind,
                        end,
                        keystroke.modifiers.shift,
                    );
                }
                _ => {}
            }
            return;
        }

        match key.as_str() {
            "backspace" => self.delete_runtime_filter_input_backward(analysis_id, input_kind, cx),
            "delete" => self.delete_runtime_filter_input_forward(analysis_id, input_kind, cx),
            "left" | "arrowleft" => self.move_runtime_filter_input_left(
                analysis_id,
                input_kind,
                keystroke.modifiers.shift,
            ),
            "right" | "arrowright" => self.move_runtime_filter_input_right(
                analysis_id,
                input_kind,
                keystroke.modifiers.shift,
            ),
            "home" => self.move_runtime_filter_input_cursor(
                analysis_id,
                input_kind,
                0,
                keystroke.modifiers.shift,
            ),
            "end" => {
                let end = self
                    .runtime_filter_input(analysis_id, input_kind)
                    .map(|input| character_count(&input.value))
                    .unwrap_or_default();
                self.move_runtime_filter_input_cursor(
                    analysis_id,
                    input_kind,
                    end,
                    keystroke.modifiers.shift,
                );
            }
            "escape" => {
                if let Some(input) = self.runtime_filter_input_mut(analysis_id, input_kind) {
                    input.is_focused = false;
                    input.marked_range = None;
                    input.selection_drag = None;
                }
            }
            _ => {
                if let Some(key_char) = keystroke.key_char.as_ref()
                    && !keystroke.modifiers.control
                    && !keystroke.modifiers.alt
                    && !keystroke.modifiers.platform
                    && !keystroke.modifiers.function
                    && !key_char.chars().any(char::is_control)
                {
                    self.insert_runtime_filter_input_text(analysis_id, input_kind, key_char, cx);
                }
            }
        }
    }

    /// 开始 Runtime 过滤输入框鼠标选择。
    pub fn begin_runtime_filter_input_pointer_selection(
        &mut self,
        analysis_id: usize,
        input_kind: RuntimeFilterInputKind,
        character_index: usize,
        granularity: TextSelectionGranularity,
    ) {
        self.focus_runtime_filter_input(analysis_id, input_kind);
        let Some(input) = self.runtime_filter_input_mut(analysis_id, input_kind) else {
            return;
        };
        let range = runtime_filter_input_range_for_granularity(input, character_index, granularity);
        input.cursor = range.end;
        input.selection_anchor = Some(range.start);
        input.marked_range = None;
        input.selection_drag = Some(InputTextSelectionDrag {
            anchor_range: range,
            granularity,
        });
    }

    /// 更新 Runtime 过滤输入框鼠标拖拽选区。
    pub fn update_runtime_filter_input_pointer_selection(
        &mut self,
        analysis_id: usize,
        input_kind: RuntimeFilterInputKind,
        character_index: usize,
    ) {
        let Some(input) = self.runtime_filter_input_mut(analysis_id, input_kind) else {
            return;
        };
        let Some(drag) = input.selection_drag.clone() else {
            return;
        };
        let focus_range =
            runtime_filter_input_range_for_granularity(input, character_index, drag.granularity);
        input.selection_anchor = Some(drag.anchor_range.start.min(focus_range.start));
        input.marked_range = None;
        input.cursor = drag.anchor_range.end.max(focus_range.end);
    }

    /// 结束 Runtime 过滤输入框鼠标选择。
    pub fn finish_runtime_filter_input_pointer_selection(
        &mut self,
        analysis_id: usize,
        input_kind: RuntimeFilterInputKind,
    ) {
        if let Some(input) = self.runtime_filter_input_mut(analysis_id, input_kind) {
            input.selection_drag = None;
        }
    }

    /// 返回指定 Runtime 过滤输入框的只读状态。
    pub fn runtime_filter_input(
        &self,
        analysis_id: usize,
        input_kind: RuntimeFilterInputKind,
    ) -> Option<&SettingsTextInputState> {
        let state = self.runtime_analyses.get(&analysis_id)?;
        Some(match input_kind {
            RuntimeFilterInputKind::Keyword => &state.filter_keyword_input,
            RuntimeFilterInputKind::Username => &state.filter_username_input,
            RuntimeFilterInputKind::StartTime => &state.filter_start_time_input,
            RuntimeFilterInputKind::EndTime => &state.filter_end_time_input,
        })
    }

    /// 返回指定 Runtime 过滤输入框的可变状态。
    pub fn runtime_filter_input_mut(
        &mut self,
        analysis_id: usize,
        input_kind: RuntimeFilterInputKind,
    ) -> Option<&mut SettingsTextInputState> {
        let state = self.runtime_analyses.get_mut(&analysis_id)?;
        Some(match input_kind {
            RuntimeFilterInputKind::Keyword => &mut state.filter_keyword_input,
            RuntimeFilterInputKind::Username => &mut state.filter_username_input,
            RuntimeFilterInputKind::StartTime => &mut state.filter_start_time_input,
            RuntimeFilterInputKind::EndTime => &mut state.filter_end_time_input,
        })
    }

    /// Runtime 过滤条件变化后标记待应用，真正过滤由防抖后台任务完成。
    pub fn after_runtime_filter_changed(&mut self, analysis_id: usize) -> Option<usize> {
        let Some(state) = self.runtime_analyses.get_mut(&analysis_id) else {
            return None;
        };
        state.filter_input_generation = state.filter_input_generation.saturating_add(1);
        state.is_filter_pending = true;
        state.cell_selection = None;
        state.cell_selection_drag = None;
        state.hovered_sql_cell = None;
        state.sql_text_dialog = None;
        state.sql_frequency_detail_rows_cache.borrow_mut().take();
        state.scrollbar_drag = None;
        self.placeholder_notice = "Runtime 过滤条件待应用".to_string();
        Some(state.filter_input_generation)
    }

    /// 全选 Runtime 过滤输入框内容。
    fn select_all_runtime_filter_input(
        &mut self,
        analysis_id: usize,
        input_kind: RuntimeFilterInputKind,
    ) {
        if let Some(input) = self.runtime_filter_input_mut(analysis_id, input_kind) {
            input.selection_anchor = Some(0);
            input.cursor = character_count(&input.value);
            input.marked_range = None;
            input.selection_drag = None;
        }
    }

    /// 复制 Runtime 过滤输入框当前选区。
    fn copy_runtime_filter_input_selection(
        &mut self,
        analysis_id: usize,
        input_kind: RuntimeFilterInputKind,
        cx: &mut Context<Self>,
    ) {
        let Some(selected_text) = self.selected_runtime_filter_input_text(analysis_id, input_kind)
        else {
            return;
        };
        let app_context: &gpui::App = (&*cx).borrow();
        app_context.write_to_clipboard(ClipboardItem::new_string(selected_text));
    }

    /// 剪切 Runtime 过滤输入框当前选区。
    fn cut_runtime_filter_input_selection(
        &mut self,
        analysis_id: usize,
        input_kind: RuntimeFilterInputKind,
        cx: &mut Context<Self>,
    ) {
        self.copy_runtime_filter_input_selection(analysis_id, input_kind, cx);
        if self.delete_runtime_filter_input_selection(analysis_id, input_kind) {
            self.queue_runtime_filter_refresh(analysis_id, cx);
        }
    }

    /// 粘贴剪贴板内容到 Runtime 过滤输入框。
    fn paste_runtime_filter_input_clipboard(
        &mut self,
        analysis_id: usize,
        input_kind: RuntimeFilterInputKind,
        cx: &mut Context<Self>,
    ) {
        let app_context: &gpui::App = (&*cx).borrow();
        let Some(item) = app_context.read_from_clipboard() else {
            return;
        };
        if let Some(text) = item.text() {
            self.insert_runtime_filter_input_text(
                analysis_id,
                input_kind,
                &text.replace(['\n', '\r'], " "),
                cx,
            );
        }
    }

    /// 返回 Runtime 过滤输入框当前选中文本。
    fn selected_runtime_filter_input_text(
        &self,
        analysis_id: usize,
        input_kind: RuntimeFilterInputKind,
    ) -> Option<String> {
        let input = self.runtime_filter_input(analysis_id, input_kind)?;
        let range = normalized_runtime_filter_input_selection_range(input)?;
        Some(slice_character_range(&input.value, range))
    }

    /// 删除 Runtime 过滤输入框当前选区。
    fn delete_runtime_filter_input_selection(
        &mut self,
        analysis_id: usize,
        input_kind: RuntimeFilterInputKind,
    ) -> bool {
        let Some(input) = self.runtime_filter_input_mut(analysis_id, input_kind) else {
            return false;
        };
        let Some(range) = normalized_runtime_filter_input_selection_range(input) else {
            return false;
        };
        input.value = replace_character_range(&input.value, range.clone(), "");
        input.cursor = range.start;
        input.selection_anchor = None;
        input.marked_range = None;
        input.selection_drag = None;
        true
    }

    /// 向 Runtime 过滤输入框插入文本。
    fn insert_runtime_filter_input_text(
        &mut self,
        analysis_id: usize,
        input_kind: RuntimeFilterInputKind,
        text: &str,
        cx: &mut Context<Self>,
    ) {
        if text.is_empty() {
            return;
        }
        let _ = self.delete_runtime_filter_input_selection(analysis_id, input_kind);
        let Some(input) = self.runtime_filter_input_mut(analysis_id, input_kind) else {
            return;
        };
        let cursor = input.cursor.min(character_count(&input.value));
        input.value = replace_character_range(&input.value, cursor..cursor, text);
        input.cursor = cursor + character_count(text);
        input.selection_anchor = None;
        input.marked_range = None;
        input.selection_drag = None;
        self.queue_runtime_filter_refresh(analysis_id, cx);
    }

    /// 删除 Runtime 过滤输入框光标前一个字符。
    fn delete_runtime_filter_input_backward(
        &mut self,
        analysis_id: usize,
        input_kind: RuntimeFilterInputKind,
        cx: &mut Context<Self>,
    ) {
        if self.delete_runtime_filter_input_selection(analysis_id, input_kind) {
            self.queue_runtime_filter_refresh(analysis_id, cx);
            return;
        }
        let Some(input) = self.runtime_filter_input_mut(analysis_id, input_kind) else {
            return;
        };
        if input.cursor == 0 {
            return;
        }
        let cursor = input.cursor.min(character_count(&input.value));
        input.value = replace_character_range(&input.value, cursor - 1..cursor, "");
        input.cursor = cursor - 1;
        input.marked_range = None;
        input.selection_drag = None;
        self.queue_runtime_filter_refresh(analysis_id, cx);
    }

    /// 删除 Runtime 过滤输入框光标后一个字符。
    fn delete_runtime_filter_input_forward(
        &mut self,
        analysis_id: usize,
        input_kind: RuntimeFilterInputKind,
        cx: &mut Context<Self>,
    ) {
        if self.delete_runtime_filter_input_selection(analysis_id, input_kind) {
            self.queue_runtime_filter_refresh(analysis_id, cx);
            return;
        }
        let Some(input) = self.runtime_filter_input_mut(analysis_id, input_kind) else {
            return;
        };
        let text_length = character_count(&input.value);
        let cursor = input.cursor.min(text_length);
        if cursor >= text_length {
            return;
        }
        input.value = replace_character_range(&input.value, cursor..cursor + 1, "");
        input.cursor = cursor;
        input.marked_range = None;
        input.selection_drag = None;
        self.queue_runtime_filter_refresh(analysis_id, cx);
    }

    /// Runtime 过滤输入框光标左移。
    fn move_runtime_filter_input_left(
        &mut self,
        analysis_id: usize,
        input_kind: RuntimeFilterInputKind,
        extend_selection: bool,
    ) {
        let cursor = self
            .runtime_filter_input(analysis_id, input_kind)
            .map(|input| input.cursor.saturating_sub(1))
            .unwrap_or_default();
        self.move_runtime_filter_input_cursor(analysis_id, input_kind, cursor, extend_selection);
    }

    /// Runtime 过滤输入框光标右移。
    fn move_runtime_filter_input_right(
        &mut self,
        analysis_id: usize,
        input_kind: RuntimeFilterInputKind,
        extend_selection: bool,
    ) {
        let cursor = self
            .runtime_filter_input(analysis_id, input_kind)
            .map(|input| (input.cursor + 1).min(character_count(&input.value)))
            .unwrap_or_default();
        self.move_runtime_filter_input_cursor(analysis_id, input_kind, cursor, extend_selection);
    }

    /// 移动 Runtime 过滤输入框光标，并按需扩展选区。
    fn move_runtime_filter_input_cursor(
        &mut self,
        analysis_id: usize,
        input_kind: RuntimeFilterInputKind,
        cursor: usize,
        extend_selection: bool,
    ) {
        let Some(input) = self.runtime_filter_input_mut(analysis_id, input_kind) else {
            return;
        };
        let text_length = character_count(&input.value);
        let cursor = cursor.min(text_length);
        if extend_selection {
            input.selection_anchor.get_or_insert(input.cursor);
        } else {
            input.selection_anchor = None;
        }
        input.cursor = cursor;
        input.marked_range = None;
        input.selection_drag = None;
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

        if matches!(node.kind, SourceKind::Archive(_))
            && !self.source_archive_probe_completed_ids.contains(&source_id)
        {
            let label = node.label.clone();
            self.start_direct_source_archive_probe(source_id, node, cx);
            self.placeholder_notice = format!("正在识别 {label}，完成后继续打开或展开");
            return;
        }

        self.start_source_child_load(source_id, node, cx);
    }

    /// 启动指定可展开节点的子级后台加载。
    fn start_source_child_load(
        &mut self,
        source_id: SourceId,
        node: SourceTreeNode,
        cx: &mut Context<Self>,
    ) {
        if !node.kind.can_expand() || node.metadata.children_loaded || node.metadata.is_loading {
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
                .spawn(async move {
                    LogSourceLoader::new(loader_config)
                        .with_deferred_archive_probe()
                        .load_children(&node)
                })
                .await;

            view.update(cx, |app, cx| {
                app.apply_child_load_report_with_context(source_id, load_generation, report, cx);
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
        self.jstack_analyses.clear();
        self.next_jstack_analysis_id = 1;
        self.runtime_analyses.clear();
        self.next_runtime_analysis_id = 1;
        self.reset_log_text_selection();
        self.log_scrollbar_drag = None;
        self.reset_log_search_runtime_state();
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
        self.pending_source_analysis_after_load = None;
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
        self.clear_source_archive_probe_state();
        self.source_picker.selected_paths.clear();
        self.reset_log_workspace_after_source_replace();
        let probe_ids = self.source_registry.tree_order_source_ids().to_vec();
        self.enqueue_source_archive_probe_ids(&probe_ids, false);

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

    /// 在 UI 事件中应用根来源加载报告，并同步清理 Jstack 方块悬浮气泡。
    pub fn apply_load_report_with_context(&mut self, report: LoadReport, _cx: &mut Context<Self>) {
        self.clear_jstack_cell_hover_preview();
        self.apply_load_report(report);
    }

    /// 应用懒加载子级报告，并挂回指定父节点。
    pub fn apply_child_load_report(
        &mut self,
        parent_id: SourceId,
        load_generation: usize,
        report: LoadReport,
    ) {
        self.apply_child_load_report_internal(parent_id, load_generation, report);
    }

    /// 在 UI 回调中应用子级加载报告，并在压缩包目录加载完毕后自动续做分析动作。
    pub fn apply_child_load_report_with_context(
        &mut self,
        parent_id: SourceId,
        load_generation: usize,
        report: LoadReport,
        cx: &mut Context<Self>,
    ) {
        if self.apply_child_load_report_internal(parent_id, load_generation, report) {
            self.resume_pending_source_analysis(parent_id, cx);
        }
    }

    /// 应用懒加载子级报告的共享实现，返回是否处理了当前有效 generation。
    fn apply_child_load_report_internal(
        &mut self,
        parent_id: SourceId,
        load_generation: usize,
        report: LoadReport,
    ) -> bool {
        if self.source_child_load_generations.get(&parent_id).copied() != Some(load_generation) {
            return false;
        }
        self.source_child_load_generations.remove(&parent_id);

        if report.registry.is_empty() && !report.errors.is_empty() {
            let message = report.errors.join("；");
            self.source_registry
                .mark_children_load_failed(parent_id, message.clone());
            self.rebuild_filtered_source_ids();
            self.placeholder_notice = format!("子级加载失败：{message}");
            return true;
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
        let child_ids = self.source_registry.child_ids(parent_id).to_vec();
        self.enqueue_source_archive_probe_ids(&child_ids, false);

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
        true
    }

    /// 子级加载成功返回后续做被挂起的分析动作，避免用户二次右键。
    fn resume_pending_source_analysis(&mut self, parent_id: SourceId, cx: &mut Context<Self>) {
        let Some(action) = self.pending_source_analysis_after_load else {
            return;
        };
        if action.source_id() != parent_id {
            return;
        }

        self.pending_source_analysis_after_load = None;
        if !self
            .source_registry
            .node(parent_id)
            .is_some_and(|node| node.metadata.children_loaded)
        {
            return;
        }

        match action {
            PendingSourceAnalysisAction::Jstack { source_id } => {
                self.open_jstack_analysis_tab(source_id, cx);
            }
            PendingSourceAnalysisAction::Runtime { source_id } => {
                self.open_runtime_analysis_tab(source_id, cx);
            }
        }
    }

    /// 清理来源树压缩包探测队列；来源树整体替换时调用。
    fn clear_source_archive_probe_state(&mut self) {
        self.source_archive_probe_queue.clear();
        self.source_archive_probe_queued_ids.clear();
        self.source_archive_probe_inflight_ids.clear();
        self.source_archive_probe_direct_inflight_ids.clear();
        self.source_archive_probe_completed_ids.clear();
        self.source_archive_probe_click_intents.clear();
        self.source_archive_probe_generation = self.source_archive_probe_generation.wrapping_add(1);
        self.pending_source_analysis_after_load = None;
    }

    /// 将可见来源节点提升到压缩包探测队列前端。
    pub fn prioritize_visible_source_archive_probes(
        &mut self,
        source_ids: &[SourceId],
        cx: &mut Context<Self>,
    ) {
        self.enqueue_source_archive_probes(source_ids, true, cx);
    }

    /// 入队来源树压缩包探测任务，支持普通追加和高优先级前插。
    fn enqueue_source_archive_probes(
        &mut self,
        source_ids: &[SourceId],
        priority: bool,
        cx: &mut Context<Self>,
    ) {
        if !self.enqueue_source_archive_probe_ids(source_ids, priority) {
            return;
        }

        self.pump_source_archive_probe_queue(cx);
    }

    /// 只把来源压缩包节点放入后台探测队列，不立即启动后台任务。
    fn enqueue_source_archive_probe_ids(
        &mut self,
        source_ids: &[SourceId],
        priority: bool,
    ) -> bool {
        let mut accepted_ids = Vec::new();
        for source_id in source_ids.iter().copied() {
            if !self.should_probe_source_archive(source_id) {
                continue;
            }
            accepted_ids.push(source_id);
        }

        if accepted_ids.is_empty() {
            return false;
        }

        if priority {
            for source_id in accepted_ids.into_iter().rev() {
                if self.source_archive_probe_queued_ids.contains(&source_id) {
                    self.source_archive_probe_queue
                        .retain(|queued_id| *queued_id != source_id);
                } else {
                    self.source_archive_probe_queued_ids.insert(source_id);
                }
                self.source_archive_probe_queue.push_front(source_id);
            }
        } else {
            for source_id in accepted_ids {
                if self.source_archive_probe_queued_ids.insert(source_id) {
                    self.source_archive_probe_queue.push_back(source_id);
                }
            }
        }
        true
    }

    /// 判断来源节点是否需要后台单文件压缩包探测。
    fn should_probe_source_archive(&self, source_id: SourceId) -> bool {
        if self.source_archive_probe_completed_ids.contains(&source_id)
            || self.source_archive_probe_inflight_ids.contains(&source_id)
            || self
                .source_archive_probe_direct_inflight_ids
                .contains(&source_id)
        {
            return false;
        }

        self.source_registry.node(source_id).is_some_and(|node| {
            matches!(node.kind, SourceKind::Archive(_)) && !node.metadata.children_loaded
        })
    }

    /// 用户直接点击未探测压缩包时，绕过批量队列立即启动单节点探测。
    fn start_direct_source_archive_probe(
        &mut self,
        source_id: SourceId,
        node: SourceTreeNode,
        cx: &mut Context<Self>,
    ) {
        self.source_archive_probe_click_intents.insert(source_id);
        if self.source_archive_probe_queued_ids.remove(&source_id) {
            self.source_archive_probe_queue
                .retain(|queued_id| *queued_id != source_id);
        }

        if self
            .source_archive_probe_direct_inflight_ids
            .contains(&source_id)
            || self.source_archive_probe_inflight_ids.contains(&source_id)
        {
            return;
        }

        self.source_archive_probe_direct_inflight_ids
            .insert(source_id);
        let loader_config = self.config.loader.clone();
        let generation = self.source_archive_probe_generation;
        let request = SourceArchiveProbeRequest { source_id, node };

        cx.spawn(async move |view, cx| {
            let results = cx
                .background_executor()
                .spawn(async move {
                    LogSourceLoader::new(loader_config).probe_archive_nodes(vec![request])
                })
                .await;

            view.update(cx, |app, cx| {
                app.apply_source_archive_probe_results(generation, results, cx);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// 按批次启动后台压缩包探测；每批完成后会再次调用自身处理后续队列。
    fn pump_source_archive_probe_queue(&mut self, cx: &mut Context<Self>) {
        if !self.source_archive_probe_inflight_ids.is_empty() {
            return;
        }

        let batch_size = self
            .config
            .loader
            .archive_probe_concurrency
            .clamp(1, 16)
            .saturating_mul(SOURCE_ARCHIVE_PROBE_BATCH_FACTOR)
            .max(1);
        let mut batch_ids = Vec::new();
        while batch_ids.len() < batch_size {
            let Some(source_id) = self.source_archive_probe_queue.pop_front() else {
                break;
            };
            self.source_archive_probe_queued_ids.remove(&source_id);
            if !self.should_probe_source_archive(source_id) {
                continue;
            }
            self.source_archive_probe_inflight_ids.insert(source_id);
            batch_ids.push(source_id);
        }

        if batch_ids.is_empty() {
            return;
        }

        let requests = batch_ids
            .iter()
            .filter_map(|source_id| {
                self.source_registry.node(*source_id).cloned().map(|node| {
                    SourceArchiveProbeRequest {
                        source_id: *source_id,
                        node,
                    }
                })
            })
            .collect::<Vec<_>>();

        if requests.is_empty() {
            for source_id in batch_ids {
                self.source_archive_probe_inflight_ids.remove(&source_id);
            }
            return;
        }

        let loader_config = self.config.loader.clone();
        let generation = self.source_archive_probe_generation;
        cx.spawn(async move |view, cx| {
            let results =
                cx.background_executor()
                    .spawn(async move {
                        LogSourceLoader::new(loader_config).probe_archive_nodes(requests)
                    })
                    .await;

            view.update(cx, |app, cx| {
                app.apply_source_archive_probe_results(generation, results, cx);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// 批量应用后台单文件压缩包探测结果。
    fn apply_source_archive_probe_results(
        &mut self,
        generation: usize,
        results: Vec<SourceArchiveProbeResult>,
        cx: &mut Context<Self>,
    ) {
        if generation != self.source_archive_probe_generation {
            return;
        }

        let mut changed_count = 0;
        let mut fallback_expand_ids = Vec::new();
        let mut open_log_ids = Vec::new();

        for result in results {
            let was_completed = self
                .source_archive_probe_completed_ids
                .contains(&result.source_id);
            self.source_archive_probe_inflight_ids
                .remove(&result.source_id);
            self.source_archive_probe_direct_inflight_ids
                .remove(&result.source_id);
            self.source_archive_probe_completed_ids
                .insert(result.source_id);
            let had_click_intent = self
                .source_archive_probe_click_intents
                .remove(&result.source_id);

            if was_completed && !had_click_intent {
                continue;
            }

            if let Some(patch) = result.patch {
                let replaced = self.source_registry.replace_node_payload(
                    result.source_id,
                    patch.kind,
                    patch.location,
                    patch.metadata,
                );
                if replaced {
                    changed_count += 1;
                    if had_click_intent {
                        open_log_ids.push(result.source_id);
                    }
                }
            } else if had_click_intent {
                fallback_expand_ids.push(result.source_id);
            }
        }

        if changed_count > 0 {
            self.source_registry.rebuild_all_indices();
            self.rebuild_filtered_source_ids();
        }

        for source_id in open_log_ids {
            self.select_source(source_id);
            self.request_open_log_content(source_id, cx);
            self.scroll_source_into_view(source_id);
        }

        for source_id in fallback_expand_ids {
            if let Some(node) = self.source_registry.node(source_id).cloned() {
                self.start_source_child_load(source_id, node, cx);
            }
        }

        self.pump_source_archive_probe_queue(cx);
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

    /// 切换自动升级开关；仅影响启动后的自动检查，不影响设置页手动检查。
    pub fn toggle_upgrade_enabled(&mut self) {
        self.config.upgrade.enabled = !self.config.upgrade.enabled;
        self.placeholder_notice = if self.config.upgrade.enabled {
            "已启用启动时自动检查升级".to_string()
        } else {
            "已关闭启动时自动检查升级".to_string()
        };
        self.persist_config_or_report();
    }

    /// 返回当前平台在升级 manifest 中使用的展示文案。
    pub fn upgrade_platform_label(&self) -> String {
        format!("{}/{}", current_platform_os(), current_platform_arch())
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

    /// 在配置中创建一个测试 SSH 链接。
    fn add_test_ssh_link(app: &mut ArgusApp) -> ConnectionNodeId {
        app.config
            .connections
            .add_ssh_link(
                None,
                "测试服务器",
                crate::connections::SshLinkConfig {
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
            crate::terminal::TerminalSessionState::connecting(session_id, &link, sender);
        session.status = crate::terminal::TerminalStatus::Connected;
        app.terminal_sessions.insert(session_id, session);
    }

    /// 插入不连接真实服务器的 SFTP 会话，并返回命令接收端。
    fn insert_test_sftp_session(
        app: &mut ArgusApp,
        session_id: usize,
        link_id: ConnectionNodeId,
    ) -> std::sync::mpsc::Receiver<crate::sftp::SftpCommand> {
        let link = app
            .config
            .connections
            .link(link_id)
            .expect("应存在测试链接")
            .clone();
        let (sender, receiver) = std::sync::mpsc::channel();
        let mut session = crate::sftp::SftpSessionState::connecting(session_id, &link, sender);
        session.status = crate::sftp::SftpStatus::Connected;
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
            session.entries = vec![crate::sftp::SftpEntry {
                name: "app.log".to_string(),
                path: remote_path.clone(),
                kind: crate::sftp::SftpEntryKind::RegularFile,
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
        assert_eq!(entries.len(), 3);
        assert!(matches!(
            entries[0].action,
            MenuAction::DownloadSftpSelection { session_id } if session_id == 1
        ));
        assert!(matches!(
            entries[1].action,
            MenuAction::RenameSftpSelection { session_id } if session_id == 1
        ));
        assert!(matches!(
            entries[2].action,
            MenuAction::DeleteSftpSelection { session_id } if session_id == 1
        ));
        assert!(entries[2].is_danger);
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
                crate::sftp::SftpEntry {
                    name: "app.log".to_string(),
                    path: first_path.clone(),
                    kind: crate::sftp::SftpEntryKind::RegularFile,
                    size: Some(128),
                    mtime: None,
                    permissions: Some(0o100644),
                },
                crate::sftp::SftpEntry {
                    name: "error.log".to_string(),
                    path: second_path.clone(),
                    kind: crate::sftp::SftpEntryKind::RegularFile,
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
            Ok(crate::sftp::SftpCommand::Disconnect)
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
            session.entries = vec![crate::sftp::SftpEntry {
                name: "current".to_string(),
                path: remote_path.clone(),
                kind: crate::sftp::SftpEntryKind::Symlink,
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
            session.entries = vec![crate::sftp::SftpEntry {
                name: "app.log".to_string(),
                path: remote_path.clone(),
                kind: crate::sftp::SftpEntryKind::RegularFile,
                size: Some(128),
                mtime: None,
                permissions: Some(0o100644),
            }];
            session.selected_paths.insert(remote_path);
            session.status = crate::sftp::SftpStatus::Transferring;
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
            state.hovered_sql_cell = Some(RuntimeSqlCellKey {
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
        let first = crate::jstack_analysis::parse_jstack_snapshot(
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
        let second = crate::jstack_analysis::parse_jstack_snapshot(
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
            crate::jstack_analysis::build_analysis_result(vec![first, second], Vec::new(), 2);
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
        let first = crate::jstack_analysis::parse_jstack_snapshot(
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
        let second = crate::jstack_analysis::parse_jstack_snapshot(
            SourceId(2),
            "002.log",
            "/tmp/002.log",
            r#""same-thread" #7 tid=0x7
   java.lang.Thread.State: RUNNABLE
        at app.Second.one(Second.java:1)
"#,
        );
        let result =
            crate::jstack_analysis::build_analysis_result(vec![first, second], Vec::new(), 2);
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

    /// 验证激活日志标签页只同步来源树选中态，不触发展开或多选清理。
    #[test]
    fn activating_log_tab_only_updates_source_tree_selection() {
        let mut app = app_with_placeholder_sources();
        let logs_id = source_id_by_label(&app, "logs");
        let app_log_id = source_id_by_label(&app, "app.log");
        let error_log_id = source_id_by_label(&app, "error.log");

        app.select_source(app_log_id);
        let app_tab_id = app.active_tab_id;
        app.select_source(error_log_id);
        app.selected_search_source_ids.insert(error_log_id);
        app.selected_search_source_ids
            .insert(source_id_by_label(&app, "nested.log"));
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
            !app.source_registry
                .node(logs_id)
                .map(|source| source.expanded)
                .unwrap_or(true)
        );
        assert!(!app.visible_source_ids().contains(&app_log_id));
        assert!(app.selected_search_source_ids.contains(&error_log_id));
        assert!(
            app.selected_search_source_ids
                .contains(&source_id_by_label(&app, "nested.log"))
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
