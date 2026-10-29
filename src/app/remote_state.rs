//! 文件职责：提取远程连接弹窗状态类型定义。
//! 创建日期：2026-07-08
//! 修改日期：2026-10-29
//! 作者：Argus 开发团队
//! 主要功能：定义连接目录表单、SSH/SMB/Git/SVN 链接表单、主机指纹确认和文件管理弹窗状态，含仓库历史版本选择。

use super::types::TextInputState;
use crate::remote::connection::{ConnectionCredentialId, ConnectionLinkKind, ConnectionNodeId};
use crate::remote::remote_file::RepositoryHistoryEntry;
use crate::remote::svn_tls::SvnCertificatePrompt;
use crate::ui::file_preview_window::FilePreviewWindow;
use gpui::{ScrollHandle, WindowHandle};

/// 链接工作区当前打开的弹窗。
#[derive(Clone, Debug)]
pub(crate) enum ConnectionDialogState {
    /// SSH 首次连接未知主机时的指纹确认弹窗。
    ConfirmHostKey(ConnectionHostKeyPromptState),
    /// SVN HTTPS 证书异常或已信任证书变化时的确认弹窗。
    SvnCertificate(ConnectionSvnCertificatePromptState),
    /// 删除链接目录或任一协议链接前的二次确认弹窗。
    ConfirmDelete(ConnectionDeletePromptState),
}

/// SVN 证书弹窗绑定单次请求，避免旧按钮批准重连后出现的新证书。
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ConnectionSvnCertificatePromptState {
    pub session_id: usize,
    pub request_id: String,
    pub prompt: SvnCertificatePrompt,
    pub error_message: Option<String>,
}

/// 新增目录表单状态。
#[derive(Clone, Debug)]
pub(crate) struct ConnectionDirectoryFormState {
    /// 新目录的父目录 ID；为空表示创建在根层级。
    pub parent_id: Option<ConnectionNodeId>,
    /// 目录名称输入框。
    pub name_input: TextInputState,
    /// 最近一次校验错误。
    pub error_message: Option<String>,
}

/// 新增远程链接表单状态。
#[derive(Clone, Debug)]
pub(crate) struct ConnectionLinkFormState {
    /// 当前表单对应的链接协议。
    pub link_kind: ConnectionLinkKind,
    /// 新链接的父目录 ID；为空表示创建在根层级。
    pub parent_id: Option<ConnectionNodeId>,
    /// 链接名称输入框。
    pub name_input: TextInputState,
    /// SSH 主机输入框。
    pub host_input: TextInputState,
    /// Git/SVN 仓库 URL 输入框。
    pub url_input: TextInputState,
    /// SSH 端口输入框。
    pub port_input: TextInputState,
    /// SSH 用户名输入框。
    pub username_input: TextInputState,
    /// 复用的共用凭据 ID；为空表示仅此连接手动输入密码。
    pub credential_id: Option<ConnectionCredentialId>,
    /// 手动输入密码时是否同时保存为共用凭据。
    pub save_as_credential: bool,
    /// SSH 密码输入框。
    pub password_input: TextInputState,
    /// SMB 共享名称输入框。
    pub share_input: TextInputState,
    /// SMB 初始目录输入框。
    pub initial_dir_input: TextInputState,
    /// SMB 域或工作组输入框。
    pub domain_input: TextInputState,
    /// SSH 私钥路径输入框。
    pub private_key_path_input: TextInputState,
    /// SSH 私钥口令输入框。
    pub private_key_passphrase_input: TextInputState,
    /// 最近一次校验错误。
    pub error_message: Option<String>,
}

/// 共用凭据管理表单状态。
#[derive(Clone, Debug)]
pub(crate) struct ConnectionCredentialFormState {
    /// 当前编辑的凭据 ID；为空表示新建。
    pub selected_id: Option<ConnectionCredentialId>,
    /// 凭据名称输入框。
    pub name_input: TextInputState,
    /// 默认用户名输入框。
    pub username_input: TextInputState,
    /// 密码输入框；编辑已有凭据时留空表示不修改。
    pub password_input: TextInputState,
    /// SMB 域或工作组输入框。
    pub domain_input: TextInputState,
    /// 是否允许 SSH 使用。
    pub allow_ssh: bool,
    /// 是否允许 SMB 使用。
    pub allow_smb: bool,
    /// 是否允许 SVN 使用。
    pub allow_svn: bool,
    /// 是否作为同用户名默认凭据。
    pub is_default: bool,
    /// 新建凭据时优先勾选的协议，用于从具体链接表单打开管理器。
    pub preferred_protocol: ConnectionLinkKind,
    /// 最近一次校验错误。
    pub error_message: Option<String>,
}

