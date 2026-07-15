//! 文件职责：使用纯 Rust ra_svn、russh 与内置 DAV 客户端实现 SVN 仓库只读文件管理。
//! 创建日期：2026-07-15
//! 作者：Argus 开发团队
//! 主要功能：按 URL 选择 ra_svn 或 HTTP DAV 会话，限制浏览根目录并支持 HEAD/固定修订切换。

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, mpsc};
use std::time::Duration;

use anyhow::{Context as AnyhowContext, Result, anyhow, bail};
use async_channel::Sender;
use chrono::DateTime;
use percent_encoding::percent_decode_str;
use svn::{NodeKind, RaSvnClient, RaSvnSession, SshAuth, SshConfig, SvnUrl};

use crate::config::paths::argus_svn_known_hosts_file;
use crate::remote::connection::SvnLinkConfig;
use crate::remote::remote_file::{
    FilePreviewContent, REMOTE_FILE_PREVIEW_MAX_FILE_SIZE, REMOTE_FILE_PREVIEW_MAX_READ,
    RemoteFileCommand, RemoteFileEntry, RemoteFileEntryKind, RemoteFileEvent, RepositoryVersion,
    RepositoryVersionKind, local_download_path, preview_content_from_bytes, remote_child_path,
    remote_file_name, resolve_remote_path, send_event_blocking, send_file_preview_loaded,
    send_operation_failed,
};
use crate::remote::svn_http::{HttpSvnNodeStat, HttpSvnSession};

/// 当前 SVN 会话的版本选择；HEAD 在每次刷新时重新解析，固定修订保持不变。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SvnRevisionSelection {
    /// 跟随仓库最新版本，同时保存本次操作实际解析到的修订号。
    Head(u64),
    /// 用户固定的非负数字修订号。
    Fixed(u64),
}

/// SVN 节点最小属性集合，统一 ra_svn 与 HTTP DAV 的下载、预览边界判断。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct SvnNodeStat {
    /// 节点类型；只有普通文件允许下载或预览。
    kind: RemoteFileEntryKind,
    /// 文件大小；服务端无法提供时为空。
    size: Option<u64>,
}

/// SVN 只读传输会话；HTTP(S) 与 svn(svn+ssh) 在 worker 上层共享同一命令语义。
enum SvnReadSession {
    /// `svn://` 或 `svn+ssh://` 使用的纯 Rust ra_svn 会话。
    RaSvn(Box<RaSvnSession>),
    /// `http://` 或 `https://` 使用的内置 HTTPv2 DAV 会话。
    Http(Box<HttpSvnSession>),
}

/// russh 预检处理器，仅捕获服务端主机公钥，不尝试任何用户认证或本地身份回退。
#[derive(Clone, Default)]
struct SvnSshHostKeyCapture {
    /// 服务端在密钥交换阶段提供的公钥。
    server_key: Arc<Mutex<Option<russh::keys::PublicKey>>>,
}

impl russh::client::Handler for SvnSshHostKeyCapture {
    type Error = russh::Error;

    /// 临时接受公钥以完成预检握手；真正信任判断紧接着在同一 worker 中完成。
    async fn check_server_key(
        &mut self,
        server_public_key: &russh::keys::PublicKey,
    ) -> std::result::Result<bool, Self::Error> {
        *self.server_key.lock().expect("主机公钥捕获锁不应中毒") = Some(server_public_key.clone());
        Ok(true)
    }
}

impl SvnRevisionSelection {
    /// 返回本次读取使用的具体修订号。
    fn resolved(self) -> u64 {
        match self {
            Self::Head(revision) | Self::Fixed(revision) => revision,
        }
    }

    /// 返回发送给 worker 的稳定版本 ID。
    fn id(self) -> String {
        match self {
            Self::Head(_) => "HEAD".to_string(),
            Self::Fixed(revision) => format!("r{revision}"),
        }
    }

    /// 返回版本输入框展示文本。
    fn display(self) -> String {
        match self {
            Self::Head(revision) => format!("HEAD (r{revision})"),
            Self::Fixed(revision) => format!("r{revision}"),
        }
    }
}

impl SvnReadSession {
    /// 按 URL 协议创建内置 SVN 读取会话。
    async fn open(config: &SvnLinkConfig) -> Result<Self> {
        if matches!(repository_url_scheme(&config.url), Some("http" | "https")) {
            return HttpSvnSession::open(config).map(Box::new).map(Self::Http);
        }
        let client = create_svn_client(config)?;
        client
            .open_session()
            .await
            .context("无法连接 SVN 仓库")
            .map(Box::new)
            .map(Self::RaSvn)
    }

