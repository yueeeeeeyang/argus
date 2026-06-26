//! 文件职责：封装 SSH SFTP 文件管理会话、后台 worker 与远程文件元数据。
//! 创建日期：2026-06-26
//! 修改日期：2026-06-26
//! 作者：Argus 开发团队
//! 主要功能：使用内嵌 ssh2 SFTP 通道读取远程目录，并支持普通文件上传、下载、重命名和删除。

use std::collections::BTreeSet;
use std::fs::File;
use std::io;
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::thread;

use anyhow::{Context as AnyhowContext, Result, anyhow, bail};
use async_channel::{Receiver, Sender};
use base64::{Engine as _, engine::general_purpose::STANDARD_NO_PAD};
use gpui::UniformListScrollHandle;
use ssh2::{HashType, Session};

use crate::app::SettingsTextInputState;
use crate::connections::{ConnectionLinkConfig, ConnectionNodeId, SshLinkConfig};
use crate::terminal::PendingHostKey;

/// 远程 Unix 文件类型掩码。
const SFTP_MODE_TYPE_MASK: u32 = 0o170000;
/// 远程普通文件类型位。
const SFTP_MODE_REGULAR_FILE: u32 = 0o100000;
/// 远程目录类型位。
const SFTP_MODE_DIRECTORY: u32 = 0o040000;
/// 远程符号链接类型位。
const SFTP_MODE_SYMLINK: u32 = 0o120000;
/// SFTP 文件管理会话运行期状态，存放在 `ArgusApp` 中并由 UI 渲染。
pub struct SftpSessionState {
    /// SFTP 会话 ID，与标签页中的 `session_id` 对应。
    pub id: usize,
    /// 关联的链接节点 ID。
    pub link_id: ConnectionNodeId,
    /// 文件管理标签标题。
    pub title: String,
    /// 远程地址展示文案。
    pub address: String,
    /// 当前连接和操作状态。
    pub status: SftpStatus,
    /// 发送给 SFTP 后台线程的命令通道。
    pub command_sender: Option<mpsc::Sender<SftpCommand>>,
    /// 等待用户确认的主机指纹；没有待确认时为空。
    pub pending_host_key: Option<PendingHostKey>,
    /// 当前远程目录。
    pub current_dir: String,
    /// 地址栏输入框状态。
    pub address_input: SettingsTextInputState,
    /// 当前目录文件列表。
    pub entries: Vec<SftpEntry>,
    /// 当前选中的远程路径集合。
    pub selected_paths: BTreeSet<String>,
    /// 文件列表滚动句柄。
    pub list_scroll: UniformListScrollHandle,
    /// 最近一次提示或错误。
    pub message: Option<String>,
}

impl SftpSessionState {
    /// 创建一个处于“连接中”的 SFTP 会话状态。
    pub fn connecting(
        id: usize,
        link: &ConnectionLinkConfig,
        command_sender: mpsc::Sender<SftpCommand>,
    ) -> Self {
        Self {
            id,
            link_id: link.id,
            title: format!("文件管理 - {}", link.name),
            address: link.address_label(),
            status: SftpStatus::Connecting,
            command_sender: Some(command_sender),
            pending_host_key: None,
            current_dir: String::new(),
            address_input: SettingsTextInputState::default(),
            entries: Vec::new(),
            selected_paths: BTreeSet::new(),
            list_scroll: UniformListScrollHandle::new(),
            message: Some("正在建立 SFTP 连接...".to_string()),
        }
    }

    /// 同步当前目录和文件列表，成功加载后清理旧选择。
    pub fn apply_directory_listing(&mut self, current_dir: String, entries: Vec<SftpEntry>) {
        self.current_dir = current_dir.clone();
        self.address_input = SettingsTextInputState::from_value(current_dir);
        self.entries = entries;
        self.selected_paths.clear();
        self.status = SftpStatus::Connected;
    }

    /// 返回当前选中的文件条目。
    pub fn selected_entries(&self) -> Vec<SftpEntry> {
        self.entries
            .iter()
            .filter(|entry| self.selected_paths.contains(&entry.path))
            .cloned()
            .collect()
    }
}

