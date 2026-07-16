//! 文件职责：在操作系统凭据库中保存和读取 AI 服务 API Key。
//! 创建日期：2026-07-15
//! 修改日期：2026-07-15
//! 作者：Argus 开发团队
//! 主要功能：按规范化端点摘要隔离凭据条目，确保 API Key 不进入 TOML、日志或错误详情。

use secrecy::SecretString;
use sha2::{Digest, Sha256};

/// 系统凭据库固定服务名。
const AI_KEYRING_SERVICE: &str = "argus.ai";

/// 保存指定模型端点的 API Key。
///
/// 参数说明：
/// - `base_url`：已经规范化的 OpenAI 兼容根地址。
/// - `api_key`：用户输入的密钥明文，仅在当前调用栈和凭据后端中短暂存在。
///
/// 返回值：保存成功返回 `Ok`；凭据库不可用时返回不包含密钥的错误文本。
pub(crate) fn save_api_key(base_url: &str, api_key: &str) -> Result<(), String> {
    let entry = keyring::Entry::new(AI_KEYRING_SERVICE, &credential_account(base_url))
        .map_err(|error| format!("无法访问系统凭据库：{error}"))?;
    entry
        .set_password(api_key)
        .map_err(|error| format!("无法保存 AI API Key：{error}"))
}

/// 读取指定模型端点的 API Key，并用 `SecretString` 限制意外 Debug 输出。
pub(crate) fn load_api_key(base_url: &str) -> Result<SecretString, String> {
    let entry = keyring::Entry::new(AI_KEYRING_SERVICE, &credential_account(base_url))
        .map_err(|error| format!("无法访问系统凭据库：{error}"))?;
    entry
        .get_password()
        .map(SecretString::from)
        .map_err(|_| "未找到当前 AI 服务的 API Key，请先在设置中保存密钥".to_string())
}

/// 使用端点 SHA-256 摘要生成不泄漏服务地址的凭据 account。
fn credential_account(base_url: &str) -> String {
    hex::encode(Sha256::digest(base_url.trim().as_bytes()))
}

#[cfg(test)]
mod tests {
    use super::credential_account;

    /// 验证凭据 account 长度固定且不包含原始端点文本。
    #[test]
    fn credential_account_hides_endpoint() {
        let account = credential_account("https://ai.example.com/v1");
        assert_eq!(account.len(), 64);
        assert!(!account.contains("example"));
    }
}