    /// 查询仓库最新修订号。
    async fn latest_revision(&mut self) -> Result<u64> {
        match self {
            Self::RaSvn(session) => session
                .get_latest_rev()
                .await
                .context("无法读取 SVN HEAD 修订号"),
            Self::Http(session) => session.latest_revision(),
        }
    }

    /// 枚举以链接位置为根的指定目录，并映射为通用远程文件条目。
    async fn list_directory(&mut self, path: &str, revision: u64) -> Result<Vec<RemoteFileEntry>> {
        match self {
            Self::RaSvn(session) => {
                let relative = repository_relative_path(path)?;
                let listing = session
                    .list_dir(&relative, Some(revision))
                    .await
                    .with_context(|| format!("无法读取 SVN 目录 {path} @ r{revision}"))?;
                Ok(listing
                    .entries
                    .into_iter()
                    .map(|entry| RemoteFileEntry {
                        path: remote_child_path(path, &entry.name),
                        name: entry.name,
                        kind: svn_node_kind(entry.kind),
                        size: entry.size,
                        mtime: entry.created_date.as_deref().and_then(parse_svn_timestamp),
                        permissions: None,
                    })
                    .collect())
            }
            Self::Http(session) => session.list_directory(path, revision),
        }
    }

    /// 查询指定路径在历史修订中的节点类型与大小。
    async fn stat(&mut self, path: &str, revision: u64) -> Result<Option<SvnNodeStat>> {
        match self {
            Self::RaSvn(session) => {
                let relative = repository_file_path(path)?;
                session
                    .stat(&relative, Some(revision))
                    .await
                    .with_context(|| format!("无法读取 SVN 文件信息：{path}"))
                    .map(|stat| {
                        stat.map(|stat| SvnNodeStat {
                            kind: svn_node_kind(stat.kind),
                            size: stat.size,
                        })
                    })
            }
            Self::Http(session) => session
                .stat(path, revision)
                .map(|stat| stat.map(svn_http_node_stat)),
        }
    }

    /// 读取指定文件前 `max_bytes` 字节。
    async fn read_file_bytes(
        &mut self,
        path: &str,
        revision: u64,
        max_bytes: u64,
    ) -> Result<Vec<u8>> {
        match self {
            Self::RaSvn(session) => {
                let relative = repository_file_path(path)?;
                session
                    .get_file_bytes(&relative, revision, max_bytes)
                    .await
                    .with_context(|| format!("读取 SVN 文件失败：{path}"))
            }
            Self::Http(session) => session.read_file_bytes(path, revision, max_bytes),
        }
    }

    /// 把指定历史修订文件写入本地路径；HTTP 分支保持流式下载。
    async fn download_file(
        &mut self,
        path: &str,
        revision: u64,
        local_path: &Path,
        max_bytes: u64,
    ) -> Result<()> {
        match self {
            Self::RaSvn(session) => {
                let relative = repository_file_path(path)?;
                let mut local = tokio::fs::File::create(local_path)
                    .await
                    .with_context(|| format!("无法创建本地文件 {}", local_path.display()))?;
                session
                    .get_file(&relative, revision, false, &mut local, max_bytes)
                    .await
                    .with_context(|| format!("下载 SVN 文件失败：{path}"))
                    .map(|_| ())
            }
            Self::Http(session) => session.download_file(path, revision, local_path),
        }
    }
}

/// SVN worker 同步入口：创建单线程异步运行时并保持一个活动只读传输会话。
pub(super) fn run_svn_worker(
    session_id: usize,
    svn: SvnLinkConfig,
    trusted_fingerprint: Option<String>,
    command_receiver: mpsc::Receiver<RemoteFileCommand>,
    event_sender: Sender<RemoteFileEvent>,
) -> Result<()> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("无法创建 SVN 异步运行时")?;
    runtime.block_on(run_svn_worker_async(
        session_id,
        svn,
        trusted_fingerprint,
        command_receiver,
        event_sender,
    ))
}

