//! 文件职责：Argus 库入口，集中导出应用、UI 与占位业务模块。
//! 创建日期：2026-06-09
//! 修改日期：2026-06-16
//! 作者：Argus 开发团队
//! 主要功能：为二进制入口和后续测试暴露模块边界。

pub mod app;
pub mod assets;
pub mod config;
pub mod connections;
pub mod fonts;
pub mod highlight;
pub mod jstack_analysis;
pub mod loader;
pub mod platform;
pub mod reader;
pub mod runtime_analysis;
pub mod search;
pub mod sftp;
pub mod terminal;
pub mod text_selection;
pub mod theme;
pub mod ui;
pub mod updater;
pub mod utils;
