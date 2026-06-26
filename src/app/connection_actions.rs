//! 文件职责：实现链接工作区的树操作、表单校验、输入框编辑和配置持久化。
//! 创建日期：2026-06-26
//! 修改日期：2026-06-26
//! 作者：Argus 开发团队
//! 主要功能：让标题栏链接入口具备新增目录、新增 SSH 链接、过滤和点击打开终端能力。

use std::ops::Range;

use gpui::{AppContext, Bounds, Context, Keystroke, WindowBounds, WindowOptions, px, size};

use crate::app::{
    AppTextInputTarget, ArgusApp, ConnectionDeletePromptState, ConnectionDialogState,
    ConnectionDirectoryFormState, ConnectionHostKeyPromptState, ConnectionLinkFormState,
    InputTextSelectionDrag, SettingsTextInputState,
};
use crate::connections::{
    ConnectionDeletedNodeKind, ConnectionNodeId, ConnectionTreeRow, SshLinkConfig,
};
use crate::terminal::PendingHostKey;
use crate::text_selection::{
    TextSelectionGranularity, character_count, replace_character_range, word_range_at,
};
use crate::ui::connection_dialog::{
    ConnectionDirectoryWindow, ConnectionDirectoryWindowMode, ConnectionLinkWindow,
    ConnectionLinkWindowMode,
};

/// 目录表单独立窗口默认宽度。
const CONNECTION_DIRECTORY_WINDOW_WIDTH: f32 = 420.0;
/// 目录表单独立窗口默认高度；按一行输入和底部按钮收紧，避免窗口底部留白过大。
const CONNECTION_DIRECTORY_WINDOW_HEIGHT: f32 = 170.0;
/// 目录表单独立窗口最小宽度。
const CONNECTION_DIRECTORY_WINDOW_MIN_WIDTH: f32 = 360.0;
/// 目录表单独立窗口最小高度。
const CONNECTION_DIRECTORY_WINDOW_MIN_HEIGHT: f32 = 150.0;
/// SSH 链接表单独立窗口默认宽度。
const CONNECTION_LINK_WINDOW_WIDTH: f32 = 520.0;
/// SSH 链接表单独立窗口默认高度；刚好容纳 7 行输入和操作区，减少底部空白。
const CONNECTION_LINK_WINDOW_HEIGHT: f32 = 430.0;
/// SSH 链接表单独立窗口最小宽度。
const CONNECTION_LINK_WINDOW_MIN_WIDTH: f32 = 460.0;
/// SSH 链接表单独立窗口最小高度。
const CONNECTION_LINK_WINDOW_MIN_HEIGHT: f32 = 380.0;

impl ArgusApp {
    /// 返回当前链接树是否处于过滤模式。
    pub fn is_connection_tree_filtering(&self) -> bool {
        self.is_connection_tree_search_open && !self.connection_tree_search_input.value.is_empty()
    }

