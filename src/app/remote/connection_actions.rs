//! 文件职责：实现链接工作区的树操作、表单校验、输入框编辑和配置持久化。
//! 创建日期：2026-06-26
//! 修改日期：2026-07-16
//! 作者：Argus 开发团队
//! 主要功能：处理多协议链接新增编辑、树选择与拖放移动、过滤、删除及会话打开能力。

use std::ops::Range;

use gpui::{AppContext, Context, Keystroke};

use crate::app::{
    AppTextInputTarget, ArgusApp, ConnectionCredentialFormState, ConnectionDeletePromptState,
    ConnectionDialogState, ConnectionDirectoryFormState, ConnectionHostKeyPromptState,
    ConnectionLinkFormState, TextInputState,
};
use crate::infra::text_selection::{
    TextSelectionGranularity, character_count, replace_character_range,
};
use crate::remote::connection::{
    ConnectionConfig, ConnectionCredentialId, ConnectionDeletedNodeKind, ConnectionLinkConfig,
    ConnectionLinkKind, ConnectionNodeId, ConnectionTreeRow, GitLinkConfig, SmbLinkConfig,
    SshLinkConfig, SvnLinkConfig,
};
use crate::remote::remote_file::RemoteFileBackend;
use crate::remote::terminal::PendingHostKey;
use crate::ui::connection_dialog::{
    ConnectionCredentialWindow, ConnectionDirectoryWindow, ConnectionDirectoryWindowMode,
    ConnectionLinkWindow, ConnectionLinkWindowMode,
};

impl ArgusApp {
    /// 返回当前链接树是否处于过滤模式。
    pub(crate) fn is_connection_tree_filtering(&self) -> bool {
        self.is_connection_tree_search_open && !self.connection_tree_search_input.value.is_empty()
    }

    /// 返回链接树当前应渲染的可见行。
    pub(crate) fn visible_connection_rows(&self) -> Vec<ConnectionTreeRow> {
        let query = if self.is_connection_tree_search_open {
            self.connection_tree_search_input.value.as_str()
        } else {
            ""
        };
        self.config
            .connections
            .visible_rows(query, self.selected_connection_node_id)
    }

    /// 打开链接树过滤输入框。
    pub(crate) fn open_connection_tree_search(&mut self) {
        self.is_connection_tree_search_open = true;
        self.connection_tree_search_input = TextInputState::default();
        self.connection_tree_search_input.is_focused = true;
        self.placeholder_notice = "已打开链接过滤".to_string();
    }

    /// 关闭链接树过滤输入框并恢复完整目录树。
    pub(crate) fn close_connection_tree_search(&mut self) {
        self.is_connection_tree_search_open = false;
        self.connection_tree_search_input = TextInputState::default();
        self.placeholder_notice = "已关闭链接过滤".to_string();
    }

    /// 收起链接目录树中的所有目录。
    pub(crate) fn collapse_all_connections(&mut self) {
        let collapsed_count = self.config.connections.collapse_all();
        self.persist_config_or_report();
        self.placeholder_notice = if collapsed_count == 0 {
            "链接目录树已处于全部收起状态".to_string()
        } else {
            format!("已收起 {collapsed_count} 个链接目录")
        };
    }

    /// 清除链接树当前选中节点；供树面板空白区域点击使用。
    ///
    /// 返回值：选中状态或活动菜单实际发生变化时返回 `true`，调用方据此刷新界面。
    pub(crate) fn clear_connection_tree_selection(&mut self) -> bool {
        let had_selection = self.selected_connection_node_id.take().is_some();
        let had_menu = self.active_menu.take().is_some();
        if had_selection {
            self.placeholder_notice = "已取消链接树选择".to_string();
        }
        had_selection || had_menu
    }

    /// 把已有链接移动到指定目录或根层级，并立即持久化新的树结构。
    ///
    /// 参数：`link_id` 是拖动源链接，`parent_id` 为空时表示树根，否则必须是现有目录。
    /// 返回值：移动实际生效时返回 `true`；同目录放下或校验失败时返回 `false`。
    pub(crate) fn move_connection_link(
        &mut self,
        link_id: ConnectionNodeId,
        parent_id: Option<ConnectionNodeId>,
    ) -> bool {
        let target_label = parent_id
            .and_then(|directory_id| self.config.connections.directory(directory_id))
            .map(|directory| format!("目录「{}」", directory.name))
            .unwrap_or_else(|| "根目录".to_string());
        match self.config.connections.move_link(link_id, parent_id) {
            Ok(true) => {
                self.selected_connection_node_id = Some(link_id);
                self.placeholder_notice = format!("已将链接移动到{target_label}");
                self.persist_config_or_report();
                true
            }
            Ok(false) => {
                self.selected_connection_node_id = Some(link_id);
                self.placeholder_notice = format!("链接已位于{target_label}");
                false
            }
            Err(error) => {
                self.selected_connection_node_id = Some(link_id);
                self.placeholder_notice = format!("无法移动链接：{error}");
                false
            }
        }
    }

    /// 点击链接树节点；目录执行展开折叠，SSH 链接打开终端标签。
    pub(crate) fn handle_connection_tree_click(
        &mut self,
        node_id: ConnectionNodeId,
        cx: &mut Context<Self>,
    ) {
        if self.is_connection_tree_search_open {
            self.close_connection_tree_search();
        }
        self.selected_connection_node_id = Some(node_id);
        if self.config.connections.is_directory(node_id) {
            self.toggle_connection_directory(node_id);
            return;
        }
        match self
            .config
            .connections
            .link(node_id)
            .and_then(|link| link.protocol())
        {
            Some(ConnectionLinkKind::Ssh) => self.open_or_focus_ssh_terminal(node_id, cx),
            Some(ConnectionLinkKind::Smb) => self.open_smb_file_manager_from_link(node_id, cx),
            Some(ConnectionLinkKind::Git) => {
                self.open_repository_file_manager_from_link(node_id, RemoteFileBackend::Git, cx)
            }
            Some(ConnectionLinkKind::Svn) => {
                self.open_repository_file_manager_from_link(node_id, RemoteFileBackend::Svn, cx)
            }
            None => self.placeholder_notice = "链接配置不完整，无法打开".to_string(),
        }
    }

    /// 切换指定链接目录展开状态。
    pub(crate) fn toggle_connection_directory(&mut self, directory_id: ConnectionNodeId) {
        if !self
            .config
            .connections
            .toggle_directory_expanded(directory_id)
        {
            self.placeholder_notice = "未找到可展开的链接目录".to_string();
            return;
        }
        self.persist_config_or_report();
        self.placeholder_notice = "已切换链接目录展开状态".to_string();
    }

    /// 打开新增目录模态框，父目录根据当前选中节点推导。
    ///
    /// 参数说明：
    /// - `cx`：主应用上下文，用于创建或更新模态框子视图。
    pub(crate) fn open_new_connection_directory_dialog(&mut self, cx: &mut Context<Self>) {
        let parent_id = self
            .config
            .connections
            .parent_for_new_directory(self.selected_connection_node_id);
        let initial_form = ConnectionDirectoryFormState {
            parent_id,
            name_input: TextInputState::default(),
            error_message: None,
        };

        if let Some(modal) = self.connection_directory_modal.clone() {
            modal.update(cx, |modal_state, cx| {
                if !modal_state.is_mode(ConnectionDirectoryWindowMode::Create) {
                    modal_state
                        .replace_form(initial_form.clone(), ConnectionDirectoryWindowMode::Create);
                    cx.notify();
                }
            });
            self.placeholder_notice = "新增目录模态框已打开".to_string();
            return;
        }

        self.open_connection_directory_window_with_form(
            initial_form,
            ConnectionDirectoryWindowMode::Create,
            cx,
        );
    }

    /// 打开新增 SSH 链接模态框，父目录根据当前选中节点推导。
    ///
    /// 参数说明：
    /// - `cx`：主应用上下文，用于创建或更新模态框子视图。
    pub(crate) fn open_new_ssh_link_dialog(&mut self, cx: &mut Context<Self>) {
        let parent_id = self
            .config
            .connections
            .parent_for_new_link(self.selected_connection_node_id);
        let mut initial_form = ConnectionLinkFormState {
            link_kind: ConnectionLinkKind::Ssh,
            parent_id,
            name_input: TextInputState::default(),
            host_input: TextInputState::default(),
            url_input: TextInputState::default(),
            port_input: TextInputState::from_value("22".to_string()),
            username_input: TextInputState::default(),
            credential_id: None,
            save_as_credential: false,
            password_input: TextInputState::default(),
            share_input: TextInputState::default(),
            initial_dir_input: TextInputState::from_value("/".to_string()),
            domain_input: TextInputState::default(),
            private_key_path_input: TextInputState::default(),
            private_key_passphrase_input: TextInputState::default(),
            error_message: None,
        };
        apply_default_credential(&mut initial_form, &self.config.connections);

        if let Some(modal) = self.connection_link_modal.clone() {
            modal.update(cx, |modal_state, cx| {
                if !modal_state.is_mode(ConnectionLinkWindowMode::Create)
                    || modal_state.link_kind() != ConnectionLinkKind::Ssh
                {
                    modal_state
                        .replace_form(initial_form.clone(), ConnectionLinkWindowMode::Create);
                    cx.notify();
                }
            });
            self.placeholder_notice = "新增链接模态框已打开".to_string();
            return;
        }

        self.open_connection_link_window_with_form(
            initial_form,
            ConnectionLinkWindowMode::Create,
            cx,
        );
    }

    /// 打开新增 SMB 链接模态框，父目录根据当前选中节点推导。
    pub(crate) fn open_new_smb_link_dialog(&mut self, cx: &mut Context<Self>) {
        let parent_id = self
            .config
            .connections
            .parent_for_new_link(self.selected_connection_node_id);
        let mut initial_form = ConnectionLinkFormState {
            link_kind: ConnectionLinkKind::Smb,
            parent_id,
            name_input: TextInputState::default(),
            host_input: TextInputState::default(),
            url_input: TextInputState::default(),
            port_input: TextInputState::from_value("445".to_string()),
            username_input: TextInputState::default(),
            credential_id: None,
            save_as_credential: false,
            password_input: TextInputState::default(),
            share_input: TextInputState::default(),
            initial_dir_input: TextInputState::from_value("/".to_string()),
            domain_input: TextInputState::default(),
            private_key_path_input: TextInputState::default(),
            private_key_passphrase_input: TextInputState::default(),
            error_message: None,
        };
        apply_default_credential(&mut initial_form, &self.config.connections);

        if let Some(modal) = self.connection_link_modal.clone() {
            modal.update(cx, |modal_state, cx| {
                if !modal_state.is_mode(ConnectionLinkWindowMode::Create)
                    || modal_state.link_kind() != ConnectionLinkKind::Smb
                {
                    modal_state
                        .replace_form(initial_form.clone(), ConnectionLinkWindowMode::Create);
                    cx.notify();
                }
            });
            self.placeholder_notice = "新增链接模态框已打开".to_string();
            return;
        }

        self.open_connection_link_window_with_form(
            initial_form,
            ConnectionLinkWindowMode::Create,
            cx,
        );
    }

