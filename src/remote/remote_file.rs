//! 文件职责：封装 SFTP、SMB、Git、SVN 通用远程文件会话、后台 worker 与文件元数据。
//! 创建日期：2026-06-26
//! 修改日期：2026-07-15
//! 作者：Argus 开发团队
//! 主要功能：以统一命令事件模型驱动可写文件协议和只读仓库协议的浏览、预览与下载。

use std::collections::BTreeSet;
use std::fs::File as LocalFile;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, mpsc};
use std::thread;
use std::time::{Duration, UNIX_EPOCH};

use anyhow::{Context as AnyhowContext, Result, anyhow, bail};
use async_channel::{Receiver, Sender};
use gpui::UniformListScrollHandle;
use smb2::{
    ClientConfig as SmbClientConfig, DirectoryEntry as SmbDirectoryEntry, SmbClient,
    Tree as SmbTree,
};

use crate::infra::text_input::TextInputState;
use crate::remote::connection::{
    ConnectionLinkConfig, ConnectionNodeId, GitLinkConfig, SmbLinkConfig, SshLinkConfig,
    SvnLinkConfig,
};
use crate::remote::git::run_git_worker;
use crate::remote::ssh::{authenticate_ssh_session, connect_ssh_session};
use crate::remote::svn::run_svn_worker;
use crate::remote::terminal::PendingHostKey;

/// 远程 Unix 文件类型掩码。
const SFTP_MODE_TYPE_MASK: u32 = 0o170000;
/// 远程普通文件类型位。
const SFTP_MODE_REGULAR_FILE: u32 = 0o100000;
/// 远程目录类型位。
const SFTP_MODE_DIRECTORY: u32 = 0o040000;
/// 远程符号链接类型位。
const SFTP_MODE_SYMLINK: u32 = 0o120000;
/// 允许预览的文件总大小上限，超过则不发起预览读取。
pub(crate) const REMOTE_FILE_PREVIEW_MAX_FILE_SIZE: u64 = 2 * 1024 * 1024;
/// 预览单次读取的字节上限，超出部分截断并提示。
pub(crate) const REMOTE_FILE_PREVIEW_MAX_READ: usize = 512 * 1024;

/// 判断远程条目是否允许文本预览：普通文件且总大小不超过预览上限。
pub(crate) fn is_remote_file_entry_previewable(entry: &RemoteFileEntry) -> bool {
    if entry.kind != RemoteFileEntryKind::RegularFile {
        return false;
    }
    entry
        .size
        .is_none_or(|size| size <= REMOTE_FILE_PREVIEW_MAX_FILE_SIZE)
}
/// 远程文件管理会话运行期状态，存放在 `ArgusApp` 中并由 UI 渲染。
pub(crate) struct RemoteFileSessionState {
    /// 远程文件管理会话 ID，与标签页中的 `session_id` 对应。
    pub id: usize,
    /// 关联的链接节点 ID。
    pub link_id: ConnectionNodeId,
    /// 文件管理标签标题。
    pub title: String,
    /// 当前文件管理会话使用的后端协议。
    pub backend: RemoteFileBackend,
    /// 后端暴露给应用与 UI 的操作能力，写操作必须在两层共同校验。
    pub capabilities: RemoteFileCapabilities,
    /// Git 分支/标签或 SVN 修订版本列表；普通文件协议为空。
    pub repository_versions: Vec<RepositoryVersion>,
    /// 当前仓库版本的稳定 ID；普通文件协议为空。
    pub selected_repository_version: Option<String>,
    /// SVN 修订号输入框；Git 使用版本下拉菜单，因此保持为空状态。
    pub version_input: TextInputState,
    /// 当前连接和操作状态。
    pub status: RemoteFileStatus,
    /// 发送给远程文件后台线程的命令通道。
    pub command_sender: Option<mpsc::Sender<RemoteFileCommand>>,
    /// 等待用户确认的主机指纹；没有待确认时为空。
    pub pending_host_key: Option<PendingHostKey>,
    /// 当前远程目录。
    pub current_dir: String,
    /// 地址栏输入框状态。
    pub address_input: TextInputState,
    /// 当前目录文件列表。
    pub entries: Vec<RemoteFileEntry>,
    /// 已按当前排序字段排序的文件列表快照，供 UI 切换页签时直接复用。
    pub sorted_entries: Arc<Vec<RemoteFileEntry>>,
    /// 当前选中的远程路径集合。
    pub selected_paths: BTreeSet<String>,
    /// 文件列表滚动句柄。
    pub list_scroll: UniformListScrollHandle,
    /// 当前排序字段。
    pub sort_field: RemoteFileSortField,
    /// 当前排序方向。
    pub sort_direction: RemoteFileSortDirection,
    /// 最近一次提示或错误。
    pub message: Option<String>,
}

impl RemoteFileSessionState {
    /// 创建一个处于“连接中”的远程文件管理会话状态。
    pub(crate) fn connecting(
        id: usize,
        link: &ConnectionLinkConfig,
        backend: RemoteFileBackend,
        command_sender: mpsc::Sender<RemoteFileCommand>,
    ) -> Self {
        let protocol_label = backend.label();
        Self {
            id,
            link_id: link.id,
            title: format!("{protocol_label} - {}", link.name),
            backend,
            capabilities: backend.capabilities(),
            repository_versions: Vec::new(),
            selected_repository_version: None,
            version_input: TextInputState::default(),
            status: RemoteFileStatus::Connecting,
            command_sender: Some(command_sender),
            pending_host_key: None,
            current_dir: String::new(),
            address_input: TextInputState::default(),
            entries: Vec::new(),
            sorted_entries: Arc::new(Vec::new()),
            selected_paths: BTreeSet::new(),
            list_scroll: UniformListScrollHandle::new(),
            sort_field: RemoteFileSortField::Name,
            sort_direction: RemoteFileSortDirection::Asc,
            message: Some(format!("正在建立 {protocol_label} 文件管理连接...")),
        }
    }

    /// 同步当前目录和文件列表，成功加载后清理旧选择。
    pub(crate) fn apply_directory_listing(
        &mut self,
        current_dir: String,
        entries: Vec<RemoteFileEntry>,
    ) {
        self.current_dir = current_dir.clone();
        self.address_input = TextInputState::from_value(current_dir);
        self.entries = entries;
        self.rebuild_sorted_entries();
        self.selected_paths.clear();
        self.status = RemoteFileStatus::Connected;
    }

    /// 根据当前排序字段重建 UI 使用的有序列表快照。
    pub(crate) fn rebuild_sorted_entries(&mut self) {
        let mut entries = self.entries.clone();
        sort_remote_file_entries(&mut entries, self.sort_field, self.sort_direction);
        self.sorted_entries = Arc::new(entries);
    }

    /// 返回当前选中的文件条目。
    pub(crate) fn selected_entries(&self) -> Vec<RemoteFileEntry> {
        self.entries
            .iter()
            .filter(|entry| self.selected_paths.contains(&entry.path))
            .cloned()
            .collect()
    }
}

/// 文件管理后端协议。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum RemoteFileBackend {
    /// SSH SFTP 文件管理。
    Sftp,
    /// SMB 共享文件管理。
    Smb,
    /// Git 仓库只读文件管理。
    Git,
    /// SVN 仓库只读文件管理。
    Svn,
}

impl RemoteFileBackend {
    /// 返回 UI 状态提示使用的协议名称。
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Sftp => "SFTP",
            Self::Smb => "SMB",
            Self::Git => "Git",
            Self::Svn => "SVN",
        }
    }

    /// 返回当前后端允许的文件操作能力。
    pub(crate) fn capabilities(self) -> RemoteFileCapabilities {
        match self {
            Self::Sftp | Self::Smb => RemoteFileCapabilities::writable(),
            Self::Git | Self::Svn => RemoteFileCapabilities::repository_read_only(),
        }
    }
}

