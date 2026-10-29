//! 文件职责：实现 SFTP、SMB、Git、SVN 通用远程文件管理标签、事件回收、文件操作和输入框交互。
//! 创建日期：2026-06-26
//! 修改日期：2026-10-29
//! 作者：Argus 开发团队
//! 主要功能：从 SSH/SMB/Git/SVN 链接打开通用文件管理，支持预览加载重试、仓库历史对比和协议能力约束。

use std::borrow::Borrow;
use std::path::PathBuf;
use std::time::Duration;

use gpui::{
    AppContext, Bounds, Context, Keystroke, PathPromptOptions, Pixels, Point, SharedString,
    TitlebarOptions, WindowBounds, WindowHandle, WindowOptions, point, px, size,
};

use crate::app::{
    AppTextInputTarget, ArgusApp, ArgusTab, ConnectionDialogState, ConnectionHostKeyPromptState,
    ConnectionSvnCertificatePromptState, FILE_PREVIEW_WINDOW_HEIGHT,
    FILE_PREVIEW_WINDOW_MIN_HEIGHT, FILE_PREVIEW_WINDOW_MIN_WIDTH, FILE_PREVIEW_WINDOW_WIDTH,
    HostKeyPromptOwner, InputTextSelectionDrag, RemoteFileDeletePromptState, RemoteFileDialogState,
    RemoteFileDiffDialogState, RemoteFilePreviewState, RemoteFileRenameDialogState, TabKind,
    TextInputState, Workspace,
};
use crate::infra::text_selection::{
    NativeTextEdit, TextSelectionGranularity, character_count, replace_character_range,
};
use crate::loader::PathBrowser;
use crate::remote::connection::ConnectionLinkConfig;
use crate::remote::remote_file::{
    FilePreviewContent, RemoteFileBackend, RemoteFileCommand, RemoteFileEntry, RemoteFileEntryKind,
    RemoteFileEvent, RemoteFileSessionState, RemoteFileStatus, RemoteFileWorkerBackend,
    RemoteFileWorkerRequest, is_remote_file_entry_previewable, remote_parent_dir,
    spawn_remote_file_worker, validate_remote_file_rename_name,
};
use crate::remote::svn_certificate::{SvnCertificateApproval, SvnCertificateDecision};
use crate::remote::svn_tls::SvnCertificateTrust;
use crate::remote::terminal::PendingHostKey;
use crate::ui::components::context_menu::{ActiveMenu, ActiveMenuKind};
use crate::ui::file_diff_window::{
    FILE_DIFF_WINDOW_HEIGHT, FILE_DIFF_WINDOW_MIN_HEIGHT, FILE_DIFF_WINDOW_MIN_WIDTH,
    FILE_DIFF_WINDOW_WIDTH, FileDiffWindow,
};
use crate::ui::file_preview_window::{FILE_PREVIEW_AUTO_RETRY_LIMIT, FilePreviewWindow};

#[cfg(test)]
#[path = "remote_file_diff_tests.rs"]
mod remote_file_diff_tests;

#[cfg(test)]
#[path = "svn_certificate_tests.rs"]
mod svn_certificate_tests;

/// 预览读取失败后自动重试前的等待时间，给瞬时网络抖动恢复机会。
const REMOTE_FILE_PREVIEW_RETRY_DELAY: Duration = Duration::from_millis(300);

/// 远程文件管理输入框的可变部件引用。
struct RemoteFileInputParts<'a> {
    /// 当前文本。
    value: &'a mut String,
    /// 当前光标字符位置。
    cursor: &'a mut usize,
    /// 当前选区锚点。
    selection_anchor: &'a mut Option<usize>,
    /// 当前输入法 marked text 范围。
    marked_range: &'a mut Option<std::ops::Range<usize>>,
    /// 当前鼠标拖拽选区状态。
    selection_drag: &'a mut Option<InputTextSelectionDrag>,
}

impl ArgusApp {
    /// 在终端正文指定窗口坐标打开右键菜单。
    pub(crate) fn open_terminal_context_menu(&mut self, session_id: usize, anchor: Point<Pixels>) {
        if !self.terminal_sessions.contains_key(&session_id) {
            self.placeholder_notice = "终端会话不存在".to_string();
            return;
        }
        self.tab_menu_scroll = gpui::UniformListScrollHandle::new();
        self.active_menu = Some(ActiveMenu {
            kind: ActiveMenuKind::TerminalContext { session_id },
            anchor,
        });
    }

    /// 在远程文件表格行指定窗口坐标打开右键菜单。
    pub(crate) fn open_remote_file_entry_context_menu(
        &mut self,
        session_id: usize,
        remote_path: String,
        anchor: Point<Pixels>,
    ) {
        let Some(session) = self.remote_file_sessions.get_mut(&session_id) else {
            self.placeholder_notice = "文件管理会话不存在".to_string();
            return;
        };
        if !session
            .entries
            .iter()
            .any(|entry| entry.path == remote_path)
        {
            self.placeholder_notice = "未找到远程文件".to_string();
            return;
        }
        if !session.selected_paths.contains(&remote_path) {
            session.selected_paths.clear();
            session.selected_paths.insert(remote_path);
        }
        self.tab_menu_scroll = gpui::UniformListScrollHandle::new();
        self.active_menu = Some(ActiveMenu {
            kind: ActiveMenuKind::RemoteFileEntry { session_id },
            anchor,
        });
    }

    /// 在指定窗口坐标打开 Git 分支/标签下拉菜单。
    pub(crate) fn open_repository_version_menu(
        &mut self,
        session_id: usize,
        anchor: Point<Pixels>,
    ) {
        let has_versions = self
            .remote_file_sessions
            .get(&session_id)
            .is_some_and(|session| !session.repository_versions.is_empty());
        if !has_versions {
            self.placeholder_notice = "仓库版本列表尚未加载".to_string();
            return;
        }
        self.tab_menu_scroll = gpui::UniformListScrollHandle::new();
        self.active_menu = Some(ActiveMenu {
            kind: ActiveMenuKind::RepositoryVersions { session_id },
            anchor,
        });
    }

    /// 从指定 SSH 终端打开一个新的 SFTP 文件管理标签页。
    pub(crate) fn open_sftp_file_manager_from_terminal(
        &mut self,
        terminal_session_id: usize,
        cx: &mut Context<Self>,
    ) {
        let Some(link_id) = self
            .terminal_sessions
            .get(&terminal_session_id)
            .map(|session| session.link_id)
        else {
            self.placeholder_notice = "当前终端无法打开文件管理".to_string();
            return;
        };
        self.create_remote_file_manager_session(link_id, RemoteFileBackend::Sftp, cx);
    }

    /// 从 SSH 链接树节点直接打开一个新的 SFTP 文件管理标签页。
    pub(crate) fn open_sftp_file_manager_from_link(
        &mut self,
        link_id: crate::remote::connection::ConnectionNodeId,
        cx: &mut Context<Self>,
    ) {
        self.create_remote_file_manager_session(link_id, RemoteFileBackend::Sftp, cx);
    }

    /// 从 SMB 链接树节点打开一个新的 SMB 文件管理标签页。
    pub(crate) fn open_smb_file_manager_from_link(
        &mut self,
        link_id: crate::remote::connection::ConnectionNodeId,
        cx: &mut Context<Self>,
    ) {
        self.create_remote_file_manager_session(link_id, RemoteFileBackend::Smb, cx);
    }

    /// 从 Git/SVN 链接打开只读文件管理；同一链接只保留一个活动会话并优先聚焦已有标签。
    pub(crate) fn open_repository_file_manager_from_link(
        &mut self,
        link_id: crate::remote::connection::ConnectionNodeId,
        backend: RemoteFileBackend,
        cx: &mut Context<Self>,
    ) {
        if !matches!(backend, RemoteFileBackend::Git | RemoteFileBackend::Svn) {
            self.placeholder_notice = "当前协议不是仓库链接".to_string();
            return;
        }
        let existing_session_id = self
            .remote_file_sessions
            .values()
            .find(|session| session.link_id == link_id && session.backend == backend)
            .map(|session| session.id);
        if let Some(session_id) = existing_session_id
            && let Some(tab) = self.tabs.iter().find(|tab| {
                matches!(tab.kind, TabKind::RemoteFileManager { session_id: id } if id == session_id)
            })
        {
            self.active_tab_id = Some(tab.id);
            self.workspace = Workspace::Connections;
            self.placeholder_notice = format!("已聚焦 {} 仓库文件管理", backend.label());
            return;
        }
        self.create_remote_file_manager_session(link_id, backend, cx);
    }

    /// 断开并移除指定链接关联的全部文件管理会话，供编辑或删除链接时清理旧配置快照。
    pub(crate) fn disconnect_remote_file_sessions_for_link(
        &mut self,
        link_id: crate::remote::connection::ConnectionNodeId,
    ) {
        let session_ids = self
            .remote_file_sessions
            .values()
            .filter(|session| session.link_id == link_id)
            .map(|session| session.id)
            .collect::<Vec<_>>();
        for session_id in session_ids {
            self.disconnect_remote_file_session(session_id);
        }
    }

    /// 断开并移除指定远程文件管理会话。
    pub(crate) fn disconnect_remote_file_session(&mut self, session_id: usize) {
        self.clear_remote_svn_certificate_request(session_id);
        if let Some(session) = self.remote_file_sessions.remove(&session_id)
            && let Some(sender) = session.command_sender
        {
            let _ = sender.send(RemoteFileCommand::Disconnect);
        }
        if let Some(dialog) = self.remote_file_dialog.as_ref()
            && remote_file_dialog_session_id(dialog) == Some(session_id)
        {
            self.remote_file_dialog = None;
        }
        self.open_next_svn_certificate_prompt();
    }

    /// 断开所有远程文件管理会话。
    pub(crate) fn disconnect_all_remote_file_sessions(&mut self) {
        let session_ids = self
            .remote_file_sessions
            .keys()
            .copied()
            .collect::<Vec<_>>();
        for session_id in session_ids {
            self.disconnect_remote_file_session(session_id);
        }
    }

    /// 用户确认当前远程文件会话的 SSH 主机指纹可信，并继续后台 worker。
    pub(crate) fn confirm_remote_file_host_key(&mut self, session_id: usize) {
        let Some((pending, sender, backend_label)) = self
            .remote_file_sessions
            .get(&session_id)
            .and_then(|session| {
                session.pending_host_key.clone().map(|pending| {
                    (
                        pending,
                        session.command_sender.clone(),
                        session.backend.label(),
                    )
                })
            })
        else {
            self.placeholder_notice = "文件管理会话不存在".to_string();
            return;
        };

        self.config
            .connections
            .trust_host_key(&pending.host, pending.port, &pending.fingerprint);
        self.persist_config_or_report();
        if let Some(sender) = sender {
            let _ = sender.send(RemoteFileCommand::TrustHostKey);
        }
        if let Some(session) = self.remote_file_sessions.get_mut(&session_id) {
            session.pending_host_key = None;
            session.status = RemoteFileStatus::Connecting;
            session.message = Some(format!("已信任主机指纹，继续建立 {backend_label} 连接..."));
        }
        self.connection_dialog = None;
        self.placeholder_notice = "已信任 SSH 主机指纹".to_string();
    }