    /// 打开新增 Git 仓库链接模态框，父目录根据当前选中节点推导。
    pub(crate) fn open_new_git_link_dialog(&mut self, cx: &mut Context<Self>) {
        let form = self.new_repository_link_form(ConnectionLinkKind::Git);
        self.open_or_replace_link_create_modal(form, cx);
    }

    /// 打开新增 SVN 仓库链接模态框，父目录根据当前选中节点推导。
    pub(crate) fn open_new_svn_link_dialog(&mut self, cx: &mut Context<Self>) {
        let form = self.new_repository_link_form(ConnectionLinkKind::Svn);
        self.open_or_replace_link_create_modal(form, cx);
    }

    /// 构造 Git/SVN 共用的空仓库链接表单。
    fn new_repository_link_form(&self, link_kind: ConnectionLinkKind) -> ConnectionLinkFormState {
        let mut form = ConnectionLinkFormState {
            link_kind,
            parent_id: self
                .config
                .connections
                .parent_for_new_link(self.selected_connection_node_id),
            name_input: TextInputState::default(),
            host_input: TextInputState::default(),
            url_input: TextInputState::default(),
            port_input: TextInputState::default(),
            username_input: TextInputState::default(),
            credential_id: None,
            save_as_credential: false,
            password_input: TextInputState::default(),
            share_input: TextInputState::default(),
            initial_dir_input: TextInputState::from_value("/".to_string()),
            domain_input: TextInputState::default(),
            private_key_path_input: TextInputState::default(),
            private_key_passphrase_input: TextInputState::default(),
            error_message: None,
        };
        apply_default_credential(&mut form, &self.config.connections);
        form
    }

    /// 复用已存在的链接模态框，或为指定仓库协议创建新的新增模态框。
    fn open_or_replace_link_create_modal(
        &mut self,
        initial_form: ConnectionLinkFormState,
        cx: &mut Context<Self>,
    ) {
        if let Some(modal) = self.connection_link_modal.clone() {
            modal.update(cx, |modal_state, cx| {
                if !modal_state.is_mode(ConnectionLinkWindowMode::Create)
                    || modal_state.link_kind() != initial_form.link_kind
                {
                    modal_state
                        .replace_form(initial_form.clone(), ConnectionLinkWindowMode::Create);
                    cx.notify();
                }
            });
            self.placeholder_notice = "新增仓库链接模态框已打开".to_string();
            return;
        }
        self.open_connection_link_window_with_form(
            initial_form,
            ConnectionLinkWindowMode::Create,
            cx,
        );
    }

    /// 打开共用凭据管理模态框；优先勾选触发入口对应的协议。
    pub(crate) fn open_connection_credential_manager(
        &mut self,
        preferred_protocol: ConnectionLinkKind,
        cx: &mut Context<Self>,
    ) {
        let app_entity = cx.entity();
        let initial_theme = self.theme.clone();
        self.connection_credential_modal = Some(cx.new(|cx| {
            ConnectionCredentialWindow::new(app_entity, initial_theme, preferred_protocol, cx)
        }));
        self.placeholder_notice = "请输入共用凭据信息".to_string();
        cx.notify();
    }

    /// 关闭共用凭据管理模态框。
    pub(crate) fn close_connection_credential_manager(&mut self) {
        self.connection_credential_modal = None;
    }

    /// 从表单新建共用凭据并持久化。
    pub(crate) fn create_connection_credential_from_form(
        &mut self,
        form: &ConnectionCredentialFormState,
        cx: &mut Context<Self>,
    ) -> Result<ConnectionCredentialId, String> {
        let previous_connections = self.config.connections.clone();
        let previous_selection = self.selected_connection_node_id;
        let protocols = credential_form_protocols(form);
        let result = self
            .config
            .connections
            .create_credential(
                &form.name_input.value,
                &form.username_input.value,
                optional_form_text(&form.domain_input.value),
                protocols,
                form.is_default,
                &form.password_input.value,
            )
            .map_err(|error| error.to_string());
        let credential_id = match result {
            Ok(credential_id) => credential_id,
            Err(message) => {
                self.placeholder_notice = message.clone();
                return Err(message);
            }
        };
        self.persist_connection_form_change_or_rollback(previous_connections, previous_selection)?;
        self.notify_connection_link_modal(cx);
        self.placeholder_notice = "已保存共用凭据".to_string();
        Ok(credential_id)
    }

    /// 从表单更新共用凭据并持久化；密码留空表示不修改。
    pub(crate) fn update_connection_credential_from_form(
        &mut self,
        form: &ConnectionCredentialFormState,
        cx: &mut Context<Self>,
    ) -> Result<(), String> {
        let Some(credential_id) = form.selected_id else {
            return Err("未选择要更新的共用凭据".to_string());
        };
        let protocols = credential_form_protocols(form);
        if let Some(modal) = self.connection_link_modal.clone() {
            let (selected, protocol) = modal.read_with(cx, |window_state, _| {
                (
                    window_state.selected_credential_id(),
                    window_state.link_kind(),
                )
            });
            if selected == Some(credential_id) && !protocols.contains(&protocol) {
                let message = "当前连接表单正在使用该凭据，不能移除它对应的协议".to_string();
                self.placeholder_notice = message.clone();
                return Err(message);
            }
        }
        let previous_connections = self.config.connections.clone();
        let previous_selection = self.selected_connection_node_id;
        let password =
            (!form.password_input.value.is_empty()).then_some(form.password_input.value.as_str());
        let result = self
            .config
            .connections
            .update_credential(
                credential_id,
                &form.name_input.value,
                &form.username_input.value,
                optional_form_text(&form.domain_input.value),
                protocols,
                form.is_default,
                password,
            )
            .map_err(|error| error.to_string());
        if let Err(message) = result {
            self.placeholder_notice = message.clone();
            return Err(message);
        }
        self.persist_connection_form_change_or_rollback(previous_connections, previous_selection)?;
        self.notify_connection_link_modal(cx);
        self.placeholder_notice = "已更新共用凭据".to_string();
        Ok(())
    }

    /// 删除未被任何连接引用的共用凭据并持久化。
    pub(crate) fn delete_connection_credential(
        &mut self,
        credential_id: ConnectionCredentialId,
        cx: &mut Context<Self>,
    ) -> Result<(), String> {
        if let Some(modal) = self.connection_link_modal.clone()
            && modal.read_with(cx, |window_state, _| {
                window_state.selected_credential_id() == Some(credential_id)
            })
        {
            let message = "当前连接表单正在使用该凭据，请先更换凭据或关闭表单".to_string();
            self.placeholder_notice = message.clone();
            return Err(message);
        }
        let previous_connections = self.config.connections.clone();
        let previous_selection = self.selected_connection_node_id;
        let result = self
            .config
            .connections
            .delete_credential(credential_id)
            .map_err(|error| error.to_string());
        if let Err(message) = result {
            self.placeholder_notice = message.clone();
            return Err(message);
        }
        self.persist_connection_form_change_or_rollback(previous_connections, previous_selection)?;
        self.notify_connection_link_modal(cx);
        self.placeholder_notice = "已删除共用凭据".to_string();
        Ok(())
    }

    /// 凭据列表变化后通知打开中的链接表单刷新下拉选项。
    fn notify_connection_link_modal(&self, cx: &mut Context<Self>) {
        if let Some(modal) = self.connection_link_modal.clone() {
            modal.update(cx, |_, modal_cx| modal_cx.notify());
        }
    }

    /// 清理目录表单模态框状态；关闭按钮、取消按钮和提交成功后统一调用。
    pub(crate) fn close_connection_directory_window(&mut self) {
        self.connection_directory_modal = None;
        self.placeholder_notice = "已关闭目录模态框".to_string();
    }

    /// 目录创建或编辑成功后清理模态框实体，不覆盖成功提示。
    pub(crate) fn finish_connection_directory_window(&mut self) {
        self.connection_directory_modal = None;
    }

    /// 清理链接表单模态框状态；关闭按钮、取消按钮和提交成功后统一调用。
    pub(crate) fn close_connection_link_window(&mut self) {
        self.connection_link_modal = None;
        self.placeholder_notice = "已关闭链接模态框".to_string();
    }

    /// 链接创建或编辑成功后清理模态框实体，不覆盖成功提示。
    pub(crate) fn finish_connection_link_window(&mut self) {
        self.connection_link_modal = None;
    }

    /// 打开连接节点编辑模态框；目录、SSH 和 SMB 链接按节点类型复用对应表单。
    pub(crate) fn open_edit_connection_node_window(
        &mut self,
        node_id: ConnectionNodeId,
        cx: &mut Context<Self>,
    ) {
        if self.config.connections.is_directory(node_id) {
            self.open_edit_connection_directory_window(node_id, cx);
        } else if let Some(link) = self.config.connections.link(node_id) {
            match link.protocol() {
                Some(ConnectionLinkKind::Ssh) => self.open_edit_ssh_link_window(node_id, cx),
                Some(ConnectionLinkKind::Smb) => self.open_edit_smb_link_window(node_id, cx),
                Some(ConnectionLinkKind::Git) => self.open_edit_git_link_window(node_id, cx),
                Some(ConnectionLinkKind::Svn) => self.open_edit_svn_link_window(node_id, cx),
                None => self.placeholder_notice = "链接配置不完整，无法编辑".to_string(),
            }
        } else {
            self.placeholder_notice = "未找到可编辑的连接节点".to_string();
        }
    }

    /// 打开编辑目录模态框。
    fn open_edit_connection_directory_window(
        &mut self,
        directory_id: ConnectionNodeId,
        cx: &mut Context<Self>,
    ) {
        let Some(directory) = self.config.connections.directory(directory_id).cloned() else {
            self.placeholder_notice = "未找到可编辑的目录".to_string();
            return;
        };
        let initial_form = ConnectionDirectoryFormState {
            parent_id: directory.parent_id,
            name_input: TextInputState::from_value(directory.name),
            error_message: None,
        };
        let mode = ConnectionDirectoryWindowMode::Edit { directory_id };

        if let Some(modal) = self.connection_directory_modal.clone() {
            modal.update(cx, |modal_state, cx| {
                modal_state.replace_form(initial_form.clone(), mode);
                cx.notify();
            });
            self.placeholder_notice = "已打开目录编辑模态框".to_string();
            return;
        }

        self.open_connection_directory_window_with_form(initial_form, mode, cx);
    }