/// SFTP 文件管理会话状态。
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SftpStatus {
    /// 正在连接服务器。
    Connecting,
    /// 已拿到未知主机指纹，等待用户确认。
    AwaitingHostKey,
    /// 正在读取目录。
    Loading,
    /// 文件管理通道可用。
    Connected,
    /// 正在上传、下载、重命名或删除。
    Transferring,
    /// 用户主动断开或远端关闭。
    Disconnected,
    /// 连接或鉴权失败。
    Failed,
}

/// 远程文件条目类型。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SftpEntryKind {
    /// 目录。
    Directory,
    /// 普通文件。
    RegularFile,
    /// 符号链接。
    Symlink,
    /// 其他类型。
    Other,
}

impl SftpEntryKind {
    /// 返回 UI 展示用中文类型。
    pub fn label(self) -> &'static str {
        match self {
            Self::Directory => "目录",
            Self::RegularFile => "文件",
            Self::Symlink => "链接",
            Self::Other => "其他",
        }
    }

    /// 判断当前条目是否支持普通文件传输。
    pub fn is_regular_file(self) -> bool {
        self == Self::RegularFile
    }
}

/// 远程目录中的单个文件条目。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SftpEntry {
    /// 文件名。
    pub name: String,
    /// 完整远程路径。
    pub path: String,
    /// 文件类型。
    pub kind: SftpEntryKind,
    /// 文件大小；目录和部分服务器可能不返回。
    pub size: Option<u64>,
    /// Unix 修改时间秒级时间戳。
    pub mtime: Option<u64>,
    /// Unix 权限 mode。
    pub permissions: Option<u32>,
}

/// 启动 SFTP worker 时需要的不可变请求数据。
#[derive(Clone, Debug)]
pub struct SftpWorkerRequest {
    /// SFTP 会话 ID。
    pub session_id: usize,
    /// 关联链接 ID。
    pub link_id: ConnectionNodeId,
    /// SSH 配置快照。
    pub ssh: SshLinkConfig,
    /// 已信任主机指纹；为空时需要 UI 二次确认。
    pub trusted_fingerprint: Option<String>,
}

/// UI 发送给 SFTP worker 的命令。
#[derive(Clone, Debug)]
pub enum SftpCommand {
    /// 用户确认当前未知主机指纹可信。
    TrustHostKey,
    /// 用户拒绝当前未知主机指纹。
    RejectHostKey,
    /// 加载指定目录。
    LoadDirectory(String),
    /// 刷新当前目录。
    Refresh,
    /// 上传本地普通文件到当前远程目录。
    UploadFiles {
        /// 本地普通文件路径。
        local_paths: Vec<PathBuf>,
    },
    /// 下载远程普通文件到指定本地路径。
    DownloadFile {
        /// 远程文件路径。
        remote_path: String,
        /// 本地保存路径。
        local_path: PathBuf,
    },
    /// 下载多个远程普通文件到本地目录。
    DownloadFiles {
        /// 远程文件条目。
        entries: Vec<SftpEntry>,
        /// 本地保存目录。
        local_dir: PathBuf,
    },
    /// 在当前目录内重命名文件或目录。
    Rename {
        /// 原始远程路径。
        remote_path: String,
        /// 新名称，不包含路径分隔符。
        new_name: String,
    },
    /// 删除远程普通文件或空目录。
    Delete {
        /// 待删除条目。
        entry: SftpEntry,
    },
    /// 主动断开 SFTP 通道。
    Disconnect,
}

