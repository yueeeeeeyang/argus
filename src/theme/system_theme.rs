//! 文件职责：声明系统主题监听的后续平台适配边界。
//! 创建日期：2026-06-09
//! 作者：Argus 开发团队
//! 主要功能：预留 macOS 与 Linux 系统主题变化事件监听入口。

/// 系统主题监听占位结构，后续由平台层发送主题变化事件。
#[derive(Debug, Default)]
pub struct SystemThemeWatcherPlaceholder;

impl SystemThemeWatcherPlaceholder {
    /// 返回模块职责说明；当前不访问任何平台 API。
    pub fn responsibility(&self) -> &'static str {
        "监听系统明暗主题变化；当前不接入真实平台能力。"
    }
}
