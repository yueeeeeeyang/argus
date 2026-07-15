#![cfg_attr(
    all(target_os = "windows", not(debug_assertions)),
    windows_subsystem = "windows"
)]

//! 文件职责：提供 Argus 桌面客户端的最小二进制入口。
//! 创建日期：2026-06-09
//! 修改日期：2026-07-15
//! 作者：Argus 开发团队
//! 主要功能：调用库内启动入口，并在 Windows Release 构建中隐藏控制台窗口。

/// 启动 Argus 桌面客户端。
fn main() {
    argus::run();
}