/// SVN worker 异步主流程：连接一次并在同一会话内串行处理读取命令。
async fn run_svn_worker_async(
    session_id: usize,
    config: SvnLinkConfig,
    trusted_fingerprint: Option<String>,
    command_receiver: mpsc::Receiver<RemoteFileCommand>,
    event_sender: Sender<RemoteFileEvent>,
) -> Result<()> {
    if config.url.to_ascii_lowercase().starts_with("svn+ssh://") {
        let parsed_url = SvnUrl::parse(&config.url).context("SVN SSH URL 无效")?;
        let server_key = preflight_svn_ssh_host_key(&parsed_url.host, parsed_url.port).await?;
        verify_svn_ssh_host_key(
            session_id,
            &parsed_url.host,
            parsed_url.port,
            &server_key,
            trusted_fingerprint.as_deref(),
            &command_receiver,
            &event_sender,
        )?;
    }
    let mut session = SvnReadSession::open(&config).await?;
    let latest_revision = session.latest_revision().await?;
    let mut selection = SvnRevisionSelection::Head(latest_revision);
    let mut current_dir = "/".to_string();
    let entries = read_svn_directory(&mut session, &current_dir, selection.resolved()).await?;
    send_event_blocking(
        &event_sender,
        RemoteFileEvent::Connected {
            session_id,
            current_dir: current_dir.clone(),
            entries,
        },
    );
    let initial_message = match repository_url_scheme(&config.url) {
        Some("svn") => Some("svn:// 链路未加密，敏感仓库请使用 svn+ssh://".to_string()),
        Some("http") => Some(
            "HTTP SVN 链路未加密，用户名、密码和仓库内容可能被窃听；敏感仓库请使用 HTTPS"
                .to_string(),
        ),
        _ => None,
    };
    send_svn_version(session_id, selection, initial_message, &event_sender);

    while let Ok(command) = command_receiver.recv() {
        match command {
            RemoteFileCommand::TrustHostKey | RemoteFileCommand::RejectHostKey => {}
            RemoteFileCommand::LoadDirectory(path) => {
                let target_path = resolve_remote_path(&current_dir, &path)
                    .and_then(|path| normalize_repository_path(&path));
                let result = match target_path {
                    Ok(target) => read_svn_directory(&mut session, &target, selection.resolved())
                        .await
                        .map(|entries| (target, entries)),
                    Err(error) => Err(error),
                };
                match result {
                    Ok((target, entries)) => {
                        current_dir = target;
                        send_event_blocking(
                            &event_sender,
                            RemoteFileEvent::DirectoryLoaded {
                                session_id,
                                current_dir: current_dir.clone(),
                                entries,
                            },
                        );
                    }
                    Err(error) => send_operation_failed(&event_sender, session_id, error),
                }
            }
            RemoteFileCommand::Refresh => {
                let next_selection = if matches!(selection, SvnRevisionSelection::Head(_)) {
                    match session.latest_revision().await {
                        Ok(revision) => SvnRevisionSelection::Head(revision),
                        Err(error) => {
                            send_operation_failed(
                                &event_sender,
                                session_id,
                                anyhow!("无法刷新 SVN HEAD：{error}"),
                            );
                            continue;
                        }
                    }
                } else {
                    selection
                };
                let refreshed_directory =
                    read_svn_directory(&mut session, &current_dir, next_selection.resolved()).await;
                let (next_dir, entries, directory_was_reset) = match refreshed_directory {
                    Ok(entries) => (current_dir.clone(), entries, false),
                    Err(current_error)
                        if next_selection != selection && current_dir.as_str() != "/" =>
                    {
                        match read_svn_directory(&mut session, "/", next_selection.resolved()).await
                        {
                            Ok(entries) => ("/".to_string(), entries, true),
                            Err(root_error) => {
                                send_operation_failed(
                                    &event_sender,
                                    session_id,
                                    root_error.context(format!(
                                        "刷新后的当前 SVN 目录不可用：{current_error}"
                                    )),
                                );
                                continue;
                            }
                        }
                    }
                    Err(error) => {
                        send_operation_failed(&event_sender, session_id, error);
                        continue;
                    }
                };
                // 只有候选修订的目录已经成功读取，才同时提交版本和目录状态。
                selection = next_selection;
                current_dir = next_dir;
                let message = if directory_was_reset {
                    "SVN 仓库已刷新；当前目录已不存在，已回到仓库根目录"
                } else {
                    "SVN 仓库已刷新"
                };
                send_svn_version(
                    session_id,
                    selection,
                    Some(message.to_string()),
                    &event_sender,
                );
                send_event_blocking(
                    &event_sender,
                    RemoteFileEvent::DirectoryLoaded {
                        session_id,
                        current_dir: current_dir.clone(),
                        entries,
                    },
                );
            }
            RemoteFileCommand::SwitchRepositoryVersion { version_id } => {
                let next_selection = match parse_svn_revision(&version_id) {
                    Ok(None) => match session.latest_revision().await {
                        Ok(revision) => SvnRevisionSelection::Head(revision),
                        Err(error) => {
                            send_svn_version_switch_failed(
                                &event_sender,
                                session_id,
                                selection,
                                anyhow!("无法解析 SVN HEAD：{error}"),
                            );
                            continue;
                        }
                    },
                    Ok(Some(revision)) => SvnRevisionSelection::Fixed(revision),
                    Err(error) => {
                        send_svn_version_switch_failed(&event_sender, session_id, selection, error);
                        continue;
                    }
                };
                match read_svn_directory(&mut session, "/", next_selection.resolved()).await {
                    Ok(entries) => {
                        selection = next_selection;
                        current_dir = "/".to_string();
                        send_svn_version(session_id, selection, None, &event_sender);
                        send_event_blocking(
                            &event_sender,
                            RemoteFileEvent::DirectoryLoaded {
                                session_id,
                                current_dir: current_dir.clone(),
                                entries,
                            },
                        );
                    }
                    Err(error) => {
                        send_svn_version_switch_failed(&event_sender, session_id, selection, error)
                    }
                }
            }
            RemoteFileCommand::DownloadFile {
                remote_path,
                local_path,
            } => {
                let result = download_svn_file(
                    &mut session,
                    selection.resolved(),
                    &remote_path,
                    &local_path,
                )
                .await
                .map(|_| format!("已下载 {}", remote_file_name(&remote_path)));
                send_svn_operation_result(session_id, result, &event_sender);
            }
            RemoteFileCommand::DownloadFiles { entries, local_dir } => {
                let result =
                    download_svn_files(&mut session, selection.resolved(), &entries, &local_dir)
                        .await;
                send_svn_operation_result(session_id, result, &event_sender);
            }
            RemoteFileCommand::ReadFileContent { remote_path } => send_file_preview_loaded(
                session_id,
                read_svn_file_preview(&mut session, selection.resolved(), &remote_path).await,
                &event_sender,
            ),
            RemoteFileCommand::UploadFiles { .. }
            | RemoteFileCommand::Rename { .. }
            | RemoteFileCommand::Delete { .. } => send_operation_failed(
                &event_sender,
                session_id,
                anyhow!("SVN 仓库是只读的，不允许写入、重命名或删除"),
            ),
            RemoteFileCommand::Disconnect => {
                send_event_blocking(
                    &event_sender,
                    RemoteFileEvent::Disconnected {
                        session_id,
                        message: "SVN 仓库会话已关闭".to_string(),
                    },
                );
                return Ok(());
            }
        }
    }

    send_event_blocking(
        &event_sender,
        RemoteFileEvent::Disconnected {
            session_id,
            message: "SVN 仓库会话已关闭".to_string(),
        },
    );
    Ok(())
}