/// SFTP worker 回传给 UI 的事件。
#[derive(Clone, Debug)]
pub enum SftpEvent {
    /// 发现未知主机指纹，需要 UI 弹窗确认。
    HostKeyVerification {
        /// 会话 ID。
        session_id: usize,
        /// 主机。
        host: String,
        /// 端口。
        port: u16,
        /// SHA256 指纹。
        fingerprint: String,
    },
    /// SFTP 通道已连接并完成首次目录读取。
    Connected {
        /// 会话 ID。
        session_id: usize,
        /// 当前目录。
        current_dir: String,
        /// 当前目录条目。
        entries: Vec<SftpEntry>,
    },
    /// 目录读取完成。
    DirectoryLoaded {
        /// 会话 ID。
        session_id: usize,
        /// 当前目录。
        current_dir: String,
        /// 当前目录条目。
        entries: Vec<SftpEntry>,
    },
    /// 操作成功。
    OperationSucceeded {
        /// 会话 ID。
        session_id: usize,
        /// 用户可读提示。
        message: String,
    },
    /// 操作失败，但 SFTP 会话仍可继续使用。
    OperationFailed {
        /// 会话 ID。
        session_id: usize,
        /// 用户可读错误。
        message: String,
    },
    /// 远端或本地已经断开。
    Disconnected {
        /// 会话 ID。
        session_id: usize,
        /// 用户可读原因。
        message: String,
    },
    /// SFTP worker 连接级失败。
    Failed {
        /// 会话 ID。
        session_id: usize,
        /// 用户可读原因。
        message: String,
    },
}

/// 启动 SFTP 后台线程，并返回命令发送端与事件接收端。
pub fn spawn_sftp_worker(
    request: SftpWorkerRequest,
) -> (mpsc::Sender<SftpCommand>, Receiver<SftpEvent>) {
    let (command_sender, command_receiver) = mpsc::channel();
    let (event_sender, event_receiver) = async_channel::unbounded();
    thread::spawn(move || {
        if let Err(error) = run_sftp_worker(request.clone(), command_receiver, event_sender.clone())
        {
            send_event_blocking(
                &event_sender,
                SftpEvent::Failed {
                    session_id: request.session_id,
                    message: error.to_string(),
                },
            );
        }
    });
    (command_sender, event_receiver)
}

/// 校验远程文件重命名的新名称，第一版不允许跨目录移动。
pub fn validate_sftp_rename_name(name: &str) -> Result<String, String> {
    let normalized = name.trim();
    if normalized.is_empty() {
        return Err("名称不能为空".to_string());
    }
    if normalized == "."
        || normalized == ".."
        || normalized.contains('/')
        || normalized.contains('\\')
    {
        return Err("名称不能包含路径分隔符".to_string());
    }
    Ok(normalized.to_string())
}

/// 返回指定远程目录的父目录。
pub fn remote_parent_dir(path: &str) -> Option<String> {
    let normalized = path.trim_end_matches('/');
    if normalized.is_empty() || normalized == "/" {
        return None;
    }
    let parent = normalized.rsplit_once('/').map(|(parent, _)| parent)?;
    if parent.is_empty() {
        Some("/".to_string())
    } else {
        Some(parent.to_string())
    }
}

/// 拼接远程目录和文件名，使用服务器通用的 POSIX 路径分隔符。
pub fn remote_child_path(directory: &str, name: &str) -> String {
    if directory == "/" {
        format!("/{name}")
    } else {
        format!("{}/{}", directory.trim_end_matches('/'), name)
    }
}

