//! 文件职责：提供 AI 模型与工具任务专用的 Tokio 多线程运行时。
//! 创建日期：2026-07-15
//! 修改日期：2026-07-15
//! 作者：Argus 开发团队
//! 主要功能：隔离网络请求、取消计时和阻塞日志适配任务，避免占用 GPUI 渲染执行器。

use std::sync::OnceLock;

/// 进程内唯一 AI Tokio 运行时。
static AGENT_RUNTIME: OnceLock<tokio::runtime::Runtime> = OnceLock::new();

/// 返回专用于 AI 分析的 Tokio 运行时。
///
/// 初始化失败属于无法恢复的进程环境错误；运行时创建不涉及用户数据或网络请求。
pub(crate) fn agent_runtime() -> &'static tokio::runtime::Runtime {
    AGENT_RUNTIME.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .thread_name("argus-ai-agent")
            .enable_all()
            .build()
            .expect("无法创建 Argus AI Tokio 运行时")
    })
}
