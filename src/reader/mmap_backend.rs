//! 文件职责：声明普通文件 mmap 后端的后续扩展边界。
//! 创建日期：2026-06-09
//! 作者：Argus 开发团队
//! 主要功能：预留随机访问文件的内存映射读取能力。

/// mmap 后端占位结构，后续用于普通文件和临时缓存文件。
#[derive(Debug, Default)]
pub struct MmapBackendPlaceholder;

impl MmapBackendPlaceholder {
    /// 返回模块职责说明；当前不创建真实内存映射。
    pub fn responsibility(&self) -> &'static str {
        "提供随机访问读取能力；当前仅保留占位边界。"
    }
}
