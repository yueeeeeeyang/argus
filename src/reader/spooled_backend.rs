//! 文件职责：声明临时缓存后 mmap 后端的后续扩展边界。
//! 创建日期：2026-06-09
//! 作者：Argus 开发团队
//! 主要功能：预留顺序来源缓存落地后获得随机访问能力的读取入口。

/// 临时缓存后端占位结构，后续读取受控缓存文件。
#[derive(Debug, Default)]
pub struct SpooledBackendPlaceholder;

impl SpooledBackendPlaceholder {
    /// 返回模块职责说明；当前不访问真实缓存文件。
    pub fn responsibility(&self) -> &'static str {
        "读取临时缓存后的随机访问来源；当前仅保留占位边界。"
    }
}