    /// 打开编辑 SSH 链接模态框。
    fn open_edit_ssh_link_window(&mut self, link_id: ConnectionNodeId, cx: &mut Context<Self>) {
        let Some(link) = self.config.connections.link(link_id).cloned() else {
            self.placeholder_notice = "未找到可编辑的 SSH 链接".to_string();
            return;
        };
        let Some(ssh) = link.ssh_config().cloned() else {
            self.placeholder_notice = "当前链接不是 SSH 链接".to_string();
            return;
        };
        let initial_form = ConnectionLinkFormState {
            link_kind: ConnectionLinkKind::Ssh,
            parent_id: link.parent_id,
            name_input: TextInputState::from_value(link.name),
            host_input: TextInputState::from_value(ssh.host),
            url_input: TextInputState::default(),
            port_input: TextInputState::from_value(ssh.port.to_string()),
            username_input: TextInputState::from_value(ssh.username),
            credential_id: ssh.credential_id,
            save_as_credential: false,
            password_input: TextInputState::from_value(ssh.password),
            share_input: TextInputState::default(),
            initial_dir_input: TextInputState::from_value("/".to_string()),
            domain_input: TextInputState::default(),
            private_key_path_input: TextInputState::from_value(
                ssh.private_key_path.unwrap_or_default(),
            ),
            private_key_passphrase_input: TextInputState::from_value(
                ssh.private_key_passphrase.unwrap_or_default(),
            ),
            error_message: None,
        };
        let mode = ConnectionLinkWindowMode::Edit { link_id };

        if let Some(modal) = self.connection_link_modal.clone() {
            modal.update(cx, |modal_state, cx| {
                modal_state.replace_form(initial_form.clone(), mode);
                cx.notify();
            });
            self.placeholder_notice = "已打开链接编辑模态框".to_string();
            return;
        }

        self.open_connection_link_window_with_form(initial_form, mode, cx);
    }

    /// 打开编辑 SMB 链接模态框。
    fn open_edit_smb_link_window(&mut self, link_id: ConnectionNodeId, cx: &mut Context<Self>) {
        let Some(link) = self.config.connections.link(link_id).cloned() else {
            self.placeholder_notice = "未找到可编辑的 SMB 链接".to_string();
            return;
        };
        let Some(smb) = link.smb_config().cloned() else {
            self.placeholder_notice = "当前链接不是 SMB 链接".to_string();
            return;
        };
        let initial_form = ConnectionLinkFormState {
            link_kind: ConnectionLinkKind::Smb,
            parent_id: link.parent_id,
            name_input: TextInputState::from_value(link.name),
            host_input: TextInputState::from_value(smb.host),
            url_input: TextInputState::default(),
            port_input: TextInputState::from_value(smb.port.to_string()),
            username_input: TextInputState::from_value(smb.username),
            credential_id: smb.credential_id,
            save_as_credential: false,
            password_input: TextInputState::from_value(smb.password),
            share_input: TextInputState::from_value(smb.share),
            initial_dir_input: TextInputState::from_value(smb.initial_dir),
            domain_input: TextInputState::from_value(smb.domain.unwrap_or_default()),
            private_key_path_input: TextInputState::default(),
            private_key_passphrase_input: TextInputState::default(),
            error_message: None,
        };
        let mode = ConnectionLinkWindowMode::Edit { link_id };

        if let Some(modal) = self.connection_link_modal.clone() {
            modal.update(cx, |modal_state, cx| {
                modal_state.replace_form(initial_form.clone(), mode);
                cx.notify();
            });
            self.placeholder_notice = "已打开链接编辑模态框".to_string();
            return;
        }

        self.open_connection_link_window_with_form(initial_form, mode, cx);
    }

    /// 打开编辑 Git 链接模态框。
    fn open_edit_git_link_window(&mut self, link_id: ConnectionNodeId, cx: &mut Context<Self>) {
        let Some(link) = self.config.connections.link(link_id).cloned() else {
            self.placeholder_notice = "未找到可编辑的 Git 链接".to_string();
            return;
        };
        let Some(git) = link.git_config().cloned() else {
            self.placeholder_notice = "当前链接不是 Git 链接".to_string();
            return;
        };
        let form = repository_edit_form(
            ConnectionLinkKind::Git,
            link.parent_id,
            link.name,
            git.url,
            git.username,
            None,
            git.access_token,
            git.private_key_path,
            git.private_key_passphrase,
        );
        self.open_or_replace_link_edit_modal(link_id, form, cx);
    }

    /// 打开编辑 SVN 链接模态框。
    fn open_edit_svn_link_window(&mut self, link_id: ConnectionNodeId, cx: &mut Context<Self>) {
        let Some(link) = self.config.connections.link(link_id).cloned() else {
            self.placeholder_notice = "未找到可编辑的 SVN 链接".to_string();
            return;
        };
        let Some(svn) = link.svn_config().cloned() else {
            self.placeholder_notice = "当前链接不是 SVN 链接".to_string();
            return;
        };
        let form = repository_edit_form(
            ConnectionLinkKind::Svn,
            link.parent_id,
            link.name,
            svn.url,
            svn.username,
            svn.credential_id,
            svn.password,
            svn.private_key_path,
            svn.private_key_passphrase,
        );
        self.open_or_replace_link_edit_modal(link_id, form, cx);
    }

    /// 复用已存在的链接模态框，或为指定仓库链接创建编辑模态框。
    fn open_or_replace_link_edit_modal(
        &mut self,
        link_id: ConnectionNodeId,
        form: ConnectionLinkFormState,
        cx: &mut Context<Self>,
    ) {
        let mode = ConnectionLinkWindowMode::Edit { link_id };
        if let Some(modal) = self.connection_link_modal.clone() {
            modal.update(cx, |modal_state, cx| {
                modal_state.replace_form(form.clone(), mode);
                cx.notify();
            });
            self.placeholder_notice = "已打开仓库链接编辑模态框".to_string();
            return;
        }
        self.open_connection_link_window_with_form(form, mode, cx);
    }

    /// 使用指定表单打开目录模态框，供新增和编辑入口复用。
    fn open_connection_directory_window_with_form(
        &mut self,
        initial_form: ConnectionDirectoryFormState,
        mode: ConnectionDirectoryWindowMode,
        cx: &mut Context<Self>,
    ) {
        let app_entity = cx.entity();
        let initial_theme = self.theme.clone();
        self.placeholder_notice = match mode {
            ConnectionDirectoryWindowMode::Create => "请输入链接目录名称".to_string(),
            ConnectionDirectoryWindowMode::Edit { .. } => "请编辑链接目录名称".to_string(),
        };

        self.connection_directory_modal = Some(cx.new(|cx| {
            ConnectionDirectoryWindow::new(app_entity, initial_theme, initial_form, mode, cx)
        }));
    }

    /// 使用指定表单打开链接模态框，供 SSH、SMB 的新增和编辑入口复用。
    fn open_connection_link_window_with_form(
        &mut self,
        initial_form: ConnectionLinkFormState,
        mode: ConnectionLinkWindowMode,
        cx: &mut Context<Self>,
    ) {
        let app_entity = cx.entity();
        let initial_theme = self.theme.clone();
        let protocol_label = match initial_form.link_kind {
            ConnectionLinkKind::Ssh => "SSH",
            ConnectionLinkKind::Smb => "SMB",
            ConnectionLinkKind::Git => "Git",
            ConnectionLinkKind::Svn => "SVN",
        };
        self.placeholder_notice = match mode {
            ConnectionLinkWindowMode::Create => format!("请输入 {protocol_label} 链接信息"),
            ConnectionLinkWindowMode::Edit { .. } => format!("请编辑 {protocol_label} 链接信息"),
        };

        self.connection_link_modal = Some(cx.new(|cx| {
            ConnectionLinkWindow::new(app_entity, initial_theme, initial_form, mode, cx)
        }));
    }

    /// 关闭当前链接工作区弹窗。
    pub(crate) fn close_connection_dialog(&mut self) {
        if let Some(ConnectionDialogState::SvnCertificate(prompt)) = self.connection_dialog.clone()
        {
            self.respond_svn_certificate_prompt(
                prompt,
                crate::remote::svn_certificate::SvnCertificateDecision::Reject,
            );
            return;
        }
        if let Some(ConnectionDialogState::ConfirmHostKey(prompt)) = self.connection_dialog.clone()
        {
            self.reject_connection_host_key_prompt(prompt);
            return;
        }
        self.connection_dialog = None;
        self.placeholder_notice = "已关闭链接弹窗".to_string();
        self.open_next_svn_certificate_prompt();
    }

    /// 提交当前链接工作区弹窗。
    pub(crate) fn submit_connection_dialog(&mut self) {
        match self.connection_dialog.clone() {
            // 证书信任必须明确点击相应按钮，避免 Enter 在意外弹出的确认框中授权。
            Some(ConnectionDialogState::SvnCertificate(_)) => {}
            Some(ConnectionDialogState::ConfirmHostKey(prompt)) => {
                self.confirm_connection_host_key_prompt(prompt)
            }
            Some(ConnectionDialogState::ConfirmDelete(prompt)) => {
                self.confirm_delete_connection_node(prompt.node_id)
            }
            None => {}
        }
    }

    /// 聚焦链接相关输入框，并清理其他链接输入框焦点态。
    pub(crate) fn focus_connection_text_input_target(&mut self, target: AppTextInputTarget) {
        self.clear_connection_text_input_focuses();
        if let Some(input) = self.connection_text_input_mut(target) {
            input.is_focused = true;
            input.cursor = character_count(&input.value);
            input.selection_anchor = None;
            input.marked_range = None;
            input.selection_drag = None;
        }
    }

    /// 清理链接树和链接表单输入框焦点态。
    pub(crate) fn clear_connection_text_input_focuses(&mut self) {
        clear_connection_input_focus(&mut self.connection_tree_search_input);
    }

    /// 返回链接相关输入框的只读引用。
    pub(crate) fn connection_text_input(
        &self,
        target: AppTextInputTarget,
    ) -> Option<&TextInputState> {
        match target {
            AppTextInputTarget::ConnectionTreeSearch => Some(&self.connection_tree_search_input),
            _ => None,
        }
    }

    /// 返回链接相关输入框的可变引用。
    pub(crate) fn connection_text_input_mut(
        &mut self,
        target: AppTextInputTarget,
    ) -> Option<&mut TextInputState> {
        match target {
            AppTextInputTarget::ConnectionTreeSearch => {
                Some(&mut self.connection_tree_search_input)
            }
            _ => None,
        }
    }

    /// 处理链接树或链接表单输入框的非文本按键。
    pub(crate) fn handle_connection_text_input_key(
        &mut self,
        target: AppTextInputTarget,
        keystroke: &Keystroke,
    ) {
        match keystroke.key.as_str() {
            "escape" => {
                if target == AppTextInputTarget::ConnectionTreeSearch {
                    self.close_connection_tree_search();
                } else {
                    self.close_connection_dialog();
                }
            }
            "enter" => {
                if target == AppTextInputTarget::ConnectionTreeSearch {
                    self.placeholder_notice = format!(
                        "链接过滤「{}」命中 {} 个节点",
                        self.connection_tree_search_input.value,
                        self.visible_connection_rows().len()
                    );
                } else {
                    self.submit_connection_dialog();
                }
            }
            "backspace" => self.delete_connection_input_backward(target),
            "delete" => self.delete_connection_input_forward(target),
            "left" | "arrowleft" => {
                self.move_connection_input_left(target, keystroke.modifiers.shift)
            }
            "right" | "arrowright" => {
                self.move_connection_input_right(target, keystroke.modifiers.shift)
            }
            "home" => self.move_connection_input_cursor(target, 0, keystroke.modifiers.shift),
            "end" => {
                let end = self
                    .connection_text_input(target)
                    .map(|input| character_count(&input.value))
                    .unwrap_or_default();
                self.move_connection_input_cursor(target, end, keystroke.modifiers.shift);
            }
            _ => {
                if let Some(key_char) = keystroke.key_char.as_ref()
                    && !keystroke.modifiers.control
                    && !keystroke.modifiers.platform
                    && !key_char.chars().any(char::is_control)
                {
                    self.insert_connection_input_text(target, key_char);
                }
            }
        }
    }

