//! 文件职责：封装终端与 SFTP 共用的 SSH 连接、指纹和鉴权步骤。
//! 创建日期：2026-07-15
//! 修改日期：2026-07-15
//! 作者：Argus 开发团队
//! 主要功能：统一 TCP/SSH 握手、SHA256 主机指纹格式，以及私钥优先且密码兜底的鉴权顺序。

use std::net::TcpStream;
use std::path::Path;
use std::time::Duration;

use anyhow::{Context as _, Result, anyhow, bail};
use base64::{Engine as _, engine::general_purpose::STANDARD_NO_PAD};
use ssh2::{HashType, Session};

use super::worker::WorkerCancellation;
use crate::remote::connection::SshLinkConfig;

/// DNS/TCP 建连的总期限，以及每次阻塞 SSH/SFTP 操作的最大等待时间。
const SSH_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const SSH_IO_TIMEOUT: Duration = Duration::from_secs(30);

/// 建立阻塞 SSH 会话并返回统一格式的 SHA256 主机指纹。
///
/// 主机信任仍由终端和 SFTP 各自的事件协议确认；此函数只负责在鉴权前完成网络握手。
pub(super) fn connect_ssh_session(
    ssh: &SshLinkConfig,
    cancellation: &WorkerCancellation,
) -> Result<(Session, String)> {
    connect_ssh_endpoint(
        &ssh.host,
        ssh.port,
        cancellation,
        SSH_CONNECT_TIMEOUT,
        SSH_IO_TIMEOUT,
    )
}

/// 在有界连接阶段处理取消，随后为 libssh2 提供阻塞 socket 和操作超时。
fn connect_ssh_endpoint(
    host: &str,
    port: u16,
    cancellation: &WorkerCancellation,
    connect_timeout: Duration,
    io_timeout: Duration,
) -> Result<(Session, String)> {
    cancellation.ensure_active()?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("无法创建 SSH 连接运行时")?;
    let connection = runtime.block_on(async {
        tokio::select! {
            biased;
            _ = cancellation.cancelled() => bail!("SSH 连接已取消"),
            result = tokio::time::timeout(connect_timeout, tokio::net::TcpStream::connect((host, port))) => {
                result.context("SSH 连接超时")?.context("SSH TCP 连接失败")?.into_std().context("无法转换 SSH socket")
            }
        }
    });
    // 操作系统 DNS 解析不可强制终止；超时后不让运行时析构等待其完成。
    runtime.shutdown_background();
    let tcp: TcpStream = connection.with_context(|| format!("无法连接到 {host}:{port}"))?;
    tcp.set_nonblocking(false)
        .context("无法配置 SSH 阻塞 socket")?;
    tcp.set_nodelay(true).context("无法配置 SSH 低延迟传输")?;
    cancellation.attach_socket(&tcp)?;

    let mut session = Session::new().context("无法创建 SSH 会话")?;
    session.set_timeout(u32::try_from(io_timeout.as_millis()).context("SSH 操作超时超出支持范围")?);
    session.set_tcp_stream(tcp);
    session.handshake().context("SSH 握手失败")?;
    cancellation.ensure_active()?;
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::remote::worker::WorkerGuard;
    use std::net::TcpListener;
    use std::sync::mpsc;
    use std::time::Instant;

    #[test]
    fn silent_ssh_handshake_times_out_and_can_be_cancelled() {
        for should_cancel in [false, true] {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let address = listener.local_addr().unwrap();
            let (guard, cancellation) = WorkerGuard::new(1);
            let (done_sender, done_receiver) = mpsc::channel();
            let started = Instant::now();
            let worker = std::thread::spawn(move || {
                let result = connect_ssh_endpoint(
                    "127.0.0.1",
                    address.port(),
                    &cancellation,
                    Duration::from_secs(2),
                    if should_cancel {
                        Duration::from_secs(30)
                    } else {
                        Duration::from_millis(150)
                    },
                );
                done_sender.send(result.is_err()).unwrap();
            });
            let (_server, _) = listener.accept().unwrap();
            if should_cancel {
                drop(guard);
            }
            assert!(done_receiver.recv_timeout(Duration::from_secs(2)).unwrap());
            assert!(started.elapsed() < Duration::from_secs(2));
            worker.join().unwrap();
        }
    }

    #[test]
    fn cancelled_ssh_connection_does_not_start_network_work() {
        let (guard, cancellation) = WorkerGuard::new(1);
        drop(guard);
        assert!(
            connect_ssh_endpoint(
                "invalid.example",
                22,
                &cancellation,
                Duration::from_secs(1),
                Duration::from_secs(1)
            )
            .is_err()
        );
    }
}
