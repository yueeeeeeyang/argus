//! 文件职责：封装终端与 SFTP 共用的 SSH 连接、指纹和鉴权步骤。
//! 创建日期：2026-07-15
//! 修改日期：2026-07-15
//! 作者：Argus 开发团队
//! 主要功能：统一 TCP/SSH 握手、SHA256 主机指纹格式，以及私钥优先且密码兜底的鉴权顺序。

use std::net::TcpStream;
use std::path::Path;

use anyhow::{Context as _, Result, anyhow, bail};
use base64::{Engine as _, engine::general_purpose::STANDARD_NO_PAD};
use ssh2::{HashType, Session};

use crate::remote::connection::SshLinkConfig;

/// 建立阻塞 SSH 会话并返回统一格式的 SHA256 主机指纹。
///
/// 主机信任仍由终端和 SFTP 各自的事件协议确认；此函数只负责在鉴权前完成网络握手。
pub(super) fn connect_ssh_session(ssh: &SshLinkConfig) -> Result<(Session, String)> {
    let tcp = TcpStream::connect((ssh.host.as_str(), ssh.port))
        .with_context(|| format!("无法连接到 {}:{}", ssh.host, ssh.port))?;
    // 禁用 Nagle 可降低交互式终端输入延迟；失败不影响连接正确性，因此保持非阻塞告警语义。
    tcp.set_nodelay(true).ok();

    let mut session = Session::new().context("无法创建 SSH 会话")?;
    session.set_tcp_stream(tcp);
    session.handshake().context("SSH 握手失败")?;
    let fingerprint = sha256_fingerprint(&session).context("无法获取 SSH 主机指纹")?;
    Ok((session, fingerprint))
}

/// 按“私钥优先、密码兜底”的固定顺序执行 SSH 鉴权。
///
/// 私钥失败不会阻断密码尝试；全部凭据失败时合并错误，便于用户判断配置问题。
pub(super) fn authenticate_ssh_session(session: &Session, ssh: &SshLinkConfig) -> Result<()> {
    let mut auth_errors = Vec::new();
    if let Some(private_key_path) = ssh.private_key_path.as_deref() {
        let passphrase = ssh.private_key_passphrase.as_deref();
        if let Err(error) = session.userauth_pubkey_file(
            &ssh.username,
            None,
            Path::new(private_key_path),
            passphrase,
        ) {
            auth_errors.push(format!("私钥鉴权失败：{error}"));
        }
    }
    if !session.authenticated()
        && !ssh.password.is_empty()
        && let Err(error) = session.userauth_password(&ssh.username, &ssh.password)
    {
        auth_errors.push(format!("密码鉴权失败：{error}"));
    }

    if session.authenticated() {
        Ok(())
    } else if auth_errors.is_empty() {
        bail!("SSH 鉴权失败：未配置可用凭据")
    } else {
        bail!("SSH 鉴权失败：{}", auth_errors.join("；"))
    }
}

/// 把 libssh2 返回的原始 SHA256 摘要编码为 OpenSSH 常见的 `SHA256:` 文本。
fn sha256_fingerprint(session: &Session) -> Result<String> {
    let hash = session
        .host_key_hash(HashType::Sha256)
        .ok_or_else(|| anyhow!("服务器未返回 SHA256 主机指纹"))?;
    Ok(format!("SHA256:{}", STANDARD_NO_PAD.encode(hash)))
}