    /// 返回链接树当前应渲染的可见行。
    pub fn visible_connection_rows(&self) -> Vec<ConnectionTreeRow> {
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
    pub fn open_connection_tree_search(&mut self) {
        self.is_connection_tree_search_open = true;
        self.connection_tree_search_input = SettingsTextInputState::default();
        self.connection_tree_search_input.is_focused = true;
        self.placeholder_notice = "已打开链接过滤".to_string();
    }

    /// 关闭链接树过滤输入框并恢复完整目录树。
    pub fn close_connection_tree_search(&mut self) {
        self.is_connection_tree_search_open = false;
        self.connection_tree_search_input = SettingsTextInputState::default();
        self.placeholder_notice = "已关闭链接过滤".to_string();
    }

    /// 收起链接目录树中的所有目录。
    pub fn collapse_all_connections(&mut self) {
        let collapsed_count = self.config.connections.collapse_all();
        self.persist_config_or_report();
        self.placeholder_notice = if collapsed_count == 0 {
            "链接目录树已处于全部收起状态".to_string()
        } else {
            format!("已收起 {collapsed_count} 个链接目录")
        };
    }

    /// 点击链接树节点；目录执行展开折叠，SSH 链接打开终端标签。
    pub fn handle_connection_tree_click(
        &mut self,
        node_id: ConnectionNodeId,
        cx: &mut Context<Self>,
    ) {
        self.selected_connection_node_id = Some(node_id);
        if self.config.connections.is_directory(node_id) {
            self.toggle_connection_directory(node_id);
        } else {
            self.open_or_focus_ssh_terminal(node_id, cx);
        }
    }

    /// 切换指定链接目录展开状态。
    pub fn toggle_connection_directory(&mut self, directory_id: ConnectionNodeId) {
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

    /// 打开新增目录独立窗口，父目录根据当前选中节点推导。
    ///
    /// 参数说明：
    /// - `cx`：主应用上下文，用于创建或激活独立窗口。
    pub fn open_new_connection_directory_dialog(&mut self, cx: &mut Context<Self>) {
        let parent_id = self
            .config
            .connections
            .parent_for_new_directory(self.selected_connection_node_id);
        let initial_form = ConnectionDirectoryFormState {
            parent_id,
            name_input: SettingsTextInputState::default(),
            error_message: None,
        };

        if self.is_connection_directory_window_open {
            if let Some(window_handle) = self.connection_directory_window_handle.clone()
                && window_handle
                    .update(cx, |window_state, window, cx| {
                        if !window_state.is_mode(ConnectionDirectoryWindowMode::Create) {
                            window_state.replace_form(
                                initial_form.clone(),
                                ConnectionDirectoryWindowMode::Create,
                            );
                            cx.notify();
                        }
                        window.activate_window();
                    })
                    .is_ok()
            {
                self.placeholder_notice = "新增目录窗口已显示到最前".to_string();
                return;
            }

            // 句柄失效表示窗口可能已被系统关闭，清理后重新创建。
            self.is_connection_directory_window_open = false;
            self.connection_directory_window_handle = None;
        }

        self.open_connection_directory_window_with_form(
            initial_form,
            ConnectionDirectoryWindowMode::Create,
            cx,
        );
    }

    /// 打开新增 SSH 链接独立窗口，父目录根据当前选中节点推导。
    ///
    /// 参数说明：
    /// - `cx`：主应用上下文，用于创建或激活独立窗口。
    pub fn open_new_ssh_link_dialog(&mut self, cx: &mut Context<Self>) {
        let parent_id = self
            .config
            .connections
            .parent_for_new_link(self.selected_connection_node_id);
        let initial_form = ConnectionLinkFormState {
            parent_id,
            name_input: SettingsTextInputState::default(),
            host_input: SettingsTextInputState::default(),
            port_input: SettingsTextInputState::from_value("22".to_string()),
            username_input: SettingsTextInputState::default(),
            password_input: SettingsTextInputState::default(),
            private_key_path_input: SettingsTextInputState::default(),
            private_key_passphrase_input: SettingsTextInputState::default(),
            error_message: None,
        };

        if self.is_connection_link_window_open {
            if let Some(window_handle) = self.connection_link_window_handle.clone()
                && window_handle
                    .update(cx, |window_state, window, cx| {
                        if !window_state.is_mode(ConnectionLinkWindowMode::Create) {
                            window_state.replace_form(
                                initial_form.clone(),
                                ConnectionLinkWindowMode::Create,
                            );
                            cx.notify();
                        }
                        window.activate_window();
                    })
                    .is_ok()
            {
                self.placeholder_notice = "新增链接窗口已显示到最前".to_string();
                return;
            }

            // 句柄失效表示窗口可能已被系统关闭，清理后重新创建。
            self.is_connection_link_window_open = false;
            self.connection_link_window_handle = None;
        }

        self.open_connection_link_window_with_form(
            initial_form,
            ConnectionLinkWindowMode::Create,
            cx,
        );
    }

    /// 清理目录表单独立窗口状态；关闭按钮、取消按钮和提交成功后统一调用。
    pub fn close_connection_directory_window(&mut self) {
        self.is_connection_directory_window_open = false;
        self.connection_directory_window_handle = None;
        self.placeholder_notice = "已关闭目录窗口".to_string();
    }

    /// 目录创建或编辑成功后清理窗口句柄，不覆盖成功提示。
    pub fn finish_connection_directory_window(&mut self) {
        self.is_connection_directory_window_open = false;
        self.connection_directory_window_handle = None;
    }

    /// 清理 SSH 链接表单独立窗口状态；关闭按钮、取消按钮和提交成功后统一调用。
    pub fn close_connection_link_window(&mut self) {
        self.is_connection_link_window_open = false;
        self.connection_link_window_handle = None;
        self.placeholder_notice = "已关闭链接窗口".to_string();
    }

    /// SSH 链接创建或编辑成功后清理窗口句柄，不覆盖成功提示。
    pub fn finish_connection_link_window(&mut self) {
        self.is_connection_link_window_open = false;
        self.connection_link_window_handle = None;
    }

    /// 打开连接节点编辑窗口；目录和 SSH 链接会按节点类型复用对应表单窗口。
    pub fn open_edit_connection_node_window(
        &mut self,
        node_id: ConnectionNodeId,
        cx: &mut Context<Self>,
    ) {
        if self.config.connections.is_directory(node_id) {
            self.open_edit_connection_directory_window(node_id, cx);
        } else if self.config.connections.is_link(node_id) {
            self.open_edit_ssh_link_window(node_id, cx);
        } else {
            self.placeholder_notice = "未找到可编辑的连接节点".to_string();
        }
    }

    /// 打开编辑目录独立窗口。
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
            name_input: SettingsTextInputState::from_value(directory.name),
            error_message: None,
        };
        let mode = ConnectionDirectoryWindowMode::Edit { directory_id };

        if self.is_connection_directory_window_open {
            if let Some(window_handle) = self.connection_directory_window_handle.clone()
                && window_handle
                    .update(cx, |window_state, window, cx| {
                        window_state.replace_form(initial_form.clone(), mode);
                        window.activate_window();
                        cx.notify();
                    })
                    .is_ok()
            {
                self.placeholder_notice = "已打开目录编辑窗口".to_string();
                return;
            }
            self.is_connection_directory_window_open = false;
            self.connection_directory_window_handle = None;
        }

        self.open_connection_directory_window_with_form(initial_form, mode, cx);
    }

