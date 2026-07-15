//! 文件职责：实现 SFTP、SMB、Git、SVN 通用远程文件管理标签、事件回收、文件操作和输入框交互。
//! 创建日期：2026-06-26
//! 修改日期：2026-07-15
//! 作者：Argus 开发团队
//! 主要功能：从 SSH/SMB/Git/SVN 链接打开通用文件管理，并按协议能力约束浏览、预览、下载和写操作。

use std::borrow::Borrow;
use std::path::PathBuf;

use gpui::{Context, Keystroke, PathPromptOptions, Pixels, Point, SharedString};

use crate::app::{
    AppTextInputTarget, ArgusApp, ArgusTab, ConnectionHostKeyPromptState, HostKeyPromptOwner,
    InputTextSelectionDrag, RemoteFileDeletePromptState, RemoteFileDialogState,
    RemoteFileRenameDialogState, TabKind, TextInputState, Workspace,
};
use crate::infra::text_selection::{
    NativeTextEdit, TextSelectionGranularity, character_count, replace_character_range,
};
use crate::loader::PathBrowser;
use crate::remote::remote_file::{
    RemoteFileBackend, RemoteFileCommand, RemoteFileEntry, RemoteFileEntryKind, RemoteFileEvent,
    RemoteFileSessionState, RemoteFileStatus, RemoteFileWorkerBackend, RemoteFileWorkerRequest,
    is_remote_file_entry_previewable, remote_parent_dir, spawn_remote_file_worker,
    validate_remote_file_rename_name,
};
use crate::remote::terminal::PendingHostKey;
use crate::ui::components::context_menu::{ActiveMenu, ActiveMenuKind};

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
            self.active_tab_id = tab.id;
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
            self.request_remote_file_preview(session_id, entry.path, entry.size);
        }
    }

    /// 请求读取远程普通文件内容用于预览；超上限或未连接时给出提示。
    pub(crate) fn request_remote_file_preview(
        &mut self,
        session_id: usize,
        remote_path: String,
        size: Option<u64>,
    ) {
        if let Some(size) = size
            && size > crate::remote::remote_file::REMOTE_FILE_PREVIEW_MAX_FILE_SIZE
        {
            self.placeholder_notice = "文件过大，无法预览".to_string();
            return;
        }
        self.send_remote_file_command(
            session_id,
            RemoteFileCommand::ReadFileContent { remote_path },
            RemoteFileStatus::Transferring,
            "正在读取文件...",
        );
    }

    /// 右键菜单触发：对当前唯一选中的可预览普通文件发起预览。
    pub(crate) fn preview_remote_file_selection(&mut self, session_id: usize) {
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
        self.request_remote_file_preview(session_id, entry.path, entry.size);
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
                RemoteFileDialogState::ConfirmDelete(_) => None,
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
                RemoteFileDialogState::ConfirmDelete(_) => None,
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
        let worker_backend = match backend {
            RemoteFileBackend::Sftp => {
                let Some(ssh) = link.ssh_config().cloned() else {
                    self.placeholder_notice = "当前链接不是 SSH 链接".to_string();
                    return;
                };
                let trusted_fingerprint = self
                    .config
                    .connections
                    .trusted_fingerprint(&ssh.host, ssh.port)
                    .map(ToString::to_string);
                RemoteFileWorkerBackend::Sftp {
                    ssh,
                    trusted_fingerprint,
                }
            }
            RemoteFileBackend::Smb => {
                let Some(smb) = link.smb_config().cloned() else {
                    self.placeholder_notice = "当前链接不是 SMB 链接".to_string();
                    return;
                };
                RemoteFileWorkerBackend::Smb { smb }
            }
            RemoteFileBackend::Git => {
                let Some(git) = link.git_config().cloned() else {
                    self.placeholder_notice = "当前链接不是 Git 链接".to_string();
                    return;
                };
                let trusted_fingerprint = git_ssh_endpoint(&git.url).and_then(|(host, port)| {
                    self.config
                        .connections
                        .trusted_fingerprint(&host, port)
                        .map(ToString::to_string)
                });
                RemoteFileWorkerBackend::Git {
                    link_id,
                    git,
                    trusted_fingerprint,
                }
            }
            RemoteFileBackend::Svn => {
                let Some(svn) = link.svn_config().cloned() else {
                    self.placeholder_notice = "当前链接不是 SVN 链接".to_string();
                    return;
                };
                let trusted_fingerprint = svn_ssh_endpoint(&svn.url).and_then(|(host, port)| {
                    self.config
                        .connections
                        .trusted_fingerprint(&host, port)
                        .map(ToString::to_string)
                });
                RemoteFileWorkerBackend::Svn {
                    svn,
                    trusted_fingerprint,
                }
            }
        };
        let session_id = self.next_remote_file_session_id;
        self.next_remote_file_session_id += 1;
        let request = RemoteFileWorkerRequest {
            session_id,
            backend: worker_backend,
        };
        let (command_sender, event_receiver) = spawn_remote_file_worker(request);
        let session =
            RemoteFileSessionState::connecting(session_id, &link, backend, command_sender);
        self.remote_file_sessions.insert(session_id, session);
        self.create_remote_file_tab_for_session(session_id);
        self.workspace = Workspace::Connections;
        self.placeholder_notice = format!(
            "正在打开 {} 的 {} 文件管理",
            link.address_label(),
            backend.label()
        );

        cx.spawn(async move |view, cx| {
            while let Ok(event) = event_receiver.recv().await {
                let _ = view.update(cx, |app, cx| {
                    app.apply_remote_file_event(event, cx);
                    cx.notify();
                });
            }
        })
        .detach();
    }

    /// 为已有远程文件会话创建标签。
    pub(crate) fn create_remote_file_tab_for_session(&mut self, session_id: usize) {
        let Some(session) = self.remote_file_sessions.get(&session_id) else {
            return;
        };
        let tab_id = if self.tabs.len() == 1 && matches!(self.tabs[0].kind, TabKind::Empty) {
            self.tabs[0].id
        } else {
            let next_id = self.next_tab_id;
            self.next_tab_id += 1;
            self.tabs.push(ArgusTab {
                id: next_id,
                title: String::new(),
                kind: TabKind::Empty,
            });
            next_id
        };

        if let Some(tab) = self.tabs.iter_mut().find(|tab| tab.id == tab_id) {
            tab.title = session.title.clone();
            tab.kind = TabKind::RemoteFileManager { session_id };
        }
        self.active_tab_id = tab_id;
    }

    /// 应用通用远程文件 worker 回传事件。
    fn apply_remote_file_event(&mut self, event: RemoteFileEvent, cx: &mut Context<Self>) {
        match event {
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
                file_name,
                content,
            } => {
                if let Some(session) = self.remote_file_sessions.get_mut(&session_id) {
                    session.status = RemoteFileStatus::Connected;
                    session.message = Some(format!("已加载预览：{file_name}"));
                }
                self.open_file_preview_window(file_name, content, cx);
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
                if let Some(session) = self.remote_file_sessions.get_mut(&session_id) {
                    session.status = RemoteFileStatus::Connected;
                    session.message = Some(message.clone());
                }
                self.placeholder_notice = message;
            }
            RemoteFileEvent::Disconnected {
                session_id,
                message,
            } => {
                if let Some(session) = self.remote_file_sessions.get_mut(&session_id) {
                    session.status = RemoteFileStatus::Disconnected;
                    session.command_sender = None;
                    session.message = Some(message.clone());
                }
                self.placeholder_notice = message;
            }
            RemoteFileEvent::Failed {
                session_id,
                message,
            } => {
                if let Some(session) = self.remote_file_sessions.get_mut(&session_id) {
                    session.status = RemoteFileStatus::Failed;
                    session.command_sender = None;
                    session.message = Some(message.clone());
                }
                self.placeholder_notice = message;
            }
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
    ) {
        let Some(session) = self.remote_file_sessions.get_mut(&session_id) else {
            self.placeholder_notice = "文件管理会话不存在".to_string();
            return;
        };
        let protocol_label = session.backend.label();
        if session.status != RemoteFileStatus::Connected {
            self.placeholder_notice = format!("{protocol_label} 尚未连接，暂不能执行文件操作");
            return;
        }
        let Some(sender) = &session.command_sender else {
            self.placeholder_notice = format!("{protocol_label} 通道不可用");
            return;
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
            RemoteFileCommand::ReadFileContent { .. } => session.capabilities.preview,
            RemoteFileCommand::TrustHostKey
            | RemoteFileCommand::RejectHostKey
            | RemoteFileCommand::Disconnect => true,
        };
        if !is_allowed {
            self.placeholder_notice = format!("{protocol_label} 当前会话不允许执行该操作");
            return;
        }
        let _ = sender.send(command);
        session.status = busy_status;
        session.message = Some(busy_message.to_string());
        self.placeholder_notice = busy_message.to_string();
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