/// 返回仓库 URL 的规范协议名，仅供已通过领域校验的 SVN 配置选择传输实现。
fn repository_url_scheme(value: &str) -> Option<&'static str> {
    let scheme = value.trim().split_once(':')?.0;
    if scheme.eq_ignore_ascii_case("http") {
        Some("http")
    } else if scheme.eq_ignore_ascii_case("https") {
        Some("https")
    } else if scheme.eq_ignore_ascii_case("svn") {
        Some("svn")
    } else if scheme.eq_ignore_ascii_case("svn+ssh") {
        Some("svn+ssh")
    } else {
        None
    }
}

/// 把 ra_svn 节点类型映射到协议无关的远程文件类型。
fn svn_node_kind(kind: NodeKind) -> RemoteFileEntryKind {
    match kind {
        NodeKind::Dir => RemoteFileEntryKind::Directory,
        NodeKind::File => RemoteFileEntryKind::RegularFile,
        NodeKind::None | NodeKind::Unknown => RemoteFileEntryKind::Other,
    }
}

/// 把 HTTP DAV 节点属性映射到 SVN worker 内部统一属性。
fn svn_http_node_stat(stat: HttpSvnNodeStat) -> SvnNodeStat {
    SvnNodeStat {
        kind: stat.kind,
        size: stat.size,
    }
}

