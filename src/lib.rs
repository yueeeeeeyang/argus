//! 文件职责：Argus 库入口，仅向桌面二进制暴露应用启动能力。
//! 创建日期：2026-06-09
//! 修改日期：2026-07-15
//! 作者：Argus 开发团队
//! 主要功能：封装全部内部模块，并提供稳定且最小化的启动入口。

mod analysis;
mod app;
mod assets;
mod bootstrap;
mod config;
mod fonts;
mod highlight;
mod infra;
mod loader;
mod platform;
mod reader;
mod remote;
mod search;
mod theme;
mod types;
mod ui;
mod utils;

/// 启动 Argus 桌面客户端。
///
/// 此函数是 crate 唯一的外部接口；应用状态、UI 和基础设施模块均保持内部可见，避免形成
/// 无意维护的 Rust 库兼容承诺。
pub fn run() {
    bootstrap::run();
}
