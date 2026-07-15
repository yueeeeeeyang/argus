//! 文件职责：提取远程连接和升级弹窗状态类型定义。
//! 创建日期：2026-07-08
//! 修改日期：2026-07-15
//! 作者：Argus 开发团队
//! 主要功能：定义连接目录表单、SSH/SMB/Git/SVN 链接表单、主机指纹确认和文件管理弹窗状态。

use super::types::TextInputState;
use crate::infra::updater::AvailableUpgrade;
use crate::remote::connection::{ConnectionLinkKind, ConnectionNodeId};

/// 链接工作区当前打开的弹窗。
#[derive(Clone, Debug)]
pub(crate) enum ConnectionDialogState {
    /// SSH 首次连接未知主机时的指纹确认弹窗。
    ConfirmHostKey(ConnectionHostKeyPromptState),
    /// 删除链接目录或任一协议链接前的二次确认弹窗。
    ConfirmDelete(ConnectionDeletePromptState),
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

/// 升级弹窗状态，覆盖发现版本、安装进度和失败提示三类用户可见流程。
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum UpgradeDialogState {
    /// 发现可安装版本，等待用户确认升级、跳过或稍后。
    Available {
        /// 待安装的新版本信息。
        upgrade: AvailableUpgrade,
    },
    /// 正在下载、校验、替换或重启。
    Progress {
        /// 正在处理的新版本号。
        version: String,
        /// 当前阶段说明。
        message: String,
    },
    /// 升级失败，等待用户关闭后继续使用旧版本。
    Failed {
        /// 失败关联版本；手动检查失败时可能没有版本号。
        version: Option<String>,
        /// 失败原因。
        message: String,
    },
}