impl Default for ConnectionCredentialFormState {
    /// 构造空凭据表单，默认允许 SSH。
    fn default() -> Self {
        Self {
            selected_id: None,
            name_input: TextInputState::default(),
            username_input: TextInputState::default(),
            password_input: TextInputState::default(),
            domain_input: TextInputState::default(),
            allow_ssh: true,
            allow_smb: false,
            allow_svn: false,
            is_default: false,
            preferred_protocol: ConnectionLinkKind::Ssh,
            error_message: None,
        }
    }
}

/// SSH 主机指纹确认弹窗状态。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum HostKeyPromptOwner {
    /// 终端会话触发的主机指纹确认。
    Terminal {
        /// 终端会话 ID。
        session_id: usize,
    },
    /// SFTP、Git SSH 或 SVN SSH 远程文件会话触发的主机指纹确认。
    RemoteFile {
        /// 远程文件管理会话 ID。
        session_id: usize,
    },
}

/// SSH 主机指纹确认弹窗状态。
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ConnectionHostKeyPromptState {
    /// 等待确认的会话 ID；具体类型由 `owner` 区分。
    pub session_id: usize,
    /// 触发确认的会话类型。
    pub owner: HostKeyPromptOwner,
    /// 关联链接节点 ID。
    pub link_id: ConnectionNodeId,
    /// 远程主机。
    pub host: String,
    /// 远程端口。
    pub port: u16,
    /// 待确认的 SHA256 指纹。
    pub fingerprint: String,
}

/// 删除链接节点二次确认弹窗状态。
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ConnectionDeletePromptState {
    /// 待删除的连接节点 ID。
    pub node_id: ConnectionNodeId,
    /// 待删除节点展示名称。
    pub label: String,
    /// 是否为目录；目录删除前会额外要求为空。
    pub is_directory: bool,
}

/// 远程文件管理内的应用弹窗。
#[derive(Clone, Debug)]
pub(crate) enum RemoteFileDialogState {
    /// 重命名远程文件或目录。
    Rename(RemoteFileRenameDialogState),
    /// 删除远程普通文件或空目录前的二次确认。
    ConfirmDelete(RemoteFileDeletePromptState),
    /// 仓库文件版本对比的历史版本选择。
    RepositoryDiff(RemoteFileDiffDialogState),
}

/// 可写远程文件后端共用的重命名弹窗状态。
#[derive(Clone, Debug)]
pub(crate) struct RemoteFileRenameDialogState {
    /// 远程文件管理会话 ID。
    pub session_id: usize,
    /// 原始远程路径。
    pub remote_path: String,
    /// 原始名称。
    pub original_name: String,
    /// 名称输入框。
    pub name_input: TextInputState,
    /// 最近一次校验错误。
    pub error_message: Option<String>,
}

/// 仓库文件版本对比弹窗状态。
#[derive(Clone, Debug)]
pub(crate) struct RemoteFileDiffDialogState {
    /// 远程文件管理会话 ID。
    pub session_id: usize,
    /// 当前文件远程路径。
    pub remote_path: String,
    /// 文件名，用于提示与 diff 窗口标题。
    pub file_name: String,
    /// 当前文件所在版本展示文本。
    pub current_version: String,
    /// 文件历史提交，按提交时间从新到旧排列。
    pub entries: Vec<RepositoryHistoryEntry>,
    /// 当前选择的对比版本稳定 ID。
    pub selected_target: Option<String>,
    /// 提交说明滚动句柄，用于自绘悬停滚动条。
    pub commit_scroll: ScrollHandle,
    /// 历史版本下拉菜单是否展开。
    pub is_versions_open: bool,
    /// 是否正在加载历史提交。
    pub is_loading_history: bool,
    /// 历史提交加载失败原因。
    pub history_error: Option<String>,
    /// 最近一次提交校验错误。
    pub error_message: Option<String>,
}

/// 单个远程文件预览窗口及其自动重试状态。
pub(crate) struct RemoteFilePreviewState {
    /// 当前预览请求 ID，用于丢弃已被新预览取代的迟到结果。
    pub request_id: u64,
    /// 当前预览的远程路径。
    pub remote_path: String,
    /// 当前预览文件名。
    pub file_name: String,
    /// 已自动重试次数。
    pub retry_count: u8,
    /// 是否仍在等待读取结果；手动重试时重新置为真。
    pub is_loading: bool,
    /// 复用并更新的独立预览窗口句柄。
    pub window: WindowHandle<FilePreviewWindow>,
}

/// 可写远程文件后端共用的删除二次确认弹窗状态。
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct RemoteFileDeletePromptState {
    /// 远程文件管理会话 ID。
    pub session_id: usize,
    /// 待删除远程路径。
    pub remote_path: String,
    /// 待删除文件或目录名称。
    pub name: String,
    /// 是否为目录。
    pub is_directory: bool,
}
