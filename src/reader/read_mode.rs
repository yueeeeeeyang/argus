//! 文件职责：声明读取模式模型的后续扩展边界。
//! 创建日期：2026-06-09
//! 作者：Argus 开发团队
//! 主要功能：预留直接模式、分页模式和顺序预览模式的选择结果。

/// 读取模式占位枚举，后续根据来源能力和文件大小选择。
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum ReadModePlaceholder {
    /// 小型随机访问来源的直接模式。
    Direct,
    /// 大型随机访问来源的分页模式。
    Paged,
    /// 不可随机访问来源的顺序预览模式。
    SequentialPreview,
}

impl ReadModePlaceholder {
    /// 返回读取模式职责说明；当前不参与真实读取决策。
    pub fn responsibility(&self) -> &'static str {
        "描述日志读取模式；当前仅保留占位枚举。"
    }
}