/// 文件管理后端能力；UI 展示与动作层授权必须同时依据该结构。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct RemoteFileCapabilities {
    /// 是否允许浏览和刷新目录。
    pub browse: bool,
    /// 是否允许读取普通文件预览。
    pub preview: bool,
    /// 是否允许下载普通文件。
    pub download: bool,
    /// 是否具备任何远程写能力。
    pub write: bool,
    /// 是否允许上传本地文件。
    pub upload: bool,
    /// 是否允许重命名条目。
    pub rename: bool,
    /// 是否允许删除条目。
    pub delete: bool,
    /// 是否允许切换仓库分支、标签或修订号。
    pub switch_version: bool,
}

impl RemoteFileCapabilities {
    /// 构造 SFTP/SMB 使用的完整文件管理能力。
    const fn writable() -> Self {
        Self {
            browse: true,
            preview: true,
            download: true,
            write: true,
            upload: true,
            rename: true,
            delete: true,
            switch_version: false,
        }
    }

    /// 构造 Git/SVN 使用的严格只读仓库能力。
    const fn repository_read_only() -> Self {
        Self {
            browse: true,
            preview: true,
            download: true,
            write: false,
            upload: false,
            rename: false,
            delete: false,
            switch_version: true,
        }
    }
}

/// 仓库版本选项类别，用于 Git 下拉菜单分组和 SVN 当前修订展示。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum RepositoryVersionKind {
    /// Git 远程分支。
    GitBranch,
    /// Git 标签。
    GitTag,
    /// 跟随 SVN HEAD。
    SvnHead,
    /// 固定 SVN 数字修订。
    SvnRevision,
}

/// 一个可切换的仓库版本。
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct RepositoryVersion {
    /// 发给 worker 的稳定版本 ID，例如完整 Git ref 或 `HEAD`/`r123`。
    pub id: String,
    /// UI 展示名称。
    pub label: String,
    /// 版本类别。
    pub kind: RepositoryVersionKind,
}

/// 远程文件管理会话状态。
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum RemoteFileStatus {
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
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) enum RemoteFileEntryKind {
    /// 目录。
    Directory,
    /// 普通文件。
    RegularFile,
    /// 符号链接。
    Symlink,
    /// 其他类型。
    Other,
}

impl RemoteFileEntryKind {
    /// 返回 UI 展示用中文类型。
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Directory => "目录",
            Self::RegularFile => "文件",
            Self::Symlink => "链接",
            Self::Other => "其他",
        }
    }

    /// 判断当前条目是否支持普通文件传输。
    pub(crate) fn is_regular_file(self) -> bool {
        self == Self::RegularFile
    }
}

/// 远程文件列表排序字段，对应表头各列。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum RemoteFileSortField {
    /// 名称列。
    Name,
    /// 类型列。
    Type,
    /// 大小列。
    Size,
    /// 修改时间列。
    Mtime,
    /// 权限列。
    Permissions,
}

/// 远程文件列表排序方向。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum RemoteFileSortDirection {
    /// 升序。
    Asc,
    /// 降序。
    Desc,
}

/// 远程目录中的单个文件条目。
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct RemoteFileEntry {
    /// 文件名。
    pub name: String,
    /// 完整远程路径。
    pub path: String,
    /// 文件类型。
    pub kind: RemoteFileEntryKind,
    /// 文件大小；目录和部分服务器可能不返回。
    pub size: Option<u64>,
    /// Unix 修改时间秒级时间戳。
    pub mtime: Option<u64>,
    /// Unix 权限 mode。
    pub permissions: Option<u32>,
}

/// 启动远程文件 worker 时需要的不可变请求数据。
#[derive(Clone, Debug)]
pub(crate) struct RemoteFileWorkerRequest {
    /// 远程文件管理会话 ID。
    pub session_id: usize,
    /// 后端连接请求快照。
    pub backend: RemoteFileWorkerBackend,
}

/// 文件管理 worker 使用的后端连接请求。
#[derive(Clone, Debug)]
pub(crate) enum RemoteFileWorkerBackend {
    /// 通过 SSH SFTP 通道管理远程文件。
    Sftp {
        /// SSH 配置快照。
        ssh: SshLinkConfig,
        /// 已信任主机指纹；为空时需要 UI 二次确认。
        trusted_fingerprint: Option<String>,
    },
    /// 通过 SMB 共享管理远程文件。
    Smb {
        /// SMB 配置快照。
        smb: SmbLinkConfig,
    },
    /// 通过本地裸仓库缓存浏览 Git 对象。
    Git {
        /// 链接 ID，用于生成稳定缓存目录。
        link_id: ConnectionNodeId,
        /// Git 链接配置快照。
        git: GitLinkConfig,
        /// 已确认可信的 SSH 主机指纹。
        trusted_fingerprint: Option<String>,
    },
    /// 通过 ra_svn 会话浏览 SVN 仓库。
    Svn {
        /// SVN 链接配置快照。
        svn: SvnLinkConfig,
        /// 已确认可信的 SSH 主机指纹；`svn://` 时为空。
        trusted_fingerprint: Option<String>,
    },
}

/// UI 发送给远程文件 worker 的命令。
#[derive(Clone, Debug)]
pub(crate) enum RemoteFileCommand {
    /// 用户确认当前未知主机指纹可信。
    TrustHostKey,
    /// 用户拒绝当前未知主机指纹。
    RejectHostKey,
    /// 加载指定目录。
    LoadDirectory(String),
    /// 刷新当前目录。
    Refresh,
    /// 切换 Git 分支/标签或 SVN HEAD/数字修订号。
    SwitchRepositoryVersion {
        /// 版本稳定 ID。
        version_id: String,
    },
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
        entries: Vec<RemoteFileEntry>,
        /// 本地保存目录。
        local_dir: PathBuf,
    },
    /// 读取远程普通文件内容用于预览。
    ReadFileContent {
        /// 远程文件路径。
        remote_path: String,
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
        entry: RemoteFileEntry,
    },
    /// 主动断开远程文件通道。
    Disconnect,
}

/// 远程文件预览内容；由 worker 读取后回传给 UI 展示。
#[derive(Clone, Debug)]
pub(crate) enum FilePreviewContent {
    /// 文本内容；`truncated` 为真表示因超过预览读取上限被截断。
    Text {
        /// 解码后的文本。
        content: String,
        /// 是否因超过预览读取上限被截断。
        truncated: bool,
    },
    /// 检测为二进制文件，无法以文本预览。
    Binary,
    /// 读取失败时携带的用户可读错误。
    Error(String),
}