    /// 用户拒绝当前远程文件会话的 SSH 主机指纹。
    pub(crate) fn reject_remote_file_host_key(&mut self, session_id: usize) {
        let Some(session) = self.remote_file_sessions.get_mut(&session_id) else {
            return;
        };
        if let Some(sender) = &session.command_sender {
            let _ = sender.send(RemoteFileCommand::RejectHostKey);
        }
        session.pending_host_key = None;
        session.status = RemoteFileStatus::Failed;
        session.message = Some("已拒绝信任 SSH 主机指纹".to_string());
        self.connection_dialog = None;
        self.placeholder_notice = "已拒绝 SSH 主机指纹".to_string();
    }

    /// 通过主机指纹弹窗 owner 分发确认动作。
    pub(crate) fn confirm_connection_host_key_prompt(
        &mut self,
        prompt: ConnectionHostKeyPromptState,
    ) {
        match prompt.owner {
            HostKeyPromptOwner::Terminal { session_id } => {
                self.confirm_terminal_host_key(session_id)
            }
            HostKeyPromptOwner::RemoteFile { session_id } => {
                self.confirm_remote_file_host_key(session_id)
            }
        }
        self.open_next_svn_certificate_prompt();
    }

    /// 通过主机指纹弹窗 owner 分发拒绝动作。
    pub(crate) fn reject_connection_host_key_prompt(
        &mut self,
        prompt: ConnectionHostKeyPromptState,
    ) {
        match prompt.owner {
            HostKeyPromptOwner::Terminal { session_id } => {
                self.reject_terminal_host_key(session_id)
            }
            HostKeyPromptOwner::RemoteFile { session_id } => {
                self.reject_remote_file_host_key(session_id)
            }
        }
        self.open_next_svn_certificate_prompt();
    }

    /// 加载远程文件管理地址栏中的目录。
    pub(crate) fn load_remote_file_address_directory(&mut self, session_id: usize) {
        let Some(path) = self
            .remote_file_sessions
            .get(&session_id)
            .map(|session| session.address_input.value.clone())
        else {
            self.placeholder_notice = "文件管理会话不存在".to_string();
            return;
        };
        self.send_remote_file_command(
            session_id,
            RemoteFileCommand::LoadDirectory(path),
            RemoteFileStatus::Loading,
            "正在读取远程目录...",
        );
    }

    /// 刷新当前远程目录。
    pub(crate) fn refresh_remote_file_directory(&mut self, session_id: usize) {
        self.send_remote_file_command(
            session_id,
            RemoteFileCommand::Refresh,
            RemoteFileStatus::Loading,
            "正在刷新远程目录...",
        );
    }

    /// 切换 Git 分支/标签或 SVN HEAD/数字修订号；成功后 worker 会自动回到仓库根目录。
    pub(crate) fn switch_repository_version(&mut self, session_id: usize, version_id: String) {
        let can_switch = self
            .remote_file_sessions
            .get(&session_id)
            .is_some_and(|session| session.capabilities.switch_version);
        if !can_switch {
            self.placeholder_notice = "当前文件协议不支持版本切换".to_string();
            return;
        }
        self.send_remote_file_command(
            session_id,
            RemoteFileCommand::SwitchRepositoryVersion { version_id },
            RemoteFileStatus::Loading,
            "正在切换仓库版本...",
        );
    }

    /// 进入当前目录的父级目录。
    pub(crate) fn open_remote_file_parent_directory(&mut self, session_id: usize) {
        let Some(parent) = self
            .remote_file_sessions
            .get(&session_id)
            .and_then(|session| remote_parent_dir(&session.current_dir))
        else {
            self.placeholder_notice = "当前目录没有可进入的父目录".to_string();
            return;
        };
        self.send_remote_file_command(
            session_id,
            RemoteFileCommand::LoadDirectory(parent),
            RemoteFileStatus::Loading,
            "正在进入上级目录...",
        );
    }

    /// 双击远程文件表格行；目录进入，可预览的普通文件打开预览。
    pub(crate) fn handle_remote_file_entry_double_click(
        &mut self,
        session_id: usize,
        path: String,
        cx: &mut Context<Self>,
    ) {
        let Some(entry) = self.remote_file_entry(session_id, &path).cloned() else {
            self.placeholder_notice = "未找到远程文件".to_string();
            return;
        };
        if entry.kind == RemoteFileEntryKind::Directory {
            self.send_remote_file_command(
                session_id,
                RemoteFileCommand::LoadDirectory(entry.path),
                RemoteFileStatus::Loading,
                "正在进入远程目录...",
            );
        } else if is_remote_file_entry_previewable(&entry) {
            self.request_remote_file_preview(session_id, entry.path, entry.size, cx);
        }
    }

    /// 打开仓库文件版本对比独立窗口。
    pub(crate) fn open_file_diff_window(
        &mut self,
        file_name: String,
        current_version: String,
        target_version: String,
        current_content: FilePreviewContent,
        target_content: FilePreviewContent,
        cx: &mut Context<Self>,
    ) {
        let initial_theme = self.theme.clone();
        let app_entity = cx.entity();
        let bounds = Bounds::centered(
            None,
            size(px(FILE_DIFF_WINDOW_WIDTH), px(FILE_DIFF_WINDOW_HEIGHT)),
            cx,
        );
        let window_options = WindowOptions {
            titlebar: Some(TitlebarOptions {
                title: None,
                appears_transparent: true,
                traffic_light_position: Some(point(px(20.0), px(14.0))),
            }),
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            window_min_size: Some(size(
                px(FILE_DIFF_WINDOW_MIN_WIDTH),
                px(FILE_DIFF_WINDOW_MIN_HEIGHT),
            )),
            ..Default::default()
        };
        match cx.open_window(window_options, move |_, cx| {
            cx.new(|cx| {
                FileDiffWindow::new(
                    app_entity,
                    initial_theme,
                    file_name,
                    current_version,
                    target_version,
                    current_content,
                    target_content,
                    cx,
                )
            })
        }) {
            Ok(_) => self.placeholder_notice = "已打开版本对比".to_string(),
            Err(error) => self.placeholder_notice = format!("打开版本对比失败：{error}"),
        }
    }

    /// 判断当前 SVN 会话是否允许对唯一选中的普通文件发起版本对比。
    pub(crate) fn can_compare_repository_file_selection(&self, session_id: usize) -> bool {
        let Some(session) = self.remote_file_sessions.get(&session_id) else {
            return false;
        };
        if !matches!(
            session.backend,
            RemoteFileBackend::Svn | RemoteFileBackend::Git
        ) || session.status != RemoteFileStatus::Connected
            || session.selected_paths.len() != 1
        {
            return false;
        }
        let Some(path) = session.selected_paths.iter().next() else {
            return false;
        };
        session
            .entries
            .iter()
            .find(|entry| entry.path == *path)
            .is_some_and(is_remote_file_entry_previewable)
    }

    /// 打开仓库文件版本对比弹窗，并向 worker 请求文件历史提交。
    pub(crate) fn open_repository_file_diff_dialog(&mut self, session_id: usize) {
        if !self.can_compare_repository_file_selection(session_id) {
            self.placeholder_notice = "请选择一个 SVN 普通文件进行版本对比".to_string();
            return;
        }
        let Some(session) = self.remote_file_sessions.get(&session_id) else {
            self.placeholder_notice = "文件管理会话不存在".to_string();
            return;
        };
        let Some(path) = session.selected_paths.iter().next().cloned() else {
            return;
        };
        let Some(entry) = session.entries.iter().find(|entry| entry.path == path) else {
            return;
        };
        let current_version = session
            .selected_repository_version
            .as_ref()
            .and_then(|id| {
                session
                    .repository_versions
                    .iter()
                    .find(|version| &version.id == id)
                    .map(|version| version.label.clone())
            })
            .or_else(|| {
                let value = session.version_input.value.trim();
                (!value.is_empty()).then(|| value.to_string())
            })
            .unwrap_or_else(|| "HEAD".to_string());
        let remote_path = entry.path.clone();
        let file_name = entry.name.clone();
        self.remote_file_dialog = Some(RemoteFileDialogState::RepositoryDiff(
            RemoteFileDiffDialogState {
                session_id,
                remote_path: remote_path.clone(),
                file_name: file_name.clone(),
                current_version,
                entries: Vec::new(),
                selected_target: None,
                commit_scroll: gpui::ScrollHandle::new(),
                is_versions_open: false,
                is_loading_history: true,
                history_error: None,
                error_message: None,
            },
        ));
        if !self.send_remote_file_command(
            session_id,
            RemoteFileCommand::LoadFileHistory { remote_path },
            RemoteFileStatus::Loading,
            "正在加载 SVN 文件历史...",
        ) {
            if let Some(RemoteFileDialogState::RepositoryDiff(dialog)) =
                self.remote_file_dialog.as_mut()
            {
                dialog.is_loading_history = false;
                dialog.history_error = Some("无法加载 SVN 提交历史".to_string());
            }
            return;
        }
        self.placeholder_notice = format!("正在加载 {file_name} 的提交历史");
    }

    /// 重新加载当前对比弹窗的 SVN 文件历史。
    pub(crate) fn reload_repository_file_history(&mut self) {
        let Some(RemoteFileDialogState::RepositoryDiff(dialog)) = self.remote_file_dialog.as_mut()
        else {
            return;
        };
        let session_id = dialog.session_id;
        let remote_path = dialog.remote_path.clone();
        dialog.entries.clear();
        dialog.selected_target = None;
        dialog.commit_scroll.set_offset(point(px(0.0), px(0.0)));
        dialog.is_versions_open = false;
        dialog.is_loading_history = true;
        dialog.history_error = None;
        dialog.error_message = None;
        if !self.send_remote_file_command(
            session_id,
            RemoteFileCommand::LoadFileHistory { remote_path },
            RemoteFileStatus::Loading,
            "正在重新加载 SVN 文件历史...",
        ) {
            if let Some(RemoteFileDialogState::RepositoryDiff(dialog)) =
                self.remote_file_dialog.as_mut()
            {
                dialog.is_loading_history = false;
                dialog.history_error = Some("无法重新加载 SVN 提交历史".to_string());
            }
            return;
        }
        self.placeholder_notice = "正在重新加载提交历史".to_string();
    }

    /// 展开或收起对比弹窗的历史版本下拉菜单。
    pub(crate) fn toggle_repository_diff_versions(&mut self) {
        let Some(RemoteFileDialogState::RepositoryDiff(dialog)) = self.remote_file_dialog.as_mut()
        else {
            return;
        };
        if dialog.is_loading_history || dialog.history_error.is_some() {
            return;
        }
        dialog.is_versions_open = !dialog.is_versions_open;
        dialog.error_message = None;
    }