/// 按链接配置创建 ra_svn 客户端；SSH 显式关闭 agent、默认密钥和 OpenSSH 配置回退。
fn create_svn_client(config: &SvnLinkConfig) -> Result<RaSvnClient> {
    let url = SvnUrl::parse(&config.url).context("SVN URL 无效")?;
    let embedded_username = url
        .username()
        .map(|username| {
            percent_decode_str(username)
                .decode_utf8()
                .context("SVN URL 用户名不是有效 UTF-8")
                .map(|username| username.into_owned())
        })
        .transpose()?;
    let effective_username = config.username.clone().or(embedded_username);
    let mut client = RaSvnClient::new(url, effective_username.clone(), config.password.clone());
    if config.url.to_ascii_lowercase().starts_with("svn+ssh://") {
        let auth = if let Some(password) = config.password.clone() {
            SshAuth::Password(password)
        } else if let Some(private_key_path) = config.private_key_path.as_deref() {
            SshAuth::KeyFile {
                path: PathBuf::from(private_key_path),
                passphrase: config.private_key_passphrase.clone(),
            }
        } else {
            bail!("svn+ssh:// 必须配置密码或私钥");
        };
        let known_hosts_path = argus_svn_known_hosts_file();
        if let Some(parent) = known_hosts_path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("无法创建 SVN known_hosts 目录 {}", parent.display()))?;
        }
        // `SshConfig::new` 默认不启用 agent 和默认身份；额外关闭 OpenSSH 配置，确保连接只受链接配置控制。
        let mut ssh = SshConfig::new(auth)
            .with_known_hosts_file(known_hosts_path)
            .with_openssh_config(false);
        if let Some(username) = effective_username {
            ssh = ssh.with_username(username);
        }
        client = client.with_ssh_config(ssh);
    }
    Ok(client)
}

/// 使用纯 Rust russh 仅完成 SSH 密钥交换，捕获服务端公钥后立即断开。
async fn preflight_svn_ssh_host_key(host: &str, port: u16) -> Result<russh::keys::PublicKey> {
    let capture = SvnSshHostKeyCapture::default();
    let shared_key = capture.server_key.clone();
    let ssh_config = russh::client::Config {
        anonymous: true,
        inactivity_timeout: Some(Duration::from_secs(10)),
        ..Default::default()
    };
    let handle = tokio::time::timeout(
        Duration::from_secs(12),
        russh::client::connect(Arc::new(ssh_config), (host, port), capture),
    )
    .await
    .context("SVN SSH 主机公钥预检超时")?
    .context("无法连接 SVN SSH 服务以校验主机公钥")?;
    let _ = handle
        .disconnect(
            russh::Disconnect::ByApplication,
            "host key checked",
            "zh-CN",
        )
        .await;
    shared_key
        .lock()
        .expect("主机公钥捕获锁不应中毒")
        .clone()
        .ok_or_else(|| anyhow!("SVN SSH 服务端未提供主机公钥"))
}

/// 对比 SVN SSH 公钥指纹并复用统一确认事件；指纹变化必须在认证前直接阻断。
fn verify_svn_ssh_host_key(
    session_id: usize,
    host: &str,
    port: u16,
    server_key: &russh::keys::PublicKey,
    trusted_fingerprint: Option<&str>,
    command_receiver: &mpsc::Receiver<RemoteFileCommand>,
    event_sender: &Sender<RemoteFileEvent>,
) -> Result<()> {
    let fingerprint = server_key
        .fingerprint(russh::keys::HashAlg::Sha256)
        .to_string();
    let known_hosts_path = argus_svn_known_hosts_file();
    let key_is_recorded = match russh::keys::known_hosts::check_known_hosts_path(
        host,
        port,
        server_key,
        &known_hosts_path,
    ) {
        Ok(is_known) => is_known,
        Err(russh::keys::Error::KeyChanged { .. }) => {
            bail!("SVN SSH 主机指纹发生变化，已阻止连接")
        }
        Err(error) => return Err(anyhow!("无法检查 SVN SSH known_hosts：{error}")),
    };
    match trusted_fingerprint {
        Some(expected) if expected != fingerprint => {
            bail!("SVN SSH 主机指纹发生变化，已阻止连接")
        }
        Some(_) => {
            if !key_is_recorded {
                russh::keys::known_hosts::learn_known_hosts_path(
                    host,
                    port,
                    server_key,
                    known_hosts_path,
                )
                .context("无法写入 Argus SVN known_hosts")?;
            }
            Ok(())
        }
        None => {
            send_event_blocking(
                event_sender,
                RemoteFileEvent::HostKeyVerification {
                    session_id,
                    host: host.to_string(),
                    port,
                    fingerprint: fingerprint.clone(),
                },
            );
            loop {
                match command_receiver.recv() {
                    Ok(RemoteFileCommand::TrustHostKey) => {
                        if !key_is_recorded {
                            russh::keys::known_hosts::learn_known_hosts_path(
                                host,
                                port,
                                server_key,
                                known_hosts_path,
                            )
                            .context("无法写入 Argus SVN known_hosts")?;
                        }
                        return Ok(());
                    }
                    Ok(RemoteFileCommand::RejectHostKey) => {
                        bail!("用户拒绝信任 SVN SSH 主机指纹")
                    }
                    Ok(RemoteFileCommand::Disconnect) | Err(_) => bail!("SVN SSH 连接已取消"),
                    Ok(_) => {}
                }
            }
        }
    }
}

