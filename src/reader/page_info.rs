//! 文件职责：声明分页信息模型的后续扩展边界。
//! 创建日期：2026-06-09
//! 作者：Argus 开发团队
//! 主要功能：预留页号、字节范围、起始行号和真实换行边界信息。

/// 分页信息占位结构，后续记录真实页边界。
#[derive(Debug, Default)]
pub struct PageInfoPlaceholder;

impl PageInfoPlaceholder {
    /// 返回模块职责说明；当前不计算真实页边界。
    pub fn responsibility(&self) -> &'static str {
        "描述分页字节范围和行号映射；当前仅保留占位边界。"
    }
}