    /// 打开编辑 SSH 链接独立窗口。
    fn open_edit_ssh_link_window(&mut self, link_id: ConnectionNodeId, cx: &mut Context<Self>) {
        let Some(link) = self.config.connections.link(link_id).cloned() else {
            self.placeholder_notice = "未找到可编辑的 SSH 链接".to_string();
            return;
        };
        let initial_form = ConnectionLinkFormState {
            parent_id: link.parent_id,
            name_input: SettingsTextInputState::from_value(link.name),
            host_input: SettingsTextInputState::from_value(link.ssh.host),
            port_input: SettingsTextInputState::from_value(link.ssh.port.to_string()),
            username_input: SettingsTextInputState::from_value(link.ssh.username),
            password_input: SettingsTextInputState::from_value(link.ssh.password),
            private_key_path_input: SettingsTextInputState::from_value(
                link.ssh.private_key_path.unwrap_or_default(),
            ),
            private_key_passphrase_input: SettingsTextInputState::from_value(
                link.ssh.private_key_passphrase.unwrap_or_default(),
            ),
            error_message: None,
        };
        let mode = ConnectionLinkWindowMode::Edit { link_id };

        if self.is_connection_link_window_open {
            if let Some(window_handle) = self.connection_link_window_handle.clone()
                && window_handle
                    .update(cx, |window_state, window, cx| {
                        window_state.replace_form(initial_form.clone(), mode);
                        window.activate_window();
                        cx.notify();
                    })
                    .is_ok()
            {
                self.placeholder_notice = "已打开链接编辑窗口".to_string();
                return;
            }
            self.is_connection_link_window_open = false;
            self.connection_link_window_handle = None;
        }

        self.open_connection_link_window_with_form(initial_form, mode, cx);
    }