    /// 鼠标开始选择链接输入框文本。
    pub(crate) fn begin_connection_input_pointer_selection(
        &mut self,
        target: AppTextInputTarget,
        character_index: usize,
        granularity: TextSelectionGranularity,
    ) {
        self.focus_connection_text_input_target(target);
        let Some(input) = self.connection_text_input_mut(target) else {
            return;
        };
        input.begin_pointer_selection(character_index, granularity);
    }

    /// 鼠标拖拽更新链接输入框选区。
    pub(crate) fn update_connection_input_pointer_selection(
        &mut self,
        target: AppTextInputTarget,
        character_index: usize,
    ) {
        let Some(input) = self.connection_text_input_mut(target) else {
            return;
        };
        input.update_pointer_selection(character_index);
    }

    /// 鼠标结束链接输入框文本选择。
    pub(crate) fn finish_connection_input_pointer_selection(&mut self, target: AppTextInputTarget) {
        if let Some(input) = self.connection_text_input_mut(target) {
            input.finish_pointer_selection();
        }
    }

    /// 设置主机指纹确认弹窗状态。
    pub(crate) fn open_host_key_prompt(
        &mut self,
        session_id: usize,
        link_id: ConnectionNodeId,
        pending: PendingHostKey,
    ) {
        self.connection_dialog = Some(ConnectionDialogState::ConfirmHostKey(
            ConnectionHostKeyPromptState {
                session_id,
                owner: crate::app::HostKeyPromptOwner::Terminal { session_id },
                link_id,
                host: pending.host,
                port: pending.port,
                fingerprint: pending.fingerprint,
            },
        ));
    }

    /// 校验并创建链接目录，供目录模态框提交时调用。
    pub(crate) fn create_connection_directory_from_form(
        &mut self,
        form: ConnectionDirectoryFormState,
    ) -> Result<ConnectionNodeId, String> {
        let previous_connections = self.config.connections.clone();
        let previous_selection = self.selected_connection_node_id;
        let directory_id = match self
            .config
            .connections
            .add_directory(form.parent_id, &form.name_input.value)
        {
            Ok(directory_id) => directory_id,
            Err(error) => {
                let message = error.to_string();
                self.placeholder_notice = message.clone();
                return Err(message);
            }
        };
        self.selected_connection_node_id = Some(directory_id);
        self.placeholder_notice = "已新增链接目录".to_string();
        self.persist_connection_form_change_or_rollback(previous_connections, previous_selection)?;
        Ok(directory_id)
    }

    /// 校验并创建链接，按表单协议分派给 SSH 或 SMB 创建逻辑。
    pub(crate) fn create_connection_link_from_form(
        &mut self,
        form: ConnectionLinkFormState,
    ) -> Result<ConnectionNodeId, String> {
        match form.link_kind {
            ConnectionLinkKind::Ssh => self.create_ssh_link_from_form(form),
            ConnectionLinkKind::Smb => self.create_smb_link_from_form(form),
            ConnectionLinkKind::Git => self.create_git_link_from_form(form),
            ConnectionLinkKind::Svn => self.create_svn_link_from_form(form),
        }
    }

    /// 校验并创建 SSH 链接，供表单模态框和兼容弹窗共同复用。
    pub(crate) fn create_ssh_link_from_form(
        &mut self,
        form: ConnectionLinkFormState,
    ) -> Result<ConnectionNodeId, String> {
        let ssh = match ssh_config_from_form(&form) {
            Ok(ssh) => ssh,
            Err(message) => {
                self.placeholder_notice = message.clone();
                return Err(message);
            }
        };
        let previous_connections = self.config.connections.clone();
        let previous_selection = self.selected_connection_node_id;
        let link_id =
            match self
                .config
                .connections
                .add_ssh_link(form.parent_id, &form.name_input.value, ssh)
            {
                Ok(link_id) => link_id,
                Err(error) => {
                    let message = error.to_string();
                    self.placeholder_notice = message.clone();
                    return Err(message);
                }
            };
        if let Err(message) = share_link_password_as_credential(self, &form, link_id) {
            self.config.connections = previous_connections;
            self.selected_connection_node_id = previous_selection;
            self.placeholder_notice = message.clone();
            return Err(message);
        }
        self.selected_connection_node_id = Some(link_id);
        self.placeholder_notice = "已新增 SSH 链接".to_string();
        self.persist_connection_form_change_or_rollback(previous_connections, previous_selection)?;
        Ok(link_id)
    }

    /// 校验并创建 SMB 链接，供表单模态框和兼容弹窗共同复用。
    pub(crate) fn create_smb_link_from_form(
        &mut self,
        form: ConnectionLinkFormState,
    ) -> Result<ConnectionNodeId, String> {
        let smb = match smb_config_from_form(&form) {
            Ok(smb) => smb,
            Err(message) => {
                self.placeholder_notice = message.clone();
                return Err(message);
            }
        };
        let previous_connections = self.config.connections.clone();
        let previous_selection = self.selected_connection_node_id;
        let link_id =
            match self
                .config
                .connections
                .add_smb_link(form.parent_id, &form.name_input.value, smb)
            {
                Ok(link_id) => link_id,
                Err(error) => {
                    let message = error.to_string();
                    self.placeholder_notice = message.clone();
                    return Err(message);
                }
            };
        if let Err(message) = share_link_password_as_credential(self, &form, link_id) {
            self.config.connections = previous_connections;
            self.selected_connection_node_id = previous_selection;
            self.placeholder_notice = message.clone();
            return Err(message);
        }
        self.selected_connection_node_id = Some(link_id);
        self.placeholder_notice = "已新增 SMB 链接".to_string();
        self.persist_connection_form_change_or_rollback(previous_connections, previous_selection)?;
        Ok(link_id)
    }

    /// 校验并创建 Git 只读仓库链接。
    pub(crate) fn create_git_link_from_form(
        &mut self,
        form: ConnectionLinkFormState,
    ) -> Result<ConnectionNodeId, String> {
        let git = git_config_from_form(&form).inspect_err(|message| {
            self.placeholder_notice = message.clone();
        })?;
        let previous_connections = self.config.connections.clone();
        let previous_selection = self.selected_connection_node_id;
        let link_id = self
            .config
            .connections
            .add_git_link(form.parent_id, &form.name_input.value, git)
            .map_err(|error| {
                let message = error.to_string();
                self.placeholder_notice = message.clone();
                message
            })?;
        self.selected_connection_node_id = Some(link_id);
        self.placeholder_notice = "已新增 Git 链接".to_string();
        self.persist_connection_form_change_or_rollback(previous_connections, previous_selection)?;
        Ok(link_id)
    }

    /// 校验并创建 SVN 只读仓库链接。
    pub(crate) fn create_svn_link_from_form(
        &mut self,
        form: ConnectionLinkFormState,
    ) -> Result<ConnectionNodeId, String> {
        let svn = svn_config_from_form(&form).inspect_err(|message| {
            self.placeholder_notice = message.clone();
        })?;
        let previous_connections = self.config.connections.clone();
        let previous_selection = self.selected_connection_node_id;
        let link_id = self
            .config
            .connections
            .add_svn_link(form.parent_id, &form.name_input.value, svn)
            .map_err(|error| {
                let message = error.to_string();
                self.placeholder_notice = message.clone();
                message
            })?;
        if let Err(message) = share_link_password_as_credential(self, &form, link_id) {
            self.config.connections = previous_connections;
            self.selected_connection_node_id = previous_selection;
            self.placeholder_notice = message.clone();
            return Err(message);
        }
        self.selected_connection_node_id = Some(link_id);
        self.placeholder_notice = "已新增 SVN 链接".to_string();
        self.persist_connection_form_change_or_rollback(previous_connections, previous_selection)?;
        Ok(link_id)
    }

    /// 校验并更新链接目录名称。
    pub(crate) fn update_connection_directory_from_form(
        &mut self,
        directory_id: ConnectionNodeId,
        form: ConnectionDirectoryFormState,
    ) -> Result<(), String> {
        let previous_connections = self.config.connections.clone();
        let previous_selection = self.selected_connection_node_id;
        match self
            .config
            .connections
            .update_directory(directory_id, &form.name_input.value)
        {
            Ok(()) => {
                self.selected_connection_node_id = Some(directory_id);
                self.placeholder_notice = "已更新链接目录".to_string();
                self.persist_connection_form_change_or_rollback(
                    previous_connections,
                    previous_selection,
                )
            }
            Err(error) => {
                let message = error.to_string();
                self.placeholder_notice = message.clone();
                Err(message)
            }
        }
    }

    /// 校验并更新 SSH 链接名称和连接参数。
    pub(crate) fn update_connection_link_from_form(
        &mut self,
        link_id: ConnectionNodeId,
        form: ConnectionLinkFormState,
    ) -> Result<(), String> {
        match form.link_kind {
            ConnectionLinkKind::Ssh => self.update_ssh_link_from_form(link_id, form),
            ConnectionLinkKind::Smb => self.update_smb_link_from_form(link_id, form),
            ConnectionLinkKind::Git => self.update_git_link_from_form(link_id, form),
            ConnectionLinkKind::Svn => self.update_svn_link_from_form(link_id, form),
        }
    }

    /// 校验并更新 SSH 链接名称和连接参数。
    pub(crate) fn update_ssh_link_from_form(
        &mut self,
        link_id: ConnectionNodeId,
        form: ConnectionLinkFormState,
    ) -> Result<(), String> {
        let ssh = match ssh_config_from_form(&form) {
            Ok(ssh) => ssh,
            Err(message) => {
                self.placeholder_notice = message.clone();
                return Err(message);
            }
        };
        let previous_connections = self.config.connections.clone();
        let previous_selection = self.selected_connection_node_id;
        match self
            .config
            .connections
            .update_ssh_link(link_id, &form.name_input.value, ssh)
        {
            Ok(()) => {
                self.move_link_to_form_parent_or_rollback(
                    link_id,
                    form.parent_id,
                    previous_connections.clone(),
                    previous_selection,
                )?;
                if let Err(message) = share_link_password_as_credential(self, &form, link_id) {
                    self.config.connections = previous_connections;
                    self.selected_connection_node_id = previous_selection;
                    self.placeholder_notice = message.clone();
                    return Err(message);
                }
                self.selected_connection_node_id = Some(link_id);
                self.placeholder_notice = "已更新 SSH 链接".to_string();
                self.persist_connection_form_change_or_rollback(
                    previous_connections,
                    previous_selection,
                )
            }
            Err(error) => {
                let message = error.to_string();
                self.placeholder_notice = message.clone();
                Err(message)
            }
        }
    }

