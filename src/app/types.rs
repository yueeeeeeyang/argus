//! 文件职责：提取应用通用类型定义。
//! 创建日期：2026-07-08
//! 修改日期：2026-07-14
//! 作者：Argus 开发团队
//! 主要功能：定义工作区、标签页、文本输入目标和占位数据等跨功能域共享类型。

use gpui::FocusHandle;

use crate::loader::SourceId;

// 从共享类型模块重导出，保持 `crate::app::SettingsTextInputState` 等路径向后兼容。
pub(crate) use crate::types::{InputTextSelectionDrag, SettingsTextInputState};

/// 当前界面工作区，驱动标题栏入口和左侧侧栏内容。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Workspace {
    /// 日志分析工作区，用于展示来源侧栏和日志内容占位界面。
    LogAnalysis,
    /// 链接工作区，用于展示 SSH/SMB 链接目录树、终端和远程文件管理标签页。
    Connections,
}

/// 设置模态框左侧导航当前选中的分类。
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum SettingsSection {
    /// 关于应用，展示版本和运行平台。
    #[default]
    About,
    /// 外观设置，包含主题选择。
    Appearance,
    /// 日志显示设置，包含字号和 Jstack 过滤规则。
    LogDisplay,
    /// 日志搜索设置，包含快搜关键字。
    LogSearch,
    /// 日志加载设置，包含压缩包和符号链接策略。
    LogLoading,
}

impl SettingsSection {
    /// 返回设置分类在导航和内容标题中使用的文案。
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::About => "关于",
            Self::Appearance => "外观",
            Self::LogDisplay => "日志显示",
            Self::LogSearch => "日志搜索",
            Self::LogLoading => "日志加载",
        }
    }
}

/// 顶部标签页类型，决定主内容区渲染哪个页面。
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum TabKind {
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
    /// 远程文件管理标签页，可由 SSH SFTP 或 SMB 后端驱动。
    SftpFileManager {
        /// 远程文件会话 ID，用于从应用状态表中读取远程文件列表和操作状态。
        session_id: usize,
    },
}

/// 顶部标签页状态。
#[derive(Clone, Debug)]
pub(crate) struct ArgusTab {
    /// 标签唯一 ID，用于选中、关闭和渲染。
    pub id: usize,
    /// 标签标题。
    pub title: String,
    /// 标签内容类型。
    pub kind: TabKind,
}

/// 子级懒加载完成后需要自动续做的来源树分析动作。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PendingSourceAnalysisAction {
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
    pub(crate) fn source_id(self) -> SourceId {
        match self {
            Self::Jstack { source_id } | Self::Runtime { source_id } => source_id,
        }
    }
}

/// 日志搜索窗口输入框类型，用于复用同一套输入状态处理。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum LogSearchInputKind {
    /// 关键字输入框。
    Keyword,
    /// 来源树目录输入框。
    Directory,
}

/// Runtime 分析页过滤输入框类型。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum RuntimeFilterInputKind {
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
pub(crate) enum RuntimeDateTimePart {
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
pub(crate) enum RuntimeDateTimeQuickAction {
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
pub(crate) enum AppTextInputTarget {
    /// 来源树过滤输入框。
    SourceTreeSearch,
    /// 链接树过滤输入框。
    ConnectionTreeSearch,
    /// 远程文件管理地址栏输入框。
    SftpAddress {
        /// 远程文件管理会话 ID。
        session_id: usize,
    },
    /// 远程文件管理重命名弹窗名称输入框。
    SftpRenameName,
    /// 压缩包密码弹窗输入框。
    ArchivePassword,
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
    /// 设置模态框快搜关键字输入框。
    SettingsQuickKeywords,
    /// 设置模态框 Jstack 线程名过滤输入框。
    SettingsJstackThreadNameFilter,
    /// 设置模态框 Jstack 完整线程段过滤输入框。
    SettingsJstackStackSegmentFilter,
    /// 设置模态框升级服务器输入框。
    SettingsUpgradeServer,
    /// 设置模态框升级验签公钥输入框。
    SettingsUpgradePublicKey,
}

/// 日志搜索窗口中的单行输入框状态。
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct LogSearchInputState {
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

/// 主窗口内输入框真实焦点句柄集合。
#[derive(Clone)]
pub(crate) struct AppInputFocusHandles {
    /// 主窗口根区域焦点，用于点击非输入区域时承接真实键盘焦点。
    pub root: FocusHandle,
    /// 来源树过滤输入框焦点。
    pub source_tree_search: FocusHandle,
    /// 链接树过滤输入框焦点。
    pub connection_tree_search: FocusHandle,
    /// 远程文件管理地址栏焦点。
    pub sftp_address: FocusHandle,
    /// 远程文件管理重命名弹窗输入框焦点。
    pub sftp_rename_name: FocusHandle,
    /// 压缩包密码弹窗输入框焦点。
    pub archive_password: FocusHandle,
    /// 设置模态框快搜关键字输入框焦点。
    pub settings_quick_keywords: FocusHandle,
    /// 设置模态框 Jstack 线程名过滤输入框焦点。
    pub settings_jstack_thread_names: FocusHandle,
    /// 设置模态框升级服务器输入框焦点。
    pub settings_upgrade_server: FocusHandle,
    /// 设置模态框升级验签公钥输入框焦点。
    pub settings_upgrade_public_key: FocusHandle,
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
