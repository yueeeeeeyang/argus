//! 文件职责：声明压缩条目临时缓存管理模块的后续扩展边界。
//! 创建日期：2026-06-09
//! 作者：Argus 开发团队
//! 主要功能：预留顺序来源转随机访问来源时的受控临时缓存入口。

/// 临时缓存管理占位结构，后续负责缓存限额、取消和清理。
#[derive(Debug, Default)]
pub(crate) struct SpoolManagerPlaceholder;

impl SpoolManagerPlaceholder {
    /// 返回模块职责说明；当前不创建任何临时文件。
    pub(crate) fn responsibility(&self) -> &'static str {
        "管理压缩条目临时缓存和清理；当前仅保留占位边界。"
    }
}
