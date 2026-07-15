//! 文件职责：实现链接工作区的树操作、表单校验、输入框编辑和配置持久化。
//! 创建日期：2026-06-26
//! 修改日期：2026-07-14
//! 作者：Argus 开发团队
//! 主要功能：让标题栏链接入口具备新增目录、新增 SSH 链接、过滤和点击打开终端能力。

use std::ops::Range;

use gpui::{AppContext, Context, Keystroke};

use crate::app::{
    AppTextInputTarget, ArgusApp, ConnectionDeletePromptState, ConnectionDialogState,
    ConnectionDirectoryFormState, ConnectionHostKeyPromptState, ConnectionLinkFormState,
    TextInputState,
};
use crate::infra::text_selection::{
    TextSelectionGranularity, character_count, replace_character_range,
};
use crate::remote::connection::{
    ConnectionDeletedNodeKind, ConnectionLinkKind, ConnectionNodeId, ConnectionTreeRow,
    SmbLinkConfig, SshLinkConfig,
};
use crate::remote::terminal::PendingHostKey;
use crate::ui::connection_dialog::{
    ConnectionDirectoryWindow, ConnectionDirectoryWindowMode, ConnectionLinkWindow,
    ConnectionLinkWindowMode,
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

    /// 点击链接树节点；目录执行展开折叠，SSH 链接打开终端标签。
    pub(crate) fn handle_connection_tree_click(
        &mut self,
        node_id: ConnectionNodeId,
        cx: &mut Context<Self>,
    ) {
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
        let initial_form = ConnectionLinkFormState {
            link_kind: ConnectionLinkKind::Ssh,
            parent_id,
            name_input: TextInputState::default(),
            host_input: TextInputState::default(),
            port_input: TextInputState::from_value("22".to_string()),
            username_input: TextInputState::default(),
            password_input: TextInputState::default(),
            share_input: TextInputState::default(),
            initial_dir_input: TextInputState::from_value("/".to_string()),
            domain_input: TextInputState::default(),
            private_key_path_input: TextInputState::default(),
            private_key_passphrase_input: TextInputState::default(),
            error_message: None,
        };

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
        let initial_form = ConnectionLinkFormState {
            link_kind: ConnectionLinkKind::Smb,
            parent_id,
            name_input: TextInputState::default(),
            host_input: TextInputState::default(),
            port_input: TextInputState::from_value("445".to_string()),
            username_input: TextInputState::default(),
            password_input: TextInputState::default(),
            share_input: TextInputState::default(),
            initial_dir_input: TextInputState::from_value("/".to_string()),
            domain_input: TextInputState::default(),
            private_key_path_input: TextInputState::default(),
            private_key_passphrase_input: TextInputState::default(),
            error_message: None,
        };

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
            port_input: TextInputState::from_value(ssh.port.to_string()),
            username_input: TextInputState::from_value(ssh.username),
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
            port_input: TextInputState::from_value(smb.port.to_string()),
            username_input: TextInputState::from_value(smb.username),
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
        if let Some(ConnectionDialogState::ConfirmHostKey(prompt)) = self.connection_dialog.clone()
        {
            self.reject_connection_host_key_prompt(prompt);
            return;
        }
        self.connection_dialog = None;
        self.placeholder_notice = "已关闭链接弹窗".to_string();
    }

    /// 提交当前链接工作区弹窗。
    pub(crate) fn submit_connection_dialog(&mut self) {
        match self.connection_dialog.clone() {
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
        self.persist_config_or_report();
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
        self.selected_connection_node_id = Some(link_id);
        self.placeholder_notice = "已新增 SSH 链接".to_string();
        self.persist_config_or_report();
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
        self.selected_connection_node_id = Some(link_id);
        self.placeholder_notice = "已新增 SMB 链接".to_string();
        self.persist_config_or_report();
        Ok(link_id)
    }

    /// 校验并更新链接目录名称。
    pub(crate) fn update_connection_directory_from_form(
        &mut self,
        directory_id: ConnectionNodeId,
        form: ConnectionDirectoryFormState,
    ) -> Result<(), String> {
        match self
            .config
            .connections
            .update_directory(directory_id, &form.name_input.value)
        {
            Ok(()) => {
                self.selected_connection_node_id = Some(directory_id);
                self.placeholder_notice = "已更新链接目录".to_string();
                self.persist_config_or_report();
                Ok(())
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
        match self
            .config
            .connections
            .update_ssh_link(link_id, &form.name_input.value, ssh)
        {
            Ok(()) => {
                self.selected_connection_node_id = Some(link_id);
                self.placeholder_notice = "已更新 SSH 链接".to_string();
                self.persist_config_or_report();
                Ok(())
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
        match self
            .config
            .connections
            .update_smb_link(link_id, &form.name_input.value, smb)
        {
            Ok(()) => {
                self.selected_connection_node_id = Some(link_id);
                self.placeholder_notice = "已更新 SMB 链接".to_string();
                self.persist_config_or_report();
                Ok(())
            }
            Err(error) => {
                let message = error.to_string();
                self.placeholder_notice = message.clone();
                Err(message)
            }
        }
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

/// 从新增 SSH 链接表单构造 SSH 配置。
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
        password: form.password_input.value.clone(),
        private_key_path: Some(form.private_key_path_input.value.clone()),
        private_key_passphrase: Some(form.private_key_passphrase_input.value.clone()),
    }
    .normalized_for_save()
    .map_err(|error| error.to_string())
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

    /// 构造使用临时配置文件的应用，避免连接表单测试读写真实用户设置。
    fn test_app(name: &str) -> ArgusApp {
        let config_dir = std::env::temp_dir().join(format!(
            "argus-connection-actions-test-{}-{name}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&config_dir);
        ArgusApp::new_with_config_manager(ConfigManager::new(config_dir.join("settings.toml")))
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
            port_input: TextInputState::from_value("70000".to_string()),
            username_input: TextInputState::from_value("deploy".to_string()),
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
            port_input: TextInputState::from_value("445".to_string()),
            username_input: TextInputState::from_value("smbuser".to_string()),
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
}
