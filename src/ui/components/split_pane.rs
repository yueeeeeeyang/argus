//! 文件职责：声明分割面板组件的后续扩展边界。
//! 创建日期：2026-06-09
//! 作者：Argus 开发团队
//! 主要功能：预留来源侧栏与内容区的可拖拽布局调整能力。

/// 分割面板占位结构，后续管理面板尺寸和拖拽状态。
#[derive(Debug, Default)]
pub struct SplitPanePlaceholder;

impl SplitPanePlaceholder {
    /// 返回组件职责说明；当前不处理真实拖拽。
    pub fn responsibility(&self) -> &'static str {
        "管理可拖拽分割布局；当前仅保留组件边界。"
    }
}
