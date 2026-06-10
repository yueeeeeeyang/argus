//! 文件职责：声明内置浅色主题的扩展入口。
//! 创建日期：2026-06-09
//! 作者：Argus 开发团队
//! 主要功能：保留浅色主题令牌定义，当前暂不接入界面切换。

/// 浅色主题占位结构，后续补齐完整令牌后由主题管理器使用。
#[derive(Debug, Default)]
pub struct LightThemePlaceholder;

impl LightThemePlaceholder {
    /// 返回模块职责说明；当前不参与运行时主题选择。
    pub fn responsibility(&self) -> &'static str {
        "提供浅色主题令牌；当前界面只使用深色主题占位。"
    }
}