    /// 校验并更新 SMB 链接名称和连接参数。
    pub(crate) fn update_smb_link_from_form(
        &mut self,
        link_id: ConnectionNodeId,
        form: ConnectionLinkFormState,
    ) -> Result<(), String> {
        let smb = match smb_config_from_form(&form) {
            Ok(smb) => smb,
            Err(message) => {
                self.placeholder_notice = message.clone();
                return Err(message);
            }
        };
        let previous_connections = self.config.connections.clone();
        let previous_selection = self.selected_connection_node_id;
        match self
            .config
            .connections
            .update_smb_link(link_id, &form.name_input.value, smb)
        {
            Ok(()) => {
                self.move_link_to_form_parent_or_rollback(
                    link_id,
                    form.parent_id,
                    previous_connections.clone(),
                    previous_selection,
                )?;
                if let Err(message) = share_link_password_as_credential(self, &form, link_id) {
                    self.config.connections = previous_connections;
                    self.selected_connection_node_id = previous_selection;
                    self.placeholder_notice = message.clone();
                    return Err(message);
                }
                self.selected_connection_node_id = Some(link_id);
                self.placeholder_notice = "已更新 SMB 链接".to_string();
                self.persist_connection_form_change_or_rollback(
                    previous_connections,
                    previous_selection,
                )
            }
            Err(error) => {
                let message = error.to_string();
                self.placeholder_notice = message.clone();
                Err(message)
            }
        }
    }

    /// 校验并更新 Git 仓库链接；成功后关闭该链接的旧文件管理会话。
    pub(crate) fn update_git_link_from_form(
        &mut self,
        link_id: ConnectionNodeId,
        form: ConnectionLinkFormState,
    ) -> Result<(), String> {
        let git = git_config_from_form(&form).inspect_err(|message| {
            self.placeholder_notice = message.clone();
        })?;
        let previous_connections = self.config.connections.clone();
        let previous_selection = self.selected_connection_node_id;
        let previous_url = self
            .config
            .connections
            .link(link_id)
            .and_then(ConnectionLinkConfig::git_config)
            .map(|old| old.url.clone());
        let url_changed = previous_url
            .as_deref()
            .is_some_and(|old_url| old_url != git.url);
        self.config
            .connections
            .update_git_link(link_id, &form.name_input.value, git)
            .map_err(|error| error.to_string())?;
        self.move_link_to_form_parent_or_rollback(
            link_id,
            form.parent_id,
            previous_connections.clone(),
            previous_selection,
        )?;
        self.selected_connection_node_id = Some(link_id);
        self.placeholder_notice = "已更新 Git 链接".to_string();
        self.persist_connection_form_change_or_rollback(previous_connections, previous_selection)?;
        self.disconnect_remote_file_sessions_for_link(link_id);
        if url_changed {
            let cache_root = self
                .config_manager
                .settings_path()
                .parent()
                .map(crate::config::paths::argus_git_repositories_dir_from_config)
                .unwrap_or_else(crate::config::paths::argus_git_repositories_dir);
            let _ = crate::remote::git::schedule_git_cache_removal_at(
                cache_root,
                link_id,
                previous_url,
            );
        }
        Ok(())
    }

    /// 校验并更新 SVN 仓库链接；成功后关闭该链接的旧文件管理会话。
    pub(crate) fn update_svn_link_from_form(
        &mut self,
        link_id: ConnectionNodeId,
        form: ConnectionLinkFormState,
    ) -> Result<(), String> {
        let svn = svn_config_from_form(&form).inspect_err(|message| {
            self.placeholder_notice = message.clone();
        })?;
        let previous_connections = self.config.connections.clone();
        let previous_selection = self.selected_connection_node_id;
        self.config
            .connections
            .update_svn_link(link_id, &form.name_input.value, svn)
            .map_err(|error| error.to_string())?;
        self.move_link_to_form_parent_or_rollback(
            link_id,
            form.parent_id,
            previous_connections.clone(),
            previous_selection,
        )?;
        if let Err(message) = share_link_password_as_credential(self, &form, link_id) {
            self.config.connections = previous_connections;
            self.selected_connection_node_id = previous_selection;
            self.placeholder_notice = message.clone();
            return Err(message);
        }
        self.selected_connection_node_id = Some(link_id);
        self.placeholder_notice = "已更新 SVN 链接".to_string();
        self.persist_connection_form_change_or_rollback(previous_connections, previous_selection)?;
        self.disconnect_remote_file_sessions_for_link(link_id);
        Ok(())
    }

    /// 把编辑中的链接移动到表单所选目录；失败时回滚整次提交。
    fn move_link_to_form_parent_or_rollback(
        &mut self,
        link_id: ConnectionNodeId,
        parent_id: Option<ConnectionNodeId>,
        previous_connections: ConnectionConfig,
        previous_selection: Option<ConnectionNodeId>,
    ) -> Result<(), String> {
        match self.config.connections.move_link(link_id, parent_id) {
            Ok(_) => Ok(()),
            Err(error) => {
                self.config.connections = previous_connections;
                self.selected_connection_node_id = previous_selection;
                let message = format!("无法移动到所选目录：{error}");
                self.placeholder_notice = message.clone();
                Err(message)
            }
        }
    }

    /// 持久化由模态表单触发的连接配置变更；失败时恢复提交前状态并让对话框继续显示错误。
    ///
    /// 参数：`previous_connections` 和 `previous_selection` 是提交表单前的完整连接状态。
    /// 返回值：设置文件原子写入成功时返回成功；失败时回滚并返回可直接展示的错误文案。
    fn persist_connection_form_change_or_rollback(
        &mut self,
        previous_connections: ConnectionConfig,
        previous_selection: Option<ConnectionNodeId>,
    ) -> Result<(), String> {
        // 共用凭据密码只保存在凭据记录中；保存前回填到连接内存字段，保证保存后立即可连接。
        self.config.connections.rehydrate_credential_passwords(None);
        if let Err(error) = self.config_manager.save(&self.config) {
            self.config.connections = previous_connections;
            self.selected_connection_node_id = previous_selection;
            let message = format!("设置保存失败，连接配置更改已回滚：{error}");
            self.placeholder_notice = message.clone();
            return Err(message);
        }
        Ok(())
    }

    /// 请求删除链接目录或 SSH 链接；删除前必须进入二次确认。
    pub(crate) fn request_delete_connection_node(&mut self, node_id: ConnectionNodeId) {
        if let Some(directory) = self.config.connections.directory(node_id) {
            if !self.config.connections.is_directory_empty(node_id) {
                self.selected_connection_node_id = Some(node_id);
                self.connection_dialog = None;
                self.placeholder_notice = "目录不为空，不能删除".to_string();
                return;
            }
            self.selected_connection_node_id = Some(node_id);
            self.connection_dialog = Some(ConnectionDialogState::ConfirmDelete(
                ConnectionDeletePromptState {
                    node_id,
                    label: directory.name.clone(),
                    is_directory: true,
                },
            ));
            self.placeholder_notice = "请确认是否删除链接目录".to_string();
            return;
        }

        if let Some(link) = self.config.connections.link(node_id) {
            self.selected_connection_node_id = Some(node_id);
            let protocol_label = match link.protocol() {
                Some(ConnectionLinkKind::Ssh) => "SSH",
                Some(ConnectionLinkKind::Smb) => "SMB",
                Some(ConnectionLinkKind::Git) => "Git",
                Some(ConnectionLinkKind::Svn) => "SVN",
                None => "远程",
            };
            self.connection_dialog = Some(ConnectionDialogState::ConfirmDelete(
                ConnectionDeletePromptState {
                    node_id,
                    label: link.name.clone(),
                    is_directory: false,
                },
            ));
            self.placeholder_notice = format!("请确认是否删除 {protocol_label} 链接");
            return;
        }

        self.placeholder_notice = "未找到可删除的连接节点".to_string();
    }

    /// 确认删除链接目录或 SSH 链接；目录只能在没有子节点时删除。
    pub(crate) fn confirm_delete_connection_node(&mut self, node_id: ConnectionNodeId) {
        self.delete_connection_node(node_id);
        self.connection_dialog = None;
        self.open_next_svn_certificate_prompt();
    }

    /// 执行链接目录或 SSH 链接删除，调用方需先完成二次确认。
    fn delete_connection_node(&mut self, node_id: ConnectionNodeId) {
        let fallback_selection = self.config.connections.parent_id_for_node(node_id);
        match self.config.connections.delete_node(node_id) {
            Ok(ConnectionDeletedNodeKind::Directory) => {
                if self.selected_connection_node_id == Some(node_id) {
                    self.selected_connection_node_id = fallback_selection;
                }
                self.persist_config_or_report();
                self.placeholder_notice = "已删除链接目录".to_string();
            }
            Ok(ConnectionDeletedNodeKind::SshLink) => {
                if self.selected_connection_node_id == Some(node_id) {
                    self.selected_connection_node_id = fallback_selection;
                }
                self.persist_config_or_report();
                self.placeholder_notice = "已删除 SSH 链接".to_string();
            }
            Ok(ConnectionDeletedNodeKind::SmbLink) => {
                if self.selected_connection_node_id == Some(node_id) {
                    self.selected_connection_node_id = fallback_selection;
                }
                self.persist_config_or_report();
                self.placeholder_notice = "已删除 SMB 链接".to_string();
            }
            Ok(ConnectionDeletedNodeKind::GitLink) => {
                self.disconnect_remote_file_sessions_for_link(node_id);
                // 删除缓存时沿用当前设置文件的配置根，避免测试实例或便携配置误删默认用户缓存。
                let cache_root = self
                    .config_manager
                    .settings_path()
                    .parent()
                    .map(crate::config::paths::argus_git_repositories_dir_from_config)
                    .unwrap_or_else(crate::config::paths::argus_git_repositories_dir);
                let _ =
                    crate::remote::git::schedule_git_cache_removal_at(cache_root, node_id, None);
                if self.selected_connection_node_id == Some(node_id) {
                    self.selected_connection_node_id = fallback_selection;
                }
                self.persist_config_or_report();
                self.placeholder_notice = "已删除 Git 链接".to_string();
            }
            Ok(ConnectionDeletedNodeKind::SvnLink) => {
                self.disconnect_remote_file_sessions_for_link(node_id);
                if self.selected_connection_node_id == Some(node_id) {
                    self.selected_connection_node_id = fallback_selection;
                }
                self.persist_config_or_report();
                self.placeholder_notice = "已删除 SVN 链接".to_string();
            }
            Ok(ConnectionDeletedNodeKind::UnknownLink) => {
                if self.selected_connection_node_id == Some(node_id) {
                    self.selected_connection_node_id = fallback_selection;
                }
                self.persist_config_or_report();
                self.placeholder_notice = "已删除损坏的链接".to_string();
            }
            Err(error) => {
                self.placeholder_notice = error.to_string();
            }
        }
    }