/// 远程文件 worker 回传给 UI 的事件。
#[derive(Clone, Debug)]
pub(crate) enum RemoteFileEvent {
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
    /// 远程文件通道已连接并完成首次目录读取。
    Connected {
        /// 会话 ID。
        session_id: usize,
        /// 当前目录。
        current_dir: String,
        /// 当前目录条目。
        entries: Vec<RemoteFileEntry>,
    },
    /// 目录读取完成。
    DirectoryLoaded {
        /// 会话 ID。
        session_id: usize,
        /// 当前目录。
        current_dir: String,
        /// 当前目录条目。
        entries: Vec<RemoteFileEntry>,
    },
    /// 仓库版本列表或当前版本发生变化。
    RepositoryVersionsLoaded {
        /// 会话 ID。
        session_id: usize,
        /// 当前可选版本；SVN 至少包含 HEAD 和当前固定修订。
        versions: Vec<RepositoryVersion>,
        /// 当前版本稳定 ID。
        selected_version: String,
        /// 当前版本输入框展示文本。
        input_value: String,
        /// 可选状态提示，例如 Git 离线缓存警告。
        message: Option<String>,
    },
    /// 网络获取或对象索引进度更新。
    TransferProgress {
        /// 会话 ID。
        session_id: usize,
        /// 面向用户的简短进度文本。
        message: String,
    },
    /// 远程文件预览内容读取完成。
    FileContentLoaded {
        /// 会话 ID。
        session_id: usize,
        /// 文件名，用于预览窗口标题。
        file_name: String,
        /// 预览内容。
        content: FilePreviewContent,
    },
    /// 操作成功。
    OperationSucceeded {
        /// 会话 ID。
        session_id: usize,
        /// 用户可读提示。
        message: String,
    },
    /// 操作失败，但远程文件会话仍可继续使用。
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
    /// 任一远程文件 worker 的连接级失败。
    Failed {
        /// 会话 ID。
        session_id: usize,
        /// 用户可读原因。
        message: String,
    },
}

/// 启动通用远程文件后台线程，并返回命令发送端与事件接收端。
pub(crate) fn spawn_remote_file_worker(
    request: RemoteFileWorkerRequest,
) -> (mpsc::Sender<RemoteFileCommand>, Receiver<RemoteFileEvent>) {
    let (command_sender, command_receiver) = mpsc::channel();
    let (event_sender, event_receiver) = async_channel::unbounded();
    thread::spawn(move || {
        if let Err(error) =
            run_remote_file_worker(request.clone(), command_receiver, event_sender.clone())
        {
            send_event_blocking(
                &event_sender,
                RemoteFileEvent::Failed {
                    session_id: request.session_id,
                    message: error.to_string(),
                },
            );
        }
    });
    (command_sender, event_receiver)
}

