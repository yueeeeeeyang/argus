//! 文件职责：声明异步任务工具的后续扩展边界。
//! 创建日期：2026-06-09
//! 作者：Argus 开发团队
//! 主要功能：预留后台加载、索引、搜索任务的取消与状态回传封装。

/// 异步任务占位结构，后续统一管理后台任务生命周期。
#[derive(Debug, Default)]
pub(crate) struct AsyncTaskPlaceholder;

impl AsyncTaskPlaceholder {
    /// 返回模块职责说明；当前不启动任何后台任务。
    pub(crate) fn responsibility(&self) -> &'static str {
        "封装后台任务取消和状态回传；当前仅保留占位边界。"
    }
}
