//! 文件职责：声明内置深色主题的扩展入口。
//! 创建日期：2026-06-09
//! 作者：Argus 开发团队
//! 主要功能：保留后续从主题文件或令牌结构生成深色主题的路径。

use crate::theme::AppTheme;
use crate::theme::ThemeManager;

/// 返回当前内置深色主题令牌，正常路径从内置 TOML 解析生成。
pub(crate) fn build_dark_theme() -> AppTheme {
    ThemeManager::builtin_dark_theme()
}
