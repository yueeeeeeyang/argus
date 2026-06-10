//! 文件职责：声明内置浅色主题的扩展入口。
//! 创建日期：2026-06-09
//! 修改日期：2026-06-10
//! 作者：Argus 开发团队
//! 主要功能：提供运行时可切换的浅色主题令牌。

use crate::theme::AppTheme;
use crate::theme::ThemeManager;

/// 返回当前内置浅色主题令牌，正常路径从内置 TOML 解析生成。
pub fn build_light_theme() -> AppTheme {
    ThemeManager::builtin_light_theme()
}