    /// 在对比弹窗中选择一个历史版本。
    pub(crate) fn select_repository_diff_target(&mut self, target_id: String) {
        let Some(RemoteFileDialogState::RepositoryDiff(dialog)) = self.remote_file_dialog.as_mut()
        else {
            return;
        };
        if !dialog
            .entries
            .iter()
            .any(|entry| entry.target_id == target_id)
        {
            return;
        }
        dialog.selected_target = Some(target_id);
        dialog.commit_scroll.set_offset(point(px(0.0), px(0.0)));
        dialog.is_versions_open = false;
        dialog.error_message = None;
    }

    /// 提交版本对比弹窗，发送读取当前版本与目标版本的命令。
    fn submit_repository_diff_dialog(&mut self, mut dialog: RemoteFileDiffDialogState) {
        if dialog.is_loading_history {
            let message = "正在加载历史版本，请稍候".to_string();
            dialog.error_message = Some(message.clone());
            self.remote_file_dialog = Some(RemoteFileDialogState::RepositoryDiff(dialog));
            self.placeholder_notice = message;
            return;
        }
        if let Some(message) = dialog.history_error.clone() {
            dialog.error_message = Some(message.clone());
            self.remote_file_dialog = Some(RemoteFileDialogState::RepositoryDiff(dialog));
            self.placeholder_notice = message;
            return;
        }
        let Some(target_version) = dialog.selected_target.clone() else {
            let message = "请选择对比版本".to_string();
            dialog.error_message = Some(message.clone());
            self.remote_file_dialog = Some(RemoteFileDialogState::RepositoryDiff(dialog));
            self.placeholder_notice = message;
            return;
        };
        let Some(target_label) = dialog
            .entries
            .iter()
            .find(|entry| entry.target_id == target_version)
            .map(|entry| entry.label.clone())
        else {
            let message = "所选对比版本已不在历史列表中".to_string();
            dialog.error_message = Some(message.clone());
            self.remote_file_dialog = Some(RemoteFileDialogState::RepositoryDiff(dialog));
            self.placeholder_notice = message;
            return;
        };
        let session_id = dialog.session_id;
        let remote_path = dialog.remote_path;
        self.remote_file_dialog = None;
        self.send_remote_file_command(
            session_id,
            RemoteFileCommand::CompareFileVersions {
                remote_path,
                target_version,
                target_label,
            },
            RemoteFileStatus::Transferring,
            "正在读取对比版本...",
        );
    }

    /// 请求读取远程普通文件内容用于预览；先打开带加载动画的预览窗口，再发送读取命令。
    pub(crate) fn request_remote_file_preview(
        &mut self,
        session_id: usize,
        remote_path: String,
        size: Option<u64>,
        cx: &mut Context<Self>,
    ) {
        if let Some(size) = size
            && size > crate::remote::remote_file::REMOTE_FILE_PREVIEW_MAX_FILE_SIZE
        {
            self.placeholder_notice = "文件过大，无法预览".to_string();
            return;
        }
        self.start_remote_file_preview(session_id, remote_path, cx);
    }

    /// 打开或复用预览窗口，并发送首个读取命令。
    fn start_remote_file_preview(
        &mut self,
        session_id: usize,
        remote_path: String,
        cx: &mut Context<Self>,
    ) {
        let Some(session) = self.remote_file_sessions.get(&session_id) else {
            self.placeholder_notice = "文件管理会话不存在".to_string();
            return;
        };
        if session.status != RemoteFileStatus::Connected {
            self.placeholder_notice = "文件管理会话尚未连接，暂不能预览".to_string();
            return;
        }
        if !session.capabilities.preview {
            self.placeholder_notice = "当前文件协议不支持预览".to_string();
            return;
        }
        let Some(entry) = session
            .entries
            .iter()
            .find(|entry| entry.path == remote_path)
        else {
            self.placeholder_notice = "未找到远程文件".to_string();
            return;
        };
        let file_name = entry.name.clone();
        let request_id = self.next_remote_file_preview_request_id;
        self.next_remote_file_preview_request_id =
            self.next_remote_file_preview_request_id.saturating_add(1);
        let window = if let Some(state) = self.remote_file_previews.get(&session_id) {
            let existing = state.window;
            match existing.update(cx, |view, window, cx| {
                view.show_loading(file_name.clone(), 0, cx);
                window.activate_window();
            }) {
                Ok(()) => existing,
                Err(_) => {
                    self.remote_file_previews.remove(&session_id);
                    let Some(window) =
                        self.open_remote_file_preview_window(session_id, file_name.clone(), cx)
                    else {
                        return;
                    };
                    window
                }
            }
        } else {
            let Some(window) =
                self.open_remote_file_preview_window(session_id, file_name.clone(), cx)
            else {
                return;
            };
            window
        };
        self.remote_file_previews.insert(
            session_id,
            RemoteFilePreviewState {
                request_id,
                remote_path: remote_path.clone(),
                file_name: file_name.clone(),
                retry_count: 0,
                is_loading: true,
                window,
            },
        );
        if !self.send_remote_file_command(
            session_id,
            RemoteFileCommand::ReadFileContent {
                remote_path,
                request_id,
            },
            RemoteFileStatus::Transferring,
            "正在读取文件...",
        ) {
            if let Some(state) = self.remote_file_previews.get_mut(&session_id) {
                state.is_loading = false;
            }
            let _ = window.update(cx, |view, _, cx| {
                view.show_error("无法发起文件预览请求，请检查连接".to_string(), cx)
            });
            return;
        }
        self.placeholder_notice = format!("正在读取 {file_name}");
    }

    /// 打开独立的预览窗口，初始状态为加载中。
    fn open_remote_file_preview_window(
        &mut self,
        session_id: usize,
        file_name: String,
        cx: &mut Context<Self>,
    ) -> Option<WindowHandle<FilePreviewWindow>> {
        let initial_theme = self.theme.clone();
        let app_entity = cx.entity();
        let bounds = Bounds::centered(
            None,
            size(
                px(FILE_PREVIEW_WINDOW_WIDTH),
                px(FILE_PREVIEW_WINDOW_HEIGHT),
            ),
            cx,
        );
        let window_options = WindowOptions {
            titlebar: Some(TitlebarOptions {
                title: None,
                appears_transparent: true,
                traffic_light_position: Some(point(px(20.0), px(14.0))),
            }),
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            window_min_size: Some(size(
                px(FILE_PREVIEW_WINDOW_MIN_WIDTH),
                px(FILE_PREVIEW_WINDOW_MIN_HEIGHT),
            )),
            ..Default::default()
        };
        match cx.open_window(window_options, move |_, cx| {
            cx.new(|cx| {
                FilePreviewWindow::new_loading(app_entity, initial_theme, file_name, session_id, cx)
            })
        }) {
            Ok(window) => Some(window),
            Err(error) => {
                self.placeholder_notice = format!("打开文件预览失败：{error}");
                None
            }
        }
    }

    /// 应用预览读取结果：成功显示内容，失败先自动重试，用尽次数后展示手动重试按钮。
    pub(crate) fn apply_remote_file_preview_result(
        &mut self,
        session_id: usize,
        request_id: u64,
        file_name: String,
        content: FilePreviewContent,
        cx: &mut Context<Self>,
    ) {
        let Some(state) = self.remote_file_previews.get(&session_id) else {
            return;
        };
        if state.request_id != request_id || !state.is_loading {
            return;
        }
        let window = state.window;
        let retry_count = state.retry_count;
        let request_path = state.remote_path.clone();

        if let FilePreviewContent::Error(message) = content {
            if retry_count < FILE_PREVIEW_AUTO_RETRY_LIMIT {
                let next_retry = retry_count + 1;
                if let Some(state) = self.remote_file_previews.get_mut(&session_id) {
                    state.retry_count = next_retry;
                }
                if window
                    .update(cx, |view, _, cx| {
                        view.show_loading(file_name.clone(), next_retry, cx)
                    })
                    .is_err()
                {
                    self.remote_file_previews.remove(&session_id);
                    if let Some(session) = self.remote_file_sessions.get_mut(&session_id) {
                        session.status = RemoteFileStatus::Connected;
                        session.message = Some("预览窗口已关闭".to_string());
                    }
                    return;
                }
                if let Some(session) = self.remote_file_sessions.get_mut(&session_id) {
                    session.status = RemoteFileStatus::Transferring;
                    session.message = Some(format!(
                        "文件读取失败，正在自动重试 {next_retry}/{FILE_PREVIEW_AUTO_RETRY_LIMIT}"
                    ));
                }
                self.placeholder_notice = format!(
                    "读取 {file_name} 失败，正在自动重试 {next_retry}/{FILE_PREVIEW_AUTO_RETRY_LIMIT}"
                );
                self.schedule_remote_file_preview_retry(session_id, request_id, cx);
            } else {
                if let Some(state) = self.remote_file_previews.get_mut(&session_id) {
                    state.is_loading = false;
                }
                let updated = window.update(cx, |view, window, cx| {
                    view.show_error(message.clone(), cx);
                    window.activate_window();
                });
                if updated.is_err() {
                    self.remote_file_previews.remove(&session_id);
                    if let Some(session) = self.remote_file_sessions.get_mut(&session_id) {
                        session.status = RemoteFileStatus::Connected;
                        session.message = Some("预览窗口已关闭".to_string());
                    }
                    return;
                }
                if let Some(session) = self.remote_file_sessions.get_mut(&session_id) {
                    session.status = RemoteFileStatus::Connected;
                    session.message = Some(format!("文件预览失败：{file_name}"));
                }
                self.placeholder_notice = format!("{file_name} 预览失败：{message}");
            }
            return;
        }

        self.remote_file_previews.remove(&session_id);
        let updated = window.update(cx, |view, window, cx| {
            view.show_content(file_name.clone(), content, cx);
            window.activate_window();
        });
        if updated.is_err() {
            self.placeholder_notice = "预览窗口已关闭".to_string();
        }
        if let Some(session) = self.remote_file_sessions.get_mut(&session_id) {
            session.status = RemoteFileStatus::Connected;
            session.message = Some(format!("已加载预览：{file_name}"));
        }
        self.placeholder_notice = format!("已加载预览：{file_name}（{request_path}）");
    }

