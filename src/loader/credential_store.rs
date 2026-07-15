//! 文件职责：声明临时凭据仓库的后续扩展边界。
//! 创建日期：2026-06-09
//! 作者：Argus 开发团队
//! 主要功能：预留压缩包密码等敏感信息的内存管理入口。

/// 临时凭据仓库占位结构，后续确保敏感信息不落盘。
#[derive(Debug, Default)]
pub(crate) struct CredentialStorePlaceholder;

impl CredentialStorePlaceholder {
    /// 返回模块职责说明；当前不保存任何真实凭据。
    pub(crate) fn responsibility(&self) -> &'static str {
        "以内存方式管理压缩包密码等敏感信息；当前仅保留占位边界。"
    }
}