    /// 使用指定表单打开目录窗口，供编辑入口复用创建窗口参数。
    fn open_connection_directory_window_with_form(
        &mut self,
        initial_form: ConnectionDirectoryFormState,
        mode: ConnectionDirectoryWindowMode,
        cx: &mut Context<Self>,
    ) {
        let app_entity = cx.entity();
        let initial_theme = self.theme.clone();
        let bounds = Bounds::centered(
            None,
            size(
                px(CONNECTION_DIRECTORY_WINDOW_WIDTH),
                px(CONNECTION_DIRECTORY_WINDOW_HEIGHT),
            ),
            cx,
        );
        let window_options = WindowOptions {
            titlebar: None,
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            window_min_size: Some(size(
                px(CONNECTION_DIRECTORY_WINDOW_MIN_WIDTH),
                px(CONNECTION_DIRECTORY_WINDOW_MIN_HEIGHT),
            )),
            ..Default::default()
        };

        self.is_connection_directory_window_open = true;
        self.placeholder_notice = match mode {
            ConnectionDirectoryWindowMode::Create => "请输入链接目录名称".to_string(),
            ConnectionDirectoryWindowMode::Edit { .. } => "请编辑链接目录名称".to_string(),
        };

        match cx.open_window(window_options, move |_, cx| {
            cx.new(|cx| {
                ConnectionDirectoryWindow::new(app_entity, initial_theme, initial_form, mode, cx)
            })
        }) {
            Ok(window_handle) => {
                self.connection_directory_window_handle = Some(window_handle);
            }
            Err(error) => {
                self.is_connection_directory_window_open = false;
                self.connection_directory_window_handle = None;
                self.placeholder_notice = format!("打开目录窗口失败：{error}");
            }
        }
    }

    /// 使用指定表单打开 SSH 链接窗口，供编辑入口复用创建窗口参数。
    fn open_connection_link_window_with_form(
        &mut self,
        initial_form: ConnectionLinkFormState,
        mode: ConnectionLinkWindowMode,
        cx: &mut Context<Self>,
    ) {
        let app_entity = cx.entity();
        let initial_theme = self.theme.clone();
        let bounds = Bounds::centered(
            None,
            size(
                px(CONNECTION_LINK_WINDOW_WIDTH),
                px(CONNECTION_LINK_WINDOW_HEIGHT),
            ),
            cx,
        );
        let window_options = WindowOptions {
            titlebar: None,
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            window_min_size: Some(size(
                px(CONNECTION_LINK_WINDOW_MIN_WIDTH),
                px(CONNECTION_LINK_WINDOW_MIN_HEIGHT),
            )),
            ..Default::default()
        };

        self.is_connection_link_window_open = true;
        self.placeholder_notice = match mode {
            ConnectionLinkWindowMode::Create => "请输入 SSH 链接信息".to_string(),
            ConnectionLinkWindowMode::Edit { .. } => "请编辑 SSH 链接信息".to_string(),
        };

        match cx.open_window(window_options, move |_, cx| {
            cx.new(|cx| {
                ConnectionLinkWindow::new(app_entity, initial_theme, initial_form, mode, cx)
            })
        }) {
            Ok(window_handle) => {
                self.connection_link_window_handle = Some(window_handle);
            }
            Err(error) => {
                self.is_connection_link_window_open = false;
                self.connection_link_window_handle = None;
                self.placeholder_notice = format!("打开链接窗口失败：{error}");
            }
        }
    }

    /// 关闭当前链接工作区弹窗。
    pub fn close_connection_dialog(&mut self) {
        if let Some(ConnectionDialogState::ConfirmHostKey(prompt)) = self.connection_dialog.clone()
        {
            self.reject_connection_host_key_prompt(prompt);
            return;
        }
        self.connection_dialog = None;
        self.placeholder_notice = "已关闭链接弹窗".to_string();
    }

