//! 文件职责：声明树形控件组件的后续扩展边界。
//! 创建日期：2026-06-09
//! 作者：Argus 开发团队
//! 主要功能：预留懒加载目录树、展开折叠和节点选择能力。

/// 树形控件占位结构，后续承载目录树展开折叠状态。
#[derive(Debug, Default)]
pub(crate) struct TreeViewPlaceholder;

impl TreeViewPlaceholder {
    /// 返回组件职责说明；当前不触发真实懒加载。
    pub(crate) fn responsibility(&self) -> &'static str {
        "展示来源目录树并支持懒加载；当前仅保留组件边界。"
    }
}