    /// 删除链接输入框当前选区。
    fn delete_connection_input_selection(&mut self, target: AppTextInputTarget) -> bool {
        let Some(input) = self.connection_text_input_mut(target) else {
            return false;
        };
        let Some(range) = normalized_connection_input_selection_range(input) else {
            return false;
        };
        input.value = replace_character_range(&input.value, range.clone(), "");
        input.cursor = range.start;
        input.selection_anchor = None;
        input.marked_range = None;
        input.selection_drag = None;
        true
    }

    /// 向链接输入框插入文本。
    fn insert_connection_input_text(&mut self, target: AppTextInputTarget, text: &str) {
        if text.is_empty() {
            return;
        }
        let _ = self.delete_connection_input_selection(target);
        let Some(input) = self.connection_text_input_mut(target) else {
            return;
        };
        let cursor = input.cursor.min(character_count(&input.value));
        input.value = replace_character_range(&input.value, cursor..cursor, text);
        input.cursor = cursor + character_count(text);
        input.selection_anchor = None;
        input.marked_range = None;
        input.selection_drag = None;
    }

    /// 删除链接输入框光标前一个字符。
    fn delete_connection_input_backward(&mut self, target: AppTextInputTarget) {
        if self.delete_connection_input_selection(target) {
            return;
        }
        let Some(input) = self.connection_text_input_mut(target) else {
            return;
        };
        if input.cursor == 0 {
            return;
        }
        let cursor = input.cursor.min(character_count(&input.value));
        input.value = replace_character_range(&input.value, cursor - 1..cursor, "");
        input.cursor = cursor - 1;
        input.marked_range = None;
        input.selection_drag = None;
    }

    /// 删除链接输入框光标后一个字符。
    fn delete_connection_input_forward(&mut self, target: AppTextInputTarget) {
        if self.delete_connection_input_selection(target) {
            return;
        }
        let Some(input) = self.connection_text_input_mut(target) else {
            return;
        };
        let text_length = character_count(&input.value);
        let cursor = input.cursor.min(text_length);
        if cursor >= text_length {
            return;
        }
        input.value = replace_character_range(&input.value, cursor..cursor + 1, "");
        input.cursor = cursor;
        input.marked_range = None;
        input.selection_drag = None;
    }

    /// 链接输入框光标左移。
    fn move_connection_input_left(&mut self, target: AppTextInputTarget, extend_selection: bool) {
        let cursor = self
            .connection_text_input(target)
            .map(|input| input.cursor.saturating_sub(1))
            .unwrap_or_default();
        self.move_connection_input_cursor(target, cursor, extend_selection);
    }

    /// 链接输入框光标右移。
    fn move_connection_input_right(&mut self, target: AppTextInputTarget, extend_selection: bool) {
        let cursor = self
            .connection_text_input(target)
            .map(|input| (input.cursor + 1).min(character_count(&input.value)))
            .unwrap_or_default();
        self.move_connection_input_cursor(target, cursor, extend_selection);
    }

