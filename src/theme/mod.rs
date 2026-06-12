//! 文件职责：导出主题系统的公共模块与当前占位主题令牌。
//! 创建日期：2026-06-09
//! 修改日期：2026-06-12
//! 作者：Argus 开发团队
//! 主要功能：集中暴露主题定义、主题管理器和内置暗色主题入口。

#![allow(clippy::module_inception)]
// 当前目录使用 `theme/theme.rs` 承载核心主题模型，`theme/mod.rs` 仅作为领域出口保留。

pub mod builtin;
pub mod theme;
pub mod theme_manager;

pub use theme::{AppTheme, SyntaxTheme};
pub use theme_manager::{BUILTIN_DARK_THEME_ID, ThemeError, ThemeManager, ThemeOption};
