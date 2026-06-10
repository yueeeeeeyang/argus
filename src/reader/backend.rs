//! 文件职责：声明读取后端抽象的后续扩展边界。
//! 创建日期：2026-06-09
//! 作者：Argus 开发团队
//! 主要功能：预留普通文件 mmap、顺序流和临时缓存后 mmap 的统一能力描述。

/// 读取后端占位结构，后续表示不同来源的访问能力。
#[derive(Debug, Default)]
pub struct ReadBackendPlaceholder;

impl ReadBackendPlaceholder {
    /// 返回模块职责说明；当前不暴露真实读取接口。
    pub fn responsibility(&self) -> &'static str {
        "抽象不同读取后端的能力和降级策略；当前仅保留占位边界。"
    }
}