/// SFTP worker 主流程：连接、校验主机指纹、鉴权、建立 SFTP 通道并串行执行文件操作。
fn run_sftp_worker(
    request: SftpWorkerRequest,
    command_receiver: mpsc::Receiver<SftpCommand>,
    event_sender: Sender<SftpEvent>,
) -> Result<()> {
    let tcp = TcpStream::connect((request.ssh.host.as_str(), request.ssh.port))
        .with_context(|| format!("无法连接到 {}:{}", request.ssh.host, request.ssh.port))?;
    tcp.set_nodelay(true).ok();

    let mut session = Session::new().context("无法创建 SSH 会话")?;
    session.set_tcp_stream(tcp);
    session.handshake().context("SSH 握手失败")?;

    let fingerprint = sha256_fingerprint(&session).context("无法获取 SSH 主机指纹")?;
    verify_host_key(&request, &command_receiver, &event_sender, &fingerprint)?;
    authenticate(&session, &request.ssh)?;

    let sftp = session.sftp().context("无法创建 SFTP 通道")?;
    let mut current_dir =
        normalize_remote_path(sftp.realpath(Path::new(".")).context("无法解析登录目录")?);
    let entries = read_remote_directory(&sftp, &current_dir)?;
    send_event_blocking(
        &event_sender,
        SftpEvent::Connected {
            session_id: request.session_id,
            current_dir: current_dir.clone(),
            entries,
        },
    );

    while let Ok(command) = command_receiver.recv() {
        match command {
            SftpCommand::TrustHostKey | SftpCommand::RejectHostKey => {}
            SftpCommand::LoadDirectory(path) => match load_directory(&sftp, &current_dir, &path) {
                Ok((next_dir, entries)) => {
                    current_dir = next_dir;
                    send_event_blocking(
                        &event_sender,
                        SftpEvent::DirectoryLoaded {
                            session_id: request.session_id,
                            current_dir: current_dir.clone(),
                            entries,
                        },
                    );
                }
                Err(error) => send_operation_failed(&event_sender, request.session_id, error),
            },
            SftpCommand::Refresh => {
                send_directory_listing(&sftp, &current_dir, request.session_id, &event_sender);
            }
            SftpCommand::UploadFiles { local_paths } => {
                let result = upload_files(&sftp, &current_dir, &local_paths);
                send_operation_result(
                    &sftp,
                    &current_dir,
                    request.session_id,
                    &event_sender,
                    result,
                );
            }
            SftpCommand::DownloadFile {
                remote_path,
                local_path,
            } => {
                let result = download_file(&sftp, &remote_path, &local_path)
                    .map(|_| format!("已下载 {}", remote_file_name(&remote_path)));
                send_operation_result_without_refresh(request.session_id, &event_sender, result);
            }
            SftpCommand::DownloadFiles { entries, local_dir } => {
                let result = download_files(&sftp, &entries, &local_dir);
                send_operation_result_without_refresh(request.session_id, &event_sender, result);
            }
            SftpCommand::Rename {
                remote_path,
                new_name,
            } => {
                let result = rename_entry(&sftp, &remote_path, &new_name);
                send_operation_result(
                    &sftp,
                    &current_dir,
                    request.session_id,
                    &event_sender,
                    result,
                );
            }
            SftpCommand::Delete { entry } => {
                let result = delete_entry(&sftp, &entry);
                send_operation_result(
                    &sftp,
                    &current_dir,
                    request.session_id,
                    &event_sender,
                    result,
                );
            }
            SftpCommand::Disconnect => {
                send_event_blocking(
                    &event_sender,
                    SftpEvent::Disconnected {
                        session_id: request.session_id,
                        message: "SFTP 连接已断开".to_string(),
                    },
                );
                return Ok(());
            }
        }
    }

    send_event_blocking(
        &event_sender,
        SftpEvent::Disconnected {
            session_id: request.session_id,
            message: "SFTP 连接已断开".to_string(),
        },
    );
    Ok(())
}

/// 加载用户输入的目录，并返回服务器规范化路径和条目。
fn load_directory(
    sftp: &ssh2::Sftp,
    current_dir: &str,
    input: &str,
) -> Result<(String, Vec<SftpEntry>)> {
    let target = resolve_remote_path(current_dir, input)?;
    let current_dir = normalize_remote_path(
        sftp.realpath(Path::new(&target))
            .with_context(|| format!("无法进入目录 {target}"))?,
    );
    let entries = read_remote_directory(sftp, &current_dir)?;
    Ok((current_dir, entries))
}