/// 读取 SVN 目录的直接子项，并将 crate 元数据映射到通用文件条目。
async fn read_svn_directory(
    session: &mut SvnReadSession,
    path: &str,
    revision: u64,
) -> Result<Vec<RemoteFileEntry>> {
    let normalized = normalize_repository_path(path)?;
    let mut entries = session
        .list_directory(&normalized, revision)
        .await
        .with_context(|| format!("无法读取 SVN 目录 {normalized} @ r{revision}"))?;
    entries.sort_by_cached_key(|entry| {
        (
            entry.kind != RemoteFileEntryKind::Directory,
            entry.name.to_ascii_lowercase(),
            entry.name.clone(),
        )
    });
    Ok(entries)
}

/// 解析 SVN 返回的 RFC3339 时间；负时间戳不映射为无符号 Unix 秒。
fn parse_svn_timestamp(value: &str) -> Option<u64> {
    DateTime::parse_from_rfc3339(value)
        .ok()
        .and_then(|time| u64::try_from(time.timestamp()).ok())
}

/// 发送 SVN 当前 HEAD 或固定修订信息。
fn send_svn_version(
    session_id: usize,
    selection: SvnRevisionSelection,
    message: Option<String>,
    event_sender: &Sender<RemoteFileEvent>,
) {
    let kind = match selection {
        SvnRevisionSelection::Head(_) => RepositoryVersionKind::SvnHead,
        SvnRevisionSelection::Fixed(_) => RepositoryVersionKind::SvnRevision,
    };
    send_event_blocking(
        event_sender,
        RemoteFileEvent::RepositoryVersionsLoaded {
            session_id,
            versions: vec![RepositoryVersion {
                id: selection.id(),
                label: selection.display(),
                kind,
            }],
            selected_version: selection.id(),
            input_value: selection.display(),
            message,
        },
    );
}

/// SVN 版本切换失败时先恢复当前有效版本展示，再发送具体错误提示。
///
/// 参数：`selection` 是仍在生效的版本，其余参数用于构造统一版本和失败事件。
fn send_svn_version_switch_failed(
    event_sender: &Sender<RemoteFileEvent>,
    session_id: usize,
    selection: SvnRevisionSelection,
    error: anyhow::Error,
) {
    send_svn_version(session_id, selection, None, event_sender);
    send_operation_failed(event_sender, session_id, error);
}

/// 解析 SVN 版本输入；支持 HEAD、当前展示的 `HEAD (rN)`、`rN` 和裸数字。
fn parse_svn_revision(value: &str) -> Result<Option<u64>> {
    let normalized = value.trim();
    if normalized.eq_ignore_ascii_case("HEAD") {
        return Ok(None);
    }
    // 输入框可能再次提交当前展示值；仅接受完整的 HEAD (rN)，避免把任意 HEAD (...) 当成合法版本。
    if normalized
        .get(..7)
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case("HEAD (r"))
        && normalized.ends_with(')')
    {
        let digits = &normalized[7..normalized.len() - 1];
        if !digits.is_empty() && digits.bytes().all(|byte| byte.is_ascii_digit()) {
            return Ok(None);
        }
    }
    let digits = normalized
        .strip_prefix('r')
        .or_else(|| normalized.strip_prefix('R'))
        .unwrap_or(normalized);
    if digits.is_empty() || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
        bail!("SVN 版本必须是 HEAD 或非负数字修订号");
    }
    digits
        .parse::<u64>()
        .map(Some)
        .map_err(|_| anyhow!("SVN 修订号超出支持范围"))
}

