//! 文件职责：声明内置深色主题的扩展入口。
//! 创建日期：2026-06-09
//! 作者：Argus 开发团队
//! 主要功能：保留后续从主题文件或令牌结构生成深色主题的路径。

use crate::theme::AppTheme;

/// 返回当前内置深色主题令牌。
pub fn build_dark_theme() -> AppTheme {
    AppTheme::dark()
}