/// 读取远程目录条目，并按目录优先、名称升序排序。
fn read_remote_directory(sftp: &ssh2::Sftp, directory: &str) -> Result<Vec<SftpEntry>> {
    let mut entries = sftp
        .readdir(Path::new(directory))
        .with_context(|| format!("无法读取目录 {directory}"))?
        .into_iter()
        .filter_map(|(path, stat)| sftp_entry_from_stat(directory, path, stat))
        .collect::<Vec<_>>();
    entries.sort_by(|left, right| {
        let left_group = if left.kind == SftpEntryKind::Directory {
            0
        } else {
            1
        };
        let right_group = if right.kind == SftpEntryKind::Directory {
            0
        } else {
            1
        };
        left_group
            .cmp(&right_group)
            .then_with(|| {
                left.name
                    .to_ascii_lowercase()
                    .cmp(&right.name.to_ascii_lowercase())
            })
            .then_with(|| left.name.cmp(&right.name))
    });
    Ok(entries)
}

/// 将 ssh2 的目录项转换为 UI 需要的远程文件条目。
fn sftp_entry_from_stat(directory: &str, path: PathBuf, stat: ssh2::FileStat) -> Option<SftpEntry> {
    let name = path.file_name()?.to_string_lossy().to_string();
    if name == "." || name == ".." {
        return None;
    }
    let kind = sftp_entry_kind(stat.perm);
    Some(SftpEntry {
        path: remote_child_path(directory, &name),
        name,
        kind,
        size: stat.size,
        mtime: stat.mtime,
        permissions: stat.perm,
    })
}

/// 根据 Unix mode 判断远程文件类型。
fn sftp_entry_kind(permissions: Option<u32>) -> SftpEntryKind {
    match permissions.map(|perm| perm & SFTP_MODE_TYPE_MASK) {
        Some(SFTP_MODE_DIRECTORY) => SftpEntryKind::Directory,
        Some(SFTP_MODE_REGULAR_FILE) => SftpEntryKind::RegularFile,
        Some(SFTP_MODE_SYMLINK) => SftpEntryKind::Symlink,
        _ => SftpEntryKind::Other,
    }
}

/// 上传本地普通文件集合到远程当前目录。
fn upload_files(sftp: &ssh2::Sftp, current_dir: &str, local_paths: &[PathBuf]) -> Result<String> {
    if local_paths.is_empty() {
        bail!("未选择要上传的文件");
    }

    for local_path in local_paths {
        let metadata = std::fs::metadata(local_path)
            .with_context(|| format!("无法读取本地文件 {}", local_path.display()))?;
        if !metadata.is_file() {
            bail!("仅支持上传普通文件：{}", local_path.display());
        }
        let file_name = local_path
            .file_name()
            .map(|name| name.to_string_lossy().to_string())
            .filter(|name| !name.is_empty())
            .ok_or_else(|| anyhow!("本地文件名无效：{}", local_path.display()))?;
        let remote_path = remote_child_path(current_dir, &file_name);
        let mut local_file = File::open(local_path)
            .with_context(|| format!("无法打开本地文件 {}", local_path.display()))?;
        let mut remote_file = sftp
            .create(Path::new(&remote_path))
            .with_context(|| format!("无法写入远程文件 {remote_path}"))?;
        io::copy(&mut local_file, &mut remote_file)
            .with_context(|| format!("上传文件失败：{}", local_path.display()))?;
    }

    Ok(format!("已上传 {} 个文件", local_paths.len()))
}

/// 下载单个远程文件到本地路径。
fn download_file(sftp: &ssh2::Sftp, remote_path: &str, local_path: &Path) -> Result<()> {
    let mut remote_file = sftp
        .open(Path::new(remote_path))
        .with_context(|| format!("无法打开远程文件 {remote_path}"))?;
    let mut local_file = File::create(local_path)
        .with_context(|| format!("无法创建本地文件 {}", local_path.display()))?;
    io::copy(&mut remote_file, &mut local_file)
        .with_context(|| format!("下载文件失败：{remote_path}"))?;
    Ok(())
}

/// 下载多个远程普通文件到本地目录。
fn download_files(sftp: &ssh2::Sftp, entries: &[SftpEntry], local_dir: &Path) -> Result<String> {
    if entries.is_empty() {
        bail!("未选择要下载的文件");
    }
    if !local_dir.is_dir() {
        bail!("请选择有效的本地目录");
    }

    for entry in entries {
        if !entry.kind.is_regular_file() {
            bail!("仅支持下载普通文件：{}", entry.name);
        }
        download_file(sftp, &entry.path, &local_dir.join(&entry.name))?;
    }

    Ok(format!("已下载 {} 个文件", entries.len()))
}