/// 校验远程文件重命名的新名称，第一版不允许跨目录移动。
pub(crate) fn validate_remote_file_rename_name(name: &str) -> Result<String, String> {
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

/// 为多文件下载构造不会逃逸目标目录的本地文件路径。
///
/// 参数：`local_dir` 为用户选择的目标目录，`file_name` 为远端条目的原始名称。
/// 返回值：名称是单个普通路径组件时返回目标路径；空名称、父目录或任一平台分隔符会被拒绝。
pub(crate) fn local_download_path(local_dir: &Path, file_name: &str) -> Result<PathBuf> {
    if file_name.is_empty()
        || file_name == "."
        || file_name == ".."
        || file_name.contains('/')
        || file_name.contains('\\')
    {
        bail!("远程文件名包含不安全的路径字符，无法下载：{file_name}");
    }
    Ok(local_dir.join(file_name))
}

/// 返回指定远程目录的父目录。
pub(crate) fn remote_parent_dir(path: &str) -> Option<String> {
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
pub(crate) fn remote_child_path(directory: &str, name: &str) -> String {
    if directory == "/" {
        format!("/{name}")
    } else {
        format!("{}/{}", directory.trim_end_matches('/'), name)
    }
}

/// 文件管理 worker 主流程：按后端协议连接并串行执行文件操作。
fn run_remote_file_worker(
    request: RemoteFileWorkerRequest,
    command_receiver: mpsc::Receiver<RemoteFileCommand>,
    event_sender: Sender<RemoteFileEvent>,
) -> Result<()> {
    match request.backend.clone() {
        RemoteFileWorkerBackend::Sftp {
            ssh,
            trusted_fingerprint,
        } => run_ssh_sftp_worker(
            request.session_id,
            ssh,
            trusted_fingerprint,
            command_receiver,
            event_sender,
        ),
        RemoteFileWorkerBackend::Smb { smb } => {
            run_smb_worker(request.session_id, smb, command_receiver, event_sender)
        }
        RemoteFileWorkerBackend::Git {
            link_id,
            git,
            trusted_fingerprint,
        } => run_git_worker(
            request.session_id,
            link_id,
            git,
            trusted_fingerprint,
            command_receiver,
            event_sender,
        ),
        RemoteFileWorkerBackend::Svn {
            svn,
            trusted_fingerprint,
        } => run_svn_worker(
            request.session_id,
            svn,
            trusted_fingerprint,
            command_receiver,
            event_sender,
        ),
    }
}

/// SFTP worker 主流程：连接、校验主机指纹、鉴权、建立 SFTP 通道并串行执行文件操作。
fn run_ssh_sftp_worker(
    session_id: usize,
    ssh: SshLinkConfig,
    trusted_fingerprint: Option<String>,
    command_receiver: mpsc::Receiver<RemoteFileCommand>,
    event_sender: Sender<RemoteFileEvent>,
) -> Result<()> {
    let (session, fingerprint) = connect_ssh_session(&ssh)?;
    verify_host_key(
        session_id,
        &ssh,
        trusted_fingerprint.as_deref(),
        &command_receiver,
        &event_sender,
        &fingerprint,
    )?;
    authenticate_ssh_session(&session, &ssh)?;

    let sftp = session.sftp().context("无法创建 SFTP 通道")?;
    let mut current_dir =
        normalize_remote_path(sftp.realpath(Path::new(".")).context("无法解析登录目录")?);
    let entries = read_remote_directory(&sftp, &current_dir)?;
    send_event_blocking(
        &event_sender,
        RemoteFileEvent::Connected {
            session_id,
            current_dir: current_dir.clone(),
            entries,
        },
    );

    while let Ok(command) = command_receiver.recv() {
        match command {
            RemoteFileCommand::TrustHostKey | RemoteFileCommand::RejectHostKey => {}
            RemoteFileCommand::LoadDirectory(path) => {
                match load_directory(&sftp, &current_dir, &path) {
                    Ok((next_dir, entries)) => {
                        current_dir = next_dir;
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
                send_directory_listing(&sftp, &current_dir, session_id, &event_sender);
            }
            RemoteFileCommand::SwitchRepositoryVersion { .. } => send_operation_failed(
                &event_sender,
                session_id,
                anyhow!("SFTP 不支持仓库版本切换"),
            ),
            RemoteFileCommand::UploadFiles { local_paths } => {
                let result = upload_files(&sftp, &current_dir, &local_paths);
                send_operation_result(&sftp, &current_dir, session_id, &event_sender, result);
            }
            RemoteFileCommand::DownloadFile {
                remote_path,
                local_path,
            } => {
                let result = download_file(&sftp, &remote_path, &local_path)
                    .map(|_| format!("已下载 {}", remote_file_name(&remote_path)));
                send_operation_result_without_refresh(session_id, &event_sender, result);
            }
            RemoteFileCommand::DownloadFiles { entries, local_dir } => {
                let result = download_files(&sftp, &entries, &local_dir);
                send_operation_result_without_refresh(session_id, &event_sender, result);
            }
            RemoteFileCommand::ReadFileContent { remote_path } => {
                send_file_preview_loaded(
                    session_id,
                    read_sftp_file_preview(&sftp, &remote_path),
                    &event_sender,
                );
            }
            RemoteFileCommand::Rename {
                remote_path,
                new_name,
            } => {
                let result = rename_entry(&sftp, &remote_path, &new_name);
                send_operation_result(&sftp, &current_dir, session_id, &event_sender, result);
            }
            RemoteFileCommand::Delete { entry } => {
                let result = delete_entry(&sftp, &entry);
                send_operation_result(&sftp, &current_dir, session_id, &event_sender, result);
            }
            RemoteFileCommand::Disconnect => {
                send_event_blocking(
                    &event_sender,
                    RemoteFileEvent::Disconnected {
                        session_id,
                        message: "SFTP 连接已断开".to_string(),
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
            message: "SFTP 连接已断开".to_string(),
        },
    );
    Ok(())
}

/// 在专属后台线程中运行异步 SMB worker。
fn run_smb_worker(
    session_id: usize,
    smb: SmbLinkConfig,
    command_receiver: mpsc::Receiver<RemoteFileCommand>,
    event_sender: Sender<RemoteFileEvent>,
) -> Result<()> {
    // 使用多线程运行时：命令循环在 block_on 线程上通过同步 recv() 阻塞等待用户操作，
    // 期间 smb2 客户端派生的后台任务（socket 读取循环、oplock/lease 通知）仍能在
    // worker 线程上被轮询，避免空闲时错过服务端推送的帧或连接关闭。
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(1)
        .enable_all()
        .build()
        .context("无法创建 SMB 异步运行时")?;
    runtime.block_on(run_smb_worker_async(
        session_id,
        smb,
        command_receiver,
        event_sender,
    ))
}

/// SMB worker 异步主流程：连接共享、进入初始目录并串行执行文件操作。
async fn run_smb_worker_async(
    session_id: usize,
    smb: SmbLinkConfig,
    command_receiver: mpsc::Receiver<RemoteFileCommand>,
    event_sender: Sender<RemoteFileEvent>,
) -> Result<()> {
    let mut client = create_smb_client(&smb).await?;
    let mut current_dir = crate::remote::connection::normalized_smb_initial_dir(&smb.initial_dir);
    let entries = read_smb_directory(&mut client, &current_dir).await?;
    send_event_blocking(
        &event_sender,
        RemoteFileEvent::Connected {
            session_id,
            current_dir: current_dir.clone(),
            entries,
        },
    );

    while let Ok(command) = command_receiver.recv() {
        match command {
            RemoteFileCommand::TrustHostKey | RemoteFileCommand::RejectHostKey => {}
            RemoteFileCommand::LoadDirectory(path) => {
                match load_smb_directory(&mut client, &current_dir, &path).await {
                    Ok((next_dir, entries)) => {
                        current_dir = next_dir;
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
                send_smb_directory_listing(&mut client, &current_dir, session_id, &event_sender)
                    .await;
            }
            RemoteFileCommand::SwitchRepositoryVersion { .. } => {
                send_operation_failed(&event_sender, session_id, anyhow!("SMB 不支持仓库版本切换"))
            }
            RemoteFileCommand::UploadFiles { local_paths } => {
                let result = upload_smb_files(&mut client, &current_dir, &local_paths).await;
                send_smb_operation_result(
                    &mut client,
                    &current_dir,
                    session_id,
                    &event_sender,
                    result,
                )
                .await;
            }
            RemoteFileCommand::DownloadFile {
                remote_path,
                local_path,
            } => {
                let result = download_smb_file(&mut client, &remote_path, &local_path)
                    .await
                    .map(|_| format!("已下载 {}", remote_file_name(&remote_path)));
                send_operation_result_without_refresh(session_id, &event_sender, result);
            }
            RemoteFileCommand::DownloadFiles { entries, local_dir } => {
                let result = download_smb_files(&mut client, &entries, &local_dir).await;
                send_operation_result_without_refresh(session_id, &event_sender, result);
            }
            RemoteFileCommand::ReadFileContent { remote_path } => {
                send_file_preview_loaded(
                    session_id,
                    read_smb_file_preview(&mut client, &remote_path).await,
                    &event_sender,
                );
            }
            RemoteFileCommand::Rename {
                remote_path,
                new_name,
            } => {
                let result = rename_smb_entry(&mut client, &remote_path, &new_name).await;
                send_smb_operation_result(
                    &mut client,
                    &current_dir,
                    session_id,
                    &event_sender,
                    result,
                )
                .await;
            }
            RemoteFileCommand::Delete { entry } => {
                let result = delete_smb_entry(&mut client, &entry).await;
                send_smb_operation_result(
                    &mut client,
                    &current_dir,
                    session_id,
                    &event_sender,
                    result,
                )
                .await;
            }
            RemoteFileCommand::Disconnect => {
                close_smb_client(&mut client).await;
                send_event_blocking(
                    &event_sender,
                    RemoteFileEvent::Disconnected {
                        session_id,
                        message: "SMB 连接已断开".to_string(),
                    },
                );
                return Ok(());
            }
        }
    }

    close_smb_client(&mut client).await;
    send_event_blocking(
        &event_sender,
        RemoteFileEvent::Disconnected {
            session_id,
            message: "SMB 连接已断开".to_string(),
        },
    );
    Ok(())
}

/// 加载用户输入的目录，并返回服务器规范化路径和条目。
fn load_directory(
    sftp: &ssh2::Sftp,
    current_dir: &str,
    input: &str,
) -> Result<(String, Vec<RemoteFileEntry>)> {
    let target = resolve_remote_path(current_dir, input)?;
    let current_dir = normalize_remote_path(
        sftp.realpath(Path::new(&target))
            .with_context(|| format!("无法进入目录 {target}"))?,
    );
    let entries = read_remote_directory(sftp, &current_dir)?;
    Ok((current_dir, entries))
}

/// 读取远程目录条目，并按目录优先、名称升序排序。
fn read_remote_directory(sftp: &ssh2::Sftp, directory: &str) -> Result<Vec<RemoteFileEntry>> {
    let mut entries = sftp
        .readdir(Path::new(directory))
        .with_context(|| format!("无法读取目录 {directory}"))?
        .into_iter()
        .filter_map(|(path, stat)| sftp_entry_from_stat(directory, path, stat))
        .collect::<Vec<_>>();
    sort_remote_entries(&mut entries);
    Ok(entries)
}

/// 将 ssh2 的目录项转换为 UI 需要的远程文件条目。
fn sftp_entry_from_stat(
    directory: &str,
    path: PathBuf,
    stat: ssh2::FileStat,
) -> Option<RemoteFileEntry> {
    let name = path.file_name()?.to_string_lossy().to_string();
    if name == "." || name == ".." {
        return None;
    }
    let kind = sftp_entry_kind(stat.perm);
    Some(RemoteFileEntry {
        path: remote_child_path(directory, &name),
        name,
        kind,
        size: stat.size,
        mtime: stat.mtime,
        permissions: stat.perm,
    })
}

/// 根据 Unix mode 判断远程文件类型。
fn sftp_entry_kind(permissions: Option<u32>) -> RemoteFileEntryKind {
    match permissions.map(|perm| perm & SFTP_MODE_TYPE_MASK) {
        Some(SFTP_MODE_DIRECTORY) => RemoteFileEntryKind::Directory,
        Some(SFTP_MODE_REGULAR_FILE) => RemoteFileEntryKind::RegularFile,
        Some(SFTP_MODE_SYMLINK) => RemoteFileEntryKind::Symlink,
        _ => RemoteFileEntryKind::Other,
    }
}

/// 已连接到指定共享的 SMB 客户端封装。
struct SmbShareClient {
    /// SMB 客户端实例，内部持有连接和认证会话。
    client: SmbClient,
    /// 已连接的 SMB 共享 tree。
    tree: SmbTree,
}

/// 根据 SMB 配置创建并连接纯 Rust SMB 客户端。
async fn create_smb_client(smb: &SmbLinkConfig) -> Result<SmbShareClient> {
    let mut client = SmbClient::connect(SmbClientConfig {
        addr: format!("{}:{}", smb.host, smb.port),
        timeout: Duration::from_secs(10),
        username: smb.username.clone(),
        password: smb.password.clone(),
        domain: smb.domain.clone().unwrap_or_default(),
        auto_reconnect: false,
        compression: true,
        dfs_enabled: true,
        dfs_target_overrides: Default::default(),
    })
    .await
    .with_context(|| format!("无法连接 SMB 服务器 {}:{}", smb.host, smb.port))?;
    let tree = client
        .connect_share(&smb.share)
        .await
        .with_context(|| format!("无法连接 SMB 共享 {}", smb.address_label()))?;
    Ok(SmbShareClient { client, tree })
}

/// 关闭 SMB 客户端持有的共享连接。
async fn close_smb_client(client: &mut SmbShareClient) {
    let _ = client.client.disconnect_share(&client.tree).await;
}

/// 加载 SMB 目录，并返回共享内规范化路径和条目。
async fn load_smb_directory(
    client: &mut SmbShareClient,
    current_dir: &str,
    input: &str,
) -> Result<(String, Vec<RemoteFileEntry>)> {
    let target = crate::remote::connection::normalized_smb_initial_dir(&resolve_remote_path(
        current_dir,
        input,
    )?);
    let entries = read_smb_directory(client, &target).await?;
    Ok((target, entries))
}

/// 读取 SMB 目录条目，并按目录优先、名称升序排序。
async fn read_smb_directory(
    client: &mut SmbShareClient,
    directory: &str,
) -> Result<Vec<RemoteFileEntry>> {
    let mut entries = client
        .client
        .list_directory(&mut client.tree, &smb_relative_path(directory))
        .await
        .with_context(|| format!("无法读取 SMB 目录 {directory}"))?
        .into_iter()
        .filter_map(|entry| smb_entry_from_directory_info(directory, entry))
        .collect::<Vec<_>>();
    sort_remote_entries(&mut entries);
    Ok(entries)
}

/// 将 SMB 目录枚举条目转换为 UI 需要的远程文件条目。
fn smb_entry_from_directory_info(
    directory: &str,
    entry: SmbDirectoryEntry,
) -> Option<RemoteFileEntry> {
    let name = entry.name;
    if name == "." || name == ".." {
        return None;
    }
    let kind = if entry.is_directory {
        RemoteFileEntryKind::Directory
    } else {
        RemoteFileEntryKind::RegularFile
    };
    Some(RemoteFileEntry {
        name: name.clone(),
        path: remote_child_path(directory, &name),
        kind,
        size: Some(entry.size),
        mtime: smb_filetime_to_unix_seconds(entry.modified),
        permissions: None,
    })
}

/// 上传本地普通文件集合到 SMB 当前目录。
async fn upload_smb_files(
    client: &mut SmbShareClient,
    current_dir: &str,
    local_paths: &[PathBuf],
) -> Result<String> {
    if local_paths.is_empty() {
        bail!("未选择要上传的文件");
    }

    // 复用同一块读取缓冲，避免每个分块都新分配并清零 64 KiB。
    let mut chunk_scratch = vec![0_u8; 64 * 1024];
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
        let mut local_file = LocalFile::open(local_path)
            .with_context(|| format!("无法打开本地文件 {}", local_path.display()))?;
        let mut next_chunk = || read_next_local_chunk(&mut local_file, &mut chunk_scratch);
        client
            .client
            .write_file_streamed(
                &mut client.tree,
                &smb_relative_path(&remote_path),
                &mut next_chunk,
            )
            .await
            .with_context(|| format!("上传文件失败：{}", local_path.display()))?;
    }

    Ok(format!("已上传 {} 个文件", local_paths.len()))
}

/// 从本地文件读取下一块上传数据，使用调用方提供的复用缓冲区。
fn read_next_local_chunk(
    local_file: &mut LocalFile,
    scratch: &mut [u8],
) -> Option<std::result::Result<Vec<u8>, io::Error>> {
    match local_file.read(scratch) {
        Ok(0) => None,
        Ok(read_len) => Some(Ok(scratch[..read_len].to_vec())),
        Err(error) => Some(Err(error)),
    }
}

/// 下载单个 SMB 远程文件到本地路径。
async fn download_smb_file(
    client: &mut SmbShareClient,
    remote_path: &str,
    local_path: &Path,
) -> Result<()> {
    let mut download = client
        .client
        .download(&client.tree, &smb_relative_path(remote_path))
        .await
        .with_context(|| format!("无法打开 SMB 文件 {remote_path}"))?;
    let mut local_file = LocalFile::create(local_path)
        .with_context(|| format!("无法创建本地文件 {}", local_path.display()))?;
    while let Some(chunk) = download.next_chunk().await {
        let chunk = chunk.with_context(|| format!("下载文件失败：{remote_path}"))?;
        local_file.write_all(&chunk)?;
    }
    local_file.flush()?;
    Ok(())
}

/// 下载多个 SMB 远程普通文件到本地目录。
async fn download_smb_files(
    client: &mut SmbShareClient,
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
        download_smb_file(client, &entry.path, &local_path).await?;
    }

    Ok(format!("已下载 {} 个文件", entries.len()))
}

/// 在当前 SMB 目录内重命名远程条目。
async fn rename_smb_entry(
    client: &mut SmbShareClient,
    remote_path: &str,
    new_name: &str,
) -> Result<String> {
    let new_name = validate_remote_file_rename_name(new_name).map_err(anyhow::Error::msg)?;
    let parent = remote_parent_dir(remote_path).ok_or_else(|| anyhow!("无法解析远程父目录"))?;
    let next_path = remote_child_path(&parent, &new_name);
    client
        .client
        .rename(
            &mut client.tree,
            &smb_relative_path(remote_path),
            &smb_relative_path(&next_path),
        )
        .await
        .with_context(|| format!("重命名失败：{remote_path}"))?;
    Ok(format!("已重命名为 {new_name}"))
}

/// 删除 SMB 远程普通文件或空目录。
async fn delete_smb_entry(client: &mut SmbShareClient, entry: &RemoteFileEntry) -> Result<String> {
    match entry.kind {
        RemoteFileEntryKind::RegularFile => client
            .client
            .delete_file(&mut client.tree, &smb_relative_path(&entry.path))
            .await
            .with_context(|| format!("删除文件失败：{}", entry.name))
            .map(|_| format!("已删除 {}", entry.name)),
        RemoteFileEntryKind::Directory => {
            match client
                .client
                .delete_directory(&mut client.tree, &smb_relative_path(&entry.path))
                .await
            {
                Ok(_) => Ok(format!("已删除目录 {}", entry.name)),
                Err(error) => {
                    // 根据 SMB 错误类型提供更精确的提示。
                    use smb2::ErrorKind;
                    let message = match error.kind() {
                        ErrorKind::AccessDenied => {
                            format!("无权限删除目录：{}", entry.name)
                        }
                        ErrorKind::NotFound => {
                            format!("目录不存在：{}", entry.name)
                        }
                        ErrorKind::SharingViolation => {
                            format!("目录正被其他进程使用：{}", entry.name)
                        }
                        _ => {
                            // 其他错误（可能是非空目录等）显示底层错误。
                            format!("目录可能非空或无法删除：{error}")
                        }
                    };
                    Err(anyhow!(message))
                }
            }
        }
        RemoteFileEntryKind::Symlink | RemoteFileEntryKind::Other => {
            Err(anyhow!("仅支持删除普通文件或空目录：{}", entry.name))
        }
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
        let mut local_file = LocalFile::open(local_path)
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
    let mut local_file = LocalFile::create(local_path)
        .with_context(|| format!("无法创建本地文件 {}", local_path.display()))?;
    io::copy(&mut remote_file, &mut local_file)
        .with_context(|| format!("下载文件失败：{remote_path}"))?;
    Ok(())
}

/// 下载多个远程普通文件到本地目录。
fn download_files(
    sftp: &ssh2::Sftp,
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
        download_file(sftp, &entry.path, &local_path)?;
    }

    Ok(format!("已下载 {} 个文件", entries.len()))
}

/// 在当前目录内重命名远程条目。
fn rename_entry(sftp: &ssh2::Sftp, remote_path: &str, new_name: &str) -> Result<String> {
    let new_name = validate_remote_file_rename_name(new_name).map_err(anyhow::Error::msg)?;
    let parent = remote_parent_dir(remote_path).ok_or_else(|| anyhow!("无法解析远程父目录"))?;
    let next_path = remote_child_path(&parent, &new_name);
    sftp.rename(Path::new(remote_path), Path::new(&next_path), None)
        .with_context(|| format!("重命名失败：{remote_path}"))?;
    Ok(format!("已重命名为 {new_name}"))
}

/// 删除远程普通文件或空目录。
fn delete_entry(sftp: &ssh2::Sftp, entry: &RemoteFileEntry) -> Result<String> {
    match entry.kind {
        RemoteFileEntryKind::RegularFile => {
            sftp.unlink(Path::new(&entry.path))
                .with_context(|| format!("删除文件失败：{}", entry.name))?;
            Ok(format!("已删除 {}", entry.name))
        }
        RemoteFileEntryKind::Directory => {
            sftp.rmdir(Path::new(&entry.path))
                .map_err(|error| describe_sftp_rmdir_error(&entry.name, error))?;
            Ok(format!("已删除目录 {}", entry.name))
        }
        RemoteFileEntryKind::Symlink | RemoteFileEntryKind::Other => {
            bail!("仅支持删除普通文件或空目录：{}", entry.name)
        }
    }
}

/// 根据 libssh2 SFTP 状态码，将删除目录失败转换为面向用户的中文说明。
fn describe_sftp_rmdir_error(entry_name: &str, error: ssh2::Error) -> anyhow::Error {
    // libssh2 SFTP 状态码：3=无权限，18=目录非空。
    const LIBSSH2_FX_PERMISSION_DENIED: i32 = 3;
    const LIBSSH2_FX_DIR_NOT_EMPTY: i32 = 18;
    match error.code() {
        ssh2::ErrorCode::SFTP(LIBSSH2_FX_DIR_NOT_EMPTY) => {
            anyhow!("目录非空，无法删除：{entry_name}")
        }
        ssh2::ErrorCode::SFTP(LIBSSH2_FX_PERMISSION_DENIED) => {
            anyhow!("无权限删除目录：{entry_name}")
        }
        _ => anyhow!("删除目录失败：{entry_name}：{error}"),
    }
}

/// 刷新当前目录并发送完整目录事件。
fn send_directory_listing(
    sftp: &ssh2::Sftp,
    current_dir: &str,
    session_id: usize,
    event_sender: &Sender<RemoteFileEvent>,
) {
    match read_remote_directory(sftp, current_dir) {
        Ok(entries) => send_event_blocking(
            event_sender,
            RemoteFileEvent::DirectoryLoaded {
                session_id,
                current_dir: current_dir.to_string(),
                entries,
            },
        ),
        Err(error) => send_operation_failed(event_sender, session_id, error),
    }
}

/// 刷新 SMB 当前目录并发送完整目录事件。
async fn send_smb_directory_listing(
    client: &mut SmbShareClient,
    current_dir: &str,
    session_id: usize,
    event_sender: &Sender<RemoteFileEvent>,
) {
    match read_smb_directory(client, current_dir).await {
        Ok(entries) => send_event_blocking(
            event_sender,
            RemoteFileEvent::DirectoryLoaded {
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
    event_sender: &Sender<RemoteFileEvent>,
    result: Result<String>,
) {
    match result {
        Ok(message) => {
            send_event_blocking(
                event_sender,
                RemoteFileEvent::OperationSucceeded {
                    session_id,
                    message,
                },
            );
            send_directory_listing(sftp, current_dir, session_id, event_sender);
        }
        Err(error) => send_operation_failed(event_sender, session_id, error),
    }
}

/// 发送带刷新 SMB 目录的操作结果。
async fn send_smb_operation_result(
    client: &mut SmbShareClient,
    current_dir: &str,
    session_id: usize,
    event_sender: &Sender<RemoteFileEvent>,
    result: Result<String>,
) {
    match result {
        Ok(message) => {
            send_event_blocking(
                event_sender,
                RemoteFileEvent::OperationSucceeded {
                    session_id,
                    message,
                },
            );
            send_smb_directory_listing(client, current_dir, session_id, event_sender).await;
        }
        Err(error) => send_operation_failed(event_sender, session_id, error),
    }
}

/// 发送无需刷新目录的操作结果。
fn send_operation_result_without_refresh(
    session_id: usize,
    event_sender: &Sender<RemoteFileEvent>,
    result: Result<String>,
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

/// 发送远程文件预览内容读取完成事件。
pub(crate) fn send_file_preview_loaded(
    session_id: usize,
    preview: (String, FilePreviewContent),
    event_sender: &Sender<RemoteFileEvent>,
) {
    let (file_name, content) = preview;
    send_event_blocking(
        event_sender,
        RemoteFileEvent::FileContentLoaded {
            session_id,
            file_name,
            content,
        },
    );
}

/// 把读取到的字节转换为预览内容；含空字节判定为二进制，否则按 UTF-8 失败安全解码。
pub(crate) fn preview_content_from_bytes(buf: Vec<u8>, truncated: bool) -> FilePreviewContent {
    if buf.contains(&0) {
        FilePreviewContent::Binary
    } else {
        FilePreviewContent::Text {
            content: String::from_utf8_lossy(&buf).into_owned(),
            truncated,
        }
    }
}

/// 读取 SFTP 远程普通文件前若干字节用于预览，返回文件名与预览内容。
fn read_sftp_file_preview(sftp: &ssh2::Sftp, remote_path: &str) -> (String, FilePreviewContent) {
    let file_name = remote_file_name(remote_path);
    let mut remote = match sftp.open(Path::new(remote_path)) {
        Ok(file) => file,
        Err(error) => {
            return (
                file_name,
                FilePreviewContent::Error(format!("无法打开远程文件：{error}")),
            );
        }
    };
    // 多读 1 字节用于判断是否达到读取上限被截断。
    let mut buf = Vec::with_capacity(REMOTE_FILE_PREVIEW_MAX_READ + 1);
    if let Err(error) = (&mut remote)
        .take((REMOTE_FILE_PREVIEW_MAX_READ + 1) as u64)
        .read_to_end(&mut buf)
    {
        return (
            file_name,
            FilePreviewContent::Error(format!("读取文件失败：{error}")),
        );
    }
    let truncated = buf.len() > REMOTE_FILE_PREVIEW_MAX_READ;
    if truncated {
        buf.truncate(REMOTE_FILE_PREVIEW_MAX_READ);
    }
    (file_name, preview_content_from_bytes(buf, truncated))
}

/// 读取 SMB 远程普通文件前若干字节用于预览，返回文件名与预览内容。
async fn read_smb_file_preview(
    client: &mut SmbShareClient,
    remote_path: &str,
) -> (String, FilePreviewContent) {
    let file_name = remote_file_name(remote_path);
    let mut download = match client
        .client
        .download(&client.tree, &smb_relative_path(remote_path))
        .await
    {
        Ok(download) => download,
        Err(error) => {
            return (
                file_name,
                FilePreviewContent::Error(format!("无法打开远程文件：{error}")),
            );
        }
    };
    let mut buf = Vec::with_capacity(REMOTE_FILE_PREVIEW_MAX_READ + 1);
    while buf.len() <= REMOTE_FILE_PREVIEW_MAX_READ {
        match download.next_chunk().await {
            Some(Ok(chunk)) => buf.extend_from_slice(&chunk),
            Some(Err(error)) => {
                return (
                    file_name,
                    FilePreviewContent::Error(format!("读取文件失败：{error}")),
                );
            }
            None => break,
        }
    }
    let truncated = buf.len() > REMOTE_FILE_PREVIEW_MAX_READ;
    if truncated {
        buf.truncate(REMOTE_FILE_PREVIEW_MAX_READ);
    }
    (file_name, preview_content_from_bytes(buf, truncated))
}

/// 发送可恢复的操作失败事件。
pub(crate) fn send_operation_failed(
    event_sender: &Sender<RemoteFileEvent>,
    session_id: usize,
    error: anyhow::Error,
) {
    send_event_blocking(
        event_sender,
        RemoteFileEvent::OperationFailed {
            session_id,
            message: error.to_string(),
        },
    );
}

/// 把用户输入的远程目录解析为绝对路径文本。
pub(crate) fn resolve_remote_path(current_dir: &str, input: &str) -> Result<String> {
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

/// 将共享内绝对路径转换为 SMB UNC 中的相对路径，不携带前导分隔符。
fn smb_relative_path(path: &str) -> String {
    crate::remote::connection::normalized_smb_initial_dir(path)
        .trim_start_matches('/')
        .replace('/', "\\")
}

/// 将 SMB FILETIME 转为 Unix 秒级时间戳；零值视为无时间。
fn smb_filetime_to_unix_seconds(file_time: smb2::pack::FileTime) -> Option<u64> {
    if file_time == smb2::pack::FileTime::ZERO {
        return None;
    }
    file_time
        .to_system_time()
        .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
        .map(|duration| duration.as_secs())
}

/// 按目录优先和名称升序排序远程文件条目。
fn sort_remote_entries(entries: &mut [RemoteFileEntry]) {
    // 使用 sort_by_cached_key，每个条目只计算一次小写键，避免比较时反复分配。
    entries.sort_by_cached_key(|entry| {
        let group = if entry.kind == RemoteFileEntryKind::Directory {
            0_u8
        } else {
            1_u8
        };
        (group, entry.name.to_ascii_lowercase(), entry.name.clone())
    });
}

/// 按指定字段和方向排序远程文件条目；目录始终分组靠前，方向只在各自分组内生效。
pub(crate) fn sort_remote_file_entries(
    entries: &mut [RemoteFileEntry],
    field: RemoteFileSortField,
    direction: RemoteFileSortDirection,
) {
    entries.sort_by(|a, b| {
        let group_a = if a.kind == RemoteFileEntryKind::Directory {
            0_u8
        } else {
            1_u8
        };
        let group_b = if b.kind == RemoteFileEntryKind::Directory {
            0_u8
        } else {
            1_u8
        };
        group_a.cmp(&group_b).then_with(|| {
            let order = compare_remote_file_entries(a, b, field);
            if direction == RemoteFileSortDirection::Desc {
                order.reverse()
            } else {
                order
            }
        })
    });
}

/// 按排序字段比较两个条目；同列相等时回退名称，保证排序稳定可预期。
fn compare_remote_file_entries(
    a: &RemoteFileEntry,
    b: &RemoteFileEntry,
    field: RemoteFileSortField,
) -> std::cmp::Ordering {
    match field {
        RemoteFileSortField::Name => a
            .name
            .to_ascii_lowercase()
            .cmp(&b.name.to_ascii_lowercase())
            .then_with(|| a.name.cmp(&b.name)),
        RemoteFileSortField::Type => a.kind.cmp(&b.kind).then_with(|| {
            a.name
                .to_ascii_lowercase()
                .cmp(&b.name.to_ascii_lowercase())
        }),
        RemoteFileSortField::Size => {
            a.size.unwrap_or(0).cmp(&b.size.unwrap_or(0)).then_with(|| {
                a.name
                    .to_ascii_lowercase()
                    .cmp(&b.name.to_ascii_lowercase())
            })
        }
        RemoteFileSortField::Mtime => {
            a.mtime
                .unwrap_or(0)
                .cmp(&b.mtime.unwrap_or(0))
                .then_with(|| {
                    a.name
                        .to_ascii_lowercase()
                        .cmp(&b.name.to_ascii_lowercase())
                })
        }
        RemoteFileSortField::Permissions => a
            .permissions
            .unwrap_or(0)
            .cmp(&b.permissions.unwrap_or(0))
            .then_with(|| {
                a.name
                    .to_ascii_lowercase()
                    .cmp(&b.name.to_ascii_lowercase())
            }),
    }
}

/// 返回远程路径中的文件名。
pub(crate) fn remote_file_name(path: &str) -> String {
    path.rsplit('/')
        .next()
        .filter(|name| !name.is_empty())
        .unwrap_or(path)
        .to_string()
}

/// 校验主机指纹；未知主机需要等待 UI 发送确认或拒绝命令。
fn verify_host_key(
    session_id: usize,
    ssh: &SshLinkConfig,
    trusted_fingerprint: Option<&str>,
    command_receiver: &mpsc::Receiver<RemoteFileCommand>,
    event_sender: &Sender<RemoteFileEvent>,
    fingerprint: &str,
) -> Result<()> {
    match trusted_fingerprint {
        Some(expected) if expected == fingerprint => Ok(()),
        Some(_) => bail!("SSH 主机指纹发生变化，已阻止连接"),
        None => {
            send_event_blocking(
                event_sender,
                RemoteFileEvent::HostKeyVerification {
                    session_id,
                    host: ssh.host.clone(),
                    port: ssh.port,
                    fingerprint: fingerprint.to_string(),
                },
            );
            loop {
                match command_receiver.recv() {
                    Ok(RemoteFileCommand::TrustHostKey) => return Ok(()),
                    Ok(RemoteFileCommand::RejectHostKey) => bail!("用户拒绝信任 SSH 主机指纹"),
                    Ok(RemoteFileCommand::Disconnect) | Err(_) => bail!("SFTP 连接已取消"),
                    Ok(
                        RemoteFileCommand::LoadDirectory(_)
                        | RemoteFileCommand::Refresh
                        | RemoteFileCommand::SwitchRepositoryVersion { .. }
                        | RemoteFileCommand::UploadFiles { .. }
                        | RemoteFileCommand::DownloadFile { .. }
                        | RemoteFileCommand::DownloadFiles { .. }
                        | RemoteFileCommand::ReadFileContent { .. }
                        | RemoteFileCommand::Rename { .. }
                        | RemoteFileCommand::Delete { .. },
                    ) => {}
                }
            }
        }
    }
}

/// 后台线程向 UI 事件通道发送消息；接收端关闭时直接忽略。
pub(crate) fn send_event_blocking(event_sender: &Sender<RemoteFileEvent>, event: RemoteFileEvent) {
    let _ = event_sender.send_blocking(event);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 验证文件管理标签使用实际后端协议名称，不再统一显示“文件管理”。
    #[test]
    fn remote_file_session_title_uses_backend_protocol() {
        let link = ConnectionLinkConfig {
            id: 7,
            parent_id: None,
            name: "测试链接".to_string(),
            ssh: None,
            smb: None,
            git: None,
            svn: None,
        };
        for (backend, expected) in [
            (RemoteFileBackend::Sftp, "SFTP - 测试链接"),
            (RemoteFileBackend::Smb, "SMB - 测试链接"),
            (RemoteFileBackend::Git, "Git - 测试链接"),
            (RemoteFileBackend::Svn, "SVN - 测试链接"),
        ] {
            let (sender, _receiver) = mpsc::channel();
            let session = RemoteFileSessionState::connecting(1, &link, backend, sender);
            assert_eq!(session.title, expected);
        }
    }

    /// 验证仓库后端只开放浏览、预览、下载和版本切换，所有写能力均关闭。
    #[test]
    fn repository_capabilities_are_strictly_read_only() {
        for backend in [RemoteFileBackend::Git, RemoteFileBackend::Svn] {
            let capabilities = backend.capabilities();
            assert!(capabilities.browse);
            assert!(capabilities.preview);
            assert!(capabilities.download);
            assert!(capabilities.switch_version);
            assert!(!capabilities.write);
            assert!(!capabilities.upload);
            assert!(!capabilities.rename);
            assert!(!capabilities.delete);
        }
    }

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
    fn validate_remote_file_rename_name_rejects_empty_and_paths() {
        assert_eq!(
            validate_remote_file_rename_name(" app.log ").unwrap(),
            "app.log"
        );
        assert!(validate_remote_file_rename_name("").is_err());
        assert!(validate_remote_file_rename_name("../app.log").is_err());
        assert!(validate_remote_file_rename_name("dir/app.log").is_err());
    }

    /// 验证多文件下载保留安全文件名，并在 Unix/Windows 分隔符下都无法逃逸目标目录。
    #[test]
    fn local_download_path_rejects_cross_platform_path_escape() {
        let root = Path::new("/tmp/downloads");
        assert_eq!(
            local_download_path(root, " app.log ").unwrap(),
            root.join(" app.log ")
        );
        assert!(local_download_path(root, "../outside.log").is_err());
        assert!(local_download_path(root, "..\\outside.log").is_err());
        assert!(local_download_path(root, ".").is_err());
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

    /// 构造一个最小可用的远程文件条目用于排序测试。
    fn make_entry(
        name: &str,
        kind: RemoteFileEntryKind,
        size: Option<u64>,
        mtime: Option<u64>,
    ) -> RemoteFileEntry {
        RemoteFileEntry {
            name: name.to_string(),
            path: format!("/{name}"),
            kind,
            size,
            mtime,
            permissions: None,
        }
    }

    /// 验证排序始终把目录置于文件之前，即使降序也不改变分组顺序。
    #[test]
    fn sort_remote_file_entries_keeps_directories_first_regardless_of_direction() {
        let mut entries = vec![
            make_entry("app.log", RemoteFileEntryKind::RegularFile, Some(10), None),
            make_entry("configs", RemoteFileEntryKind::Directory, None, None),
            make_entry("z-dir", RemoteFileEntryKind::Directory, None, None),
        ];

        sort_remote_file_entries(
            &mut entries,
            RemoteFileSortField::Name,
            RemoteFileSortDirection::Asc,
        );
        assert_eq!(entries[0].name, "configs");
        assert_eq!(entries[1].name, "z-dir");
        assert_eq!(entries[2].name, "app.log");

        // 降序只在目录组内反转，文件仍排在所有目录之后。
        sort_remote_file_entries(
            &mut entries,
            RemoteFileSortField::Name,
            RemoteFileSortDirection::Desc,
        );
        assert_eq!(entries[0].name, "z-dir");
        assert_eq!(entries[1].name, "configs");
        assert_eq!(entries[2].name, "app.log");
    }

    /// 验证按大小排序在文件组内正确升降序，并以名称作为稳定回退。
    #[test]
    fn sort_remote_file_entries_orders_by_size_within_file_group() {
        let mut entries = vec![
            make_entry("big.log", RemoteFileEntryKind::RegularFile, Some(300), None),
            make_entry(
                "small.log",
                RemoteFileEntryKind::RegularFile,
                Some(10),
                None,
            ),
            make_entry("mid.log", RemoteFileEntryKind::RegularFile, Some(100), None),
        ];

        sort_remote_file_entries(
            &mut entries,
            RemoteFileSortField::Size,
            RemoteFileSortDirection::Asc,
        );
        assert_eq!(
            entries.iter().map(|e| e.name.as_str()).collect::<Vec<_>>(),
            vec!["small.log", "mid.log", "big.log"]
        );

        sort_remote_file_entries(
            &mut entries,
            RemoteFileSortField::Size,
            RemoteFileSortDirection::Desc,
        );
        assert_eq!(
            entries.iter().map(|e| e.name.as_str()).collect::<Vec<_>>(),
            vec!["big.log", "mid.log", "small.log"]
        );
    }

    /// 验证名称排序大小写不敏感，并保留原大小写形式。
    #[test]
    fn sort_remote_file_entries_sorts_names_case_insensitively() {
        let mut entries = vec![
            make_entry("Banana", RemoteFileEntryKind::RegularFile, None, None),
            make_entry("apple", RemoteFileEntryKind::RegularFile, None, None),
            make_entry("Cherry", RemoteFileEntryKind::RegularFile, None, None),
        ];

        sort_remote_file_entries(
            &mut entries,
            RemoteFileSortField::Name,
            RemoteFileSortDirection::Asc,
        );
        assert_eq!(
            entries.iter().map(|e| e.name.as_str()).collect::<Vec<_>>(),
            vec!["apple", "Banana", "Cherry"]
        );
    }

    /// 验证缺少大小信息的条目按 0 参与比较，不会导致排序崩溃。
    #[test]
    fn sort_remote_file_entries_treats_missing_size_as_zero() {
        let mut entries = vec![
            make_entry("with-size", RemoteFileEntryKind::RegularFile, Some(5), None),
            make_entry("no-size", RemoteFileEntryKind::RegularFile, None, None),
        ];

        sort_remote_file_entries(
            &mut entries,
            RemoteFileSortField::Size,
            RemoteFileSortDirection::Asc,
        );
        // 两条目大小同为 0，回退名称升序。
        assert_eq!(entries[0].name, "no-size");
        assert_eq!(entries[1].name, "with-size");
    }

    /// 验证纯文本字节解码为文本内容并携带截断标记。
    #[test]
    fn preview_content_from_bytes_decodes_utf8_text() {
        match preview_content_from_bytes(b"hello\nworld".to_vec(), false) {
            FilePreviewContent::Text { content, truncated } => {
                assert_eq!(content, "hello\nworld");
                assert!(!truncated);
            }
            other => panic!("expected Text, got {other:?}"),
        }
    }

    /// 验证包含空字节的字节流被识别为二进制文件。
    #[test]
    fn preview_content_from_bytes_detects_binary_by_null_byte() {
        let buf = b"some\x00binary".to_vec();
        match preview_content_from_bytes(buf, true) {
            FilePreviewContent::Binary => {}
            other => panic!("expected Binary, got {other:?}"),
        }
    }

    /// 验证截断标记透传到文本预览结果。
    #[test]
    fn preview_content_from_bytes_propagates_truncated_flag() {
        match preview_content_from_bytes(b"truncated payload".to_vec(), true) {
            FilePreviewContent::Text { content, truncated } => {
                assert_eq!(content, "truncated payload");
                assert!(truncated);
            }
            other => panic!("expected Text, got {other:?}"),
        }
    }
}
