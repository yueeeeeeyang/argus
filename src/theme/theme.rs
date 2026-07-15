//! 文件职责：定义 Argus 界面使用的主题令牌。
//! 创建日期：2026-06-09
//! 修改日期：2026-06-12
//! 作者：Argus 开发团队
//! 主要功能：提供运行期主题颜色、日志级别颜色和主题文件损坏时的紧急兜底令牌。

/// 语法高亮主题令牌，供日志、配置文件和 Java 线程栈高亮复用。
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct SyntaxTheme {
    /// 注释颜色。
    pub comment: u32,
    /// 配置键或 JSON/YAML 属性名颜色。
    pub key: u32,
    /// 字符串颜色。
    pub string: u32,
    /// 数字颜色。
    pub number: u32,
    /// 布尔值和 null 颜色。
    pub boolean: u32,
    /// 标点符号颜色。
    pub punctuation: u32,
    /// XML 标签颜色。
    pub tag: u32,
    /// XML 属性名颜色。
    pub attribute: u32,
    /// 日志时间戳颜色。
    pub timestamp: u32,
    /// Java 线程名颜色。
    pub thread: u32,
    /// Java 类名颜色。
    pub class: u32,
    /// Java 方法名颜色。
    pub method: u32,
    /// Java 锁对象或等待目标颜色。
    pub lock: u32,
    /// 异常、错误和死锁提示颜色。
    pub exception: u32,
}

impl SyntaxTheme {
    /// 构造深色主题语法高亮紧急兜底令牌。
    pub(crate) fn dark() -> Self {
        Self {
            comment: 0x6a9955,
            key: 0x9cdcfe,
            string: 0xce9178,
            number: 0xb5cea8,
            boolean: 0x569cd6,
            punctuation: 0x808080,
            tag: 0x569cd6,
            attribute: 0x9cdcfe,
            timestamp: 0x8cdcfe,
            thread: 0xdcdcaa,
            class: 0x4ec9b0,
            method: 0xdcdcaa,
            lock: 0xc586c0,
            exception: 0xf48771,
        }
    }
}

/// 应用主题令牌，正常运行时由主题管理器读取 TOML 后生成。
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct AppTheme {
    /// 主窗口背景色。
    pub background: u32,
    /// 自定义标题栏背景色。
    pub title_bar: u32,
    /// 左侧活动栏背景色。
    pub activity_bar: u32,
    /// 来源侧栏背景色。
    pub side_bar: u32,
    /// 主内容区背景色。
    pub content: u32,
    /// 状态栏背景色。
    pub status_bar: u32,
    /// 主文本颜色。
    pub foreground: u32,
    /// 次级文本颜色。
    pub foreground_muted: u32,
    /// 边框与分割线颜色。
    pub border: u32,
    /// 选中项背景色。
    pub selection: u32,
    /// 当前行背景色。
    pub current_line: u32,
    /// DEBUG 日志颜色。
    pub debug: u32,
    /// INFO 日志颜色。
    pub info: u32,
    /// WARN 日志颜色。
    pub warning: u32,
    /// ERROR 日志颜色。
    pub error: u32,
    /// 成功或就绪状态颜色。
    pub success: u32,
    /// 模态框遮罩 RGBA 颜色，用于弱化背景但避免全黑压暗。
    pub modal_overlay: u32,
    /// 语法高亮颜色。
    pub syntax: SyntaxTheme,
}

impl AppTheme {
    /// 构造深色主题紧急兜底令牌；正常路径应优先读取 `themes/dark.toml`。
    pub(crate) fn dark() -> Self {
        Self {
            background: 0x1e1e1e,
            title_bar: 0x333333,
            activity_bar: 0x252526,
            side_bar: 0x252526,
            content: 0x1e1e1e,
            status_bar: 0x202020,
            foreground: 0xd4d4d4,
            foreground_muted: 0x858585,
            border: 0x3c3c3c,
            selection: 0x264f78,
            current_line: 0x2a2d2e,
            debug: 0xb5cea8,
            info: 0x75beff,
            warning: 0xcca700,
            error: 0xf48771,
            success: 0x89d185,
            modal_overlay: 0x1e1e1eb8,
            syntax: SyntaxTheme::dark(),
        }
    }

    /// 根据日志级别返回对应颜色，未知级别回退为主文本颜色。
    pub(crate) fn color_for_level(&self, level: &str) -> u32 {
        match level {
            "DEBUG" => self.debug,
            "INFO" => self.info,
            "WARN" => self.warning,
            "ERROR" => self.error,
            _ => self.foreground,
        }
    }
}
