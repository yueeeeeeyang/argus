//! 文件职责：声明搜索任务状态模块的后续扩展边界。
//! 创建日期：2026-06-09
//! 作者：Argus 开发团队
//! 主要功能：预留搜索任务生命周期、取消标记和进度状态。

/// 搜索任务占位结构，后续跟踪运行、取消和完成状态。
#[derive(Debug, Default)]
pub struct SearchTaskPlaceholder;

impl SearchTaskPlaceholder {
    /// 返回模块职责说明；当前不启动后台任务。
    pub fn responsibility(&self) -> &'static str {
        "管理搜索任务状态和取消信号；当前仅保留占位边界。"
    }
}