/// 在当前目录内重命名远程条目。
fn rename_entry(sftp: &ssh2::Sftp, remote_path: &str, new_name: &str) -> Result<String> {
    let new_name = validate_sftp_rename_name(new_name).map_err(anyhow::Error::msg)?;
    let parent = remote_parent_dir(remote_path).ok_or_else(|| anyhow!("无法解析远程父目录"))?;
    let next_path = remote_child_path(&parent, &new_name);
    sftp.rename(Path::new(remote_path), Path::new(&next_path), None)
        .with_context(|| format!("重命名失败：{remote_path}"))?;
    Ok(format!("已重命名为 {new_name}"))
}

/// 删除远程普通文件或空目录。
fn delete_entry(sftp: &ssh2::Sftp, entry: &SftpEntry) -> Result<String> {
    match entry.kind {
        SftpEntryKind::RegularFile => {
            sftp.unlink(Path::new(&entry.path))
                .with_context(|| format!("删除文件失败：{}", entry.name))?;
            Ok(format!("已删除 {}", entry.name))
        }
        SftpEntryKind::Directory => {
            sftp.rmdir(Path::new(&entry.path))
                .map_err(|error| anyhow!("目录非空或无权限，无法删除：{error}"))?;
            Ok(format!("已删除目录 {}", entry.name))
        }
        SftpEntryKind::Symlink | SftpEntryKind::Other => {
            bail!("仅支持删除普通文件或空目录：{}", entry.name)
        }
    }
}

/// 刷新当前目录并发送完整目录事件。
fn send_directory_listing(
    sftp: &ssh2::Sftp,
    current_dir: &str,
    session_id: usize,
    event_sender: &Sender<SftpEvent>,
) {
    match read_remote_directory(sftp, current_dir) {
        Ok(entries) => send_event_blocking(
            event_sender,
            SftpEvent::DirectoryLoaded {
                session_id,
                current_dir: current_dir.to_string(),
                entries,
            },
        ),
        Err(error) => send_operation_failed(event_sender, session_id, error),
    }
}

/// 发送带刷新目录的操作结果。
fn send_operation_result(
    sftp: &ssh2::Sftp,
    current_dir: &str,
    session_id: usize,
    event_sender: &Sender<SftpEvent>,
    result: Result<String>,
) {
    match result {
        Ok(message) => {
            send_event_blocking(
                event_sender,
                SftpEvent::OperationSucceeded {
                    session_id,
                    message,
                },
            );
            send_directory_listing(sftp, current_dir, session_id, event_sender);
        }
        Err(error) => send_operation_failed(event_sender, session_id, error),
    }
}

/// 发送无需刷新目录的操作结果。
fn send_operation_result_without_refresh(
    session_id: usize,
    event_sender: &Sender<SftpEvent>,
    result: Result<String>,
) {
    match result {
        Ok(message) => send_event_blocking(
            event_sender,
            SftpEvent::OperationSucceeded {
                session_id,
                message,
            },
        ),
        Err(error) => send_operation_failed(event_sender, session_id, error),
    }
}

/// 发送可恢复的操作失败事件。
fn send_operation_failed(
    event_sender: &Sender<SftpEvent>,
    session_id: usize,
    error: anyhow::Error,
) {
    send_event_blocking(
        event_sender,
        SftpEvent::OperationFailed {
            session_id,
            message: error.to_string(),
        },
    );
}

/// 把用户输入的远程目录解析为绝对路径文本。
fn resolve_remote_path(current_dir: &str, input: &str) -> Result<String> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        bail!("远程目录不能为空");
    }
    if trimmed.starts_with('/') {
        Ok(trimmed.to_string())
    } else {
        Ok(remote_child_path(current_dir, trimmed))
    }
}

