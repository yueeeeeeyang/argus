//! 文件职责：声明右键上下文菜单组件的后续扩展边界。
//! 创建日期：2026-06-09
//! 作者：Argus 开发团队
//! 主要功能：预留日志行、来源节点和标签页的上下文操作菜单。

/// 上下文菜单占位结构，后续负责菜单分组和快捷键提示。
#[derive(Debug, Default)]
pub struct ContextMenuPlaceholder;

impl ContextMenuPlaceholder {
    /// 返回组件职责说明；当前不响应真实右键事件。
    pub fn responsibility(&self) -> &'static str {
        "提供分组上下文操作菜单；当前仅保留组件边界。"
    }
}
