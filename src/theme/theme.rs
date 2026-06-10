//! 文件职责：定义 Argus 界面使用的主题令牌。
//! 创建日期：2026-06-09
//! 作者：Argus 开发团队
//! 主要功能：提供深色主题颜色、日志级别颜色和紧凑布局尺寸。

/// 应用主题令牌，当前阶段只提供内置深色主题的占位值。
#[derive(Clone, Debug)]
pub struct AppTheme {
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
}

impl AppTheme {
    /// 构造设计文档中的内置深色主题。
    pub fn dark() -> Self {
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
        }
    }

    /// 根据日志级别返回对应颜色，未知级别回退为主文本颜色。
    pub fn color_for_level(&self, level: &str) -> u32 {
        match level {
            "DEBUG" => self.debug,
            "INFO" => self.info,
            "WARN" => self.warning,
            "ERROR" => self.error,
            _ => self.foreground,
        }
    }
}