/// 将服务器返回的路径统一转换为 UTF-8 字符串。
fn normalize_remote_path(path: PathBuf) -> String {
    let text = path.to_string_lossy().to_string();
    if text.is_empty() {
        "/".to_string()
    } else {
        text
    }
}

/// 返回远程路径中的文件名。
fn remote_file_name(path: &str) -> String {
    path.rsplit('/')
        .next()
        .filter(|name| !name.is_empty())
        .unwrap_or(path)
        .to_string()
}

/// 生成 libssh2 握手后返回的 SHA256 主机指纹文本。
fn sha256_fingerprint(session: &Session) -> Result<String> {
    let hash = session
        .host_key_hash(HashType::Sha256)
        .ok_or_else(|| anyhow!("服务器未返回 SHA256 主机指纹"))?;
    Ok(format!("SHA256:{}", STANDARD_NO_PAD.encode(hash)))
}

/// 校验主机指纹；未知主机需要等待 UI 发送确认或拒绝命令。
fn verify_host_key(
    request: &SftpWorkerRequest,
    command_receiver: &mpsc::Receiver<SftpCommand>,
    event_sender: &Sender<SftpEvent>,
    fingerprint: &str,
) -> Result<()> {
    match request.trusted_fingerprint.as_deref() {
        Some(expected) if expected == fingerprint => Ok(()),
        Some(_) => bail!("SSH 主机指纹发生变化，已阻止连接"),
        None => {
            send_event_blocking(
                event_sender,
                SftpEvent::HostKeyVerification {
                    session_id: request.session_id,
                    host: request.ssh.host.clone(),
                    port: request.ssh.port,
                    fingerprint: fingerprint.to_string(),
                },
            );
            loop {
                match command_receiver.recv() {
                    Ok(SftpCommand::TrustHostKey) => return Ok(()),
                    Ok(SftpCommand::RejectHostKey) => bail!("用户拒绝信任 SSH 主机指纹"),
                    Ok(SftpCommand::Disconnect) | Err(_) => bail!("SFTP 连接已取消"),
                    Ok(
                        SftpCommand::LoadDirectory(_)
                        | SftpCommand::Refresh
                        | SftpCommand::UploadFiles { .. }
                        | SftpCommand::DownloadFile { .. }
                        | SftpCommand::DownloadFiles { .. }
                        | SftpCommand::Rename { .. }
                        | SftpCommand::Delete { .. },
                    ) => {}
                }
            }
        }
    }
}

/// 按“私钥优先、密码兜底”的顺序执行 SSH 鉴权。
fn authenticate(session: &Session, ssh: &SshLinkConfig) -> Result<()> {
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

/// 后台线程向 UI 事件通道发送消息；接收端关闭时直接忽略。
fn send_event_blocking(event_sender: &Sender<SftpEvent>, event: SftpEvent) {
    let _ = event_sender.send_blocking(event);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 验证远程父目录解析覆盖根目录和普通路径。
    #[test]
    fn remote_parent_dir_handles_root_and_nested_paths() {
        assert_eq!(
            remote_parent_dir("/var/log/app.log"),
            Some("/var/log".to_string())
        );
        assert_eq!(remote_parent_dir("/app.log"), Some("/".to_string()));
        assert_eq!(remote_parent_dir("/"), None);
    }

    /// 验证重命名名称不能为空或包含路径分隔符。
    #[test]
    fn validate_sftp_rename_name_rejects_empty_and_paths() {
        assert_eq!(validate_sftp_rename_name(" app.log ").unwrap(), "app.log");
        assert!(validate_sftp_rename_name("").is_err());
        assert!(validate_sftp_rename_name("../app.log").is_err());
        assert!(validate_sftp_rename_name("dir/app.log").is_err());
    }

    /// 验证远程子路径拼接使用 POSIX 分隔符。
    #[test]
    fn remote_child_path_joins_root_and_nested_directories() {
        assert_eq!(remote_child_path("/", "tmp"), "/tmp");
        assert_eq!(
            remote_child_path("/var/log/", "app.log"),
            "/var/log/app.log"
        );
    }
}