/// 下载单个 SVN 普通文件到用户选择的本地路径。
async fn download_svn_file(
    session: &mut SvnReadSession,
    revision: u64,
    remote_path: &str,
    local_path: &Path,
) -> Result<()> {
    let normalized = normalize_repository_path(remote_path)?;
    let stat = session
        .stat(&normalized, revision)
        .await
        .with_context(|| format!("无法读取 SVN 文件信息：{remote_path}"))?
        .ok_or_else(|| anyhow!("SVN 文件不存在：{remote_path}"))?;
    if stat.kind != RemoteFileEntryKind::RegularFile {
        bail!("仅支持下载 SVN 普通文件：{remote_path}");
    }
    let max_bytes = stat.size.unwrap_or(u64::MAX);
    session
        .download_file(&normalized, revision, local_path, max_bytes)
        .await
        .with_context(|| format!("下载 SVN 文件失败：{remote_path}"))?;
    Ok(())
}

/// 下载多个 SVN 普通文件；目录和未知节点在创建任何本地文件前拒绝。
async fn download_svn_files(
    session: &mut SvnReadSession,
    revision: u64,
    entries: &[RemoteFileEntry],
    local_dir: &Path,
) -> Result<String> {
    if entries.is_empty() {
        bail!("未选择要下载的文件");
    }
    if !local_dir.is_dir() {
        bail!("请选择有效的本地目录");
    }
    let targets = entries
        .iter()
        .map(|entry| {
            if !entry.kind.is_regular_file() {
                bail!("仅支持下载普通文件：{}", entry.name);
            }
            Ok((entry, local_download_path(local_dir, &entry.name)?))
        })
        .collect::<Result<Vec<_>>>()?;
    for (entry, local_path) in targets {
        download_svn_file(session, revision, &entry.path, &local_path).await?;
    }
    Ok(format!("已下载 {} 个文件", entries.len()))
}

/// 读取 SVN 文件用于预览；通过 stat 强制执行 2 MiB 总大小上限。
async fn read_svn_file_preview(
    session: &mut SvnReadSession,
    revision: u64,
    remote_path: &str,
) -> (String, FilePreviewContent) {
    let file_name = remote_file_name(remote_path);
    let normalized = match normalize_repository_path(remote_path) {
        Ok(path) => path,
        Err(error) => return (file_name, FilePreviewContent::Error(error.to_string())),
    };
    let stat = match session.stat(&normalized, revision).await {
        Ok(Some(stat)) => stat,
        Ok(None) => {
            return (
                file_name,
                FilePreviewContent::Error("SVN 文件不存在".to_string()),
            );
        }
        Err(error) => {
            return (
                file_name,
                FilePreviewContent::Error(format!("无法读取 SVN 文件信息：{error}")),
            );
        }
    };
    if stat.kind != RemoteFileEntryKind::RegularFile {
        return (
            file_name,
            FilePreviewContent::Error("仅支持预览普通文件".to_string()),
        );
    }
    let Some(file_size) = stat.size else {
        return (
            file_name,
            FilePreviewContent::Error("无法确认 SVN 文件大小，已拒绝预览".to_string()),
        );
    };
    if file_size > REMOTE_FILE_PREVIEW_MAX_FILE_SIZE {
        return (
            file_name,
            FilePreviewContent::Error("文件超过 2 MiB，无法预览".to_string()),
        );
    }
    let max_bytes = (REMOTE_FILE_PREVIEW_MAX_READ + 1) as u64;
    let mut bytes = match session
        .read_file_bytes(&normalized, revision, max_bytes)
        .await
    {
        Ok(bytes) => bytes,
        Err(error) => {
            return (
                file_name,
                FilePreviewContent::Error(format!("读取 SVN 文件失败：{error}")),
            );
        }
    };
    let truncated = bytes.len() > REMOTE_FILE_PREVIEW_MAX_READ;
    if truncated {
        bytes.truncate(REMOTE_FILE_PREVIEW_MAX_READ);
    }
    (file_name, preview_content_from_bytes(bytes, truncated))
}