    /// 提交当前链接工作区弹窗。
    pub fn submit_connection_dialog(&mut self) {
        match self.connection_dialog.clone() {
            Some(ConnectionDialogState::NewDirectory(form)) => {
                self.submit_connection_directory_form(form)
            }
            Some(ConnectionDialogState::NewSshLink(form)) => self.submit_ssh_link_form(form),
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
    pub fn focus_connection_text_input_target(&mut self, target: AppTextInputTarget) {
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
    pub fn clear_connection_text_input_focuses(&mut self) {
        clear_connection_input_focus(&mut self.connection_tree_search_input);
        if let Some(dialog) = self.connection_dialog.as_mut() {
            match dialog {
                ConnectionDialogState::NewDirectory(form) => {
                    clear_connection_input_focus(&mut form.name_input);
                }
                ConnectionDialogState::NewSshLink(form) => {
                    clear_connection_input_focus(&mut form.name_input);
                    clear_connection_input_focus(&mut form.host_input);
                    clear_connection_input_focus(&mut form.port_input);
                    clear_connection_input_focus(&mut form.username_input);
                    clear_connection_input_focus(&mut form.password_input);
                    clear_connection_input_focus(&mut form.private_key_path_input);
                    clear_connection_input_focus(&mut form.private_key_passphrase_input);
                }
                ConnectionDialogState::ConfirmHostKey(_)
                | ConnectionDialogState::ConfirmDelete(_) => {}
            }
        }
    }

    /// 返回链接相关输入框的只读引用。
    pub(crate) fn connection_text_input(
        &self,
        target: AppTextInputTarget,
    ) -> Option<&SettingsTextInputState> {
        match target {
            AppTextInputTarget::ConnectionTreeSearch => Some(&self.connection_tree_search_input),
            AppTextInputTarget::ConnectionDirectoryName => {
                match self.connection_dialog.as_ref()? {
                    ConnectionDialogState::NewDirectory(form) => Some(&form.name_input),
                    _ => None,
                }
            }
            AppTextInputTarget::ConnectionLinkName => match self.connection_dialog.as_ref()? {
                ConnectionDialogState::NewSshLink(form) => Some(&form.name_input),
                _ => None,
            },
            AppTextInputTarget::ConnectionLinkHost => match self.connection_dialog.as_ref()? {
                ConnectionDialogState::NewSshLink(form) => Some(&form.host_input),
                _ => None,
            },
            AppTextInputTarget::ConnectionLinkPort => match self.connection_dialog.as_ref()? {
                ConnectionDialogState::NewSshLink(form) => Some(&form.port_input),
                _ => None,
            },
            AppTextInputTarget::ConnectionLinkUsername => match self.connection_dialog.as_ref()? {
                ConnectionDialogState::NewSshLink(form) => Some(&form.username_input),
                _ => None,
            },
            AppTextInputTarget::ConnectionLinkPassword => match self.connection_dialog.as_ref()? {
                ConnectionDialogState::NewSshLink(form) => Some(&form.password_input),
                _ => None,
            },
            AppTextInputTarget::ConnectionLinkPrivateKeyPath => {
                match self.connection_dialog.as_ref()? {
                    ConnectionDialogState::NewSshLink(form) => Some(&form.private_key_path_input),
                    _ => None,
                }
            }
            AppTextInputTarget::ConnectionLinkPrivateKeyPassphrase => {
                match self.connection_dialog.as_ref()? {
                    ConnectionDialogState::NewSshLink(form) => {
                        Some(&form.private_key_passphrase_input)
                    }
                    _ => None,
                }
            }
            _ => None,
        }
    }

    /// 返回链接相关输入框的可变引用。
    pub(crate) fn connection_text_input_mut(
        &mut self,
        target: AppTextInputTarget,
    ) -> Option<&mut SettingsTextInputState> {
        match target {
            AppTextInputTarget::ConnectionTreeSearch => {
                Some(&mut self.connection_tree_search_input)
            }
            AppTextInputTarget::ConnectionDirectoryName => {
                match self.connection_dialog.as_mut()? {
                    ConnectionDialogState::NewDirectory(form) => Some(&mut form.name_input),
                    _ => None,
                }
            }
            AppTextInputTarget::ConnectionLinkName => match self.connection_dialog.as_mut()? {
                ConnectionDialogState::NewSshLink(form) => Some(&mut form.name_input),
                _ => None,
            },
            AppTextInputTarget::ConnectionLinkHost => match self.connection_dialog.as_mut()? {
                ConnectionDialogState::NewSshLink(form) => Some(&mut form.host_input),
                _ => None,
            },
            AppTextInputTarget::ConnectionLinkPort => match self.connection_dialog.as_mut()? {
                ConnectionDialogState::NewSshLink(form) => Some(&mut form.port_input),
                _ => None,
            },
            AppTextInputTarget::ConnectionLinkUsername => match self.connection_dialog.as_mut()? {
                ConnectionDialogState::NewSshLink(form) => Some(&mut form.username_input),
                _ => None,
            },
            AppTextInputTarget::ConnectionLinkPassword => match self.connection_dialog.as_mut()? {
                ConnectionDialogState::NewSshLink(form) => Some(&mut form.password_input),
                _ => None,
            },
            AppTextInputTarget::ConnectionLinkPrivateKeyPath => {
                match self.connection_dialog.as_mut()? {
                    ConnectionDialogState::NewSshLink(form) => {
                        Some(&mut form.private_key_path_input)
                    }
                    _ => None,
                }
            }
            AppTextInputTarget::ConnectionLinkPrivateKeyPassphrase => {
                match self.connection_dialog.as_mut()? {
                    ConnectionDialogState::NewSshLink(form) => {
                        Some(&mut form.private_key_passphrase_input)
                    }
                    _ => None,
                }
            }
            _ => None,
        }
    }

    /// 处理链接树或链接表单输入框的非文本按键。
    pub fn handle_connection_text_input_key(
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
    pub fn begin_connection_input_pointer_selection(
        &mut self,
        target: AppTextInputTarget,
        character_index: usize,
        granularity: TextSelectionGranularity,
    ) {
        self.focus_connection_text_input_target(target);
        let Some(input) = self.connection_text_input_mut(target) else {
            return;
        };
        let range = connection_input_range_for_granularity(input, character_index, granularity);
        input.selection_anchor = Some(range.start);
        input.cursor = range.end;
        input.selection_drag = Some(InputTextSelectionDrag {
            anchor_range: range,
            granularity,
        });
        input.marked_range = None;
    }

    /// 鼠标拖拽更新链接输入框选区。
    pub fn update_connection_input_pointer_selection(
        &mut self,
        target: AppTextInputTarget,
        character_index: usize,
    ) {
        let Some(input) = self.connection_text_input_mut(target) else {
            return;
        };
        let Some(drag) = input.selection_drag.clone() else {
            return;
        };
        let focus_range =
            connection_input_range_for_granularity(input, character_index, drag.granularity);
        if focus_range.start < drag.anchor_range.start {
            input.selection_anchor = Some(drag.anchor_range.end);
            input.cursor = focus_range.start;
        } else {
            input.selection_anchor = Some(drag.anchor_range.start);
            input.cursor = focus_range.end;
        }
        input.marked_range = None;
    }

    /// 鼠标结束链接输入框文本选择。
    pub fn finish_connection_input_pointer_selection(&mut self, target: AppTextInputTarget) {
        if let Some(input) = self.connection_text_input_mut(target) {
            input.selection_drag = None;
            if normalized_connection_input_selection_range(input).is_none() {
                input.selection_anchor = None;
            }
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

    /// 提交新增目录表单。
    fn submit_connection_directory_form(&mut self, form: ConnectionDirectoryFormState) {
        match self.create_connection_directory_from_form(form) {
            Ok(_) => self.connection_dialog = None,
            Err(error) => self.update_directory_form_error(error.to_string()),
        }
    }

    /// 提交新增 SSH 链接表单。
    fn submit_ssh_link_form(&mut self, form: ConnectionLinkFormState) {
        match self.create_ssh_link_from_form(form) {
            Ok(_) => self.connection_dialog = None,
            Err(error) => self.update_link_form_error(error),
        }
    }

    /// 校验并创建链接目录，供独立窗口和兼容弹窗共同复用。
    pub fn create_connection_directory_from_form(
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

    /// 校验并创建 SSH 链接，供独立窗口和兼容弹窗共同复用。
    pub fn create_ssh_link_from_form(
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

    /// 校验并更新链接目录名称。
    pub fn update_connection_directory_from_form(
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
    pub fn update_ssh_link_from_form(
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

    /// 请求删除链接目录或 SSH 链接；删除前必须进入二次确认。
    pub fn request_delete_connection_node(&mut self, node_id: ConnectionNodeId) {
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
            self.connection_dialog = Some(ConnectionDialogState::ConfirmDelete(
                ConnectionDeletePromptState {
                    node_id,
                    label: link.name.clone(),
                    is_directory: false,
                },
            ));
            self.placeholder_notice = "请确认是否删除 SSH 链接".to_string();
            return;
        }

        self.placeholder_notice = "未找到可删除的连接节点".to_string();
    }

    /// 确认删除链接目录或 SSH 链接；目录只能在没有子节点时删除。
    pub fn confirm_delete_connection_node(&mut self, node_id: ConnectionNodeId) {
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
            Err(error) => {
                self.placeholder_notice = error.to_string();
            }
        }
    }

    /// 更新新增目录表单错误。
    fn update_directory_form_error(&mut self, message: String) {
        if let Some(ConnectionDialogState::NewDirectory(form)) = self.connection_dialog.as_mut() {
            form.error_message = Some(message.clone());
        }
        self.placeholder_notice = message;
    }

    /// 更新新增 SSH 链接表单错误。
    fn update_link_form_error(&mut self, message: String) {
        if let Some(ConnectionDialogState::NewSshLink(form)) = self.connection_dialog.as_mut() {
            form.error_message = Some(message.clone());
        }
        self.placeholder_notice = message;
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

/// 清理单个链接输入框焦点态。
fn clear_connection_input_focus(input: &mut SettingsTextInputState) {
    input.is_focused = false;
    input.selection_anchor = None;
    input.marked_range = None;
    input.selection_drag = None;
}

/// 返回输入框规范化后的非空选区。
pub(crate) fn normalized_connection_input_selection_range(
    input: &SettingsTextInputState,
) -> Option<Range<usize>> {
    let anchor = input.selection_anchor?;
    if anchor == input.cursor {
        return None;
    }
    Some(anchor.min(input.cursor)..anchor.max(input.cursor))
}

/// 按鼠标点击粒度生成链接输入框字符范围。
fn connection_input_range_for_granularity(
    input: &SettingsTextInputState,
    character_index: usize,
    granularity: TextSelectionGranularity,
) -> Range<usize> {
    let text_length = character_count(&input.value);
    let cursor = character_index.min(text_length);
    match granularity {
        TextSelectionGranularity::Character => cursor..cursor,
        TextSelectionGranularity::Word => {
            word_range_at(&input.value, cursor).unwrap_or(cursor..cursor)
        }
        TextSelectionGranularity::Line => 0..text_length,
    }
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
        app.connection_dialog = Some(ConnectionDialogState::NewDirectory(
            ConnectionDirectoryFormState {
                parent_id: None,
                name_input: SettingsTextInputState::from_value("生产环境".to_string()),
                error_message: None,
            },
        ));

        app.submit_connection_dialog();

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
        app.connection_dialog = Some(ConnectionDialogState::NewSshLink(ConnectionLinkFormState {
            parent_id: None,
            name_input: SettingsTextInputState::from_value("app-01".to_string()),
            host_input: SettingsTextInputState::from_value("10.0.0.1".to_string()),
            port_input: SettingsTextInputState::from_value("70000".to_string()),
            username_input: SettingsTextInputState::from_value("deploy".to_string()),
            password_input: SettingsTextInputState::from_value("secret".to_string()),
            private_key_path_input: SettingsTextInputState::default(),
            private_key_passphrase_input: SettingsTextInputState::default(),
            error_message: None,
        }));

        app.submit_connection_dialog();

        assert!(app.config.connections.links.is_empty());
        assert!(matches!(
            app.connection_dialog,
            Some(ConnectionDialogState::NewSshLink(_))
        ));
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
