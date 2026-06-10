//! 文件职责：声明虚拟列表组件的后续扩展边界。
//! 创建日期：2026-06-09
//! 作者：Argus 开发团队
//! 主要功能：预留大日志行虚拟滚动渲染能力，当前不实现真实范围请求。

/// 虚拟列表占位结构，后续负责按可见范围请求日志行。
#[derive(Debug, Default)]
pub struct VirtualListPlaceholder;

impl VirtualListPlaceholder {
    /// 返回组件职责说明；当前不执行真实虚拟滚动计算。
    pub fn responsibility(&self) -> &'static str {
        "按视口范围渲染日志行；当前仅保留组件边界。"
    }
}
