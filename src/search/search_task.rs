//! 文件职责：定义日志搜索任务的 UI 生命周期状态。
//! 创建日期：2026-06-09
//! 修改日期：2026-06-11
//! 作者：Argus 开发团队
//! 主要功能：让应用层以稳定枚举描述搜索任务空闲、运行、完成、取消和失败状态。

/// 搜索任务 UI 状态；真实搜索结果和进度由应用状态分别保存。
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum SearchTaskState {
    /// 尚未启动搜索。
    Idle,
    /// 搜索正在后台执行。
    Running,
    /// 搜索完成。
    Finished,
    /// 搜索被用户取消。
    Cancelled,
    /// 搜索启动或运行失败。
    Failed(String),
}

impl Default for SearchTaskState {
    /// 默认搜索状态为空闲。
    fn default() -> Self {
        Self::Idle
    }
}

impl SearchTaskState {
    /// 返回任务是否仍在运行，用于按钮禁用和取消逻辑判断。
    pub(crate) fn is_running(&self) -> bool {
        matches!(self, Self::Running)
    }
}