    /// 移动链接输入框光标，并按需扩展选区。
    fn move_connection_input_cursor(
        &mut self,
        target: AppTextInputTarget,
        cursor: usize,
        extend_selection: bool,
    ) {
        let Some(input) = self.connection_text_input_mut(target) else {
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

/// 为新建连接预选指定协议下唯一的默认凭据。
fn apply_default_credential(form: &mut ConnectionLinkFormState, connections: &ConnectionConfig) {
    let Some(credential) = connections.default_credential_for_new_link(form.link_kind) else {
        return;
    };
    form.credential_id = Some(credential.id);
    form.username_input = TextInputState::from_value(credential.username.clone());
    if form.link_kind == ConnectionLinkKind::Smb {
        form.domain_input =
            TextInputState::from_value(credential.domain.clone().unwrap_or_default());
    }
    form.password_input = TextInputState::default();
}

/// 从新增 SSH 链接表单构造 SSH 配置。
/// 构造 Git/SVN 编辑表单；两种协议共用 URL、用户名、秘密和私钥输入状态。
fn repository_edit_form(
    link_kind: ConnectionLinkKind,
    parent_id: Option<ConnectionNodeId>,
    name: String,
    url: String,
    username: Option<String>,
    credential_id: Option<ConnectionCredentialId>,
    secret: Option<String>,
    private_key_path: Option<String>,
    private_key_passphrase: Option<String>,
) -> ConnectionLinkFormState {
    ConnectionLinkFormState {
        link_kind,
        parent_id,
        name_input: TextInputState::from_value(name),
        host_input: TextInputState::default(),
        url_input: TextInputState::from_value(url),
        port_input: TextInputState::default(),
        username_input: TextInputState::from_value(username.unwrap_or_default()),
        credential_id,
        save_as_credential: false,
        password_input: TextInputState::from_value(secret.unwrap_or_default()),
        share_input: TextInputState::default(),
        initial_dir_input: TextInputState::from_value("/".to_string()),
        domain_input: TextInputState::default(),
        private_key_path_input: TextInputState::from_value(private_key_path.unwrap_or_default()),
        private_key_passphrase_input: TextInputState::from_value(
            private_key_passphrase.unwrap_or_default(),
        ),
        error_message: None,
    }
}

/// 从凭据表单收集勾选的适用协议。
fn credential_form_protocols(form: &ConnectionCredentialFormState) -> Vec<ConnectionLinkKind> {
    let mut protocols = Vec::new();
    if form.allow_ssh {
        protocols.push(ConnectionLinkKind::Ssh);
    }
    if form.allow_smb {
        protocols.push(ConnectionLinkKind::Smb);
    }
    if form.allow_svn {
        protocols.push(ConnectionLinkKind::Svn);
    }
    protocols
}

/// 把表单里手动输入的密码保存为共用凭据，并把当前连接绑定到该凭据。
fn share_link_password_as_credential(
    app: &mut ArgusApp,
    form: &ConnectionLinkFormState,
    link_id: ConnectionNodeId,
) -> Result<(), String> {
    if !form.save_as_credential
        || form.credential_id.is_some()
        || form.link_kind == ConnectionLinkKind::Git
    {
        return Ok(());
    }
    let password = form.password_input.value.as_str();
    if password.is_empty() {
        return Err("保存为共用凭据前请先输入密码".to_string());
    }
    let username = form.username_input.value.trim();
    let domain = (form.link_kind == ConnectionLinkKind::Smb)
        .then(|| optional_form_text(&form.domain_input.value))
        .flatten();
    // 已存在同协议、同用户名、同域且密码相同的凭据时直接复用，避免每次保存都产生副本。
    let existing = app
        .config
        .connections
        .credentials
        .iter()
        .find(|credential| {
            credential.supports_protocol(form.link_kind)
                && credential.username.eq_ignore_ascii_case(username)
                && credential.domain == domain
                && credential.password == password
        })
        .map(|credential| credential.id);
    let credential_id = if let Some(credential_id) = existing {
        credential_id
    } else {
        let base = if username.is_empty() {
            format!("{} 凭据", form.name_input.value.trim())
        } else {
            format!("{username} 凭据")
        };
        let name = app.config.connections.unique_credential_name(&base);
        app.config
            .connections
            .create_credential(
                &name,
                &form.username_input.value,
                domain,
                vec![form.link_kind],
                false,
                password,
            )
            .map_err(|error| error.to_string())?
    };
    let Some(link) = app
        .config
        .connections
        .links
        .iter_mut()
        .find(|link| link.id == link_id)
    else {
        return Err("新连接不存在，无法绑定共用凭据".to_string());
    };
    link.bind_credential(credential_id);
    app.config
        .connections
        .rehydrate_credential_passwords(Some(credential_id));
    Ok(())
}

/// 将普通表单文本转为可选值；首尾空白在此去除，空值不落盘。
fn optional_form_text(value: &str) -> Option<String> {
    let normalized = value.trim();
    (!normalized.is_empty()).then(|| normalized.to_string())
}

/// 将敏感表单文本转为可选值；只过滤真正空值，避免改写令牌或口令中的合法空白。
fn optional_form_secret(value: &str) -> Option<String> {
    (!value.is_empty()).then(|| value.to_string())
}

fn ssh_config_from_form(form: &ConnectionLinkFormState) -> Result<SshLinkConfig, String> {
    let port = form
        .port_input
        .value
        .trim()
        .parse::<u16>()
        .map_err(|_| "端口必须在 1 到 65535 之间".to_string())?;
    SshLinkConfig {
        host: form.host_input.value.clone(),
        port,
        username: form.username_input.value.clone(),
        credential_id: form.credential_id,
        password: form.password_input.value.clone(),
        private_key_path: Some(form.private_key_path_input.value.clone()),
        private_key_passphrase: Some(form.private_key_passphrase_input.value.clone()),
    }
    .normalized_for_save()
    .map_err(|error| error.to_string())
}

/// 从通用链接表单提取 Git 配置，最终协议校验由领域模型统一执行。
fn git_config_from_form(form: &ConnectionLinkFormState) -> Result<GitLinkConfig, String> {
    let uses_ssh_credentials = git_form_uses_ssh_credentials(&form.url_input.value);
    GitLinkConfig {
        url: form.url_input.value.clone(),
        username: optional_form_text(&form.username_input.value),
        access_token: (!uses_ssh_credentials)
            .then(|| optional_form_secret(&form.password_input.value))
            .flatten(),
        private_key_path: uses_ssh_credentials
            .then(|| optional_form_text(&form.private_key_path_input.value))
            .flatten(),
        private_key_passphrase: uses_ssh_credentials
            .then(|| optional_form_secret(&form.private_key_passphrase_input.value))
            .flatten(),
    }
    .normalized_for_save()
    .map_err(|error| error.to_string())
}

/// 从通用链接表单提取 SVN 配置，最终协议校验由领域模型统一执行。
fn svn_config_from_form(form: &ConnectionLinkFormState) -> Result<SvnLinkConfig, String> {
    let uses_ssh_credentials = svn_form_uses_ssh_credentials(&form.url_input.value);
    SvnLinkConfig {
        url: form.url_input.value.clone(),
        username: optional_form_text(&form.username_input.value),
        credential_id: form.credential_id,
        password: optional_form_secret(&form.password_input.value),
        private_key_path: uses_ssh_credentials
            .then(|| optional_form_text(&form.private_key_path_input.value))
            .flatten(),
        private_key_passphrase: uses_ssh_credentials
            .then(|| optional_form_secret(&form.private_key_passphrase_input.value))
            .flatten(),
    }
    .normalized_for_save()
    .map_err(|error| error.to_string())
}

/// 判断 Git 表单当前 URL 是否使用 SSH 凭据；规则与动态字段显示保持一致。
fn git_form_uses_ssh_credentials(url: &str) -> bool {
    let normalized_url = url.trim().to_ascii_lowercase();
    normalized_url.starts_with("ssh://")
        || (!normalized_url.starts_with("https://")
            && normalized_url.contains('@')
            && normalized_url.contains(':'))
}

/// 判断 SVN 表单当前 URL 是否使用 SSH 凭据；其他协议必须丢弃隐藏的私钥字段。
fn svn_form_uses_ssh_credentials(url: &str) -> bool {
    url.trim().to_ascii_lowercase().starts_with("svn+ssh://")
}

/// 从新增 SMB 链接表单构造 SMB 配置。
fn smb_config_from_form(form: &ConnectionLinkFormState) -> Result<SmbLinkConfig, String> {
    let port = form
        .port_input
        .value
        .trim()
        .parse::<u16>()
        .map_err(|_| "端口必须在 1 到 65535 之间".to_string())?;
    // 主机框若填了完整 UNC 地址（如 \\host\share\path），则从中拆出主机/共享名/初始目录，
    // 覆盖对应的共享名称、初始目录字段；纯主机名时回退到分字段填写，保持编辑已有链接兼容。
    let (host, share, initial_dir) =
        match crate::remote::connection::parse_smb_unc_address(&form.host_input.value) {
            Some((host, share, initial_dir)) => (host, share, initial_dir),
            None => (
                form.host_input.value.clone(),
                form.share_input.value.clone(),
                form.initial_dir_input.value.clone(),
            ),
        };
    SmbLinkConfig {
        host,
        port,
        share,
        initial_dir,
        domain: Some(form.domain_input.value.clone()),
        username: form.username_input.value.clone(),
        credential_id: form.credential_id,
        password: form.password_input.value.clone(),
    }
    .normalized_for_save()
    .map_err(|error| error.to_string())
}

/// 清理单个链接输入框焦点态。
fn clear_connection_input_focus(input: &mut TextInputState) {
    input.clear_focus();
}

/// 返回输入框规范化后的非空选区。
pub(crate) fn normalized_connection_input_selection_range(
    input: &TextInputState,
) -> Option<Range<usize>> {
    input.selection_range()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ConfigManager;
    use crate::config::paths::isolated_test_dir;

    /// 构造使用临时配置文件的应用，避免连接表单测试读写真实用户设置。
    fn test_app(name: &str) -> ArgusApp {
        let config_dir = isolated_test_dir(&format!("connection-actions-{name}"));
        ArgusApp::new_with_config_manager(ConfigManager::new(config_dir.join("settings.toml")))
    }

    /// 构造仓库链接表单，测试可按传输类型覆盖凭据字段。
    fn repository_link_form(link_kind: ConnectionLinkKind, url: &str) -> ConnectionLinkFormState {
        ConnectionLinkFormState {
            link_kind,
            parent_id: None,
            name_input: TextInputState::from_value("repository".to_string()),
            host_input: TextInputState::default(),
            url_input: TextInputState::from_value(url.to_string()),
            port_input: TextInputState::default(),
            username_input: TextInputState::default(),
            credential_id: None,
            save_as_credential: false,
            password_input: TextInputState::default(),
            share_input: TextInputState::default(),
            initial_dir_input: TextInputState::from_value("/".to_string()),
            domain_input: TextInputState::default(),
            private_key_path_input: TextInputState::default(),
            private_key_passphrase_input: TextInputState::default(),
            error_message: None,
        }
    }

    /// 验证新增目录会写入链接配置并选中新目录。
    #[test]
    fn submit_directory_form_creates_directory() {
        let mut app = test_app("submit-directory");
        app.create_connection_directory_from_form(ConnectionDirectoryFormState {
            parent_id: None,
            name_input: TextInputState::from_value("生产环境".to_string()),
            error_message: None,
        })
        .expect("有效目录表单应创建成功");

        assert_eq!(app.config.connections.directories.len(), 1);
        assert_eq!(app.config.connections.directories[0].name, "生产环境");
        assert_eq!(
            app.selected_connection_node_id,
            Some(app.config.connections.directories[0].id)
        );
    }

    /// 验证新增链接表单会按端口和凭据规则校验。
    #[test]
    fn submit_link_form_rejects_invalid_port() {
        let mut app = test_app("submit-link-invalid-port");
        let result = app.create_connection_link_from_form(ConnectionLinkFormState {
            link_kind: ConnectionLinkKind::Ssh,
            parent_id: None,
            name_input: TextInputState::from_value("app-01".to_string()),
            host_input: TextInputState::from_value("10.0.0.1".to_string()),
            url_input: TextInputState::default(),
            port_input: TextInputState::from_value("70000".to_string()),
            username_input: TextInputState::from_value("deploy".to_string()),
            credential_id: None,
            save_as_credential: false,
            password_input: TextInputState::from_value("secret".to_string()),
            share_input: TextInputState::default(),
            initial_dir_input: TextInputState::from_value("/".to_string()),
            domain_input: TextInputState::default(),
            private_key_path_input: TextInputState::default(),
            private_key_passphrase_input: TextInputState::default(),
            error_message: None,
        });

        assert!(result.is_err());
        assert!(app.config.connections.links.is_empty());
    }

    /// 验证新增 SMB 链接会写入共享配置并保留密码原文。
    #[test]
    fn submit_smb_link_form_creates_smb_link() {
        let mut app = test_app("submit-smb-link");
        app.create_connection_link_from_form(ConnectionLinkFormState {
            link_kind: ConnectionLinkKind::Smb,
            parent_id: None,
            name_input: TextInputState::from_value("共享日志".to_string()),
            host_input: TextInputState::from_value("10.0.0.2".to_string()),
            url_input: TextInputState::default(),
            port_input: TextInputState::from_value("445".to_string()),
            username_input: TextInputState::from_value("smbuser".to_string()),
            credential_id: None,
            save_as_credential: false,
            password_input: TextInputState::from_value(" secret ".to_string()),
            share_input: TextInputState::from_value("logs".to_string()),
            initial_dir_input: TextInputState::from_value("runtime".to_string()),
            domain_input: TextInputState::from_value("WORKGROUP".to_string()),
            private_key_path_input: TextInputState::default(),
            private_key_passphrase_input: TextInputState::default(),
            error_message: None,
        })
        .expect("有效 SMB 表单应创建成功");

        let link = app
            .config
            .connections
            .links
            .first()
            .expect("应创建 SMB 链接");
        let smb = link.smb_config().expect("应保存 SMB 配置");
        assert_eq!(smb.share, "logs");
        assert_eq!(smb.initial_dir, "/runtime");
        assert_eq!(smb.password, " secret ");
    }

    /// 仓库传输方式变化后，已隐藏的旧凭据不得阻止新配置保存。
    #[test]
    fn repository_form_ignores_credentials_hidden_by_current_transport() {
        let mut git_ssh = repository_link_form(
            ConnectionLinkKind::Git,
            "ssh://git@example.com/team/repository.git",
        );
        git_ssh.password_input = TextInputState::from_value("stale-token".to_string());
        git_ssh.private_key_path_input =
            TextInputState::from_value("/keys/git_ed25519".to_string());
        let git_ssh = git_config_from_form(&git_ssh).expect("SSH Git 应忽略隐藏令牌");
        assert!(git_ssh.access_token.is_none());
        assert_eq!(
            git_ssh.private_key_path.as_deref(),
            Some("/keys/git_ed25519")
        );

        let mut git_https = repository_link_form(
            ConnectionLinkKind::Git,
            "https://example.com/team/repository.git",
        );
        git_https.username_input = TextInputState::from_value("reader".to_string());
        git_https.password_input = TextInputState::from_value("token".to_string());
        git_https.private_key_path_input = TextInputState::from_value("/keys/stale".to_string());
        let git_https = git_config_from_form(&git_https).expect("HTTPS Git 应忽略隐藏私钥");
        assert!(git_https.private_key_path.is_none());
        assert_eq!(git_https.access_token.as_deref(), Some("token"));

        let mut svn_http = repository_link_form(
            ConnectionLinkKind::Svn,
            "https://example.com/svn/repository/",
        );
        svn_http.username_input = TextInputState::from_value("reader".to_string());
        svn_http.password_input = TextInputState::from_value("password".to_string());
        svn_http.private_key_path_input = TextInputState::from_value("/keys/stale".to_string());
        let svn_http = svn_config_from_form(&svn_http).expect("HTTP SVN 应忽略隐藏私钥");
        assert!(svn_http.private_key_path.is_none());
        assert_eq!(svn_http.password.as_deref(), Some("password"));
    }

    /// 新增 SVN 链接必须真实写入设置文件，并在重新创建应用后恢复。
    #[test]
    fn svn_link_survives_application_restart() {
        let config_dir = isolated_test_dir("connection-actions-svn-restart");
        let manager = ConfigManager::new(config_dir.join("settings.toml"));
        let mut app = ArgusApp::new_with_config_manager(manager.clone());
        let form = repository_link_form(ConnectionLinkKind::Svn, "http://10.1.3.12/svn/example/");

        let link_id = app
            .create_svn_link_from_form(form)
            .expect("有效 SVN 表单应创建并保存成功");
        drop(app);

        let restarted = ArgusApp::new_with_config_manager(manager);
        let svn = restarted
            .config
            .connections
            .link(link_id)
            .and_then(ConnectionLinkConfig::svn_config)
            .expect("重新启动后应恢复 SVN 链接");
        assert_eq!(svn.url, "http://10.1.3.12/svn/example/");
    }

    /// 设置文件无法写入时，SVN 表单必须返回错误并回滚内存链接，避免制造重启后消失的假成功。
    #[test]
    fn svn_link_save_failure_is_reported_and_rolled_back() {
        let config_dir = isolated_test_dir("connection-actions-svn-save-failure");
        let settings_path = config_dir.join("settings.toml");
        std::fs::create_dir_all(&settings_path).expect("应创建用于触发保存失败的同名目录");
        let mut app = ArgusApp::new_with_config_manager(ConfigManager::new(settings_path));
        let form = repository_link_form(ConnectionLinkKind::Svn, "http://10.1.3.12/svn/example/");

        let error = app
            .create_svn_link_from_form(form)
            .expect_err("设置文件不可写时必须拒绝关闭表单");

        assert!(error.contains("设置保存失败"));
        assert!(app.config.connections.links.is_empty());
        assert!(app.selected_connection_node_id.is_none());
    }

    /// 验证删除非空目录时不会进入确认弹窗，而是直接给出错误提示。
    #[test]
    fn request_delete_non_empty_directory_shows_error() {
        let mut app = test_app("delete-non-empty-directory");
        let parent = app
            .config
            .connections
            .add_directory(None, "生产环境")
            .unwrap();
        app.config
            .connections
            .add_directory(Some(parent), "应用服务器")
            .unwrap();

        app.request_delete_connection_node(parent);

        assert!(app.config.connections.directory(parent).is_some());
        assert!(app.connection_dialog.is_none());
        assert_eq!(app.placeholder_notice, "目录不为空，不能删除");
    }

    /// 验证删除 SSH 链接需要先二次确认，确认后才真正写入删除结果。
    #[test]
    fn confirm_delete_link_removes_after_prompt() {
        let mut app = test_app("confirm-delete-link");
        let link_id = app
            .config
            .connections
            .add_ssh_link(
                None,
                "app-01",
                SshLinkConfig {
                    host: "10.0.0.1".to_string(),
                    port: 22,
                    username: "deploy".to_string(),
                    credential_id: None,
                    password: "secret".to_string(),
                    private_key_path: None,
                    private_key_passphrase: None,
                },
            )
            .unwrap();

        app.request_delete_connection_node(link_id);

        assert!(app.config.connections.link(link_id).is_some());
        assert!(matches!(
            app.connection_dialog,
            Some(ConnectionDialogState::ConfirmDelete(_))
        ));

        app.confirm_delete_connection_node(link_id);

        assert!(app.config.connections.link(link_id).is_none());
        assert!(app.connection_dialog.is_none());
    }
    /// 手动密码勾选共用后应只创建一个凭据，并让连接引用它。
    #[test]
    fn save_as_shared_credential_binds_new_link() {
        let mut app = test_app("save-as-shared-credential");
        let form = ConnectionLinkFormState {
            link_kind: ConnectionLinkKind::Ssh,
            parent_id: None,
            name_input: TextInputState::from_value("app-01".to_string()),
            host_input: TextInputState::from_value("10.0.0.1".to_string()),
            url_input: TextInputState::default(),
            port_input: TextInputState::from_value("22".to_string()),
            username_input: TextInputState::from_value("deploy".to_string()),
            credential_id: None,
            save_as_credential: true,
            password_input: TextInputState::from_value("shared-secret".to_string()),
            share_input: TextInputState::default(),
            initial_dir_input: TextInputState::default(),
            domain_input: TextInputState::default(),
            private_key_path_input: TextInputState::default(),
            private_key_passphrase_input: TextInputState::default(),
            error_message: None,
        };
        let link_id = app
            .create_connection_link_from_form(form)
            .expect("密码应自动保存为共用凭据");

        let credential = app
            .config
            .connections
            .credentials
            .first()
            .expect("应创建共用凭据");
        assert_eq!(credential.username, "deploy");
        assert_eq!(credential.password, "shared-secret");
        let credential_id = credential.id;
        let link = app.config.connections.link(link_id).unwrap();
        assert_eq!(link.credential_id(), Some(credential_id));
        assert_eq!(
            link.ssh.as_ref().unwrap().password,
            "shared-secret",
            "保存后连接应立即可用，无需重启"
        );

        // 第二个连接使用相同账号密码时复用同一凭据，不再产生副本。
        let mut second_form = ConnectionLinkFormState {
            link_kind: ConnectionLinkKind::Ssh,
            parent_id: None,
            name_input: TextInputState::from_value("app-02".to_string()),
            host_input: TextInputState::from_value("10.0.0.2".to_string()),
            url_input: TextInputState::default(),
            port_input: TextInputState::from_value("22".to_string()),
            username_input: TextInputState::from_value("deploy".to_string()),
            credential_id: None,
            save_as_credential: true,
            password_input: TextInputState::from_value("shared-secret".to_string()),
            share_input: TextInputState::default(),
            initial_dir_input: TextInputState::default(),
            domain_input: TextInputState::default(),
            private_key_path_input: TextInputState::default(),
            private_key_passphrase_input: TextInputState::default(),
            error_message: None,
        };
        let second_link_id = app
            .create_connection_link_from_form(second_form.clone())
            .expect("相同账号密码应复用已有共用凭据");
        assert_eq!(app.config.connections.credentials.len(), 1);
        assert_eq!(
            app.config
                .connections
                .link(second_link_id)
                .unwrap()
                .credential_id(),
            Some(credential_id)
        );
        second_form.credential_id = Some(credential_id);
        second_form.save_as_credential = false;

        let path = app.config_manager.settings_path().to_path_buf();
        let (reloaded, warnings) =
            ConfigManager::load_from_path(&path).expect("共用凭据配置应可重新加载");
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(
            reloaded.connections.link(link_id).unwrap().credential_id(),
            Some(credential_id)
        );
        assert_eq!(
            reloaded
                .connections
                .credential(credential_id)
                .unwrap()
                .password,
            "shared-secret"
        );
    }

    /// 凭据管理弹窗应能正常渲染保存、关闭和列表区域。
    #[gpui::test]
    fn credential_manager_modal_renders(cx: &mut gpui::TestAppContext) {
        let (app, cx) = cx.add_window_view(|_, _| test_app("credential-manager-modal"));
        app.update(cx, |app, app_cx| {
            app.open_connection_credential_manager(ConnectionLinkKind::Ssh, app_cx);
        });
        cx.simulate_resize(gpui::size(gpui::px(1400.0), gpui::px(900.0)));
        assert!(
            cx.debug_bounds("connection-credential-save").is_some(),
            "应渲染保存按钮"
        );
        assert!(
            cx.debug_bounds("connection-credential-cancel").is_some(),
            "应渲染关闭按钮"
        );
        assert!(
            cx.debug_bounds("connection-credential-list").is_some(),
            "应渲染凭据列表"
        );
        app.update(cx, |app, _| app.close_connection_credential_manager());
    }
    /// 新建连接选择已有共用凭据时不再要求重复输入密码。
    #[test]
    fn link_form_uses_existing_shared_credential() {
        let mut app = test_app("link-form-shared-credential");
        let credential_id = app
            .config
            .connections
            .create_credential(
                "运维账号",
                "deploy",
                None,
                vec![ConnectionLinkKind::Ssh],
                false,
                "shared-secret",
            )
            .unwrap();
        let form = ConnectionLinkFormState {
            link_kind: ConnectionLinkKind::Ssh,
            parent_id: None,
            name_input: TextInputState::from_value("app-02".to_string()),
            host_input: TextInputState::from_value("10.0.0.2".to_string()),
            url_input: TextInputState::default(),
            port_input: TextInputState::from_value("22".to_string()),
            username_input: TextInputState::from_value("deploy".to_string()),
            credential_id: Some(credential_id),
            save_as_credential: false,
            password_input: TextInputState::default(),
            share_input: TextInputState::default(),
            initial_dir_input: TextInputState::default(),
            domain_input: TextInputState::default(),
            private_key_path_input: TextInputState::default(),
            private_key_passphrase_input: TextInputState::default(),
            error_message: None,
        };
        let link_id = app
            .create_connection_link_from_form(form)
            .expect("选择共用凭据后应允许空密码");

        let link = app.config.connections.link(link_id).unwrap();
        assert_eq!(link.credential_id(), Some(credential_id));
        assert_eq!(link.ssh.as_ref().unwrap().password, "shared-secret");
        assert_eq!(
            app.config.connections.credential_usage_count(credential_id),
            1
        );
    }
    /// 链接表单可从下拉框选择共用凭据，并隐藏手动密码输入。
    #[gpui::test]
    fn link_form_credential_dropdown_selects_shared_credential(cx: &mut gpui::TestAppContext) {
        let (app, cx) = cx.add_window_view(|_, _| test_app("link-credential-dropdown"));
        let credential_id = app.update(cx, |app, _| {
            app.config
                .connections
                .create_credential(
                    "运维账号",
                    "deploy",
                    None,
                    vec![ConnectionLinkKind::Ssh],
                    false,
                    "shared-secret",
                )
                .unwrap()
        });
        app.update(cx, |app, app_cx| {
            app.open_new_ssh_link_dialog(app_cx);
        });
        cx.simulate_resize(gpui::size(gpui::px(1400.0), gpui::px(900.0)));

        let dropdown = cx
            .debug_bounds("connection-link-credential-dropdown-button")
            .expect("应渲染凭据下拉框");
        cx.simulate_click(dropdown.center(), gpui::Modifiers::default());
        assert_eq!(credential_id, 1);
        let item = cx
            .debug_bounds("connection-link-credential-menu-item-credential-1")
            .expect("展开后应显示共用凭据选项");
        cx.simulate_click(item.center(), gpui::Modifiers::default());
        cx.run_until_parked();
        cx.simulate_resize(gpui::size(gpui::px(1401.0), gpui::px(901.0)));
        let selected = app.read_with(cx, |app, app_cx| {
            app.connection_link_modal
                .as_ref()
                .and_then(|modal| modal.read(app_cx).credential_id_for_test())
        });
        assert_eq!(selected, Some(credential_id), "应记录选中的共用凭据");

        assert!(
            cx.debug_bounds("connection-link-shared-credential")
                .is_some(),
            "选择共用凭据后应显示凭据来源说明"
        );
        app.update(cx, |app, _| app.close_connection_link_window());
    }
    /// 新建连接应预选唯一的默认凭据，避免重复输入密码。
    #[gpui::test]
    fn default_shared_credential_preselects_new_link(cx: &mut gpui::TestAppContext) {
        let (app, cx) = cx.add_window_view(|_, _| test_app("default-credential-preselect"));
        let credential_id = app.update(cx, |app, _| {
            app.config
                .connections
                .create_credential(
                    "运维账号",
                    "deploy",
                    None,
                    vec![ConnectionLinkKind::Ssh],
                    true,
                    "shared-secret",
                )
                .unwrap()
        });
        app.update(cx, |app, app_cx| {
            app.open_new_ssh_link_dialog(app_cx);
        });
        cx.simulate_resize(gpui::size(gpui::px(1400.0), gpui::px(900.0)));
        let selected = app.read_with(cx, |app, app_cx| {
            app.connection_link_modal
                .as_ref()
                .and_then(|modal| modal.read(app_cx).credential_id_for_test())
        });
        assert_eq!(selected, Some(credential_id));
        assert!(
            cx.debug_bounds("connection-link-shared-credential")
                .is_some(),
            "默认凭据应直接显示为当前凭据来源"
        );
        app.update(cx, |app, _| app.close_connection_link_window());
    }
    /// 链接表单应能通过目录下拉框修改新建链接的所属目录。
    #[gpui::test]
    fn link_form_directory_dropdown_updates_parent(cx: &mut gpui::TestAppContext) {
        let (app, cx) = cx.add_window_view(|_, _| test_app("link-directory-dropdown"));
        let directory_id = app.update(cx, |app, _| {
            app.config
                .connections
                .add_directory(None, "生产环境")
                .unwrap()
        });
        app.update(cx, |app, app_cx| {
            app.open_new_ssh_link_dialog(app_cx);
        });
        cx.simulate_resize(gpui::size(gpui::px(1400.0), gpui::px(900.0)));

        let dropdown = cx
            .debug_bounds("connection-link-directory-dropdown-button")
            .expect("应渲染所属目录下拉框");
        cx.simulate_click(dropdown.center(), gpui::Modifiers::default());
        cx.run_until_parked();
        cx.simulate_resize(gpui::size(gpui::px(1401.0), gpui::px(900.0)));
        assert_eq!(directory_id, 1);
        let item = cx
            .debug_bounds("connection-link-directory-menu-item-directory-1")
            .expect("目录下拉框应展示已建目录");
        cx.simulate_click(item.center(), gpui::Modifiers::default());
        cx.run_until_parked();

        let parent = app.read_with(cx, |app, app_cx| {
            app.connection_link_modal
                .as_ref()
                .and_then(|modal| modal.read(app_cx).form_parent_id())
        });
        assert_eq!(parent, Some(directory_id));
        app.update(cx, |app, _| app.close_connection_link_window());
    }
}