    /// 延迟后重发同一预览请求；同一请求 ID 保证不会串到新预览。
    fn schedule_remote_file_preview_retry(
        &mut self,
        session_id: usize,
        request_id: u64,
        cx: &mut Context<Self>,
    ) {
        cx.spawn(async move |view, cx| {
            cx.background_executor()
                .timer(REMOTE_FILE_PREVIEW_RETRY_DELAY)
                .await;
            let _ = view.update(cx, |app, cx| {
                let Some(state) = app.remote_file_previews.get(&session_id) else {
                    return;
                };
                if state.request_id != request_id || !state.is_loading {
                    return;
                }
                let window = state.window;
                let remote_path = state.remote_path.clone();
                if window.update(cx, |_, _, _| ()).is_err() {
                    app.remote_file_previews.remove(&session_id);
                    if let Some(session) = app.remote_file_sessions.get_mut(&session_id) {
                        session.status = RemoteFileStatus::Connected;
                        session.message = Some("预览窗口已关闭".to_string());
                    }
                    return;
                }
                if let Some(session) = app.remote_file_sessions.get_mut(&session_id) {
                    session.status = RemoteFileStatus::Connected;
                }
                if !app.send_remote_file_command(
                    session_id,
                    RemoteFileCommand::ReadFileContent {
                        remote_path,
                        request_id,
                    },
                    RemoteFileStatus::Transferring,
                    "正在重新读取文件...",
                ) {
                    if let Some(state) = app.remote_file_previews.get_mut(&session_id) {
                        state.is_loading = false;
                    }
                    let _ = window.update(cx, |view, _, cx| {
                        view.show_error("无法重新发起预览读取，请检查连接".to_string(), cx)
                    });
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// 连接断开或失败时终止仍处于加载状态的预览，并保留错误提示。
    fn fail_pending_remote_file_preview(
        &mut self,
        session_id: usize,
        message: String,
        cx: &mut Context<Self>,
    ) {
        let Some(state) = self.remote_file_previews.get(&session_id) else {
            return;
        };
        if !state.is_loading {
            return;
        }
        let window = state.window;
        if let Some(state) = self.remote_file_previews.get_mut(&session_id) {
            state.is_loading = false;
        }
        let _ = window.update(cx, |view, _, cx| view.show_error(message, cx));
    }

    /// 预览窗口手动重试按钮：重置自动重试计数并立即重新读取。
    pub(crate) fn retry_remote_file_preview(&mut self, session_id: usize, cx: &mut Context<Self>) {
        if !self
            .remote_file_sessions
            .get(&session_id)
            .is_some_and(|session| {
                session.status == RemoteFileStatus::Connected && session.command_sender.is_some()
            })
        {
            self.placeholder_notice = "文件管理连接不可用，请重新连接后重试".to_string();
            return;
        }
        let Some((request_id, remote_path, file_name, window)) =
            self.remote_file_previews.get(&session_id).map(|state| {
                (
                    state.request_id,
                    state.remote_path.clone(),
                    state.file_name.clone(),
                    state.window,
                )
            })
        else {
            return;
        };
        if let Some(state) = self.remote_file_previews.get_mut(&session_id) {
            state.retry_count = 0;
            state.is_loading = true;
        }
        if window
            .update(cx, |view, window, cx| {
                view.show_loading(file_name, 0, cx);
                window.activate_window();
            })
            .is_err()
        {
            self.remote_file_previews.remove(&session_id);
            if let Some(session) = self.remote_file_sessions.get_mut(&session_id) {
                session.status = RemoteFileStatus::Connected;
                session.message = Some("预览窗口已关闭".to_string());
            }
            self.placeholder_notice = "预览窗口已关闭，请重新打开预览".to_string();
            return;
        }
        if let Some(session) = self.remote_file_sessions.get_mut(&session_id) {
            session.status = RemoteFileStatus::Connected;
        }
        if !self.send_remote_file_command(
            session_id,
            RemoteFileCommand::ReadFileContent {
                remote_path,
                request_id,
            },
            RemoteFileStatus::Transferring,
            "正在重新读取文件...",
        ) {
            if let Some(state) = self.remote_file_previews.get_mut(&session_id) {
                state.is_loading = false;
            }
            let _ = window.update(cx, |view, _, cx| {
                view.show_error("无法重新发起预览读取，请检查连接".to_string(), cx)
            });
        }
    }

    /// 右键菜单触发：对当前唯一选中的可预览普通文件发起预览。
    pub(crate) fn preview_remote_file_selection(
        &mut self,
        session_id: usize,
        cx: &mut Context<Self>,
    ) {
        let Some(session) = self.remote_file_sessions.get(&session_id) else {
            self.placeholder_notice = "文件管理会话不存在".to_string();
            return;
        };
        if session.selected_paths.len() != 1 {
            self.placeholder_notice = "请选择单个文件预览".to_string();
            return;
        }
        let path = session.selected_paths.iter().next().cloned();
        let Some(path) = path else {
            return;
        };
        let Some(entry) = self.remote_file_entry(session_id, &path).cloned() else {
            self.placeholder_notice = "未找到远程文件".to_string();
            return;
        };
        if !is_remote_file_entry_previewable(&entry) {
            self.placeholder_notice = "该文件不支持预览".to_string();
            return;
        }
        self.request_remote_file_preview(session_id, entry.path, entry.size, cx);
    }

    /// 设置远程文件列表当前选中项。
    pub(crate) fn select_remote_file_entry(
        &mut self,
        session_id: usize,
        path: String,
        extend: bool,
    ) {
        let Some(session) = self.remote_file_sessions.get_mut(&session_id) else {
            return;
        };
        if extend {
            if !session.selected_paths.insert(path.clone()) {
                session.selected_paths.remove(&path);
            }
        } else {
            session.selected_paths.clear();
            session.selected_paths.insert(path);
        }
    }

    /// 切换远程文件列表排序字段与方向；同列点击翻转方向，异列点击切到该列升序。
    pub(crate) fn set_remote_file_sort(
        &mut self,
        session_id: usize,
        field: crate::remote::remote_file::RemoteFileSortField,
    ) {
        let Some(session) = self.remote_file_sessions.get_mut(&session_id) else {
            return;
        };
        if session.sort_field == field {
            session.sort_direction = match session.sort_direction {
                crate::remote::remote_file::RemoteFileSortDirection::Asc => {
                    crate::remote::remote_file::RemoteFileSortDirection::Desc
                }
                crate::remote::remote_file::RemoteFileSortDirection::Desc => {
                    crate::remote::remote_file::RemoteFileSortDirection::Asc
                }
            };
        } else {
            session.sort_field = field;
            session.sort_direction = crate::remote::remote_file::RemoteFileSortDirection::Asc;
        }
        session.rebuild_sorted_entries();
    }

    /// 打开本地文件选择器，并把选中的普通文件上传到当前远程目录。
    pub(crate) fn choose_remote_file_upload_files(
        &mut self,
        session_id: usize,
        cx: &mut Context<Self>,
    ) {
        let Some(session) = self.remote_file_sessions.get(&session_id) else {
            self.placeholder_notice = "文件管理会话不存在".to_string();
            return;
        };
        if !session.capabilities.upload {
            self.placeholder_notice = format!("{} 仓库为只读，不能上传", session.backend.label());
            return;
        }
        let receiver = {
            let app_context: &gpui::App = (*cx).borrow();
            app_context.prompt_for_paths(PathPromptOptions {
                files: true,
                directories: false,
                multiple: true,
                prompt: Some(SharedString::from("选择要上传的文件")),
            })
        };
        cx.spawn(async move |view, cx| {
            if let Ok(Ok(Some(paths))) = receiver.await {
                let _ = view.update(cx, |app, cx| {
                    app.upload_remote_files(session_id, paths);
                    cx.notify();
                });
            }
        })
        .detach();
    }

    /// 发送上传文件命令。
    pub(crate) fn upload_remote_files(&mut self, session_id: usize, paths: Vec<PathBuf>) {
        if paths.is_empty() {
            self.placeholder_notice = "未选择要上传的文件".to_string();
            return;
        }
        self.send_remote_file_command(
            session_id,
            RemoteFileCommand::UploadFiles { local_paths: paths },
            RemoteFileStatus::Transferring,
            "正在上传文件...",
        );
    }

    /// 打开本地路径选择器，并下载当前选中的远程普通文件。
    pub(crate) fn choose_remote_file_download_target(
        &mut self,
        session_id: usize,
        cx: &mut Context<Self>,
    ) {
        if !self
            .remote_file_sessions
            .get(&session_id)
            .is_some_and(|session| session.capabilities.download)
        {
            self.placeholder_notice = "当前文件协议不允许下载".to_string();
            return;
        }
        let selected = self.selected_remote_file_entries(session_id);
        if selected.is_empty() {
            self.placeholder_notice = "请选择要下载的文件".to_string();
            return;
        }
        if let Some(entry) = selected.iter().find(|entry| !entry.kind.is_regular_file()) {
            self.placeholder_notice = format!("仅支持下载普通文件：{}", entry.name);
            return;
        }

        if selected.len() == 1 {
            let entry = selected[0].clone();
            let default_dir = PathBrowser::default_start_directory();
            let receiver = {
                let app_context: &gpui::App = (*cx).borrow();
                app_context.prompt_for_new_path(&default_dir, Some(&entry.name))
            };
            cx.spawn(async move |view, cx| {
                if let Ok(Ok(Some(local_path))) = receiver.await {
                    let _ = view.update(cx, |app, cx| {
                        app.download_remote_file(session_id, entry.path.clone(), local_path);
                        cx.notify();
                    });
                }
            })
            .detach();
        } else {
            let entries = selected;
            let receiver = {
                let app_context: &gpui::App = (*cx).borrow();
                app_context.prompt_for_paths(PathPromptOptions {
                    files: false,
                    directories: true,
                    multiple: false,
                    prompt: Some(SharedString::from("选择下载保存目录")),
                })
            };
            cx.spawn(async move |view, cx| {
                if let Ok(Ok(Some(paths))) = receiver.await
                    && let Some(local_dir) = paths.into_iter().next()
                {
                    let _ = view.update(cx, |app, cx| {
                        app.download_remote_files(session_id, entries.clone(), local_dir);
                        cx.notify();
                    });
                }
            })
            .detach();
        }
    }

    /// 发送下载单文件命令。
    pub(crate) fn download_remote_file(
        &mut self,
        session_id: usize,
        remote_path: String,
        local_path: PathBuf,
    ) {
        self.send_remote_file_command(
            session_id,
            RemoteFileCommand::DownloadFile {
                remote_path,
                local_path,
            },
            RemoteFileStatus::Transferring,
            "正在下载文件...",
        );
    }

    /// 发送下载多文件命令。
    pub(crate) fn download_remote_files(
        &mut self,
        session_id: usize,
        entries: Vec<RemoteFileEntry>,
        local_dir: PathBuf,
    ) {
        self.send_remote_file_command(
            session_id,
            RemoteFileCommand::DownloadFiles { entries, local_dir },
            RemoteFileStatus::Transferring,
            "正在下载文件...",
        );
    }

    /// 打开可写远程文件后端共用的重命名弹窗。
    pub(crate) fn open_remote_file_rename_dialog(&mut self, session_id: usize) {
        if !self
            .remote_file_sessions
            .get(&session_id)
            .is_some_and(|session| session.capabilities.rename)
        {
            self.placeholder_notice = "当前仓库为只读，不能重命名".to_string();
            return;
        }
        let selected = self.selected_remote_file_entries(session_id);
        if selected.len() != 1 {
            self.placeholder_notice = "请选择一个文件或目录进行重命名".to_string();
            return;
        }
        let entry = selected[0].clone();
        self.remote_file_dialog =
            Some(RemoteFileDialogState::Rename(RemoteFileRenameDialogState {
                session_id,
                remote_path: entry.path,
                original_name: entry.name.clone(),
                name_input: TextInputState::from_value(entry.name),
                error_message: None,
            }));
    }

    /// 请求删除当前选中的远程普通文件或空目录。
    pub(crate) fn request_delete_remote_file_entry(&mut self, session_id: usize) {
        if !self
            .remote_file_sessions
            .get(&session_id)
            .is_some_and(|session| session.capabilities.delete)
        {
            self.placeholder_notice = "当前仓库为只读，不能删除".to_string();
            return;
        }
        let selected = self.selected_remote_file_entries(session_id);
        if selected.len() != 1 {
            self.placeholder_notice = "请选择一个文件或空目录进行删除".to_string();
            return;
        }
        let entry = selected[0].clone();
        if !matches!(
            entry.kind,
            RemoteFileEntryKind::RegularFile | RemoteFileEntryKind::Directory
        ) {
            self.placeholder_notice = format!("仅支持删除普通文件或空目录：{}", entry.name);
            return;
        }
        self.remote_file_dialog = Some(RemoteFileDialogState::ConfirmDelete(
            RemoteFileDeletePromptState {
                session_id,
                remote_path: entry.path,
                name: entry.name,
                is_directory: entry.kind == RemoteFileEntryKind::Directory,
            },
        ));
    }

    /// 关闭当前远程文件管理弹窗。
    pub(crate) fn close_remote_file_dialog(&mut self) {
        self.remote_file_dialog = None;
        self.placeholder_notice = "已关闭文件管理弹窗".to_string();
    }

    /// 提交当前远程文件管理弹窗。
    pub(crate) fn submit_remote_file_dialog(&mut self) {
        match self.remote_file_dialog.clone() {
            Some(RemoteFileDialogState::Rename(dialog)) => self.submit_remote_file_rename(dialog),
            Some(RemoteFileDialogState::ConfirmDelete(prompt)) => {
                self.confirm_delete_remote_file_entry(prompt)
            }
            Some(RemoteFileDialogState::RepositoryDiff(dialog)) => {
                self.submit_repository_diff_dialog(dialog)
            }
            None => {}
        }
    }

    /// 确认删除远程普通文件或空目录。
    pub(crate) fn confirm_delete_remote_file_entry(&mut self, prompt: RemoteFileDeletePromptState) {
        let Some(entry) = self
            .remote_file_sessions
            .get(&prompt.session_id)
            .and_then(|session| {
                session
                    .entries
                    .iter()
                    .find(|entry| entry.path == prompt.remote_path)
                    .cloned()
            })
        else {
            self.placeholder_notice = "待删除文件不存在".to_string();
            self.remote_file_dialog = None;
            return;
        };
        self.remote_file_dialog = None;
        self.send_remote_file_command(
            prompt.session_id,
            RemoteFileCommand::Delete { entry },
            RemoteFileStatus::Transferring,
            "正在删除远程文件...",
        );
    }

    /// 返回远程文件输入框选区。
    pub(crate) fn remote_file_input_selection_range(
        &self,
        target: AppTextInputTarget,
    ) -> Option<std::ops::Range<usize>> {
        let input = self.remote_file_text_input(target)?;
        normalized_input_selection_range(input)
    }

    /// 返回指定远程文件输入框状态。
    pub(crate) fn remote_file_text_input(
        &self,
        target: AppTextInputTarget,
    ) -> Option<&TextInputState> {
        match target {
            AppTextInputTarget::RemoteFileAddress { session_id } => self
                .remote_file_sessions
                .get(&session_id)
                .map(|session| &session.address_input),
            AppTextInputTarget::RemoteFileVersion { session_id } => self
                .remote_file_sessions
                .get(&session_id)
                .map(|session| &session.version_input),
            AppTextInputTarget::RemoteFileRenameName => match self.remote_file_dialog.as_ref()? {
                RemoteFileDialogState::Rename(dialog) => Some(&dialog.name_input),
                RemoteFileDialogState::ConfirmDelete(_)
                | RemoteFileDialogState::RepositoryDiff(_) => None,
            },
            _ => None,
        }
    }

    /// 返回指定远程文件输入框可变状态。
    pub(crate) fn remote_file_text_input_mut(
        &mut self,
        target: AppTextInputTarget,
    ) -> Option<&mut TextInputState> {
        match target {
            AppTextInputTarget::RemoteFileAddress { session_id } => self
                .remote_file_sessions
                .get_mut(&session_id)
                .map(|session| &mut session.address_input),
            AppTextInputTarget::RemoteFileVersion { session_id } => self
                .remote_file_sessions
                .get_mut(&session_id)
                .map(|session| &mut session.version_input),
            AppTextInputTarget::RemoteFileRenameName => match self.remote_file_dialog.as_mut()? {
                RemoteFileDialogState::Rename(dialog) => Some(&mut dialog.name_input),
                RemoteFileDialogState::ConfirmDelete(_)
                | RemoteFileDialogState::RepositoryDiff(_) => None,
            },
            _ => None,
        }
    }

    /// 聚焦远程文件相关输入框，并清理其他远程文件输入焦点。
    pub(crate) fn focus_remote_file_text_input_target(&mut self, target: AppTextInputTarget) {
        self.clear_remote_file_text_input_focuses();
        if let Some(input) = self.remote_file_text_input_mut(target) {
            input.is_focused = true;
            input.cursor = character_count(&input.value);
            input.selection_anchor = None;
            input.marked_range = None;
            input.selection_drag = None;
        }
    }

    /// 清理远程文件地址栏和弹窗输入框焦点。
    pub(crate) fn clear_remote_file_text_input_focuses(&mut self) {
        for session in self.remote_file_sessions.values_mut() {
            clear_remote_file_input_focus(&mut session.address_input);
            clear_remote_file_input_focus(&mut session.version_input);
        }
        if let Some(RemoteFileDialogState::Rename(dialog)) = self.remote_file_dialog.as_mut() {
            clear_remote_file_input_focus(&mut dialog.name_input);
        }
    }

    /// 处理远程文件地址栏或重命名输入框按键。
    pub(crate) fn handle_remote_file_text_input_key(
        &mut self,
        target: AppTextInputTarget,
        keystroke: &Keystroke,
    ) {
        match keystroke.key.as_str() {
            "escape" => {
                if target == AppTextInputTarget::RemoteFileRenameName {
                    self.close_remote_file_dialog();
                } else if let Some(input) = self.remote_file_text_input_mut(target) {
                    input.is_focused = false;
                    input.selection_anchor = None;
                    input.marked_range = None;
                    input.selection_drag = None;
                }
            }
            "enter" => match target {
                AppTextInputTarget::RemoteFileAddress { session_id } => {
                    self.load_remote_file_address_directory(session_id);
                }
                AppTextInputTarget::RemoteFileVersion { session_id } => {
                    let version_id = self
                        .remote_file_sessions
                        .get(&session_id)
                        .map(|session| session.version_input.value.clone());
                    if let Some(version_id) = version_id {
                        self.switch_repository_version(session_id, version_id);
                    }
                }
                AppTextInputTarget::RemoteFileRenameName => self.submit_remote_file_dialog(),
                _ => {}
            },
            "backspace" => self.delete_remote_file_input_backward(target),
            "delete" => self.delete_remote_file_input_forward(target),
            "left" | "arrowleft" => {
                self.move_remote_file_input_left(target, keystroke.modifiers.shift)
            }
            "right" | "arrowright" => {
                self.move_remote_file_input_right(target, keystroke.modifiers.shift)
            }
            "home" => self.move_remote_file_input_cursor(target, 0, keystroke.modifiers.shift),
            "end" => {
                let end = self
                    .remote_file_text_input(target)
                    .map(|input| character_count(&input.value))
                    .unwrap_or_default();
                self.move_remote_file_input_cursor(target, end, keystroke.modifiers.shift);
            }
            _ => {
                if let Some(key_char) = keystroke.key_char.as_ref()
                    && !keystroke.modifiers.control
                    && !keystroke.modifiers.platform
                    && !key_char.chars().any(char::is_control)
                {
                    self.insert_remote_file_input_text(target, key_char);
                }
            }
        }
    }

    /// 鼠标开始选择远程文件输入框文本。
    pub(crate) fn begin_remote_file_input_pointer_selection(
        &mut self,
        target: AppTextInputTarget,
        character_index: usize,
        granularity: TextSelectionGranularity,
    ) {
        self.focus_remote_file_text_input_target(target);
        let Some(input) = self.remote_file_text_input_mut(target) else {
            return;
        };
        input.begin_pointer_selection(character_index, granularity);
    }

    /// 鼠标拖拽更新远程文件输入框选区。
    pub(crate) fn update_remote_file_input_pointer_selection(
        &mut self,
        target: AppTextInputTarget,
        character_index: usize,
    ) {
        let Some(input) = self.remote_file_text_input_mut(target) else {
            return;
        };
        input.update_pointer_selection(character_index);
    }

    /// 鼠标结束远程文件输入框文本选择。
    pub(crate) fn finish_remote_file_input_pointer_selection(
        &mut self,
        target: AppTextInputTarget,
    ) {
        if let Some(input) = self.remote_file_text_input_mut(target) {
            input.finish_pointer_selection();
        }
    }

    /// 应用远程文件输入框原生输入法编辑结果。
    pub(crate) fn apply_native_remote_file_edit(
        &mut self,
        target: AppTextInputTarget,
        edit: &NativeTextEdit,
    ) {
        self.focus_remote_file_text_input_target(target);
        let Some(input) = self.remote_file_text_input_mut(target) else {
            return;
        };
        apply_native_edit_to_remote_file_input(input, edit);
        match target {
            AppTextInputTarget::RemoteFileAddress { session_id } => {
                if let Some(session) = self.remote_file_sessions.get_mut(&session_id) {
                    session.message = None;
                }
            }
            AppTextInputTarget::RemoteFileVersion { session_id } => {
                if let Some(session) = self.remote_file_sessions.get_mut(&session_id) {
                    session.message = None;
                }
            }
            AppTextInputTarget::RemoteFileRenameName => {
                if let Some(RemoteFileDialogState::Rename(dialog)) =
                    self.remote_file_dialog.as_mut()
                {
                    dialog.error_message = None;
                }
            }
            _ => {}
        }
    }

    /// 返回当前选中的远程文件条目。
    pub(crate) fn selected_remote_file_entries(&self, session_id: usize) -> Vec<RemoteFileEntry> {
        self.remote_file_sessions
            .get(&session_id)
            .map(RemoteFileSessionState::selected_entries)
            .unwrap_or_default()
    }

    /// 判断指定远程文件会话是否选中了单个可重命名条目。
    pub(crate) fn can_rename_remote_file_selection(&self, session_id: usize) -> bool {
        self.remote_file_sessions
            .get(&session_id)
            .is_some_and(|session| {
                session.capabilities.rename
                    && session.status == RemoteFileStatus::Connected
                    && session.selected_entries().len() == 1
            })
    }

    /// 判断指定远程文件会话是否选中了可下载的普通文件。
    pub(crate) fn can_download_remote_file_selection(&self, session_id: usize) -> bool {
        self.remote_file_sessions
            .get(&session_id)
            .is_some_and(|session| {
                let selected = session.selected_entries();
                session.capabilities.download
                    && session.status == RemoteFileStatus::Connected
                    && !selected.is_empty()
                    && selected.iter().all(|entry| entry.kind.is_regular_file())
            })
    }

    /// 判断指定远程文件会话是否选中了单个可预览的普通文件。
    pub(crate) fn can_preview_remote_file_selection(&self, session_id: usize) -> bool {
        self.remote_file_sessions
            .get(&session_id)
            .is_some_and(|session| {
                let selected = session.selected_entries();
                session.capabilities.preview
                    && session.status == RemoteFileStatus::Connected
                    && selected.len() == 1
                    && selected.iter().all(is_remote_file_entry_previewable)
            })
    }

    /// 判断指定远程文件会话是否选中了单个可删除条目。
    pub(crate) fn can_delete_remote_file_selection(&self, session_id: usize) -> bool {
        self.remote_file_sessions
            .get(&session_id)
            .is_some_and(|session| {
                let selected = session.selected_entries();
                session.capabilities.delete
                    && session.status == RemoteFileStatus::Connected
                    && selected.len() == 1
                    && matches!(
                        selected[0].kind,
                        RemoteFileEntryKind::RegularFile | RemoteFileEntryKind::Directory
                    )
            })
    }

    /// 返回指定远程文件会话中的远程文件条目。
    fn remote_file_entry(&self, session_id: usize, path: &str) -> Option<&RemoteFileEntry> {
        self.remote_file_sessions
            .get(&session_id)?
            .entries
            .iter()
            .find(|entry| entry.path == path)
    }

    /// 创建新的远程文件管理会话并启动后台 worker。
    fn create_remote_file_manager_session(
        &mut self,
        link_id: crate::remote::connection::ConnectionNodeId,
        backend: RemoteFileBackend,
        cx: &mut Context<Self>,
    ) {
        let Some(link) = self.config.connections.link(link_id).cloned() else {
            self.placeholder_notice = "未找到链接".to_string();
            return;
        };
        let session_id = self.next_remote_file_session_id;
        self.next_remote_file_session_id += 1;
        if !self.start_remote_file_worker(session_id, &link, backend, cx) {
            return;
        }
        self.create_remote_file_tab_for_session(session_id);
        self.workspace = Workspace::Connections;
        self.placeholder_notice = format!(
            "正在打开 {} 的 {} 文件管理",
            link.address_label(),
            backend.label()
        );
    }

    /// 重连已断开的远程文件管理会话；复用同一会话 ID 与标签，重建 worker 与目录状态。
    pub(crate) fn reconnect_remote_file_session(
        &mut self,
        session_id: usize,
        cx: &mut Context<Self>,
    ) {
        let Some((status, link_id, backend)) = self
            .remote_file_sessions
            .get(&session_id)
            .map(|session| (session.status.clone(), session.link_id, session.backend))
        else {
            self.placeholder_notice = "文件管理会话不存在".to_string();
            return;
        };
        if !matches!(
            status,
            RemoteFileStatus::Disconnected | RemoteFileStatus::Failed
        ) {
            return;
        }
        let Some(link) = self.config.connections.link(link_id).cloned() else {
            self.placeholder_notice = "未找到链接，无法重连".to_string();
            return;
        };
        if !self.start_remote_file_worker(session_id, &link, backend, cx) {
            return;
        }
        self.placeholder_notice = format!("正在重连 {}", link.address_label());
    }

    /// 为新的或重连的远程文件会话启动 worker，并以全新会话状态替换同 ID 的旧状态。
    ///
    /// 返回值：worker 是否成功启动；链接协议不匹配时返回 false 并如实提示。
    ///
    /// 说明：重连必须重建会话状态——旧目录列表与旧命令通道随断开一起失效，
    /// 继续复用会让已断开列表看起来仍可操作。
    fn start_remote_file_worker(
        &mut self,
        session_id: usize,
        link: &ConnectionLinkConfig,
        backend: RemoteFileBackend,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(worker_backend) = self.remote_file_worker_backend(link, backend) else {
            return false;
        };
        self.clear_remote_svn_certificate_request(session_id);
        let request = RemoteFileWorkerRequest {
            session_id,
            backend: worker_backend,
        };
        let (command_sender, event_receiver, worker_guard) = spawn_remote_file_worker(request);
        let mut session =
            RemoteFileSessionState::connecting(session_id, link, backend, command_sender);
        session.worker_guard = Some(worker_guard);
        let worker_generation = session.worker_generation;
        self.remote_file_sessions.insert(session_id, session);
        self.open_next_svn_certificate_prompt();
        Self::spawn_remote_file_event_pump(cx, session_id, worker_generation, event_receiver);
        true
    }

    /// 组装远程文件 worker 的协议后端配置；链接缺失或协议不匹配时更新提示并返回 None。
    fn remote_file_worker_backend(
        &mut self,
        link: &ConnectionLinkConfig,
        backend: RemoteFileBackend,
    ) -> Option<RemoteFileWorkerBackend> {
        let link_id = link.id;
        match backend {
            RemoteFileBackend::Sftp => {
                let Some(ssh) = link.ssh_config().cloned() else {
                    self.placeholder_notice = "当前链接不是 SSH 链接".to_string();
                    return None;
                };
                let trusted_fingerprint = self
                    .config
                    .connections
                    .trusted_fingerprint(&ssh.host, ssh.port)
                    .map(ToString::to_string);
                Some(RemoteFileWorkerBackend::Sftp {
                    ssh,
                    trusted_fingerprint,
                })
            }
            RemoteFileBackend::Smb => {
                let Some(smb) = link.smb_config().cloned() else {
                    self.placeholder_notice = "当前链接不是 SMB 链接".to_string();
                    return None;
                };
                Some(RemoteFileWorkerBackend::Smb { smb })
            }
            RemoteFileBackend::Git => {
                let Some(git) = link.git_config().cloned() else {
                    self.placeholder_notice = "当前链接不是 Git 链接".to_string();
                    return None;
                };
                let trusted_fingerprint = git_ssh_endpoint(&git.url).and_then(|(host, port)| {
                    self.config
                        .connections
                        .trusted_fingerprint(&host, port)
                        .map(ToString::to_string)
                });
                Some(RemoteFileWorkerBackend::Git {
                    link_id,
                    git,
                    trusted_fingerprint,
                })
            }
            RemoteFileBackend::Svn => {
                let Some(svn) = link.svn_config().cloned() else {
                    self.placeholder_notice = "当前链接不是 SVN 链接".to_string();
                    return None;
                };
                let trusted_fingerprint = svn_ssh_endpoint(&svn.url).and_then(|(host, port)| {
                    self.config
                        .connections
                        .trusted_fingerprint(&host, port)
                        .map(ToString::to_string)
                });
                let trusted_certificate = self
                    .config
                    .connections
                    .trusted_svn_certificate(link_id, &svn.url)
                    .cloned();
                Some(RemoteFileWorkerBackend::Svn {
                    svn,
                    trusted_fingerprint,
                    trusted_certificate,
                })
            }
        }
    }

    /// 消费远程文件 worker 事件并回写应用状态，直到事件通道关闭。
    fn spawn_remote_file_event_pump(
        cx: &mut Context<Self>,
        session_id: usize,
        worker_generation: uuid::Uuid,
        event_receiver: async_channel::Receiver<RemoteFileEvent>,
    ) {
        cx.spawn(async move |view, cx| {
            while let Ok(event) = event_receiver.recv().await {
                let applied = view.update(cx, |app, cx| {
                    app.apply_remote_file_event_for_generation(
                        session_id,
                        worker_generation,
                        event,
                        cx,
                    )
                });
                if !matches!(applied, Ok(true)) {
                    break;
                }
            }
        })
        .detach();
    }

    /// 重连复用会话 ID，但旧 worker 的迟到事件不能关闭或批准新一轮连接。
    fn apply_remote_file_event_for_generation(
        &mut self,
        session_id: usize,
        worker_generation: uuid::Uuid,
        event: RemoteFileEvent,
        cx: &mut Context<Self>,
    ) -> bool {
        if !self
            .remote_file_sessions
            .get(&session_id)
            .is_some_and(|session| session.worker_generation == worker_generation)
        {
            return false;
        }
        self.apply_remote_file_event(event, cx);
        cx.notify();
        true
    }

    /// 为已有远程文件会话创建标签。
    pub(crate) fn create_remote_file_tab_for_session(&mut self, session_id: usize) {
        let Some(session) = self.remote_file_sessions.get(&session_id) else {
            return;
        };
        // 没有选择日志时不会再有占位标签，创建页签一律新建。
        let tab_id = self.next_tab_id;
        self.next_tab_id += 1;
        self.tabs.push(ArgusTab {
            id: tab_id,
            title: session.title.clone(),
            kind: TabKind::RemoteFileManager { session_id },
        });
        self.active_tab_id = Some(tab_id);
    }

    /// 应用通用远程文件 worker 回传事件。
    fn apply_remote_file_event(&mut self, event: RemoteFileEvent, cx: &mut Context<Self>) {
        match event {
            RemoteFileEvent::SvnCertificateVerification {
                session_id,
                request,
            } => {
                self.apply_remote_svn_certificate_event(session_id, request);
            }
            RemoteFileEvent::HostKeyVerification {
                session_id,
                host,
                port,
                fingerprint,
            } => self.apply_remote_file_host_key_event(session_id, host, port, fingerprint),
            RemoteFileEvent::Connected {
                session_id,
                current_dir,
                entries,
            }
            | RemoteFileEvent::DirectoryLoaded {
                session_id,
                current_dir,
                entries,
            } => {
                if let Some(session) = self.remote_file_sessions.get_mut(&session_id) {
                    session.apply_directory_listing(current_dir.clone(), entries);
                    session.message = Some(format!("已读取目录 {current_dir}"));
                }
                self.placeholder_notice = "远程目录已加载".to_string();
            }
            RemoteFileEvent::RepositoryVersionsLoaded {
                session_id,
                versions,
                selected_version,
                input_value,
                message,
            } => {
                if let Some(session) = self.remote_file_sessions.get_mut(&session_id) {
                    session.repository_versions = versions;
                    session.selected_repository_version = Some(selected_version);
                    session.version_input = TextInputState::from_value(input_value);
                    session.status = RemoteFileStatus::Connected;
                    if let Some(message) = message.clone() {
                        session.message = Some(message);
                    }
                }
                if let Some(message) = message {
                    self.placeholder_notice = message;
                }
            }
            RemoteFileEvent::TransferProgress {
                session_id,
                message,
            } => {
                if let Some(session) = self.remote_file_sessions.get_mut(&session_id) {
                    session.message = Some(message.clone());
                }
                self.placeholder_notice = message;
            }
            RemoteFileEvent::FileContentLoaded {
                session_id,
                request_id,
                file_name,
                content,
            } => {
                self.apply_remote_file_preview_result(
                    session_id, request_id, file_name, content, cx,
                );
            }
            RemoteFileEvent::FileHistoryLoaded {
                session_id,
                remote_path,
                entries,
                error,
            } => {
                let entry_count = entries.len();
                let history_error = error.clone();
                if let Some(session) = self.remote_file_sessions.get_mut(&session_id) {
                    session.status = RemoteFileStatus::Connected;
                    session.message = Some(match &history_error {
                        Some(message) => message.clone(),
                        None => format!("已加载文件历史：{entry_count} 个提交"),
                    });
                }
                if let Some(RemoteFileDialogState::RepositoryDiff(dialog)) =
                    self.remote_file_dialog.as_mut()
                    && dialog.session_id == session_id
                    && dialog.remote_path == remote_path
                {
                    dialog.is_loading_history = false;
                    dialog.is_versions_open = false;
                    dialog.history_error = history_error.clone();
                    dialog.entries = entries;
                    dialog.selected_target =
                        dialog.entries.first().map(|entry| entry.target_id.clone());
                    dialog.commit_scroll.set_offset(point(px(0.0), px(0.0)));
                }
                self.placeholder_notice = match history_error {
                    Some(message) => message,
                    None => format!("已加载 {entry_count} 个历史版本"),
                };
            }
            RemoteFileEvent::FileComparisonLoaded {
                session_id,
                file_name,
                current_version,
                target_version,
                current_content,
                target_content,
            } => {
                if let Some(session) = self.remote_file_sessions.get_mut(&session_id) {
                    session.status = RemoteFileStatus::Connected;
                    session.message = Some(format!("已加载版本对比：{file_name}"));
                }
                self.open_file_diff_window(
                    file_name,
                    current_version,
                    target_version,
                    current_content,
                    target_content,
                    cx,
                );
            }
            RemoteFileEvent::OperationSucceeded {
                session_id,
                message,
            } => {
                if let Some(session) = self.remote_file_sessions.get_mut(&session_id) {
                    session.status = RemoteFileStatus::Connected;
                    session.message = Some(message.clone());
                }
                self.placeholder_notice = message;
            }
            RemoteFileEvent::OperationFailed {
                session_id,
                message,
            } => {
                self.clear_remote_svn_certificate_request(session_id);
                if let Some(session) = self.remote_file_sessions.get_mut(&session_id) {
                    session.status = RemoteFileStatus::Connected;
                    session.message = Some(message.clone());
                }
                self.placeholder_notice = message;
                self.open_next_svn_certificate_prompt();
            }
            RemoteFileEvent::Disconnected {
                session_id,
                message,
            } => {
                self.clear_remote_svn_certificate_request(session_id);
                if let Some(session) = self.remote_file_sessions.get_mut(&session_id) {
                    session.status = RemoteFileStatus::Disconnected;
                    session.command_sender = None;
                    session.worker_guard = None;
                    session.message = Some(message.clone());
                }
                self.fail_pending_remote_file_preview(
                    session_id,
                    "文件管理连接已断开，无法继续预览".to_string(),
                    cx,
                );
                self.placeholder_notice = message;
                self.open_next_svn_certificate_prompt();
            }
            RemoteFileEvent::Failed {
                session_id,
                message,
            } => {
                self.clear_remote_svn_certificate_request(session_id);
                if let Some(session) = self.remote_file_sessions.get_mut(&session_id) {
                    session.status = RemoteFileStatus::Failed;
                    session.command_sender = None;
                    session.worker_guard = None;
                    session.message = Some(message.clone());
                }
                self.fail_pending_remote_file_preview(
                    session_id,
                    "文件管理连接失败，无法继续预览".to_string(),
                    cx,
                );
                self.placeholder_notice = message;
                self.open_next_svn_certificate_prompt();
            }
        }
    }

    /// 保存证书挑战到所属会话；其他弹窗仍可继续操作，证书请求保留在等待队列中。
    fn apply_remote_svn_certificate_event(
        &mut self,
        session_id: usize,
        request: SvnCertificateApproval,
    ) {
        if !request.is_pending() {
            return;
        }
        if !self
            .remote_file_sessions
            .get(&session_id)
            .is_some_and(|session| {
                session.backend == RemoteFileBackend::Svn
                    && !matches!(
                        session.status,
                        RemoteFileStatus::Disconnected | RemoteFileStatus::Failed
                    )
            })
        {
            if let Err(error) = request.respond(SvnCertificateDecision::Reject) {
                self.placeholder_notice =
                    format!("会话 {session_id} 的失效证书请求无法拒绝：{error}");
            }
            return;
        }
        if self
            .remote_file_sessions
            .get(&session_id)
            .is_some_and(|session| {
                session
                    .pending_svn_certificate
                    .as_ref()
                    .is_some_and(|pending| pending.request_id == request.request_id)
            })
        {
            return;
        }
        self.clear_remote_svn_certificate_request(session_id);
        if let Some(session) = self.remote_file_sessions.get_mut(&session_id) {
            session.status = RemoteFileStatus::AwaitingCertificate;
            session.pending_svn_certificate = Some(request);
            session.message = Some("请确认 SVN HTTPS 服务器证书".to_string());
        }
        self.open_next_svn_certificate_prompt();
        self.placeholder_notice = "请确认 SVN HTTPS 服务器证书".to_string();
    }

    /// 从等待页面重新打开本会话证书；不得覆盖另一个尚未处理的弹窗。
    pub(crate) fn open_svn_certificate_prompt(&mut self, session_id: usize) {
        if self.connection_dialog.is_some() {
            self.placeholder_notice = "请先处理当前弹窗，再查看 SVN 证书".to_string();
            return;
        }
        let Some(request) = self
            .remote_file_sessions
            .get(&session_id)
            .and_then(|session| session.pending_svn_certificate.as_ref())
            .filter(|request| request.is_pending())
        else {
            self.placeholder_notice = "证书确认请求已失效，请重新连接".to_string();
            return;
        };
        self.connection_dialog = Some(ConnectionDialogState::SvnCertificate(
            ConnectionSvnCertificatePromptState {
                session_id,
                request_id: request.request_id.clone(),
                prompt: request.prompt.clone(),
                error_message: None,
            },
        ));
    }

    /// 每个会话保留自己的待确认请求，依会话顺序逐一展示，避免并发连接覆盖证书信息。
    pub(crate) fn open_next_svn_certificate_prompt(&mut self) {
        if self.connection_dialog.is_some() {
            return;
        }
        let next_session = self
            .remote_file_sessions
            .values()
            .filter(|session| {
                session
                    .pending_svn_certificate
                    .as_ref()
                    .is_some_and(|request| request.is_pending())
            })
            .map(|session| session.id)
            .min();
        if let Some(session_id) = next_session {
            self.open_svn_certificate_prompt(session_id);
        }
    }

    /// 处理用户明确点击的证书决定；保存失败保留请求，旧请求不能授权新连接。
    pub(crate) fn respond_svn_certificate_prompt(
        &mut self,
        displayed: ConnectionSvnCertificatePromptState,
        decision: SvnCertificateDecision,
    ) {
        let current = self
            .remote_file_sessions
            .get(&displayed.session_id)
            .filter(|session| session.status == RemoteFileStatus::AwaitingCertificate)
            .and_then(|session| {
                session
                    .pending_svn_certificate
                    .as_ref()
                    .map(|request| (session.link_id, request.clone()))
            })
            .filter(|(_, request)| {
                request.request_id == displayed.request_id
                    && request.prompt == displayed.prompt
                    && request.is_pending()
            });
        let Some((link_id, request)) = current else {
            self.close_svn_certificate_prompt_if_matches(
                displayed.session_id,
                &displayed.request_id,
            );
            self.placeholder_notice = "证书确认请求已失效，请查看当前请求或重新连接".to_string();
            self.open_next_svn_certificate_prompt();
            return;
        };
        let trust = SvnCertificateTrust {
            host: request.prompt.host.clone(),
            port: request.prompt.port,
            fingerprint: request.prompt.fingerprint.clone(),
            allowed_failures: request.prompt.failures.clone(),
        };
        let mut persisted_config = None;
        if decision != SvnCertificateDecision::Reject {
            // 临时信任同样检查连接端点；编辑链接后，旧证书不能批准新地址。
            let mut candidate = self.config.clone();
            if let Err(error) = candidate.connections.trust_svn_certificate(link_id, trust) {
                self.report_svn_certificate_prompt_error(
                    &displayed,
                    format!("证书确认失败：{error}"),
                );
                return;
            }
            if decision == SvnCertificateDecision::TrustPermanently {
                if let Err(error) = self.config_manager.save(&candidate) {
                    self.report_svn_certificate_prompt_error(
                        &displayed,
                        format!("证书信任记录保存失败，尚未继续连接：{error}"),
                    );
                    return;
                }
                persisted_config = Some(candidate);
            }
        }
        if let Err(error) = request.respond(decision) {
            let mut message = format!("证书确认未生效：{error}");
            if persisted_config.is_some()
                && let Err(rollback_error) = self.config_manager.save(&self.config)
            {
                message.push_str(&format!(
                    "；信任记录回滚保存失败，磁盘可能保留该记录：{rollback_error}"
                ));
            }
            self.report_svn_certificate_prompt_error(&displayed, message);
            return;
        }
        if let Some(config) = persisted_config {
            self.config = config;
        }
        let message = match decision {
            SvnCertificateDecision::Reject => "已拒绝 SVN HTTPS 服务器证书",
            SvnCertificateDecision::TrustOnce => "已仅为本次连接信任 SVN 证书，继续连接...",
            SvnCertificateDecision::TrustPermanently => "已保存 SVN 证书信任记录，继续连接...",
        }
        .to_string();
        if let Some(session) = self.remote_file_sessions.get_mut(&displayed.session_id) {
            session.pending_svn_certificate = None;
            session.status = if decision == SvnCertificateDecision::Reject {
                RemoteFileStatus::Failed
            } else {
                RemoteFileStatus::Connecting
            };
            session.message = Some(message.clone());
        }
        self.close_svn_certificate_prompt_if_matches(displayed.session_id, &displayed.request_id);
        self.placeholder_notice = message;
        self.open_next_svn_certificate_prompt();
    }

    /// 同时在当前弹窗和会话中保留失败原因，不把配置保存失败伪装成已信任。
    fn report_svn_certificate_prompt_error(
        &mut self,
        displayed: &ConnectionSvnCertificatePromptState,
        message: String,
    ) {
        if let Some(ConnectionDialogState::SvnCertificate(current)) =
            self.connection_dialog.as_mut()
            && current.session_id == displayed.session_id
            && current.request_id == displayed.request_id
        {
            current.error_message = Some(message.clone());
        }
        if let Some(session) = self.remote_file_sessions.get_mut(&displayed.session_id)
            && session
                .pending_svn_certificate
                .as_ref()
                .is_some_and(|request| request.request_id == displayed.request_id)
        {
            session.message = Some(message.clone());
        }
        self.placeholder_notice = message;
    }

    /// 仅关闭仍绑定同一挑战的弹窗，不影响后来出现的另一张证书。
    fn close_svn_certificate_prompt_if_matches(&mut self, session_id: usize, request_id: &str) {
        if matches!(&self.connection_dialog, Some(ConnectionDialogState::SvnCertificate(displayed))
            if displayed.session_id == session_id && displayed.request_id == request_id)
        {
            self.connection_dialog = None;
        }
    }

    /// 会话关闭、失败或重连时拒绝残留挑战，防止后台继续等待已经不存在的弹窗。
    fn clear_remote_svn_certificate_request(&mut self, session_id: usize) {
        let request = self
            .remote_file_sessions
            .get_mut(&session_id)
            .and_then(|session| session.pending_svn_certificate.take());
        if let Some(request) = request {
            if request.is_pending()
                && let Err(error) = request.respond(SvnCertificateDecision::Reject)
            {
                self.placeholder_notice = format!("会话 {session_id} 的证书请求关闭失败：{error}");
            }
            self.close_svn_certificate_prompt_if_matches(session_id, &request.request_id);
        }
    }

    /// 应用远程文件后端的未知 SSH 主机指纹事件，并打开确认弹窗。
    fn apply_remote_file_host_key_event(
        &mut self,
        session_id: usize,
        host: String,
        port: u16,
        fingerprint: String,
    ) {
        let Some(link_id) = self
            .remote_file_sessions
            .get(&session_id)
            .map(|session| session.link_id)
        else {
            return;
        };
        let pending = PendingHostKey {
            host,
            port,
            fingerprint,
        };
        if let Some(session) = self.remote_file_sessions.get_mut(&session_id) {
            session.status = RemoteFileStatus::AwaitingHostKey;
            session.pending_host_key = Some(pending.clone());
            session.message = Some("请确认 SSH 主机指纹".to_string());
        }
        self.open_remote_file_host_key_prompt(session_id, link_id, pending);
        self.placeholder_notice = "请确认 SSH 主机指纹".to_string();
    }

    /// 设置远程文件会话的 SSH 主机指纹确认弹窗状态。
    fn open_remote_file_host_key_prompt(
        &mut self,
        session_id: usize,
        link_id: crate::remote::connection::ConnectionNodeId,
        pending: PendingHostKey,
    ) {
        self.connection_dialog = Some(crate::app::ConnectionDialogState::ConfirmHostKey(
            ConnectionHostKeyPromptState {
                session_id,
                owner: HostKeyPromptOwner::RemoteFile { session_id },
                link_id,
                host: pending.host,
                port: pending.port,
                fingerprint: pending.fingerprint,
            },
        ));
    }

    /// 向通用远程文件 worker 发送命令，并把 UI 状态切换到指定忙碌状态。
    fn send_remote_file_command(
        &mut self,
        session_id: usize,
        command: RemoteFileCommand,
        busy_status: RemoteFileStatus,
        busy_message: &str,
    ) -> bool {
        let Some(session) = self.remote_file_sessions.get_mut(&session_id) else {
            self.placeholder_notice = "文件管理会话不存在".to_string();
            return false;
        };
        let protocol_label = session.backend.label();
        if session.status != RemoteFileStatus::Connected {
            self.placeholder_notice = format!("{protocol_label} 尚未连接，暂不能执行文件操作");
            return false;
        }
        let Some(sender) = &session.command_sender else {
            self.placeholder_notice = format!("{protocol_label} 通道不可用");
            return false;
        };
        let is_allowed = match &command {
            RemoteFileCommand::UploadFiles { .. } => {
                session.capabilities.write && session.capabilities.upload
            }
            RemoteFileCommand::Rename { .. } => {
                session.capabilities.write && session.capabilities.rename
            }
            RemoteFileCommand::Delete { .. } => {
                session.capabilities.write && session.capabilities.delete
            }
            RemoteFileCommand::DownloadFile { .. } | RemoteFileCommand::DownloadFiles { .. } => {
                session.capabilities.download
            }
            RemoteFileCommand::SwitchRepositoryVersion { .. } => {
                session.capabilities.switch_version
            }
            RemoteFileCommand::LoadDirectory(_) | RemoteFileCommand::Refresh => {
                session.capabilities.browse
            }
            RemoteFileCommand::ReadFileContent { .. }
            | RemoteFileCommand::CompareFileVersions { .. }
            | RemoteFileCommand::LoadFileHistory { .. } => session.capabilities.preview,
            RemoteFileCommand::TrustHostKey
            | RemoteFileCommand::RejectHostKey
            | RemoteFileCommand::Disconnect => true,
        };
        if !is_allowed {
            self.placeholder_notice = format!("{protocol_label} 当前会话不允许执行该操作");
            return false;
        }
        if sender.send(command).is_err() {
            self.placeholder_notice = format!("{protocol_label} 无法发送文件操作命令");
            return false;
        }
        session.status = busy_status;
        session.message = Some(busy_message.to_string());
        self.placeholder_notice = busy_message.to_string();
        true
    }

    /// 提交远程文件重命名弹窗。
    fn submit_remote_file_rename(&mut self, mut dialog: RemoteFileRenameDialogState) {
        let new_name = match validate_remote_file_rename_name(&dialog.name_input.value) {
            Ok(name) => name,
            Err(message) => {
                dialog.error_message = Some(message.clone());
                self.remote_file_dialog = Some(RemoteFileDialogState::Rename(dialog));
                self.placeholder_notice = message;
                return;
            }
        };
        if new_name == dialog.original_name {
            self.remote_file_dialog = None;
            self.placeholder_notice = "文件名称未变化".to_string();
            return;
        }
        let session_id = dialog.session_id;
        let remote_path = dialog.remote_path;
        self.remote_file_dialog = None;
        self.send_remote_file_command(
            session_id,
            RemoteFileCommand::Rename {
                remote_path,
                new_name,
            },
            RemoteFileStatus::Transferring,
            "正在重命名远程文件...",
        );
    }

    /// 删除远程文件输入框选区。
    fn delete_remote_file_input_selection(&mut self, target: AppTextInputTarget) -> bool {
        let Some(input) = self.remote_file_text_input_mut(target) else {
            return false;
        };
        let Some(range) = normalized_input_selection_range(input) else {
            return false;
        };
        input.value = replace_character_range(&input.value, range.clone(), "");
        input.cursor = range.start;
        input.selection_anchor = None;
        input.marked_range = None;
        input.selection_drag = None;
        true
    }

    /// 插入远程文件输入框文本。
    fn insert_remote_file_input_text(&mut self, target: AppTextInputTarget, text: &str) {
        let Some(input) = self.remote_file_text_input_mut(target) else {
            return;
        };
        if let Some(range) = normalized_input_selection_range(input) {
            input.value = replace_character_range(&input.value, range.clone(), text);
            input.cursor = range.start + character_count(text);
            input.selection_anchor = None;
        } else {
            input.value = replace_character_range(&input.value, input.cursor..input.cursor, text);
            input.cursor += character_count(text);
        }
        input.marked_range = None;
        input.selection_drag = None;
    }

    /// 删除远程文件输入框光标前字符。
    fn delete_remote_file_input_backward(&mut self, target: AppTextInputTarget) {
        if self.delete_remote_file_input_selection(target) {
            return;
        }
        let Some(input) = self.remote_file_text_input_mut(target) else {
            return;
        };
        if input.cursor == 0 {
            return;
        }
        let remove_start = input.cursor - 1;
        input.value = replace_character_range(&input.value, remove_start..input.cursor, "");
        input.cursor = remove_start;
        input.marked_range = None;
    }

    /// 删除远程文件输入框光标后字符。
    fn delete_remote_file_input_forward(&mut self, target: AppTextInputTarget) {
        if self.delete_remote_file_input_selection(target) {
            return;
        }
        let Some(input) = self.remote_file_text_input_mut(target) else {
            return;
        };
        let text_length = character_count(&input.value);
        if input.cursor >= text_length {
            return;
        }
        input.value = replace_character_range(&input.value, input.cursor..input.cursor + 1, "");
        input.marked_range = None;
    }

    /// 左移远程文件输入框光标。
    fn move_remote_file_input_left(&mut self, target: AppTextInputTarget, extend_selection: bool) {
        let cursor = self
            .remote_file_text_input(target)
            .map(|input| input.cursor.saturating_sub(1))
            .unwrap_or_default();
        self.move_remote_file_input_cursor(target, cursor, extend_selection);
    }

    /// 右移远程文件输入框光标。
    fn move_remote_file_input_right(&mut self, target: AppTextInputTarget, extend_selection: bool) {
        let cursor = self
            .remote_file_text_input(target)
            .map(|input| (input.cursor + 1).min(character_count(&input.value)))
            .unwrap_or_default();
        self.move_remote_file_input_cursor(target, cursor, extend_selection);
    }

    /// 移动远程文件输入框光标，并按需扩展选区。
    fn move_remote_file_input_cursor(
        &mut self,
        target: AppTextInputTarget,
        cursor: usize,
        extend_selection: bool,
    ) {
        let Some(input) = self.remote_file_text_input_mut(target) else {
            return;
        };
        let cursor = cursor.min(character_count(&input.value));
        if extend_selection {
            input.selection_anchor.get_or_insert(input.cursor);
        } else {
            input.selection_anchor = None;
        }
        input.cursor = cursor;
        input.marked_range = None;
        input.selection_drag = None;
    }
}

/// 解析 Git SSH 地址的主机和端口，HTTPS 地址返回空以避免写入无关主机信任记录。
fn git_ssh_endpoint(url: &str) -> Option<(String, u16)> {
    if url.trim().to_ascii_lowercase().starts_with("ssh://") {
        let parsed = url::Url::parse(url).ok()?;
        return Some((parsed.host_str()?.to_string(), parsed.port().unwrap_or(22)));
    }
    let authority = url.split_once(':')?.0;
    let host = authority.rsplit_once('@')?.1;
    Some((host.to_string(), 22))
}

/// 解析 `svn+ssh://` 地址的主机和端口；未加密的 `svn://` 不参与 SSH 主机信任。
fn svn_ssh_endpoint(url: &str) -> Option<(String, u16)> {
    if !url.trim().to_ascii_lowercase().starts_with("svn+ssh://") {
        return None;
    }
    let parsed = url::Url::parse(url).ok()?;
    Some((parsed.host_str()?.to_string(), parsed.port().unwrap_or(22)))
}

/// 返回远程文件弹窗关联的会话 ID。
fn remote_file_dialog_session_id(dialog: &RemoteFileDialogState) -> Option<usize> {
    match dialog {
        RemoteFileDialogState::Rename(dialog) => Some(dialog.session_id),
        RemoteFileDialogState::ConfirmDelete(prompt) => Some(prompt.session_id),
        RemoteFileDialogState::RepositoryDiff(dialog) => Some(dialog.session_id),
    }
}

/// 清理远程文件输入框焦点态。
fn clear_remote_file_input_focus(input: &mut TextInputState) {
    input.clear_focus();
}

/// 应用系统原生文本输入编辑结果。
fn apply_native_edit_to_remote_file_input(input: &mut TextInputState, edit: &NativeTextEdit) {
    let parts = RemoteFileInputParts {
        value: &mut input.value,
        cursor: &mut input.cursor,
        selection_anchor: &mut input.selection_anchor,
        marked_range: &mut input.marked_range,
        selection_drag: &mut input.selection_drag,
    };
    let replacement = edit.text.as_str();
    *parts.value =
        replace_character_range(parts.value, edit.replacement_range.clone(), replacement);
    *parts.cursor = edit.replacement_range.start + character_count(replacement);
    *parts.selection_anchor = None;
    *parts.marked_range = edit.marked_range.clone();
    *parts.selection_drag = None;
}

/// 返回输入框当前规范化选区。
fn normalized_input_selection_range(input: &TextInputState) -> Option<std::ops::Range<usize>> {
    input.selection_range()
}