/// 把地址栏路径归一化并阻止越过链接 URL 指向的 SVN 浏览根目录。
fn normalize_repository_path(path: &str) -> Result<String> {
    let trimmed = path.trim();
    let absolute = if trimmed.is_empty() || trimmed == "." {
        "/".to_string()
    } else if trimmed.starts_with('/') {
        trimmed.to_string()
    } else {
        format!("/{trimmed}")
    };
    let mut components = Vec::new();
    for component in absolute.split(['/', '\\']) {
        match component {
            "" | "." => {}
            ".." => bail!("仓库路径不能越过浏览根目录"),
            value => components.push(value),
        }
    }
    Ok(if components.is_empty() {
        "/".to_string()
    } else {
        format!("/{}", components.join("/"))
    })
}

/// 将绝对目录路径转换成 ra_svn 相对目录路径；根目录表示为空字符串。
fn repository_relative_path(path: &str) -> Result<String> {
    Ok(normalize_repository_path(path)?
        .trim_start_matches('/')
        .to_string())
}

/// 将绝对文件路径转换成非空 ra_svn 相对文件路径。
fn repository_file_path(path: &str) -> Result<String> {
    let relative = repository_relative_path(path)?;
    if relative.is_empty() {
        bail!("仓库根目录不是普通文件");
    }
    Ok(relative)
}

/// 将 SVN 下载结果转换为统一成功/失败事件。
fn send_svn_operation_result(
    session_id: usize,
    result: Result<String>,
    event_sender: &Sender<RemoteFileEvent>,
) {
    match result {
        Ok(message) => send_event_blocking(
            event_sender,
            RemoteFileEvent::OperationSucceeded {
                session_id,
                message,
            },
        ),
        Err(error) => send_operation_failed(event_sender, session_id, error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 验证 HEAD、r 前缀与裸数字都能解析，非法输入不会被接受。
    #[test]
    fn svn_revision_parser_accepts_supported_forms() {
        assert_eq!(parse_svn_revision("HEAD").unwrap(), None);
        assert_eq!(parse_svn_revision("HEAD (r42)").unwrap(), None);
        assert_eq!(parse_svn_revision("r42").unwrap(), Some(42));
        assert_eq!(parse_svn_revision("0").unwrap(), Some(0));
        assert!(parse_svn_revision("-1").is_err());
        assert!(parse_svn_revision("main").is_err());
        assert!(parse_svn_revision("HEAD (latest)").is_err());
        assert!(parse_svn_revision("HEAD (r)").is_err());
    }

    /// 验证 SVN 浏览路径无法通过父目录逃逸出链接 URL 指向的根目录。
    #[test]
    fn svn_repository_path_rejects_parent_escape() {
        assert_eq!(
            normalize_repository_path("trunk/src").unwrap(),
            "/trunk/src"
        );
        assert!(normalize_repository_path("../outside").is_err());
    }

    /// `svn://user@host` 的 URL 用户名必须传给 ra_svn，而不是只用于表单校验。
    #[test]
    fn svn_client_uses_embedded_url_username() {
        let config = SvnLinkConfig {
            url: "svn://read%65r@example.com/repository".to_string(),
            username: None,
            password: Some("password".to_string()),
            private_key_path: None,
            private_key_passphrase: None,
        };

        let client = create_svn_client(&config).expect("应创建带 URL 用户名的客户端");

        assert_eq!(client.username(), Some("reader"));
    }

    /// 非法或不存在的 SVN 版本切换失败后必须先恢复当前有效版本展示。
    #[test]
    fn svn_version_switch_failure_restores_current_version_before_error() {
        let (sender, receiver) = async_channel::unbounded();
        send_svn_version_switch_failed(
            &sender,
            7,
            SvnRevisionSelection::Head(16),
            anyhow!("非法版本"),
        );

        let version_event = receiver.recv_blocking().expect("应收到版本恢复事件");
        assert!(matches!(
            version_event,
            RemoteFileEvent::RepositoryVersionsLoaded {
                session_id: 7,
                input_value,
                ..
            } if input_value == "HEAD (r16)"
        ));
        let failure_event = receiver.recv_blocking().expect("应收到切换失败事件");
        assert!(matches!(
            failure_event,
            RemoteFileEvent::OperationFailed { session_id: 7, .. }
        ));
    }
}
