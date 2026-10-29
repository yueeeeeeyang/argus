//! 文件职责：渲染链接工作区的目录、链接和确认模态框。
//! 创建日期：2026-06-26
//! 修改日期：2026-10-29
//! 作者：Argus 开发团队
//! 主要功能：提供目录与多协议链接表单、共享凭据管理、统一删除确认样式，并展示首次连接主机指纹确认。

use std::ops::Range;
use std::sync::Arc;

use gpui::{
    AnyElement, App, ClickEvent, ClipboardItem, Context, Entity, FocusHandle, FontWeight,
    IntoElement, KeyDownEvent, MouseButton, MouseDownEvent, Render, SharedString, Subscription,
    Window, div, prelude::*, px, rgb,
};

use crate::app::{
    ArgusApp, ConnectionCredentialFormState, ConnectionDeletePromptState, ConnectionDialogState,
    ConnectionDirectoryFormState, ConnectionHostKeyPromptState, ConnectionLinkFormState,
    ConnectionSvnCertificatePromptState, TextInputState,
};
use crate::fonts::ARGUS_UI_FONT_FAMILY;
use crate::infra::text_selection::{
    NativeTextEdit, TextSelectionGranularity, character_count, replace_character_range,
    slice_character_range,
};
use crate::remote::connection::{ConnectionCredentialId, ConnectionLinkKind};
use crate::remote::svn_certificate::SvnCertificateDecision;
use crate::theme::AppTheme;
use crate::ui::components::dropdown::{Dropdown, DropdownItem, render_dropdown};
use crate::ui::components::icon::{ArgusIcon, render_icon};
use crate::ui::components::icon_button::{IconButtonSize, render_icon_button};
use crate::ui::components::input::{
    Input, InputPointerAction, InputPointerEvent, InputSize, NativeInput, render_input,
};
use crate::ui::components::modal_dialog::{
    MODAL_ACTION_HEIGHT, MODAL_BOTTOM_PADDING, MODAL_FORM_NOTE_HEIGHT, MODAL_FORM_ROW_GAP,
    MODAL_FORM_ROW_HEIGHT, ModalDialog, render_modal_dialog,
};

/// 新增目录窗口固定标题栏高度，和设置窗口保持一致。
const CONNECTION_WINDOW_HEADER_HEIGHT: f32 = 56.0;
/// 目录/链接模态框标题图标尺寸，匹配设置模态框标题视觉比例。
const CONNECTION_WINDOW_TITLE_ICON_SIZE: f32 = 16.0;
/// 目录表单模态框宽度，沿用改造前独立窗口尺寸。
const CONNECTION_DIRECTORY_MODAL_WIDTH: f32 = 420.0;
/// 链接表单模态框宽度，沿用改造前独立窗口尺寸。
const CONNECTION_LINK_MODAL_WIDTH: f32 = 520.0;
/// 共用凭据管理模态框宽度。
const CONNECTION_CREDENTIAL_MODAL_WIDTH: f32 = 680.0;
/// 共用凭据管理模态框高度。
const CONNECTION_CREDENTIAL_MODAL_HEIGHT: f32 = 520.0;
/// 链接表单凭据下拉浮层相对弹窗顶部的偏移。
///
/// 凭据行固定在名称行之后；浮层挂在弹窗根节点而不是滚动容器内，避免被表单
/// 滚动区域裁切，同时保持点击命中顺序正确。
const CONNECTION_LINK_CREDENTIAL_MENU_TOP: f32 = 132.0;
/// 链接表单凭据下拉浮层相对弹窗左侧的偏移。
const CONNECTION_LINK_CREDENTIAL_MENU_LEFT: f32 = 114.0;
/// 链接表单凭据下拉浮层宽度。
const CONNECTION_LINK_CREDENTIAL_MENU_WIDTH: f32 = 260.0;
/// 带凭据选择行时，目录下拉浮层相对弹窗顶部的偏移。
const CONNECTION_LINK_DIRECTORY_MENU_TOP_WITH_CREDENTIAL: f32 = 174.0;
/// Git 表单没有凭据行时，目录下拉浮层相对弹窗顶部的偏移。
const CONNECTION_LINK_DIRECTORY_MENU_TOP_REPOSITORY: f32 = 132.0;
/// 表单滚动区滚动条宽度；滚动条槽位会占用正文可用宽度。
const CONNECTION_FORM_SCROLLBAR_WIDTH: f32 = 6.0;
/// 表单滚动区右侧内边距：左右 20px 视觉边距扣除滚动条槽位，保证输入框右缘与底栏按钮对齐。
const CONNECTION_FORM_SCROLL_RIGHT_PADDING: f32 = 14.0;
/// 连接弹窗内容根节点 1px 边框在上下两侧占用的高度。
///
/// 边框在布局盒内，计算外壳高度时必须计入，否则正文会被压缩 2px 并出现微小滚动。
const CONNECTION_WINDOW_BORDER_HEIGHT: f32 = 2.0;
/// 表单内容与底部操作区之间的间距，和表单行间距之外的呼吸区保持一致。
const CONNECTION_WINDOW_FOOTER_GAP: f32 = 20.0;
/// 底部操作区高度：上间距 + 操作按钮。
const CONNECTION_WINDOW_FOOTER_HEIGHT: f32 = CONNECTION_WINDOW_FOOTER_GAP + MODAL_ACTION_HEIGHT;
/// 主机指纹确认弹窗宽度。
const HOST_KEY_DIALOG_WIDTH: f32 = 520.0;
/// 证书元信息较长，正文允许滚动而标题和三个明确操作保持固定。
const SVN_CERTIFICATE_DIALOG_WIDTH: f32 = 680.0;
const SVN_CERTIFICATE_DIALOG_HEIGHT: f32 = 580.0;
/// 主机指纹确认弹窗高度：标题栏 + 说明 + 指纹框 + 底部操作区 + 底部内边距。
const HOST_KEY_DIALOG_HEIGHT: f32 = CONNECTION_WINDOW_HEADER_HEIGHT
    + 18.0
    + MODAL_FORM_ROW_GAP
    + 42.0
    + CONNECTION_WINDOW_FOOTER_GAP
    + 32.0
    + MODAL_BOTTOM_PADDING;
/// 删除确认弹窗宽度。
const DELETE_DIALOG_WIDTH: f32 = 420.0;
/// 删除确认弹窗高度：标题栏 + 说明 + 提示 + 底部操作区 + 底部内边距。
const DELETE_DIALOG_HEIGHT: f32 = CONNECTION_WINDOW_HEADER_HEIGHT
    + 18.0
    + MODAL_FORM_ROW_GAP
    + 16.0
    + CONNECTION_WINDOW_FOOTER_GAP
    + 32.0
    + MODAL_BOTTOM_PADDING
    + CONNECTION_WINDOW_BORDER_HEIGHT;

/// 目录窗口模式，区分新增和编辑已有目录。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ConnectionDirectoryWindowMode {
    /// 新增目录。
    Create,
    /// 编辑已有目录。
    Edit {
        /// 正在编辑的目录节点 ID。
        directory_id: crate::remote::connection::ConnectionNodeId,
    },
}

/// SSH 链接窗口模式，区分新增和编辑已有链接。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ConnectionLinkWindowMode {
    /// 新增 SSH 链接。
    Create,
    /// 编辑已有 SSH 链接。
    Edit {
        /// 正在编辑的 SSH 链接节点 ID。
        link_id: crate::remote::connection::ConnectionNodeId,
    },
}

/// 目录表单模态框子视图；表单状态保存在子视图内，提交时写回主应用配置。
pub(crate) struct ConnectionDirectoryWindow {
    /// 主应用实体，提交和关闭状态同步都写回 `ArgusApp`。
    app: Entity<ArgusApp>,
    /// 当前模态框使用的主题快照。
    theme: AppTheme,
    /// 目录表单状态。
    form: ConnectionDirectoryFormState,
    /// 当前模态框处于新增还是编辑模式。
    mode: ConnectionDirectoryWindowMode,
    /// 模态框根区域和输入框焦点句柄。
    focus_handles: ConnectionDirectoryWindowFocusHandles,
    /// 主应用状态订阅，主题切换后模态框跟随刷新。
    _app_observer: Subscription,
}

/// 目录表单窗口焦点句柄集合。
#[derive(Clone)]
struct ConnectionDirectoryWindowFocusHandles {
    /// 根区域焦点，用于点击非输入区域时承接键盘焦点。
    root: FocusHandle,
    /// 目录名称输入框真实焦点。
    name: FocusHandle,
}

impl ConnectionDirectoryWindow {
    /// 创建目录表单模态框子视图。
    ///
    /// 参数说明：
    /// - `app`：主应用实体。
    /// - `theme`：首次绘制使用的主题。
    /// - `form`：按当前选中目录推导出的初始表单。
    /// - `cx`：子视图上下文，用于创建焦点句柄和订阅主应用变化。
    ///
    /// 返回值：可渲染的目录表单模态框子视图。
    pub(crate) fn new(
        app: Entity<ArgusApp>,
        theme: AppTheme,
        mut form: ConnectionDirectoryFormState,
        mode: ConnectionDirectoryWindowMode,
        cx: &mut Context<Self>,
    ) -> Self {
        focus_input_state(&mut form.name_input);
        let _app_observer = cx.observe(&app, |window_state, app_entity, cx| {
            let theme = app_entity.read_with(cx, |app, _| app.theme.clone());
            if window_state.theme == theme {
                return;
            }
            window_state.theme = theme;
            cx.notify();
        });

        Self {
            app,
            theme,
            form,
            mode,
            focus_handles: ConnectionDirectoryWindowFocusHandles {
                root: cx.focus_handle(),
                name: cx.focus_handle(),
            },
            _app_observer,
        }
    }

    /// 判断当前目录模态框是否为指定模式，用于重复点击按钮时决定是否替换表单。
    pub(crate) fn is_mode(&self, mode: ConnectionDirectoryWindowMode) -> bool {
        self.mode == mode
    }

    /// 替换目录模态框表单和模式，供右键编辑入口复用已经打开的子视图。
    pub(crate) fn replace_form(
        &mut self,
        mut form: ConnectionDirectoryFormState,
        mode: ConnectionDirectoryWindowMode,
    ) {
        focus_input_state(&mut form.name_input);
        self.form = form;
        self.mode = mode;
    }

    /// 聚焦目录窗口中的指定输入框。
    fn focus_input(&mut self, target: ConnectionFormInputTarget) {
        clear_input_focus_state(&mut self.form.name_input);
        if let Some(input) = self.input_mut(target) {
            focus_input_state(input);
        }
    }

    /// 清理目录窗口中的所有输入焦点。
    fn clear_input_focuses(&mut self) {
        clear_input_focus_state(&mut self.form.name_input);
    }

    /// 返回目录窗口输入框的可变引用。
    fn input_mut(&mut self, target: ConnectionFormInputTarget) -> Option<&mut TextInputState> {
        match target {
            ConnectionFormInputTarget::DirectoryName => Some(&mut self.form.name_input),
            _ => None,
        }
    }

    /// 处理目录模态框输入事件，并返回是否需要提交或关闭模态框。
    fn handle_input_key(
        &mut self,
        target: ConnectionFormInputTarget,
        keystroke: &gpui::Keystroke,
        cx: &mut Context<Self>,
    ) -> ConnectionWindowInputAction {
        let Some(input) = self.input_mut(target) else {
            return ConnectionWindowInputAction::None;
        };
        if let Some(action) = handle_text_input_clipboard(input, keystroke, cx) {
            if action == ConnectionWindowInputAction::Changed {
                self.form.error_message = None;
            }
            return action;
        }
        let action = handle_text_input_key(input, keystroke);
        if action == ConnectionWindowInputAction::Changed {
            self.form.error_message = None;
        }
        action
    }

    /// 鼠标开始选择目录窗口输入框文本。
    fn begin_pointer_selection(
        &mut self,
        target: ConnectionFormInputTarget,
        character_index: usize,
        granularity: TextSelectionGranularity,
    ) {
        self.focus_input(target);
        let Some(input) = self.input_mut(target) else {
            return;
        };
        begin_input_pointer_selection(input, character_index, granularity);
    }

    /// 鼠标拖拽更新目录窗口输入框选区。
    fn update_pointer_selection(
        &mut self,
        target: ConnectionFormInputTarget,
        character_index: usize,
    ) {
        let Some(input) = self.input_mut(target) else {
            return;
        };
        update_input_pointer_selection(input, character_index);
    }

    /// 鼠标结束目录窗口输入框文本选择。
    fn finish_pointer_selection(&mut self, target: ConnectionFormInputTarget) {
        let Some(input) = self.input_mut(target) else {
            return;
        };
        finish_input_pointer_selection(input);
    }

    /// 应用系统输入法提交的目录窗口文本编辑。
    fn apply_native_edit(&mut self, target: ConnectionFormInputTarget, edit: NativeTextEdit) {
        self.focus_input(target);
        let Some(input) = self.input_mut(target) else {
            return;
        };
        apply_native_edit_to_input(input, &edit);
        self.form.error_message = None;
    }
}

impl Render for ConnectionDirectoryWindow {
    /// 渲染目录表单模态框主体。
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        render_directory_window_content(
            self.app.clone(),
            cx.entity(),
            self.theme.clone(),
            self.form.clone(),
            self.mode,
            self.focus_handles.clone(),
        )
    }
}

/// 链接表单模态框子视图；表单状态保存在子视图内，提交时写回主应用配置。
pub(crate) struct ConnectionLinkWindow {
    /// 主应用实体，提交和关闭状态同步都写回 `ArgusApp`。
    app: Entity<ArgusApp>,
    /// 当前模态框使用的主题快照。
    theme: AppTheme,
    /// SSH 链接表单状态。
    form: ConnectionLinkFormState,
    /// 当前模态框处于新增还是编辑模式。
    mode: ConnectionLinkWindowMode,
    /// 凭据下拉菜单是否展开。
    credential_menu_open: bool,
    /// 目录下拉菜单是否展开。
    directory_menu_open: bool,
    /// 模态框根区域和输入框焦点句柄。
    focus_handles: ConnectionLinkWindowFocusHandles,
    /// 主应用状态订阅，主题切换后模态框跟随刷新。
    _app_observer: Subscription,
}

/// SSH 链接表单窗口焦点句柄集合。
#[derive(Clone)]
struct ConnectionLinkWindowFocusHandles {
    /// 根区域焦点，用于点击非输入区域时承接键盘焦点。
    root: FocusHandle,
    /// 链接名称输入框真实焦点。
    name: FocusHandle,
    /// 主机输入框真实焦点。
    host: FocusHandle,
    /// 仓库 URL 输入框真实焦点。
    url: FocusHandle,
    /// 端口输入框真实焦点。
    port: FocusHandle,
    /// 用户名输入框真实焦点。
    username: FocusHandle,
    /// 密码输入框真实焦点。
    password: FocusHandle,
    /// SMB 共享名称输入框真实焦点。
    share: FocusHandle,
    /// SMB 初始目录输入框真实焦点。
    initial_dir: FocusHandle,
    /// SMB 域或工作组输入框真实焦点。
    domain: FocusHandle,
    /// 私钥路径输入框真实焦点。
    private_key_path: FocusHandle,
    /// 私钥口令输入框真实焦点。
    private_key_passphrase: FocusHandle,
}

impl ConnectionLinkWindow {
    /// 创建链接表单模态框子视图。
    ///
    /// 参数说明：
    /// - `app`：主应用实体。
    /// - `theme`：首次绘制使用的主题。
    /// - `form`：按当前选中目录推导出的初始表单。
    /// - `cx`：子视图上下文，用于创建焦点句柄和订阅主应用变化。
    ///
    /// 返回值：可渲染的链接表单模态框子视图。
    pub(crate) fn new(
        app: Entity<ArgusApp>,
        theme: AppTheme,
        mut form: ConnectionLinkFormState,
        mode: ConnectionLinkWindowMode,
        cx: &mut Context<Self>,
    ) -> Self {
        focus_input_state(&mut form.name_input);
        let _app_observer = cx.observe(&app, |window_state, app_entity, cx| {
            let theme = app_entity.read_with(cx, |app, _| app.theme.clone());
            if window_state.theme == theme {
                return;
            }
            window_state.theme = theme;
            cx.notify();
        });

        Self {
            app,
            theme,
            form,
            mode,
            credential_menu_open: false,
            directory_menu_open: false,
            focus_handles: ConnectionLinkWindowFocusHandles {
                root: cx.focus_handle(),
                name: cx.focus_handle(),
                host: cx.focus_handle(),
                url: cx.focus_handle(),
                port: cx.focus_handle(),
                username: cx.focus_handle(),
                password: cx.focus_handle(),
                share: cx.focus_handle(),
                initial_dir: cx.focus_handle(),
                domain: cx.focus_handle(),
                private_key_path: cx.focus_handle(),
                private_key_passphrase: cx.focus_handle(),
            },
            _app_observer,
        }
    }

    /// 判断当前链接模态框是否为指定模式，用于重复点击按钮时决定是否替换表单。
    pub(crate) fn is_mode(&self, mode: ConnectionLinkWindowMode) -> bool {
        self.mode == mode
    }

    /// 返回当前链接模态框表单协议，用于重复打开不同协议表单时判断是否需要替换。
    pub(crate) fn link_kind(&self) -> ConnectionLinkKind {
        self.form.link_kind
    }

    /// 返回当前表单选中的共用凭据，供删除保护和交互测试使用。
    pub(crate) fn selected_credential_id(&self) -> Option<ConnectionCredentialId> {
        self.form.credential_id
    }

    /// 返回当前表单选中的共用凭据，供交互测试断言。
    #[cfg(test)]
    pub(crate) fn credential_id_for_test(&self) -> Option<ConnectionCredentialId> {
        self.form.credential_id
    }

    /// 返回当前表单父目录，供菜单新建链路断言父目录推导。
    #[cfg(test)]
    pub(crate) fn form_parent_id(&self) -> Option<crate::remote::connection::ConnectionNodeId> {
        self.form.parent_id
    }

    /// 根据当前协议、URL 与校验状态返回模态框应使用的内容高度。
    ///
    /// Git/SVN 会随 URL 传输方式增减密码或私钥字段，因此不能按协议使用一个
    /// 固定高度。返回值包含通用模态外壳的内边距。
    pub(crate) fn preferred_modal_height(&self) -> f32 {
        preferred_connection_link_modal_height(&self.form)
    }

    /// 替换链接模态框表单和模式，供右键编辑入口复用已经打开的子视图。
    pub(crate) fn replace_form(
        &mut self,
        mut form: ConnectionLinkFormState,
        mode: ConnectionLinkWindowMode,
    ) {
        focus_input_state(&mut form.name_input);
        self.form = form;
        self.mode = mode;
        self.credential_menu_open = false;
        self.directory_menu_open = false;
    }

    /// 切换凭据下拉菜单。
    fn toggle_credential_menu(&mut self) {
        self.credential_menu_open = !self.credential_menu_open;
        self.directory_menu_open = false;
    }

    /// 切换所属目录下拉菜单。
    fn toggle_directory_menu(&mut self) {
        self.directory_menu_open = !self.directory_menu_open;
        self.credential_menu_open = false;
    }

    /// 选择新增或编辑链接所属的目录。
    fn select_directory(
        &mut self,
        directory_id: Option<crate::remote::connection::ConnectionNodeId>,
    ) {
        self.form.parent_id = directory_id;
        self.directory_menu_open = false;
        self.credential_menu_open = false;
        self.form.error_message = None;
    }

    /// 选择共用凭据或切回手动输入。
    fn select_credential(
        &mut self,
        credential_id: Option<ConnectionCredentialId>,
        cx: &mut Context<Self>,
    ) {
        self.credential_menu_open = false;
        self.directory_menu_open = false;
        self.form.credential_id = credential_id;
        self.form.save_as_credential = false;
        self.form.error_message = None;
        self.form.password_input = TextInputState::default();
        if let Some(credential_id) = credential_id
            && let Some(credential) = self.app.read_with(cx, |app, _| {
                app.config.connections.credential(credential_id).cloned()
            })
        {
            self.form.username_input = TextInputState::from_value(credential.username);
            if self.form.link_kind == ConnectionLinkKind::Smb {
                self.form.domain_input =
                    TextInputState::from_value(credential.domain.clone().unwrap_or_default());
            }
        }
    }

    /// 打开共用凭据管理弹窗，并优先勾选当前协议。
    fn open_credential_manager(&mut self, cx: &mut Context<Self>) {
        let protocol = self.form.link_kind;
        self.credential_menu_open = false;
        self.directory_menu_open = false;
        self.app.update(cx, |app, app_cx| {
            app.open_connection_credential_manager(protocol, app_cx)
        });
    }

    /// 切换“保存为共用凭据”选项。
    fn toggle_save_as_credential(&mut self) {
        self.form.save_as_credential = !self.form.save_as_credential;
    }

    /// 应用凭据下拉选项。
    fn apply_credential_menu_selection(&mut self, item_id: &str, cx: &mut Context<Self>) {
        if item_id == "manage" {
            self.open_credential_manager(cx);
        } else if item_id == "inline" {
            self.select_credential(None, cx);
        } else if let Some(credential_id) = item_id
            .strip_prefix("credential-")
            .and_then(|value| value.parse::<usize>().ok())
        {
            self.select_credential(Some(credential_id), cx);
        }
    }

    /// 聚焦链接窗口中的指定输入框。
    fn focus_input(&mut self, target: ConnectionFormInputTarget) {
        self.clear_input_focuses();
        if let Some(input) = self.input_mut(target) {
            focus_input_state(input);
        }
    }

    /// 清理链接窗口中的所有输入焦点。
    fn clear_input_focuses(&mut self) {
        clear_input_focus_state(&mut self.form.name_input);
        clear_input_focus_state(&mut self.form.host_input);
        clear_input_focus_state(&mut self.form.url_input);
        clear_input_focus_state(&mut self.form.port_input);
        clear_input_focus_state(&mut self.form.username_input);
        clear_input_focus_state(&mut self.form.password_input);
        clear_input_focus_state(&mut self.form.share_input);
        clear_input_focus_state(&mut self.form.initial_dir_input);
        clear_input_focus_state(&mut self.form.domain_input);
        clear_input_focus_state(&mut self.form.private_key_path_input);
        clear_input_focus_state(&mut self.form.private_key_passphrase_input);
    }

    /// 返回链接窗口输入框的可变引用。
    fn input_mut(&mut self, target: ConnectionFormInputTarget) -> Option<&mut TextInputState> {
        match target {
            ConnectionFormInputTarget::LinkName => Some(&mut self.form.name_input),
            ConnectionFormInputTarget::LinkHost => Some(&mut self.form.host_input),
            ConnectionFormInputTarget::LinkUrl => Some(&mut self.form.url_input),
            ConnectionFormInputTarget::LinkPort => Some(&mut self.form.port_input),
            ConnectionFormInputTarget::LinkUsername => Some(&mut self.form.username_input),
            ConnectionFormInputTarget::LinkPassword => Some(&mut self.form.password_input),
            ConnectionFormInputTarget::LinkShare => Some(&mut self.form.share_input),
            ConnectionFormInputTarget::LinkInitialDir => Some(&mut self.form.initial_dir_input),
            ConnectionFormInputTarget::LinkDomain => Some(&mut self.form.domain_input),
            ConnectionFormInputTarget::LinkPrivateKeyPath => {
                Some(&mut self.form.private_key_path_input)
            }
            ConnectionFormInputTarget::LinkPrivateKeyPassphrase => {
                Some(&mut self.form.private_key_passphrase_input)
            }
            _ => None,
        }
    }

    /// 处理链接模态框输入事件，并返回是否需要提交或关闭模态框。
    fn handle_input_key(
        &mut self,
        target: ConnectionFormInputTarget,
        keystroke: &gpui::Keystroke,
        cx: &mut Context<Self>,
    ) -> ConnectionWindowInputAction {
        let Some(input) = self.input_mut(target) else {
            return ConnectionWindowInputAction::None;
        };
        if let Some(action) = handle_text_input_clipboard(input, keystroke, cx) {
            if action == ConnectionWindowInputAction::Changed {
                self.form.error_message = None;
            }
            return action;
        }
        let action = handle_text_input_key(input, keystroke);
        if action == ConnectionWindowInputAction::Changed {
            self.form.error_message = None;
        }
        action
    }

    /// 鼠标开始选择链接窗口输入框文本。
    fn begin_pointer_selection(
        &mut self,
        target: ConnectionFormInputTarget,
        character_index: usize,
        granularity: TextSelectionGranularity,
    ) {
        self.focus_input(target);
        let Some(input) = self.input_mut(target) else {
            return;
        };
        begin_input_pointer_selection(input, character_index, granularity);
    }

    /// 鼠标拖拽更新链接窗口输入框选区。
    fn update_pointer_selection(
        &mut self,
        target: ConnectionFormInputTarget,
        character_index: usize,
    ) {
        let Some(input) = self.input_mut(target) else {
            return;
        };
        update_input_pointer_selection(input, character_index);
    }

    /// 鼠标结束链接窗口输入框文本选择。
    fn finish_pointer_selection(&mut self, target: ConnectionFormInputTarget) {
        let Some(input) = self.input_mut(target) else {
            return;
        };
        finish_input_pointer_selection(input);
    }

    /// 应用系统输入法提交的链接窗口文本编辑。
    fn apply_native_edit(&mut self, target: ConnectionFormInputTarget, edit: NativeTextEdit) {
        self.focus_input(target);
        let Some(input) = self.input_mut(target) else {
            return;
        };
        apply_native_edit_to_input(input, &edit);
        self.form.error_message = None;
    }
}

impl Render for ConnectionLinkWindow {
    /// 渲染链接表单模态框主体。
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let link_kind = self.form.link_kind;
        let credentials = self.app.read_with(cx, |app, _| {
            app.config
                .connections
                .credentials_for_protocol(link_kind)
                .into_iter()
                .cloned()
                .collect::<Vec<_>>()
        });
        let (directory_choices, directory_label) = self.app.read_with(cx, |app, _| {
            (
                app.config.connections.directory_choices(),
                app.config
                    .connections
                    .directory_display_path(self.form.parent_id),
            )
        });
        render_link_window_content(
            self.app.clone(),
            cx.entity(),
            self.theme.clone(),
            self.form.clone(),
            self.mode,
            self.credential_menu_open,
            self.directory_menu_open,
            credentials,
            directory_choices,
            directory_label,
            self.focus_handles.clone(),
        )
    }
}

/// 模态框表单输入目标，目录和链接子视图共用输入处理函数。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ConnectionFormInputTarget {
    /// 目录名称输入框。
    DirectoryName,
    /// SSH 链接名称输入框。
    LinkName,
    /// SSH 主机输入框。
    LinkHost,
    /// Git/SVN 仓库 URL 输入框。
    LinkUrl,
    /// SSH 端口输入框。
    LinkPort,
    /// SSH 用户名输入框。
    LinkUsername,
    /// SSH 密码输入框。
    LinkPassword,
    /// SMB 共享名称输入框。
    LinkShare,
    /// SMB 初始目录输入框。
    LinkInitialDir,
    /// SMB 域或工作组输入框。
    LinkDomain,
    /// SSH 私钥路径输入框。
    LinkPrivateKeyPath,
    /// SSH 私钥口令输入框。
    LinkPrivateKeyPassphrase,
}

/// 输入框按键处理后需要触发的窗口级动作。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ConnectionWindowInputAction {
    /// 无需额外动作。
    None,
    /// 输入内容或选区发生变化。
    Changed,
    /// 提交当前表单。
    Submit,
    /// 关闭当前模态框。
    Close,
}

/// 渲染当前链接工作区主机指纹确认弹窗。
pub(crate) fn render(app: &ArgusApp, cx: &mut Context<ArgusApp>) -> impl IntoElement {
    let theme = app.theme.clone();
    let Some(dialog) = app.connection_dialog.clone() else {
        return div().into_any_element();
    };

    match dialog {
        ConnectionDialogState::SvnCertificate(prompt) => render_modal_dialog(
            ModalDialog {
                overlay_id: "connection-svn-certificate-dialog-overlay",
                container_id: "connection-svn-certificate-dialog-container",
                width: SVN_CERTIFICATE_DIALOG_WIDTH,
                height: SVN_CERTIFICATE_DIALOG_HEIGHT,
                content: render_svn_certificate_prompt(prompt, &theme, cx).into_any_element(),
            },
            theme,
            cx,
        )
        .into_any_element(),
        ConnectionDialogState::ConfirmHostKey(prompt) => render_modal_dialog(
            ModalDialog {
                overlay_id: "connection-host-key-dialog-overlay",
                container_id: "connection-host-key-dialog-container",
                width: HOST_KEY_DIALOG_WIDTH,
                height: HOST_KEY_DIALOG_HEIGHT,
                content: render_host_key_prompt(prompt, &theme, cx).into_any_element(),
            },
            theme,
            cx,
        )
        .into_any_element(),
        ConnectionDialogState::ConfirmDelete(prompt) => render_modal_dialog(
            ModalDialog {
                overlay_id: "connection-delete-dialog-overlay",
                container_id: "connection-delete-dialog-container",
                width: DELETE_DIALOG_WIDTH,
                height: DELETE_DIALOG_HEIGHT,
                content: render_delete_prompt(prompt, &theme, cx).into_any_element(),
            },
            theme,
            cx,
        )
        .into_any_element(),
    }
}

/// 将目录表单子视图包裹为主窗口模态框。
pub(crate) fn render_connection_directory_modal(
    modal: Entity<ConnectionDirectoryWindow>,
    theme: &AppTheme,
    cx: &mut Context<ArgusApp>,
) -> AnyElement {
    // 错误行出现或清除时高度需要同步变化；子视图会在错误状态变化时通知主应用重绘外壳。
    let has_error = modal.read_with(cx, |window_state, _| {
        window_state.form.error_message.is_some()
    });
    render_modal_dialog(
        ModalDialog {
            overlay_id: "connection-directory-modal-overlay",
            container_id: "connection-directory-modal-container",
            width: CONNECTION_DIRECTORY_MODAL_WIDTH,
            height: connection_directory_modal_height(has_error),
            content: modal.into_any_element(),
        },
        theme.clone(),
        cx,
    )
    .into_any_element()
}

/// 将链接表单子视图包裹为主窗口模态框。
pub(crate) fn render_connection_link_modal(
    modal: Entity<ConnectionLinkWindow>,
    theme: &AppTheme,
    cx: &mut Context<ArgusApp>,
) -> AnyElement {
    // 高度必须在移交子视图 Entity 前读取；URL 编辑会通知主应用重绘该外壳。
    let modal_height = modal.read_with(cx, |window_state, _| window_state.preferred_modal_height());
    render_modal_dialog(
        ModalDialog {
            overlay_id: "connection-link-modal-overlay",
            container_id: "connection-link-modal-container",
            width: CONNECTION_LINK_MODAL_WIDTH,
            height: modal_height,
            content: modal.into_any_element(),
        },
        theme.clone(),
        cx,
    )
    .into_any_element()
}

/// 渲染目录表单模态框内容。
fn render_directory_window_content(
    app_handle: Entity<ArgusApp>,
    window_entity: Entity<ConnectionDirectoryWindow>,
    theme: AppTheme,
    form: ConnectionDirectoryFormState,
    mode: ConnectionDirectoryWindowMode,
    focus_handles: ConnectionDirectoryWindowFocusHandles,
) -> impl IntoElement {
    let close_app = app_handle.clone();
    let close_window_entity = window_entity.clone();
    let root_focus_for_track = focus_handles.root.clone();
    let root_focus_for_click = focus_handles.root.clone();
    let (title, close_tooltip) = match mode {
        ConnectionDirectoryWindowMode::Create => ("新增目录", "关闭新增目录"),
        ConnectionDirectoryWindowMode::Edit { .. } => ("编辑目录", "关闭编辑目录"),
    };

    div()
        .id("connection-directory-window-root")
        .size_full()
        .relative()
        .flex()
        .flex_col()
        .rounded_lg()
        .overflow_hidden()
        .bg(rgb(theme.content))
        .border_1()
        .border_color(rgb(theme.border))
        .font_family(ARGUS_UI_FONT_FAMILY)
        .text_color(rgb(theme.foreground))
        .occlude()
        .focusable()
        .track_focus(&root_focus_for_track)
        .on_click(move |_, window, cx| {
            root_focus_for_click.focus(window);
            close_window_entity.update(cx, |window_state, state_cx| {
                window_state.clear_input_focuses();
                state_cx.notify();
            });
        })
        .child(render_connection_window_header(
            title,
            ArgusIcon::FolderPlus,
            "connection-directory-window-close",
            close_tooltip,
            &theme,
            move |_, window, cx| {
                cx.stop_propagation();
                close_directory_window(&close_app, window, cx);
            },
        ))
        .child(
            div()
                .id("connection-directory-window-scroll")
                .debug_selector(|| "connection-directory-window-scroll".to_string())
                .flex_1()
                .min_h(px(0.0))
                .overflow_y_scroll()
                .scrollbar_width(px(CONNECTION_FORM_SCROLLBAR_WIDTH))
                .px_5()
                .pr(px(CONNECTION_FORM_SCROLL_RIGHT_PADDING))
                .flex()
                .flex_col()
                .gap_3()
                .child(render_directory_input_row(
                    "目录名称",
                    ConnectionFormInputTarget::DirectoryName,
                    &form.name_input,
                    focus_handles.name.clone(),
                    "生产环境",
                    false,
                    &theme,
                    &window_entity,
                    &app_handle,
                ))
                .when_some(form.error_message.clone(), |this, message| {
                    this.child(error_text(message, &theme))
                }),
        )
        .child(
            div()
                .flex_none()
                .px_5()
                .pt_5()
                .pb(px(MODAL_BOTTOM_PADDING))
                .flex()
                .justify_end()
                .gap_2()
                .child(render_directory_actions(
                    &theme,
                    &app_handle,
                    &window_entity,
                    mode,
                )),
        )
}

/// 链接表单当前需要展示的可变字段与传输提示。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ConnectionLinkFieldVisibility {
    /// 是否使用仓库 URL，而非主机与端口。
    is_repository: bool,
    /// 是否需要展示私钥路径和私钥口令。
    uses_ssh_credentials: bool,
    /// 是否需要展示密码或 HTTPS 访问令牌。
    show_secret: bool,
    /// SVN 明文传输协议的安全提示。
    svn_transport_warning: Option<&'static str>,
}

/// 根据链接协议与 URL 推导表单可见字段。
///
/// 参数说明：
/// - `link_kind`：当前表单协议。
/// - `url`：Git/SVN 仓库地址；SSH/SMB 调用时可为空。
///
/// 返回值：渲染和高度计算共用的可见性快照，避免两处规则漂移。
fn connection_link_field_visibility(
    link_kind: ConnectionLinkKind,
    url: &str,
) -> ConnectionLinkFieldVisibility {
    let is_repository = matches!(link_kind, ConnectionLinkKind::Git | ConnectionLinkKind::Svn);
    let normalized_url = url.trim().to_ascii_lowercase();
    let uses_ssh_credentials = link_kind == ConnectionLinkKind::Ssh
        || (link_kind == ConnectionLinkKind::Git
            && (normalized_url.starts_with("ssh://")
                || (!normalized_url.starts_with("https://")
                    && normalized_url.contains('@')
                    && normalized_url.contains(':'))))
        || (link_kind == ConnectionLinkKind::Svn && normalized_url.starts_with("svn+ssh://"));
    let show_secret = link_kind != ConnectionLinkKind::Git || !uses_ssh_credentials;
    let svn_transport_warning = if link_kind != ConnectionLinkKind::Svn {
        None
    } else if normalized_url.starts_with("http://") {
        Some("HTTP SVN 链路不加密，用户名、密码和仓库内容可能被窃听；敏感仓库请使用 HTTPS")
    } else if normalized_url.starts_with("svn://") {
        Some("svn:// 链路不加密；敏感仓库请使用 svn+ssh://")
    } else {
        None
    };

    ConnectionLinkFieldVisibility {
        is_repository,
        uses_ssh_credentials,
        show_secret,
        svn_transport_warning,
    }
}

/// 计算目录表单模态框高度：标题栏 + 输入行 + 可选错误行 + 底部操作区 + 底部内边距。
///
/// 说明：高度贴合内容后，按钮到边框的距离恒等于 `MODAL_BOTTOM_PADDING`，
/// 不会像固定高度那样在底部留下多余空白。
fn connection_directory_modal_height(has_error: bool) -> f32 {
    let error_height = if has_error {
        MODAL_FORM_ROW_GAP + MODAL_FORM_NOTE_HEIGHT
    } else {
        0.0
    };
    CONNECTION_WINDOW_HEADER_HEIGHT
        + MODAL_FORM_ROW_HEIGHT
        + error_height
        + CONNECTION_WINDOW_FOOTER_HEIGHT
        + MODAL_BOTTOM_PADDING
        + CONNECTION_WINDOW_BORDER_HEIGHT
}

/// 按当前可见输入行和提示行数计算链接模态框高度。
///
/// 参数说明：`form` 是当前链接表单快照。
/// 返回值：刚好容纳当前内容的像素高度，避免短表单底部出现大块空白。
fn preferred_connection_link_modal_height(form: &ConnectionLinkFormState) -> f32 {
    preferred_connection_link_modal_height_for(
        form.link_kind,
        &form.url_input.value,
        form.error_message.is_some(),
        form.credential_id.is_some(),
    )
}

/// 使用可单元测试的输入计算链接模态框高度。
///
/// 参数说明：
/// - `link_kind`：链接协议。
/// - `url`：仓库 URL，用于判断 SSH 凭据和 SVN 安全提示。
/// - `has_error`：当前是否展示表单校验错误。
///
/// 返回值：对应可见行数的模态框像素高度。
fn preferred_connection_link_modal_height_for(
    link_kind: ConnectionLinkKind,
    url: &str,
    has_error: bool,
    has_shared_credential: bool,
) -> f32 {
    let visibility = connection_link_field_visibility(link_kind, url);
    // 名称和用户名始终展示；非仓库链接使用主机+端口，仓库链接使用 URL。
    let mut input_count = 2 + if visibility.is_repository { 1 } else { 2 };
    // 所属目录始终展示。
    input_count += 1;
    input_count += usize::from(visibility.show_secret);
    if visibility.show_secret && link_kind != ConnectionLinkKind::Git {
        // 凭据来源选择行；Git 访问令牌不参与共用账号凭据。
        input_count += 1;
    }
    if link_kind == ConnectionLinkKind::Smb {
        input_count += 3;
    }
    if visibility.uses_ssh_credentials {
        input_count += 2;
    }

    // 仅手动输入密码时展示凭据存储说明；提示行与输入行共用同一套行距。
    let note_count = usize::from(visibility.show_secret && !has_shared_credential)
        + usize::from(visibility.svn_transport_warning.is_some())
        + usize::from(has_error);
    let child_count = input_count + note_count;
    let body_height = input_count as f32 * MODAL_FORM_ROW_HEIGHT
        + note_count as f32 * MODAL_FORM_NOTE_HEIGHT
        + child_count.saturating_sub(1) as f32 * MODAL_FORM_ROW_GAP;
    CONNECTION_WINDOW_HEADER_HEIGHT
        + body_height
        + CONNECTION_WINDOW_FOOTER_HEIGHT
        + MODAL_BOTTOM_PADDING
        + CONNECTION_WINDOW_BORDER_HEIGHT
}

/// 渲染 SSH/SMB 链接表单模态框内容。
fn render_link_window_content(
    app_handle: Entity<ArgusApp>,
    window_entity: Entity<ConnectionLinkWindow>,
    theme: AppTheme,
    form: ConnectionLinkFormState,
    mode: ConnectionLinkWindowMode,
    credential_menu_open: bool,
    directory_menu_open: bool,
    credentials: Vec<crate::remote::connection::ConnectionCredentialConfig>,
    directory_choices: Vec<(Option<crate::remote::connection::ConnectionNodeId>, String)>,
    directory_label: String,
    focus_handles: ConnectionLinkWindowFocusHandles,
) -> impl IntoElement {
    let close_app = app_handle.clone();
    let close_window_entity = window_entity.clone();
    let root_focus_for_track = focus_handles.root.clone();
    let root_focus_for_click = focus_handles.root.clone();
    let (title, close_tooltip) = match (mode, form.link_kind) {
        (ConnectionLinkWindowMode::Create, ConnectionLinkKind::Ssh) => {
            ("新增 SSH 链接", "关闭新增链接")
        }
        (ConnectionLinkWindowMode::Create, ConnectionLinkKind::Smb) => {
            ("新增 SMB 链接", "关闭新增链接")
        }
        (ConnectionLinkWindowMode::Create, ConnectionLinkKind::Git) => {
            ("新增 Git 链接", "关闭新增链接")
        }
        (ConnectionLinkWindowMode::Create, ConnectionLinkKind::Svn) => {
            ("新增 SVN 链接", "关闭新增链接")
        }
        (ConnectionLinkWindowMode::Edit { .. }, ConnectionLinkKind::Ssh) => {
            ("编辑 SSH 链接", "关闭编辑链接")
        }
        (ConnectionLinkWindowMode::Edit { .. }, ConnectionLinkKind::Smb) => {
            ("编辑 SMB 链接", "关闭编辑链接")
        }
        (ConnectionLinkWindowMode::Edit { .. }, ConnectionLinkKind::Git) => {
            ("编辑 Git 链接", "关闭编辑链接")
        }
        (ConnectionLinkWindowMode::Edit { .. }, ConnectionLinkKind::Svn) => {
            ("编辑 SVN 链接", "关闭编辑链接")
        }
    };
    let port_placeholder = match form.link_kind {
        ConnectionLinkKind::Ssh => "22",
        ConnectionLinkKind::Smb => "445",
        ConnectionLinkKind::Git | ConnectionLinkKind::Svn => "",
    };
    let (password_label, password_placeholder) = match form.link_kind {
        ConnectionLinkKind::Ssh => ("密码", "可选"),
        ConnectionLinkKind::Smb => ("密码", "必填"),
        ConnectionLinkKind::Git => ("访问令牌", "HTTPS 私有仓库填写"),
        ConnectionLinkKind::Svn => ("密码", "匿名仓库可留空"),
    };
    let visibility = connection_link_field_visibility(form.link_kind, &form.url_input.value);
    let is_repository = visibility.is_repository;
    let uses_ssh_credentials = visibility.uses_ssh_credentials;
    let show_secret = visibility.show_secret;
    let supports_shared_credential = show_secret && form.link_kind != ConnectionLinkKind::Git;
    let selected_credential_name = form
        .credential_id
        .and_then(|credential_id| {
            credentials
                .iter()
                .find(|credential| credential.id == credential_id)
        })
        .map(|credential| credential.name.clone());
    let svn_transport_warning = visibility.svn_transport_warning;

    div()
        .id("connection-link-window-root")
        .size_full()
        .relative()
        .flex()
        .flex_col()
        .rounded_lg()
        .overflow_hidden()
        .bg(rgb(theme.content))
        .border_1()
        .border_color(rgb(theme.border))
        .font_family(ARGUS_UI_FONT_FAMILY)
        .text_color(rgb(theme.foreground))
        .occlude()
        .focusable()
        .track_focus(&root_focus_for_track)
        .on_click(move |_, window, cx| {
            root_focus_for_click.focus(window);
            close_window_entity.update(cx, |window_state, state_cx| {
                window_state.clear_input_focuses();
                window_state.credential_menu_open = false;
                window_state.directory_menu_open = false;
                state_cx.notify();
            });
        })
        .child(render_connection_window_header(
            title,
            ArgusIcon::Link,
            "connection-link-window-close",
            close_tooltip,
            &theme,
            move |_, window, cx| {
                cx.stop_propagation();
                close_link_window(&close_app, window, cx);
            },
        ))
        .child(
            div()
                .id("connection-link-window-scroll")
                .debug_selector(|| "connection-link-window-scroll".to_string())
                .flex_1()
                .min_h(px(0.0))
                .overflow_y_scroll()
                .scrollbar_width(px(CONNECTION_FORM_SCROLLBAR_WIDTH))
                .px_5()
                .pr(px(CONNECTION_FORM_SCROLL_RIGHT_PADDING))
                .flex()
                .flex_col()
                .gap_3()
                .child(render_link_input_row(
                    "链接名称",
                    ConnectionFormInputTarget::LinkName,
                    &form.name_input,
                    focus_handles.name.clone(),
                    "app-01",
                    false,
                    &theme,
                    &window_entity,
                    &app_handle,
                ))
                .when(supports_shared_credential, |this| {
                    this.child(render_link_credential_selector(
                        &form,
                        credential_menu_open,
                        &credentials,
                        &theme,
                        &window_entity,
                        &app_handle,
                    ))
                })
                .child(render_link_directory_selector(
                    &form,
                    directory_menu_open,
                    &directory_choices,
                    directory_label,
                    &theme,
                    &window_entity,
                ))
                .when(!is_repository, |this| {
                    this.child(render_link_input_row(
                        "主机",
                        ConnectionFormInputTarget::LinkHost,
                        &form.host_input,
                        focus_handles.host.clone(),
                        "10.0.0.1",
                        false,
                        &theme,
                        &window_entity,
                        &app_handle,
                    ))
                    .child(render_link_input_row(
                        "端口",
                        ConnectionFormInputTarget::LinkPort,
                        &form.port_input,
                        focus_handles.port.clone(),
                        port_placeholder,
                        false,
                        &theme,
                        &window_entity,
                        &app_handle,
                    ))
                })
                .when(is_repository, |this| {
                    this.child(render_link_input_row(
                        "仓库 URL",
                        ConnectionFormInputTarget::LinkUrl,
                        &form.url_input,
                        focus_handles.url.clone(),
                        if form.link_kind == ConnectionLinkKind::Git {
                            "https://host/team/repository.git"
                        } else {
                            "http://host/svn/repository/"
                        },
                        false,
                        &theme,
                        &window_entity,
                        &app_handle,
                    ))
                })
                .child(render_link_input_row(
                    "用户名",
                    ConnectionFormInputTarget::LinkUsername,
                    &form.username_input,
                    focus_handles.username.clone(),
                    "deploy",
                    false,
                    &theme,
                    &window_entity,
                    &app_handle,
                ))
                .when(show_secret && form.credential_id.is_none(), |this| {
                    this.child(render_link_input_row(
                        password_label,
                        ConnectionFormInputTarget::LinkPassword,
                        &form.password_input,
                        focus_handles.password.clone(),
                        password_placeholder,
                        true,
                        &theme,
                        &window_entity,
                        &app_handle,
                    ))
                    // 凭据存储位置对用户可见：密码写入系统凭据库，不会出现在设置文件中。
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .text_size(px(11.0))
                            .line_height(px(MODAL_FORM_NOTE_HEIGHT))
                            .text_color(rgb(theme.foreground_muted))
                            .child(
                                div()
                                    .flex_1()
                                    .truncate()
                                    .child("密码保存在系统凭据库；不会写入设置文件。"),
                            )
                            .child(render_link_save_credential_toggle(
                                form.save_as_credential,
                                &theme,
                                &window_entity,
                            )),
                    )
                })
                .when(show_secret && form.credential_id.is_some(), |this| {
                    this.child(
                        div()
                            .id("connection-link-shared-credential")
                            .debug_selector(|| "connection-link-shared-credential".to_string())
                            .flex()
                            .items_center()
                            .gap_3()
                            .child(
                                div()
                                    .w(px(82.0))
                                    .text_size(px(12.0))
                                    .text_color(rgb(theme.foreground_muted))
                                    .child(password_label),
                            )
                            .child(
                                div()
                                    .flex_1()
                                    .text_size(px(12.0))
                                    .text_color(rgb(theme.foreground))
                                    .child(format!(
                                        "由凭据「{}」提供",
                                        selected_credential_name.as_deref().unwrap_or("已删除")
                                    )),
                            ),
                    )
                })
                .when(form.link_kind == ConnectionLinkKind::Smb, |this| {
                    this.child(render_link_input_row(
                        "共享名称",
                        ConnectionFormInputTarget::LinkShare,
                        &form.share_input,
                        focus_handles.share.clone(),
                        "share",
                        false,
                        &theme,
                        &window_entity,
                        &app_handle,
                    ))
                    .child(render_link_input_row(
                        "初始目录",
                        ConnectionFormInputTarget::LinkInitialDir,
                        &form.initial_dir_input,
                        focus_handles.initial_dir.clone(),
                        "/",
                        false,
                        &theme,
                        &window_entity,
                        &app_handle,
                    ))
                    .child(render_link_input_row(
                        "域/工作组",
                        ConnectionFormInputTarget::LinkDomain,
                        &form.domain_input,
                        focus_handles.domain.clone(),
                        "可选",
                        false,
                        &theme,
                        &window_entity,
                        &app_handle,
                    ))
                })
                .when(uses_ssh_credentials, |this| {
                    this.child(render_link_input_row(
                        "私钥路径",
                        ConnectionFormInputTarget::LinkPrivateKeyPath,
                        &form.private_key_path_input,
                        focus_handles.private_key_path.clone(),
                        "~/.ssh/id_ed25519",
                        false,
                        &theme,
                        &window_entity,
                        &app_handle,
                    ))
                    .child(render_link_input_row(
                        "私钥口令",
                        ConnectionFormInputTarget::LinkPrivateKeyPassphrase,
                        &form.private_key_passphrase_input,
                        focus_handles.private_key_passphrase.clone(),
                        "可选",
                        true,
                        &theme,
                        &window_entity,
                        &app_handle,
                    ))
                })
                .when_some(svn_transport_warning, |this, warning| {
                    this.child(
                        div()
                            .text_size(px(12.0))
                            .line_height(px(MODAL_FORM_NOTE_HEIGHT))
                            .text_color(rgb(theme.warning))
                            .child(warning),
                    )
                })
                .when_some(form.error_message.clone(), |this, message| {
                    this.child(error_text(message, &theme))
                }),
        )
        .child(
            div()
                .flex_none()
                .px_5()
                .pt_5()
                .pb(px(MODAL_BOTTOM_PADDING))
                .flex()
                .justify_end()
                .gap_2()
                .child(render_link_actions(
                    &theme,
                    &app_handle,
                    &window_entity,
                    mode,
                )),
        )
        .when(credential_menu_open, |this| {
            this.child(render_link_credential_menu(
                &form,
                &credentials,
                &theme,
                &window_entity,
            ))
        })
        .when(directory_menu_open, |this| {
            this.child(render_link_directory_menu(
                &form,
                &directory_choices,
                supports_shared_credential,
                &theme,
                &window_entity,
            ))
        })
}

/// 渲染模态框标题栏；结构和设置模态框一致，不额外绘制分割线。
fn render_connection_window_header(
    title: &'static str,
    icon: ArgusIcon,
    close_id: &'static str,
    close_tooltip: &'static str,
    theme: &AppTheme,
    on_close: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    div()
        .h(px(CONNECTION_WINDOW_HEADER_HEIGHT))
        .flex_none()
        .px_5()
        .flex()
        .items_center()
        .justify_between()
        .occlude()
        .child(
            div()
                .flex()
                .items_center()
                .gap_2()
                .text_size(px(14.0))
                .line_height(px(18.0))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(rgb(theme.foreground))
                .child(render_icon(
                    icon,
                    theme.foreground_muted,
                    CONNECTION_WINDOW_TITLE_ICON_SIZE,
                ))
                .child(title),
        )
        .child(render_icon_button(
            close_id,
            ArgusIcon::Close,
            close_tooltip,
            false,
            IconButtonSize::Small,
            theme,
            on_close,
        ))
}

/// 渲染目录窗口输入行。
#[allow(clippy::too_many_arguments)]
fn render_directory_input_row(
    label: &'static str,
    target: ConnectionFormInputTarget,
    input_state: &TextInputState,
    focus_handle: FocusHandle,
    placeholder: &'static str,
    is_secret: bool,
    theme: &AppTheme,
    window_entity: &Entity<ConnectionDirectoryWindow>,
    app_handle: &Entity<ArgusApp>,
) -> impl IntoElement {
    let native_input = directory_native_input(
        window_entity.clone(),
        target,
        focus_handle.clone(),
        app_handle.clone(),
    );
    let key_window = window_entity.clone();
    let click_window = window_entity.clone();
    let pointer_window = window_entity.clone();
    let submit_window = window_entity.clone();
    let submit_app = app_handle.clone();
    let close_app = app_handle.clone();
    let layout_app = app_handle.clone();

    div()
        .flex()
        .items_center()
        .gap_3()
        .child(
            div()
                .w(px(82.0))
                .text_size(px(12.0))
                .text_color(rgb(theme.foreground_muted))
                .child(label),
        )
        .child(div().flex_1().child(render_input(
            Input {
                id: input_id_for_target(target),
                placeholder,
                value: input_state.value.clone(),
                is_disabled: false,
                is_focused: input_state.is_focused,
                cursor_index: input_state.cursor,
                selection_range: input_selection_range(input_state),
                marked_range: input_state.marked_range.clone(),
                is_pointer_selecting: input_state.selection_drag.is_some(),
                is_secret,
                size: InputSize::Regular,
                leading_accessory: None,
                trailing_accessory: None,
                native_input: Some(native_input),
                scroll_handle: input_state.scroll_handle.clone(),
            },
            theme,
            move |event: &KeyDownEvent, window, cx| {
                cx.stop_propagation();
                let action = key_window.update(cx, |window_state, state_cx| {
                    let action = window_state.handle_input_key(target, &event.keystroke, state_cx);
                    state_cx.notify();
                    action
                });
                if action == ConnectionWindowInputAction::Changed {
                    // 输入可能清除错误行，主视图需要重算模态外壳高度。
                    notify_connection_modal_layout(&layout_app, cx);
                }
                handle_directory_window_action(action, &submit_app, &submit_window, window, cx);
                if action == ConnectionWindowInputAction::Close {
                    close_directory_window(&close_app, window, cx);
                }
            },
            move |_, window, cx| {
                cx.stop_propagation();
                focus_handle.focus(window);
                click_window.update(cx, |window_state, state_cx| {
                    window_state.focus_input(target);
                    state_cx.notify();
                });
            },
            move |event: &InputPointerEvent, _, cx| {
                cx.stop_propagation();
                pointer_window.update(cx, |window_state, state_cx| {
                    match event.action {
                        InputPointerAction::Begin => window_state.begin_pointer_selection(
                            target,
                            event.character_index,
                            event.granularity,
                        ),
                        InputPointerAction::Extend => {
                            window_state.update_pointer_selection(target, event.character_index)
                        }
                        InputPointerAction::Finish => window_state.finish_pointer_selection(target),
                    }
                    state_cx.notify();
                });
            },
            |_, _, _| {},
        )))
}

/// 渲染链接表单凭据下拉浮层；浮层挂在弹窗根节点，避免被表单滚动区域裁切。
fn render_link_credential_menu(
    form: &ConnectionLinkFormState,
    credentials: &[crate::remote::connection::ConnectionCredentialConfig],
    theme: &AppTheme,
    window_entity: &Entity<ConnectionLinkWindow>,
) -> impl IntoElement {
    let mut items = vec![DropdownItem {
        id: "inline".to_string(),
        label: "仅此连接（手动输入密码）".to_string(),
    }];
    items.extend(credentials.iter().map(|credential| DropdownItem {
        id: format!("credential-{}", credential.id),
        label: format!("{}（{}）", credential.name, credential.username),
    }));
    items.push(DropdownItem {
        id: "manage".to_string(),
        label: "管理凭据…".to_string(),
    });
    let selected_id = form
        .credential_id
        .map(|credential_id| format!("credential-{credential_id}"))
        .unwrap_or_else(|| "inline".to_string());
    let height = (items.len() as f32 * 30.0).clamp(30.0, 220.0);

    div()
        .id("connection-link-credential-menu")
        .debug_selector(|| "connection-link-credential-menu".to_string())
        .absolute()
        .top(px(CONNECTION_LINK_CREDENTIAL_MENU_TOP))
        .left(px(CONNECTION_LINK_CREDENTIAL_MENU_LEFT))
        .w(px(CONNECTION_LINK_CREDENTIAL_MENU_WIDTH))
        .h(px(height))
        // 浮层挂在弹窗根节点，不继承表单字号；显式与 InputSize::Regular 的 13px 一致。
        .text_size(px(13.0))
        .line_height(px(18.0))
        .rounded_sm()
        .border_1()
        .border_color(rgb(theme.border))
        .bg(rgb(theme.content))
        .shadow_lg()
        .overflow_y_scroll()
        .occlude()
        .children(items.into_iter().map(|item| {
            let is_selected = item.id == selected_id;
            let item_id = item.id.clone();
            let item_debug_id = format!("connection-link-credential-menu-item-{}", item.id);
            let entity = window_entity.clone();
            let item_theme = theme.clone();
            let hover_background = item_theme.current_line;
            let foreground = item_theme.foreground;
            div()
                .id(SharedString::from(format!(
                    "connection-link-credential-menu-item-{}",
                    item.id
                )))
                .debug_selector(move || item_debug_id.clone())
                .h(px(30.0))
                .w_full()
                .px_2()
                .flex()
                .items_center()
                .cursor_pointer()
                .bg(rgb(if is_selected {
                    item_theme.current_line
                } else {
                    item_theme.content
                }))
                .hover(move |this| this.bg(rgb(hover_background)))
                .text_color(rgb(foreground))
                .child(div().flex_1().truncate().child(item.label))
                .on_click(move |_, _, cx| {
                    cx.stop_propagation();
                    entity.update(cx, |window_state, state_cx| {
                        window_state.apply_credential_menu_selection(&item_id, state_cx);
                        state_cx.notify();
                    });
                })
        }))
}

/// 渲染链接表单所属目录选择行。
fn render_link_directory_selector(
    form: &ConnectionLinkFormState,
    is_open: bool,
    choices: &[(Option<crate::remote::connection::ConnectionNodeId>, String)],
    selected_label: String,
    theme: &AppTheme,
    window_entity: &Entity<ConnectionLinkWindow>,
) -> impl IntoElement {
    let mut items = Vec::with_capacity(choices.len());
    for (directory_id, label) in choices {
        items.push(DropdownItem {
            id: directory_id
                .map(|directory_id| format!("directory-{directory_id}"))
                .unwrap_or_else(|| "root".to_string()),
            label: label.clone(),
        });
    }
    let selected_id = form
        .parent_id
        .map(|directory_id| format!("directory-{directory_id}"))
        .unwrap_or_else(|| "root".to_string());
    let toggle_window = window_entity.clone();
    let select_window = window_entity.clone();

    div()
        .flex()
        .items_start()
        .gap_3()
        .child(
            div()
                .w(px(82.0))
                .pt(px(6.0))
                .text_size(px(12.0))
                .text_color(rgb(theme.foreground_muted))
                .child("所属目录"),
        )
        .child(div().flex_1().child(render_dropdown(
            Dropdown {
                id: "connection-link-directory-dropdown",
                selected_id,
                selected_label,
                placeholder: "根目录",
                is_open,
                items,
                show_inline_menu: false,
            },
            theme,
            move |_, _, cx| {
                cx.stop_propagation();
                toggle_window.update(cx, |window_state, state_cx| {
                    window_state.toggle_directory_menu();
                    state_cx.notify();
                });
            },
            Arc::new(move |item_id, _, cx| {
                select_window.update(cx, |window_state, state_cx| {
                    let directory_id = item_id
                        .strip_prefix("directory-")
                        .and_then(|value| value.parse::<usize>().ok());
                    window_state.select_directory(directory_id);
                    state_cx.notify();
                });
            }),
        )))
}

/// 渲染所属目录下拉浮层，与凭据浮层一样挂在弹窗根节点避免裁切。
fn render_link_directory_menu(
    form: &ConnectionLinkFormState,
    choices: &[(Option<crate::remote::connection::ConnectionNodeId>, String)],
    has_credential_row: bool,
    theme: &AppTheme,
    window_entity: &Entity<ConnectionLinkWindow>,
) -> impl IntoElement {
    let mut items = Vec::with_capacity(choices.len());
    for (directory_id, label) in choices {
        items.push(DropdownItem {
            id: directory_id
                .map(|directory_id| format!("directory-{directory_id}"))
                .unwrap_or_else(|| "root".to_string()),
            label: label.clone(),
        });
    }
    let selected_id = form
        .parent_id
        .map(|directory_id| format!("directory-{directory_id}"))
        .unwrap_or_else(|| "root".to_string());
    let height = (items.len() as f32 * 30.0).clamp(30.0, 220.0);
    let top = if has_credential_row {
        CONNECTION_LINK_DIRECTORY_MENU_TOP_WITH_CREDENTIAL
    } else {
        CONNECTION_LINK_DIRECTORY_MENU_TOP_REPOSITORY
    };

    div()
        .id("connection-link-directory-menu")
        .debug_selector(|| "connection-link-directory-menu".to_string())
        .absolute()
        .top(px(top))
        .left(px(CONNECTION_LINK_CREDENTIAL_MENU_LEFT))
        .w(px(CONNECTION_LINK_CREDENTIAL_MENU_WIDTH))
        .h(px(height))
        .text_size(px(13.0))
        .line_height(px(18.0))
        .rounded_sm()
        .border_1()
        .border_color(rgb(theme.border))
        .bg(rgb(theme.content))
        .shadow_lg()
        .overflow_y_scroll()
        .occlude()
        .children(items.into_iter().map(|item| {
            let is_selected = item.id == selected_id;
            let item_id = item.id.clone();
            let item_debug_id = format!("connection-link-directory-menu-item-{}", item.id);
            let entity = window_entity.clone();
            let item_theme = theme.clone();
            let hover_background = item_theme.current_line;
            let foreground = item_theme.foreground;
            div()
                .id(SharedString::from(format!(
                    "connection-link-directory-menu-item-{}",
                    item.id
                )))
                .debug_selector(move || item_debug_id.clone())
                .h(px(30.0))
                .w_full()
                .px_2()
                .flex()
                .items_center()
                .cursor_pointer()
                .bg(rgb(if is_selected {
                    item_theme.current_line
                } else {
                    item_theme.content
                }))
                .hover(move |this| this.bg(rgb(hover_background)))
                .text_color(rgb(foreground))
                .child(div().flex_1().truncate().child(item.label))
                .on_click(move |_, _, cx| {
                    cx.stop_propagation();
                    entity.update(cx, |window_state, state_cx| {
                        let directory_id = item_id
                            .strip_prefix("directory-")
                            .and_then(|value| value.parse::<usize>().ok());
                        window_state.select_directory(directory_id);
                        state_cx.notify();
                    });
                })
        }))
}

/// 渲染链接表单中的共用凭据选择行。
fn render_link_credential_selector(
    form: &ConnectionLinkFormState,
    is_open: bool,
    credentials: &[crate::remote::connection::ConnectionCredentialConfig],
    theme: &AppTheme,
    window_entity: &Entity<ConnectionLinkWindow>,
    _app_handle: &Entity<ArgusApp>,
) -> impl IntoElement {
    let mut items = vec![DropdownItem {
        id: "inline".to_string(),
        label: "仅此连接（手动输入密码）".to_string(),
    }];
    items.extend(credentials.iter().map(|credential| DropdownItem {
        id: format!("credential-{}", credential.id),
        label: format!("{}（{}）", credential.name, credential.username),
    }));
    items.push(DropdownItem {
        id: "manage".to_string(),
        label: "管理凭据…".to_string(),
    });
    let selected_id = form
        .credential_id
        .map(|credential_id| format!("credential-{credential_id}"))
        .unwrap_or_else(|| "inline".to_string());
    let selected_label = form
        .credential_id
        .and_then(|credential_id| {
            credentials
                .iter()
                .find(|credential| credential.id == credential_id)
        })
        .map(|credential| format!("{}（{}）", credential.name, credential.username))
        .unwrap_or_else(|| "仅此连接（手动输入密码）".to_string());
    let toggle_window = window_entity.clone();
    let select_window = window_entity.clone();

    div()
        .flex()
        .items_start()
        .gap_3()
        .child(
            div()
                .w(px(82.0))
                .pt(px(6.0))
                .text_size(px(12.0))
                .text_color(rgb(theme.foreground_muted))
                .child("凭据"),
        )
        .child(
            div()
                .flex_1()
                .flex()
                .flex_col()
                .gap_1()
                .child(render_dropdown(
                    Dropdown {
                        id: "connection-link-credential-dropdown",
                        selected_id,
                        selected_label,
                        placeholder: "仅此连接",
                        is_open,
                        items,
                        show_inline_menu: false,
                    },
                    theme,
                    move |_, _, cx| {
                        cx.stop_propagation();
                        toggle_window.update(cx, |window_state, state_cx| {
                            window_state.toggle_credential_menu();
                            state_cx.notify();
                        });
                    },
                    Arc::new(move |item_id, _, cx| {
                        select_window.update(cx, |window_state, state_cx| {
                            window_state.apply_credential_menu_selection(&item_id, state_cx);
                            state_cx.notify();
                        });
                    }),
                )),
        )
}

/// 渲染“保存为共用凭据”开关。
fn render_link_save_credential_toggle(
    is_selected: bool,
    theme: &AppTheme,
    window_entity: &Entity<ConnectionLinkWindow>,
) -> impl IntoElement {
    let entity = window_entity.clone();
    div()
        .id("connection-link-save-as-credential")
        .flex()
        .items_center()
        .gap_1()
        .flex_none()
        .cursor_pointer()
        .on_click(move |_, _, cx| {
            entity.update(cx, |window_state, state_cx| {
                window_state.toggle_save_as_credential();
                state_cx.notify();
            });
        })
        .child(
            div()
                .w(px(12.0))
                .h(px(12.0))
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(2.0))
                .border_1()
                .border_color(rgb(theme.border))
                .text_size(px(9.0))
                .child(if is_selected { "✓" } else { "" }),
        )
        .child("保存为共用凭据")
}

/// 渲染 SSH 链接窗口输入行。
#[allow(clippy::too_many_arguments)]
fn render_link_input_row(
    label: &'static str,
    target: ConnectionFormInputTarget,
    input_state: &TextInputState,
    focus_handle: FocusHandle,
    placeholder: &'static str,
    is_secret: bool,
    theme: &AppTheme,
    window_entity: &Entity<ConnectionLinkWindow>,
    app_handle: &Entity<ArgusApp>,
) -> impl IntoElement {
    let native_input = link_native_input(
        window_entity.clone(),
        target,
        focus_handle.clone(),
        app_handle.clone(),
    );
    let key_window = window_entity.clone();
    let click_window = window_entity.clone();
    let pointer_window = window_entity.clone();
    let submit_window = window_entity.clone();
    let submit_app = app_handle.clone();
    let layout_app = app_handle.clone();
    let close_app = app_handle.clone();

    div()
        .flex()
        .items_center()
        .gap_3()
        .child(
            div()
                .w(px(82.0))
                .text_size(px(12.0))
                .text_color(rgb(theme.foreground_muted))
                .child(label),
        )
        .child(div().flex_1().child(render_input(
            Input {
                id: input_id_for_target(target),
                placeholder,
                value: input_state.value.clone(),
                is_disabled: false,
                is_focused: input_state.is_focused,
                cursor_index: input_state.cursor,
                selection_range: input_selection_range(input_state),
                marked_range: input_state.marked_range.clone(),
                is_pointer_selecting: input_state.selection_drag.is_some(),
                is_secret,
                size: InputSize::Regular,
                leading_accessory: None,
                trailing_accessory: None,
                native_input: Some(native_input),
                scroll_handle: input_state.scroll_handle.clone(),
            },
            theme,
            move |event: &KeyDownEvent, window, cx| {
                cx.stop_propagation();
                let action = key_window.update(cx, |window_state, state_cx| {
                    let action = window_state.handle_input_key(target, &event.keystroke, state_cx);
                    state_cx.notify();
                    action
                });
                if action == ConnectionWindowInputAction::Changed {
                    // URL 传输方式或错误状态变化后，主视图需要重算模态外壳高度。
                    notify_connection_modal_layout(&layout_app, cx);
                }
                handle_link_window_action(action, &submit_app, &submit_window, window, cx);
                if action == ConnectionWindowInputAction::Close {
                    close_link_window(&close_app, window, cx);
                }
            },
            move |_, window, cx| {
                cx.stop_propagation();
                focus_handle.focus(window);
                click_window.update(cx, |window_state, state_cx| {
                    window_state.focus_input(target);
                    state_cx.notify();
                });
            },
            move |event: &InputPointerEvent, _, cx| {
                cx.stop_propagation();
                pointer_window.update(cx, |window_state, state_cx| {
                    match event.action {
                        InputPointerAction::Begin => window_state.begin_pointer_selection(
                            target,
                            event.character_index,
                            event.granularity,
                        ),
                        InputPointerAction::Extend => {
                            window_state.update_pointer_selection(target, event.character_index)
                        }
                        InputPointerAction::Finish => window_state.finish_pointer_selection(target),
                    }
                    state_cx.notify();
                });
            },
            |_, _, _| {},
        )))
}

/// 渲染目录窗口底部操作按钮。
fn render_directory_actions(
    theme: &AppTheme,
    app_handle: &Entity<ArgusApp>,
    window_entity: &Entity<ConnectionDirectoryWindow>,
    mode: ConnectionDirectoryWindowMode,
) -> impl IntoElement {
    let cancel_app = app_handle.clone();
    let submit_app = app_handle.clone();
    let submit_window = window_entity.clone();
    let submit_label = match mode {
        ConnectionDirectoryWindowMode::Create => "创建目录",
        ConnectionDirectoryWindowMode::Edit { .. } => "保存目录",
    };

    div()
        .flex()
        .justify_end()
        .gap_2()
        .child(window_action_button(
            "connection-directory-window-cancel",
            ArgusIcon::Close,
            "取消",
            false,
            theme,
            move |_, window, cx| {
                cx.stop_propagation();
                close_directory_window(&cancel_app, window, cx);
            },
        ))
        .child(window_action_button(
            "connection-directory-window-submit",
            ArgusIcon::FolderPlus,
            submit_label,
            true,
            theme,
            move |_, window, cx| {
                cx.stop_propagation();
                submit_directory_window(&submit_app, &submit_window, window, cx);
            },
        ))
}

/// 渲染 SSH 链接窗口底部操作按钮。
fn render_link_actions(
    theme: &AppTheme,
    app_handle: &Entity<ArgusApp>,
    window_entity: &Entity<ConnectionLinkWindow>,
    mode: ConnectionLinkWindowMode,
) -> impl IntoElement {
    let cancel_app = app_handle.clone();
    let submit_app = app_handle.clone();
    let submit_window = window_entity.clone();
    let submit_label = match mode {
        ConnectionLinkWindowMode::Create => "创建链接",
        ConnectionLinkWindowMode::Edit { .. } => "保存链接",
    };

    div()
        .flex()
        .justify_end()
        .gap_2()
        .child(window_action_button(
            "connection-link-window-cancel",
            ArgusIcon::Close,
            "取消",
            false,
            theme,
            move |_, window, cx| {
                cx.stop_propagation();
                close_link_window(&cancel_app, window, cx);
            },
        ))
        .child(window_action_button(
            "connection-link-window-submit",
            ArgusIcon::Link,
            submit_label,
            true,
            theme,
            move |_, window, cx| {
                cx.stop_propagation();
                submit_link_window(&submit_app, &submit_window, window, cx);
            },
        ))
}

/// 渲染模态框操作按钮。
fn window_action_button(
    id: &'static str,
    icon: ArgusIcon,
    label: &'static str,
    is_primary: bool,
    theme: &AppTheme,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    let background = theme.current_line;
    let icon_color = if is_primary {
        theme.foreground
    } else {
        theme.foreground_muted
    };
    div()
        .id(id)
        .debug_selector(move || id.to_string())
        .h(px(30.0))
        .when(is_primary, |this| this.px_4())
        .when(!is_primary, |this| this.px_3())
        .flex()
        .items_center()
        .justify_center()
        .gap_1()
        .rounded_sm()
        .border_1()
        .border_color(rgb(theme.border))
        .bg(rgb(background))
        .text_size(px(12.0))
        .line_height(px(30.0))
        .text_color(rgb(theme.foreground))
        .when(is_primary, |this| this.font_weight(FontWeight::SEMIBOLD))
        .cursor_pointer()
        .hover(|this| this.bg(rgb(theme.current_line)))
        .on_click(on_click)
        .child(connection_button_icon(icon, icon_color, 13.0))
        .child(connection_button_label(label))
}

/// 渲染连接表单按钮图标，和日志加载器窗口按钮保持同样的视觉居中修正。
fn connection_button_icon(icon: ArgusIcon, color: u32, size: f32) -> impl IntoElement {
    div()
        .relative()
        .top(px(1.0))
        .child(render_icon(icon, color, size))
}

/// 渲染连接表单按钮文字，和按钮图标使用同一套视觉居中修正。
fn connection_button_label(label: &'static str) -> impl IntoElement {
    div().relative().top(px(1.0)).child(label)
}

/// 创建目录窗口的原生文本输入桥。
fn directory_native_input(
    window_entity: Entity<ConnectionDirectoryWindow>,
    target: ConnectionFormInputTarget,
    focus_handle: FocusHandle,
    app_handle: Entity<ArgusApp>,
) -> NativeInput {
    NativeInput::new(focus_handle, move |edit, _, cx| {
        window_entity.update(cx, |window_state, state_cx| {
            window_state.apply_native_edit(target, edit);
            state_cx.notify();
        });
        // 原生编辑同样可能清除错误行，需要刷新随错误状态变化的外壳高度。
        notify_connection_modal_layout(&app_handle, cx);
    })
}

/// 创建 SSH 链接窗口的原生文本输入桥。
fn link_native_input(
    window_entity: Entity<ConnectionLinkWindow>,
    target: ConnectionFormInputTarget,
    focus_handle: FocusHandle,
    app_handle: Entity<ArgusApp>,
) -> NativeInput {
    NativeInput::new(focus_handle, move |edit, _, cx| {
        window_entity.update(cx, |window_state, state_cx| {
            window_state.apply_native_edit(target, edit);
            state_cx.notify();
        });
        // IME 和系统粘贴走原生编辑回调，同样要刷新随 URL 变化的外壳高度。
        notify_connection_modal_layout(&app_handle, cx);
    })
}

/// 根据输入目标返回稳定元素 ID。
fn input_id_for_target(target: ConnectionFormInputTarget) -> &'static str {
    match target {
        ConnectionFormInputTarget::DirectoryName => "connection-directory-name-input",
        ConnectionFormInputTarget::LinkName => "connection-link-name-input",
        ConnectionFormInputTarget::LinkHost => "connection-link-host-input",
        ConnectionFormInputTarget::LinkUrl => "connection-link-url-input",
        ConnectionFormInputTarget::LinkPort => "connection-link-port-input",
        ConnectionFormInputTarget::LinkUsername => "connection-link-username-input",
        ConnectionFormInputTarget::LinkPassword => "connection-link-password-input",
        ConnectionFormInputTarget::LinkShare => "connection-link-share-input",
        ConnectionFormInputTarget::LinkInitialDir => "connection-link-initial-dir-input",
        ConnectionFormInputTarget::LinkDomain => "connection-link-domain-input",
        ConnectionFormInputTarget::LinkPrivateKeyPath => "connection-link-private-key-input",
        ConnectionFormInputTarget::LinkPrivateKeyPassphrase => {
            "connection-link-private-key-passphrase-input"
        }
    }
}

/// 处理目录窗口输入框派发出的窗口级动作。
fn handle_directory_window_action(
    action: ConnectionWindowInputAction,
    app_handle: &Entity<ArgusApp>,
    window_entity: &Entity<ConnectionDirectoryWindow>,
    window: &mut Window,
    cx: &mut App,
) {
    if action == ConnectionWindowInputAction::Submit {
        submit_directory_window(app_handle, window_entity, window, cx);
    }
}

/// 处理 SSH 链接窗口输入框派发出的窗口级动作。
fn handle_link_window_action(
    action: ConnectionWindowInputAction,
    app_handle: &Entity<ArgusApp>,
    window_entity: &Entity<ConnectionLinkWindow>,
    window: &mut Window,
    cx: &mut App,
) {
    if action == ConnectionWindowInputAction::Submit {
        submit_link_window(app_handle, window_entity, window, cx);
    }
}

/// 提交目录模态框表单，成功后关闭模态框，失败则把错误留在当前表单上。
fn submit_directory_window(
    app_handle: &Entity<ArgusApp>,
    window_entity: &Entity<ConnectionDirectoryWindow>,
    _window: &mut Window,
    cx: &mut App,
) {
    let (mode, form) = window_entity.read_with(cx, |window_state, _| {
        (window_state.mode, window_state.form.clone())
    });
    let result = match mode {
        ConnectionDirectoryWindowMode::Create => update_connection_app(app_handle, cx, |app, _| {
            app.create_connection_directory_from_form(form).map(|_| ())
        }),
        ConnectionDirectoryWindowMode::Edit { directory_id } => {
            update_connection_app(app_handle, cx, |app, _| {
                app.update_connection_directory_from_form(directory_id, form)
            })
        }
    };
    match result {
        Ok(_) => {
            update_connection_app(app_handle, cx, |app, _| {
                app.finish_connection_directory_window()
            });
        }
        Err(message) => set_directory_window_error(window_entity, cx, message),
    }
}

/// 提交链接模态框表单，成功后关闭模态框，失败则把错误留在当前表单上。
fn submit_link_window(
    app_handle: &Entity<ArgusApp>,
    window_entity: &Entity<ConnectionLinkWindow>,
    _window: &mut Window,
    cx: &mut App,
) {
    let (mode, form) = window_entity.read_with(cx, |window_state, _| {
        (window_state.mode, window_state.form.clone())
    });
    let result = match mode {
        ConnectionLinkWindowMode::Create => update_connection_app(app_handle, cx, |app, _| {
            app.create_connection_link_from_form(form).map(|_| ())
        }),
        ConnectionLinkWindowMode::Edit { link_id } => {
            update_connection_app(app_handle, cx, |app, _| {
                app.update_connection_link_from_form(link_id, form)
            })
        }
    };
    match result {
        Ok(_) => {
            update_connection_app(app_handle, cx, |app, _| app.finish_connection_link_window());
        }
        Err(message) => set_link_window_error(window_entity, cx, message),
    }
}

/// 关闭目录模态框并同步主应用状态。
fn close_directory_window(app_handle: &Entity<ArgusApp>, _window: &mut Window, cx: &mut App) {
    update_connection_app(app_handle, cx, |app, _| {
        app.close_connection_directory_window()
    });
}

/// 关闭链接模态框并同步主应用状态。
fn close_link_window(app_handle: &Entity<ArgusApp>, _window: &mut Window, cx: &mut App) {
    update_connection_app(app_handle, cx, |app, _| app.close_connection_link_window());
}

/// 写入目录窗口校验错误。
fn set_directory_window_error(
    window_entity: &Entity<ConnectionDirectoryWindow>,
    cx: &mut App,
    message: String,
) {
    let app_handle = window_entity.read_with(cx, |window_state, _| window_state.app.clone());
    window_entity.update(cx, |window_state, state_cx| {
        window_state.form.error_message = Some(message);
        state_cx.notify();
    });
    notify_connection_modal_layout(&app_handle, cx);
}

/// 写入 SSH 链接窗口校验错误。
fn set_link_window_error(
    window_entity: &Entity<ConnectionLinkWindow>,
    cx: &mut App,
    message: String,
) {
    let app_handle = window_entity.read_with(cx, |window_state, _| window_state.app.clone());
    window_entity.update(cx, |window_state, state_cx| {
        window_state.form.error_message = Some(message);
        state_cx.notify();
    });
    notify_connection_modal_layout(&app_handle, cx);
}

/// 通知主视图重绘连接弹窗外壳，以应用子表单的最新自适应高度。
///
/// 参数说明：
/// - `app_handle`：持有模态框的主应用实体。
/// - `cx`：当前应用上下文。
fn notify_connection_modal_layout(app_handle: &Entity<ArgusApp>, cx: &mut App) {
    app_handle.update(cx, |_, app_cx| app_cx.notify());
}

/// 统一更新主应用状态；创建窗口只负责表现，配置写入仍集中在 `ArgusApp`。
fn update_connection_app<R>(
    app_handle: &Entity<ArgusApp>,
    cx: &mut App,
    update: impl FnOnce(&mut ArgusApp, &mut Context<ArgusApp>) -> R,
) -> R {
    app_handle.update(cx, |app, app_cx| {
        let result = update(app, app_cx);
        app_cx.notify();
        result
    })
}

/// 渲染主机指纹确认弹窗。
fn render_host_key_prompt(
    prompt: ConnectionHostKeyPromptState,
    theme: &AppTheme,
    cx: &mut Context<ArgusApp>,
) -> impl IntoElement {
    let reject_prompt = prompt.clone();
    let confirm_prompt = prompt.clone();
    div()
        .size_full()
        .flex()
        .flex_col()
        .rounded_lg()
        .bg(rgb(theme.content))
        .child(dialog_header("确认主机指纹", ArgusIcon::Key, theme, cx))
        .child(
            div()
                .id("connection-host-key-dialog-scroll")
                .flex_1()
                .min_h(px(0.0))
                .overflow_y_scroll()
                .scrollbar_width(px(CONNECTION_FORM_SCROLLBAR_WIDTH))
                .px_5()
                .pr(px(CONNECTION_FORM_SCROLL_RIGHT_PADDING))
                .flex()
                .flex_col()
                .gap_3()
                .text_size(px(13.0))
                .line_height(px(18.0))
                .text_color(rgb(theme.foreground))
                .child(format!(
                    "{}:{} 首次连接，需要确认主机指纹。",
                    prompt.host, prompt.port
                ))
                .child(
                    div()
                        .p_3()
                        .rounded_sm()
                        .border_1()
                        .border_color(rgb(theme.border))
                        .bg(rgb(theme.content))
                        .text_color(rgb(theme.foreground))
                        .child(prompt.fingerprint),
                ),
        )
        .child(
            div()
                .flex_none()
                .px_5()
                .pt_5()
                .pb(px(MODAL_BOTTOM_PADDING))
                .flex()
                .justify_end()
                .gap_2()
                .child(action_button(
                    "connection-host-key-reject",
                    "拒绝",
                    false,
                    theme,
                    cx,
                    move |app, cx| {
                        app.reject_connection_host_key_prompt(reject_prompt.clone());
                        cx.notify();
                    },
                ))
                .child(action_button(
                    "connection-host-key-confirm",
                    "信任并继续",
                    true,
                    theme,
                    cx,
                    move |app, cx| {
                        app.confirm_connection_host_key_prompt(confirm_prompt.clone());
                        cx.notify();
                    },
                )),
        )
}

/// 展示本次握手的证书信息及精确授权范围，不为 Enter 设置默认信任动作。
fn render_svn_certificate_prompt(
    displayed: ConnectionSvnCertificatePromptState,
    theme: &AppTheme,
    cx: &mut Context<ArgusApp>,
) -> impl IntoElement {
    let prompt = &displayed.prompt;
    let mut fields = vec![
        ("目标地址", format!("{}:{}", prompt.host, prompt.port)),
        ("证书主体", prompt.subject.clone()),
        ("签发机构", prompt.issuer.clone()),
        (
            "允许的域名 / IP（SAN）",
            if prompt.subject_alt_names.is_empty() {
                "未提供".to_string()
            } else {
                prompt.subject_alt_names.join("\n")
            },
        ),
        (
            "有效期",
            format!("{} ～ {}", prompt.not_before, prompt.not_after),
        ),
        ("SHA-256 指纹", prompt.fingerprint.clone()),
    ];
    if let Some(previous) = &prompt.previous_fingerprint {
        fields.push(("此前信任的指纹", previous.clone()));
    }
    let reasons = prompt
        .failures
        .iter()
        .map(|failure| failure.label())
        .collect::<Vec<_>>()
        .join("\n");
    let has_changed = prompt
        .previous_fingerprint
        .as_ref()
        .is_some_and(|previous| previous != &prompt.fingerprint);
    let reject = displayed.clone();
    let once = displayed.clone();
    let permanent = displayed.clone();
    div()
        .size_full()
        .flex()
        .flex_col()
        .rounded_lg()
        .overflow_hidden()
        .bg(rgb(theme.content))
        .border_1()
        .border_color(rgb(theme.border))
        .child(dialog_header("确认 SVN 服务器证书", ArgusIcon::Key, theme, cx))
        .child(div().id("connection-svn-certificate-dialog-scroll")
            .debug_selector(|| "connection-svn-certificate-dialog-scroll".to_string())
            .flex_1().min_h(px(0.0)).w_full().overflow_y_scroll()
            .scrollbar_width(px(CONNECTION_FORM_SCROLLBAR_WIDTH))
            .px_5().pr(px(CONNECTION_FORM_SCROLL_RIGHT_PADDING))
            .flex().flex_col().gap_3().text_size(px(13.0)).line_height(px(19.0))
            .text_color(rgb(theme.foreground))
            .when(has_changed, |this| this.child(div().flex_none().w_full().text_color(rgb(theme.warning))
                .child("服务器证书与此前信任的指纹不同，请重新核对。")))
            .child(div().flex_none().w_full().text_color(rgb(theme.warning)).child(
                if reasons.is_empty() { "请确认以下服务器证书。".to_string() }
                else { format!("证书校验未通过：\n{reasons}") }))
            .children(fields.into_iter().map(|(label, value)| {
                div().flex_none().w_full().flex().flex_col().gap_1()
                    .child(div().w_full().text_color(rgb(theme.foreground_muted)).child(label))
                    .child(div().w_full().child(value))
            }))
            .child(div().flex_none().w_full().text_color(rgb(theme.foreground_muted))
                .child("请核对证书信息后选择是否信任。信任仅适用于当前链接、主机端口及这张证书；证书变化时将重新询问。")))
        .child(div().flex_none().px_5().pt_5().pb(px(MODAL_BOTTOM_PADDING))
            .flex().flex_col().gap_2()
            .when_some(displayed.error_message, |this, error| {
                this.child(div().id("connection-svn-certificate-error").flex_none().w_full()
                    .debug_selector(|| "connection-svn-certificate-error".to_string())
                    .text_size(px(12.0)).line_height(px(18.0))
                    .text_color(rgb(theme.error)).child(error))
            })
            .child(div().flex().justify_end().gap_2()
            .child(action_button("connection-svn-certificate-reject", "拒绝", false, theme, cx, move |app,cx| {
                app.respond_svn_certificate_prompt(reject.clone(), SvnCertificateDecision::Reject);
                cx.notify();
            }))
            .child(action_button("connection-svn-certificate-trust-once", "仅本次信任", false, theme, cx, move |app,cx| {
                app.respond_svn_certificate_prompt(once.clone(), SvnCertificateDecision::TrustOnce);
                cx.notify();
            }))
            .child(action_button("connection-svn-certificate-trust-permanently", "永久信任", true, theme, cx, move |app,cx| {
                app.respond_svn_certificate_prompt(permanent.clone(), SvnCertificateDecision::TrustPermanently);
                cx.notify();
            }))))
}

/// 渲染删除连接节点的二次确认弹窗。
fn render_delete_prompt(
    prompt: ConnectionDeletePromptState,
    theme: &AppTheme,
    cx: &mut Context<ArgusApp>,
) -> impl IntoElement {
    let node_id = prompt.node_id;
    let node_kind = if prompt.is_directory {
        "目录"
    } else {
        "链接"
    };
    div()
        .size_full()
        .relative()
        .flex()
        .flex_col()
        .rounded_lg()
        .overflow_hidden()
        .bg(rgb(theme.content))
        .border_1()
        .border_color(rgb(theme.border))
        .font_family(ARGUS_UI_FONT_FAMILY)
        .text_color(rgb(theme.foreground))
        .occlude()
        .child(render_connection_window_header(
            "确认删除",
            ArgusIcon::Trash,
            "connection-delete-dialog-close",
            "关闭删除确认",
            theme,
            cx.listener(|app, _, _, cx| {
                cx.stop_propagation();
                app.close_connection_dialog();
                cx.notify();
            }),
        ))
        .child(
            div()
                .id("connection-delete-dialog-scroll")
                .flex_1()
                .min_h(px(0.0))
                .overflow_y_scroll()
                .scrollbar_width(px(CONNECTION_FORM_SCROLLBAR_WIDTH))
                .px_5()
                .pr(px(CONNECTION_FORM_SCROLL_RIGHT_PADDING))
                .flex()
                .flex_col()
                .gap_3()
                .text_size(px(13.0))
                .line_height(px(18.0))
                .text_color(rgb(theme.foreground))
                .child(format!("确定要删除{node_kind}「{}」吗？", prompt.label))
                .child(
                    div()
                        .text_size(px(12.0))
                        .line_height(px(MODAL_FORM_NOTE_HEIGHT))
                        .text_color(rgb(theme.foreground_muted))
                        .child("此操作会立即写入本地配置。"),
                ),
        )
        .child(
            div()
                .flex_none()
                .px_5()
                .pt_5()
                .pb(px(MODAL_BOTTOM_PADDING))
                .flex()
                .justify_end()
                .gap_2()
                .child(window_action_button(
                    "connection-delete-dialog-cancel",
                    ArgusIcon::Close,
                    "取消",
                    false,
                    theme,
                    cx.listener(|app, _, _, cx| {
                        cx.stop_propagation();
                        app.close_connection_dialog();
                        cx.notify();
                    }),
                ))
                .child(window_action_button(
                    "connection-delete-dialog-submit",
                    ArgusIcon::Trash,
                    "确认删除",
                    true,
                    theme,
                    cx.listener(move |app, _, _, cx| {
                        cx.stop_propagation();
                        app.confirm_delete_connection_node(node_id);
                        cx.notify();
                    }),
                )),
        )
}

/// 渲染主机指纹确认弹窗标题栏。
fn dialog_header(
    title: &'static str,
    icon: ArgusIcon,
    theme: &AppTheme,
    cx: &mut Context<ArgusApp>,
) -> impl IntoElement {
    div()
        .h(px(CONNECTION_WINDOW_HEADER_HEIGHT))
        .flex_none()
        .px_5()
        .flex()
        .items_center()
        .justify_between()
        .occlude()
        .child(
            div()
                .flex()
                .items_center()
                .gap_2()
                .text_size(px(14.0))
                .line_height(px(18.0))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(rgb(theme.foreground))
                .child(render_icon(
                    icon,
                    theme.foreground_muted,
                    CONNECTION_WINDOW_TITLE_ICON_SIZE,
                ))
                .child(title),
        )
        .child(render_icon_button(
            "connection-dialog-close",
            ArgusIcon::Close,
            "关闭弹窗",
            false,
            IconButtonSize::Small,
            theme,
            cx.listener(|app, _, _, cx| {
                app.close_connection_dialog();
                cx.notify();
            }),
        ))
}

/// 渲染主机指纹弹窗操作按钮。
fn action_button(
    id: &'static str,
    label: &'static str,
    is_primary: bool,
    theme: &AppTheme,
    cx: &mut Context<ArgusApp>,
    on_click: impl Fn(&mut ArgusApp, &mut Context<ArgusApp>) + 'static,
) -> impl IntoElement {
    let background = if is_primary {
        theme.current_line
    } else {
        theme.content
    };
    div()
        .id(id)
        .debug_selector(move || id.to_string())
        .h(px(32.0))
        .px_3()
        .flex()
        .items_center()
        .justify_center()
        .rounded_sm()
        .border_1()
        .border_color(rgb(theme.border))
        .bg(rgb(background))
        .text_size(px(12.5))
        .text_color(rgb(theme.foreground))
        .cursor_pointer()
        .hover(|this| this.opacity(0.85))
        .child(label)
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(move |app, _: &MouseDownEvent, _, cx| {
                on_click(app, cx);
            }),
        )
}

/// 渲染表单错误文本。
fn error_text(message: String, theme: &AppTheme) -> impl IntoElement {
    div()
        .text_size(px(12.0))
        .line_height(px(MODAL_FORM_NOTE_HEIGHT))
        .text_color(rgb(theme.error))
        .child(message)
}

/// 聚焦输入框状态，并把光标移动到文本末尾。
fn focus_input_state(input: &mut TextInputState) {
    input.is_focused = true;
    input.cursor = character_count(&input.value);
    input.selection_anchor = None;
    input.marked_range = None;
    input.selection_drag = None;
}

/// 清理单个输入框焦点态。
fn clear_input_focus_state(input: &mut TextInputState) {
    input.clear_focus();
}

/// 处理输入框剪贴板快捷键（粘贴/复制/剪切/全选），命中时返回对应动作，否则返回 `None`。
///
/// 这些快捷键带有平台修饰键（macOS 为 Cmd，Windows/Linux 为 Ctrl），会被
/// [`handle_text_input_key`] 的字符插入分支跳过，因此在这里单独拦截处理，
/// 和设置窗口、Runtime 过滤输入框的粘贴行为保持一致。
fn handle_text_input_clipboard(
    input: &mut TextInputState,
    keystroke: &gpui::Keystroke,
    cx: &mut gpui::App,
) -> Option<ConnectionWindowInputAction> {
    if !keystroke.modifiers.secondary() {
        return None;
    }
    match keystroke.key.to_lowercase().as_str() {
        "v" => {
            let text = cx.read_from_clipboard().and_then(|item| item.text());
            if let Some(text) = text {
                // 单行输入框不支持换行，把换行符折叠成空格，避免光标计算错乱。
                insert_input_text(input, &text.replace(['\r', '\n'], " "));
            }
            Some(ConnectionWindowInputAction::Changed)
        }
        "c" => {
            if let Some(range) = input_selection_range(input) {
                cx.write_to_clipboard(ClipboardItem::new_string(slice_character_range(
                    &input.value,
                    range,
                )));
            }
            Some(ConnectionWindowInputAction::None)
        }
        "x" => {
            if let Some(range) = input_selection_range(input) {
                cx.write_to_clipboard(ClipboardItem::new_string(slice_character_range(
                    &input.value,
                    range,
                )));
                delete_input_selection(input);
            }
            Some(ConnectionWindowInputAction::Changed)
        }
        "a" => {
            input.cursor = character_count(&input.value);
            input.selection_anchor = Some(0);
            input.marked_range = None;
            input.selection_drag = None;
            Some(ConnectionWindowInputAction::Changed)
        }
        _ => None,
    }
}

/// 处理单行输入框按键编辑。
fn handle_text_input_key(
    input: &mut TextInputState,
    keystroke: &gpui::Keystroke,
) -> ConnectionWindowInputAction {
    match keystroke.key.as_str() {
        "escape" => ConnectionWindowInputAction::Close,
        "enter" => ConnectionWindowInputAction::Submit,
        "backspace" => {
            delete_input_backward(input);
            ConnectionWindowInputAction::Changed
        }
        "delete" => {
            delete_input_forward(input);
            ConnectionWindowInputAction::Changed
        }
        "left" | "arrowleft" => {
            move_input_left(input, keystroke.modifiers.shift);
            ConnectionWindowInputAction::Changed
        }
        "right" | "arrowright" => {
            move_input_right(input, keystroke.modifiers.shift);
            ConnectionWindowInputAction::Changed
        }
        "home" => {
            move_input_cursor(input, 0, keystroke.modifiers.shift);
            ConnectionWindowInputAction::Changed
        }
        "end" => {
            let end = character_count(&input.value);
            move_input_cursor(input, end, keystroke.modifiers.shift);
            ConnectionWindowInputAction::Changed
        }
        _ => {
            if let Some(key_char) = keystroke.key_char.as_ref()
                && !keystroke.modifiers.control
                && !keystroke.modifiers.platform
                && !key_char.chars().any(char::is_control)
            {
                insert_input_text(input, key_char);
                return ConnectionWindowInputAction::Changed;
            }
            ConnectionWindowInputAction::None
        }
    }
}

/// 鼠标开始选择输入框文本。
fn begin_input_pointer_selection(
    input: &mut TextInputState,
    character_index: usize,
    granularity: TextSelectionGranularity,
) {
    input.begin_pointer_selection(character_index, granularity);
}

/// 鼠标拖拽更新输入框选区。
fn update_input_pointer_selection(input: &mut TextInputState, character_index: usize) {
    input.update_pointer_selection(character_index);
}

/// 鼠标结束输入框文本选择。
fn finish_input_pointer_selection(input: &mut TextInputState) {
    input.finish_pointer_selection();
}

/// 删除输入框当前选区。
fn delete_input_selection(input: &mut TextInputState) -> bool {
    let Some(range) = input_selection_range(input) else {
        return false;
    };
    input.value = replace_character_range(&input.value, range.clone(), "");
    input.cursor = range.start;
    input.selection_anchor = None;
    input.marked_range = None;
    input.selection_drag = None;
    true
}

/// 向输入框插入文本。
fn insert_input_text(input: &mut TextInputState, text: &str) {
    if text.is_empty() {
        return;
    }
    let _ = delete_input_selection(input);
    let cursor = input.cursor.min(character_count(&input.value));
    input.value = replace_character_range(&input.value, cursor..cursor, text);
    input.cursor = cursor + character_count(text);
    input.selection_anchor = None;
    input.marked_range = None;
    input.selection_drag = None;
}

/// 删除输入框光标前一个字符。
fn delete_input_backward(input: &mut TextInputState) {
    if delete_input_selection(input) || input.cursor == 0 {
        return;
    }
    let cursor = input.cursor.min(character_count(&input.value));
    input.value = replace_character_range(&input.value, cursor - 1..cursor, "");
    input.cursor = cursor - 1;
    input.marked_range = None;
    input.selection_drag = None;
}

/// 删除输入框光标后一个字符。
fn delete_input_forward(input: &mut TextInputState) {
    if delete_input_selection(input) {
        return;
    }
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

/// 输入框光标左移。
fn move_input_left(input: &mut TextInputState, extend_selection: bool) {
    let cursor = input.cursor.saturating_sub(1);
    move_input_cursor(input, cursor, extend_selection);
}

/// 输入框光标右移。
fn move_input_right(input: &mut TextInputState, extend_selection: bool) {
    let cursor = (input.cursor + 1).min(character_count(&input.value));
    move_input_cursor(input, cursor, extend_selection);
}

/// 移动输入框光标，并按需扩展选区。
fn move_input_cursor(input: &mut TextInputState, cursor: usize, extend_selection: bool) {
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

/// 应用系统输入法提交的文本编辑。
fn apply_native_edit_to_input(input: &mut TextInputState, edit: &NativeTextEdit) {
    let text_length = character_count(&input.value);
    let replacement_range = clamp_range(edit.replacement_range.clone(), text_length);
    let next_value = replace_character_range(&input.value, replacement_range, &edit.text);
    let next_length = character_count(&next_value);
    let selected_range = clamp_range(edit.selected_range.clone(), next_length);
    let marked_range = edit
        .marked_range
        .clone()
        .map(|range| clamp_range(range, next_length))
        .filter(|range| range.start < range.end);

    input.value = next_value;
    input.cursor = selected_range.end;
    input.selection_anchor =
        (selected_range.start != selected_range.end).then_some(selected_range.start);
    input.marked_range = marked_range;
    input.selection_drag = None;
}

/// 返回输入框规范化后的非空选区。
fn input_selection_range(input: &TextInputState) -> Option<Range<usize>> {
    input.selection_range()
}

/// 将字符范围夹在文本长度内，并确保起止顺序稳定。
fn clamp_range(range: Range<usize>, text_length: usize) -> Range<usize> {
    let start = range.start.min(text_length);
    let end = range.end.min(text_length);
    start.min(end)..start.max(end)
}

/// 共用凭据管理表单的输入目标。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ConnectionCredentialInputTarget {
    /// 凭据名称。
    Name,
    /// 默认用户名。
    Username,
    /// 密码。
    Password,
    /// SMB 域或工作组。
    Domain,
}

impl ConnectionCredentialInputTarget {
    /// 返回稳定元素 ID。
    fn id(self) -> &'static str {
        match self {
            Self::Name => "connection-credential-name-input",
            Self::Username => "connection-credential-username-input",
            Self::Password => "connection-credential-password-input",
            Self::Domain => "connection-credential-domain-input",
        }
    }
}

/// 共用凭据管理窗口焦点句柄集合。
#[derive(Clone)]
struct ConnectionCredentialWindowFocusHandles {
    /// 根区域焦点。
    root: FocusHandle,
    /// 凭据名称输入焦点。
    name: FocusHandle,
    /// 默认用户名输入焦点。
    username: FocusHandle,
    /// 密码输入焦点。
    password: FocusHandle,
    /// SMB 域输入焦点。
    domain: FocusHandle,
}

/// 共用凭据管理模态框子视图。
pub(crate) struct ConnectionCredentialWindow {
    /// 主应用实体。
    app: Entity<ArgusApp>,
    /// 当前主题快照。
    theme: AppTheme,
    /// 凭据表单状态。
    form: ConnectionCredentialFormState,
    /// 输入框焦点句柄。
    focus_handles: ConnectionCredentialWindowFocusHandles,
    /// 主题观察订阅。
    _app_observer: Subscription,
    /// 是否已在首帧把真实键盘焦点交给名称输入框。
    did_request_focus: bool,
}

impl ConnectionCredentialWindow {
    /// 创建共用凭据管理模态框。
    pub(crate) fn new(
        app: Entity<ArgusApp>,
        theme: AppTheme,
        preferred_protocol: ConnectionLinkKind,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut form = ConnectionCredentialFormState {
            preferred_protocol,
            allow_ssh: preferred_protocol == ConnectionLinkKind::Ssh,
            allow_smb: preferred_protocol == ConnectionLinkKind::Smb,
            allow_svn: preferred_protocol == ConnectionLinkKind::Svn,
            ..ConnectionCredentialFormState::default()
        };
        focus_input_state(&mut form.name_input);
        let _app_observer = cx.observe(&app, |window_state, app_entity, cx| {
            let theme = app_entity.read_with(cx, |app, _| app.theme.clone());
            if window_state.theme == theme {
                return;
            }
            window_state.theme = theme;
            cx.notify();
        });
        Self {
            app,
            theme,
            form,
            focus_handles: ConnectionCredentialWindowFocusHandles {
                root: cx.focus_handle(),
                name: cx.focus_handle(),
                username: cx.focus_handle(),
                password: cx.focus_handle(),
                domain: cx.focus_handle(),
            },
            _app_observer,
            did_request_focus: false,
        }
    }

    /// 清空表单并回到新建状态。
    fn reset_form(&mut self) {
        let preferred_protocol = self.form.preferred_protocol;
        self.form = ConnectionCredentialFormState {
            preferred_protocol,
            allow_ssh: preferred_protocol == ConnectionLinkKind::Ssh,
            allow_smb: preferred_protocol == ConnectionLinkKind::Smb,
            allow_svn: preferred_protocol == ConnectionLinkKind::Svn,
            ..ConnectionCredentialFormState::default()
        };
        focus_input_state(&mut self.form.name_input);
    }

    /// 选中已有凭据并填充表单；密码留空等待用户按需修改。
    fn select_credential(&mut self, credential_id: ConnectionCredentialId, cx: &mut Context<Self>) {
        let credential = self.app.read_with(cx, |app, _| {
            app.config.connections.credential(credential_id).cloned()
        });
        let Some(credential) = credential else {
            self.form.error_message = Some("连接凭据不存在".to_string());
            return;
        };
        self.form.selected_id = Some(credential_id);
        self.form.allow_ssh = credential.supports_protocol(ConnectionLinkKind::Ssh);
        self.form.allow_smb = credential.supports_protocol(ConnectionLinkKind::Smb);
        self.form.allow_svn = credential.supports_protocol(ConnectionLinkKind::Svn);
        self.form.name_input = TextInputState::from_value(credential.name);
        self.form.username_input = TextInputState::from_value(credential.username);
        self.form.password_input = TextInputState::default();
        self.form.domain_input = TextInputState::from_value(credential.domain.unwrap_or_default());
        self.form.is_default = credential.is_default;
        self.form.error_message = None;
        focus_input_state(&mut self.form.name_input);
    }

    /// 聚焦指定输入框。
    fn focus_input(&mut self, target: ConnectionCredentialInputTarget) {
        self.clear_input_focuses();
        if let Some(input) = self.input_mut(target) {
            focus_input_state(input);
        }
    }

    /// 清理全部输入焦点。
    fn clear_input_focuses(&mut self) {
        clear_input_focus_state(&mut self.form.name_input);
        clear_input_focus_state(&mut self.form.username_input);
        clear_input_focus_state(&mut self.form.password_input);
        clear_input_focus_state(&mut self.form.domain_input);
    }

    /// 返回输入目标对应的可变输入状态。
    fn input_mut(
        &mut self,
        target: ConnectionCredentialInputTarget,
    ) -> Option<&mut TextInputState> {
        match target {
            ConnectionCredentialInputTarget::Name => Some(&mut self.form.name_input),
            ConnectionCredentialInputTarget::Username => Some(&mut self.form.username_input),
            ConnectionCredentialInputTarget::Password => Some(&mut self.form.password_input),
            ConnectionCredentialInputTarget::Domain => Some(&mut self.form.domain_input),
        }
    }

    /// 处理输入按键。
    fn handle_input_key(
        &mut self,
        target: ConnectionCredentialInputTarget,
        keystroke: &gpui::Keystroke,
        cx: &mut Context<Self>,
    ) -> ConnectionWindowInputAction {
        let Some(input) = self.input_mut(target) else {
            return ConnectionWindowInputAction::None;
        };
        if let Some(action) = handle_text_input_clipboard(input, keystroke, cx) {
            if action == ConnectionWindowInputAction::Changed {
                self.form.error_message = None;
            }
            return action;
        }
        let action = handle_text_input_key(input, keystroke);
        if action == ConnectionWindowInputAction::Changed {
            self.form.error_message = None;
        }
        action
    }

    /// 开始鼠标选区。
    fn begin_pointer_selection(
        &mut self,
        target: ConnectionCredentialInputTarget,
        character_index: usize,
        granularity: TextSelectionGranularity,
    ) {
        self.focus_input(target);
        let Some(input) = self.input_mut(target) else {
            return;
        };
        begin_input_pointer_selection(input, character_index, granularity);
    }

    /// 更新鼠标选区。
    fn update_pointer_selection(
        &mut self,
        target: ConnectionCredentialInputTarget,
        character_index: usize,
    ) {
        let Some(input) = self.input_mut(target) else {
            return;
        };
        update_input_pointer_selection(input, character_index);
    }

    /// 结束鼠标选区。
    fn finish_pointer_selection(&mut self, target: ConnectionCredentialInputTarget) {
        let Some(input) = self.input_mut(target) else {
            return;
        };
        finish_input_pointer_selection(input);
    }

    /// 应用输入法提交的文本编辑。
    fn apply_native_edit(&mut self, target: ConnectionCredentialInputTarget, edit: NativeTextEdit) {
        self.focus_input(target);
        let Some(input) = self.input_mut(target) else {
            return;
        };
        apply_native_edit_to_input(input, &edit);
        self.form.error_message = None;
    }

    /// 保存当前表单；失败时把错误保留在弹窗内。
    fn submit(&mut self, cx: &mut Context<Self>) {
        let form = self.form.clone();
        let result = self.app.update(cx, |app, app_cx| {
            if form.selected_id.is_some() {
                app.update_connection_credential_from_form(&form, app_cx)
                    .map(|_| form.selected_id.unwrap_or_default())
            } else {
                app.create_connection_credential_from_form(&form, app_cx)
            }
        });
        match result {
            Ok(credential_id) => {
                self.form.selected_id = Some(credential_id);
                self.form.password_input = TextInputState::default();
                self.form.error_message = None;
            }
            Err(message) => {
                self.form.error_message = Some(message);
            }
        }
    }

    /// 删除当前选中凭据。
    fn delete(&mut self, cx: &mut Context<Self>) {
        let Some(credential_id) = self.form.selected_id else {
            return;
        };
        let result = self.app.update(cx, |app, app_cx| {
            app.delete_connection_credential(credential_id, app_cx)
        });
        match result {
            Ok(()) => self.reset_form(),
            Err(message) => self.form.error_message = Some(message),
        }
    }
}

impl Render for ConnectionCredentialWindow {
    /// 渲染共用凭据管理模态框主体。
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if !self.did_request_focus {
            self.did_request_focus = true;
            self.focus_handles.name.focus(window);
        }
        let entries = self.app.read_with(cx, |app, _| {
            app.config
                .connections
                .credentials
                .iter()
                .cloned()
                .map(|credential| {
                    let usage = app.config.connections.credential_usage_count(credential.id);
                    (credential, usage)
                })
                .collect::<Vec<_>>()
        });
        render_credential_window_content(
            self.app.clone(),
            cx.entity(),
            self.theme.clone(),
            self.form.clone(),
            self.focus_handles.clone(),
            entries,
            cx,
        )
    }
}

/// 将共用凭据管理子视图包裹为主窗口模态框。
pub(crate) fn render_connection_credential_modal(
    modal: Entity<ConnectionCredentialWindow>,
    theme: &AppTheme,
    cx: &mut Context<ArgusApp>,
) -> AnyElement {
    render_modal_dialog(
        ModalDialog {
            overlay_id: "connection-credential-modal-overlay",
            container_id: "connection-credential-modal-container",
            width: CONNECTION_CREDENTIAL_MODAL_WIDTH,
            height: CONNECTION_CREDENTIAL_MODAL_HEIGHT,
            content: modal.into_any_element(),
        },
        theme.clone(),
        cx,
    )
    .into_any_element()
}

/// 渲染共用凭据管理内容。
fn render_credential_window_content(
    app_handle: Entity<ArgusApp>,
    window_entity: Entity<ConnectionCredentialWindow>,
    theme: AppTheme,
    form: ConnectionCredentialFormState,
    focus_handles: ConnectionCredentialWindowFocusHandles,
    entries: Vec<(crate::remote::connection::ConnectionCredentialConfig, usize)>,
    cx: &mut Context<ConnectionCredentialWindow>,
) -> impl IntoElement {
    let close_app = app_handle.clone();
    let root_focus_for_track = focus_handles.root.clone();
    let root_focus_for_click = focus_handles.root.clone();
    let close_window = window_entity.clone();
    let has_selection = form.selected_id.is_some();
    let password_placeholder = if has_selection {
        "留空表示不修改"
    } else {
        "必填"
    };

    div()
        .id("connection-credential-window-root")
        .size_full()
        .relative()
        .flex()
        .flex_col()
        .rounded_lg()
        .overflow_hidden()
        .bg(rgb(theme.content))
        .border_1()
        .border_color(rgb(theme.border))
        .font_family(ARGUS_UI_FONT_FAMILY)
        .text_color(rgb(theme.foreground))
        .occlude()
        .focusable()
        .track_focus(&root_focus_for_track)
        .on_click(move |_, window, cx| {
            root_focus_for_click.focus(window);
            close_window.update(cx, |window_state, state_cx| {
                window_state.clear_input_focuses();
                state_cx.notify();
            });
        })
        .child(render_connection_window_header(
            "连接凭据",
            ArgusIcon::Key,
            "connection-credential-close",
            "关闭连接凭据",
            &theme,
            move |_, window, cx| {
                cx.stop_propagation();
                close_app.update(cx, |app, app_cx| {
                    app.close_connection_credential_manager();
                    app_cx.notify();
                });
                let _ = window;
            },
        ))
        .child(
            div()
                .flex_1()
                .min_h(px(0.0))
                .flex()
                .child(
                    div()
                        .w(px(210.0))
                        .flex_none()
                        .h_full()
                        .flex()
                        .flex_col()
                        .border_r_1()
                        .border_color(rgb(theme.border))
                        .child(
                            div().p_3().child(render_credential_action_button(
                                "connection-credential-new",
                                "新建凭据",
                                false,
                                &theme,
                                cx,
                                move |window_state, state_cx| {
                                    window_state.reset_form();
                                    state_cx.notify();
                                },
                            )),
                        )
                        .child(
                            div()
                                .id("connection-credential-list")
                                .debug_selector(|| "connection-credential-list".to_string())
                                .flex_1()
                                .min_h(px(0.0))
                                .overflow_y_scroll()
                                .scrollbar_width(px(6.0))
                                .px_2()
                                .pb_3()
                                .flex()
                                .flex_col()
                                .gap_1()
                                .when(entries.is_empty(), |this| {
                                    this.child(
                                        div()
                                            .p_2()
                                            .text_size(px(11.0))
                                            .text_color(rgb(theme.foreground_muted))
                                            .child("暂无凭据"),
                                    )
                                })
                                .children(entries.into_iter().map(|(credential, usage)| {
                                    let credential_id = credential.id;
                                    let is_selected = form.selected_id == Some(credential_id);
                                    let item_window = window_entity.clone();
                                    let item_theme = theme.clone();
                                    let title = credential.name.clone();
                                    let username = credential.username.clone();
                                    let protocols = credential.protocol_labels();
                                    let protocol_summary = if credential.is_default {
                                        format!("{protocols} · 默认")
                                    } else {
                                        protocols
                                    };
                                    div()
                                        .id(SharedString::from(format!(
                                            "connection-credential-item-{credential_id}"
                                        )))
                                        .p_2()
                                        .rounded_sm()
                                        .bg(rgb(if is_selected {
                                            item_theme.current_line
                                        } else {
                                            item_theme.content
                                        }))
                                        .cursor_pointer()
                                        .hover(move |this| {
                                            this.bg(rgb(item_theme.current_line))
                                        })
                                        .on_click(move |_, _, app| {
                                            app.stop_propagation();
                                            item_window.update(app, |window_state, state_cx| {
                                                window_state
                                                    .select_credential(credential_id, state_cx);
                                                state_cx.notify();
                                            });
                                        })
                                        .child(
                                            div()
                                                .truncate()
                                                .text_size(px(12.0))
                                                .font_weight(FontWeight::MEDIUM)
                                                .child(title),
                                        )
                                        .child(
                                            div()
                                                .truncate()
                                                .text_size(px(10.5))
                                                .text_color(rgb(theme.foreground_muted))
                                                .child(format!("{username} · {protocol_summary}")),
                                        )
                                        .child(
                                            div()
                                                .text_size(px(10.0))
                                                .text_color(rgb(theme.foreground_muted))
                                                .child(format!("{usage} 个连接使用")),
                                        )
                                })),
                        ),
                )
                .child(
                    div()
                        .id("connection-credential-form")
                        .flex_1()
                        .min_w(px(0.0))
                        .min_h(px(0.0))
                        .overflow_y_scroll()
                        .scrollbar_width(px(6.0))
                        .p_4()
                        .flex()
                        .flex_col()
                        .gap_3()
                        .child(render_credential_input_row(
                            "凭据名称",
                            ConnectionCredentialInputTarget::Name,
                            &form.name_input,
                            focus_handles.name.clone(),
                            "运维账号",
                            false,
                            &theme,
                            &window_entity,
                            &app_handle,
                        ))
                        .child(render_credential_input_row(
                            "默认用户名",
                            ConnectionCredentialInputTarget::Username,
                            &form.username_input,
                            focus_handles.username.clone(),
                            "ops",
                            false,
                            &theme,
                            &window_entity,
                            &app_handle,
                        ))
                        .child(render_credential_input_row(
                            "密码",
                            ConnectionCredentialInputTarget::Password,
                            &form.password_input,
                            focus_handles.password.clone(),
                            password_placeholder,
                            true,
                            &theme,
                            &window_entity,
                            &app_handle,
                        ))
                        .child(render_credential_input_row(
                            "域/工作组",
                            ConnectionCredentialInputTarget::Domain,
                            &form.domain_input,
                            focus_handles.domain.clone(),
                            "可选，仅 SMB 使用",
                            false,
                            &theme,
                            &window_entity,
                            &app_handle,
                        ))
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap_3()
                                .child(
                                    div()
                                        .w(px(82.0))
                                        .text_size(px(12.0))
                                        .text_color(rgb(theme.foreground_muted))
                                        .child("适用协议"),
                                )
                                .child(
                                    div()
                                        .flex()
                                        .gap_2()
                                        .child(render_credential_protocol_chip(
                                            "connection-credential-protocol-ssh",
                                            "SSH",
                                            form.allow_ssh,
                                            ConnectionLinkKind::Ssh,
                                            &theme,
                                            &window_entity,
                                        ))
                                        .child(render_credential_protocol_chip(
                                            "connection-credential-protocol-smb",
                                            "SMB",
                                            form.allow_smb,
                                            ConnectionLinkKind::Smb,
                                            &theme,
                                            &window_entity,
                                        ))
                                        .child(render_credential_protocol_chip(
                                            "connection-credential-protocol-svn",
                                            "SVN",
                                            form.allow_svn,
                                            ConnectionLinkKind::Svn,
                                            &theme,
                                            &window_entity,
                                        )),
                                ),
                        )
                        .child(render_credential_default_toggle(
                            form.is_default,
                            &theme,
                            &window_entity,
                        ))
                        .child(
                            div()
                                .text_size(px(11.0))
                                .line_height(px(MODAL_FORM_NOTE_HEIGHT))
                                .text_color(rgb(theme.foreground_muted))
                                .child("密码保存在系统凭据库中，不会写入设置文件；修改后所有引用该凭据的连接下次连接生效。"),
                        )
                        .when_some(form.error_message.clone(), |this, message| {
                            this.child(error_text(message, &theme))
                        }),
                ),
        )
        .child(
            div()
                .flex_none()
                .px_5()
                .pt_4()
                .pb(px(MODAL_BOTTOM_PADDING))
                .flex()
                .justify_between()
                .items_center()
                .child(if has_selection {
                    render_credential_action_button(
                        "connection-credential-delete",
                        "删除凭据",
                        false,
                        &theme,
                        cx,
                        move |window_state, state_cx| {
                            window_state.delete(state_cx);
                            state_cx.notify();
                        },
                    )
                    .into_any_element()
                } else {
                    div().into_any_element()
                })
                .child(
                    div()
                        .flex()
                        .gap_2()
                        .child(render_credential_action_button(
                            "connection-credential-cancel",
                            "关闭",
                            false,
                            &theme,
                            cx,
                            {
                                let app_handle = app_handle.clone();
                                move |_, state_cx| {
                                    app_handle.update(state_cx, |app, app_cx| {
                                        app.close_connection_credential_manager();
                                        app_cx.notify();
                                    });
                                }
                            },
                        ))
                        .child(render_credential_action_button(
                            "connection-credential-save",
                            "保存",
                            true,
                            &theme,
                            cx,
                            move |window_state, state_cx| {
                                window_state.submit(state_cx);
                            },
                        )),
                ),
        )
}

/// 渲染凭据输入行。
fn render_credential_input_row(
    label: &'static str,
    target: ConnectionCredentialInputTarget,
    input_state: &TextInputState,
    focus_handle: FocusHandle,
    placeholder: &'static str,
    is_secret: bool,
    theme: &AppTheme,
    window_entity: &Entity<ConnectionCredentialWindow>,
    app_handle: &Entity<ArgusApp>,
) -> impl IntoElement {
    let native_input = credential_native_input(window_entity.clone(), target, focus_handle.clone());
    let key_window = window_entity.clone();
    let click_window = window_entity.clone();
    let pointer_window = window_entity.clone();
    let submit_window = window_entity.clone();
    let close_app = app_handle.clone();

    div()
        .flex()
        .items_center()
        .gap_3()
        .child(
            div()
                .w(px(82.0))
                .text_size(px(12.0))
                .text_color(rgb(theme.foreground_muted))
                .child(label),
        )
        .child(div().flex_1().child(render_input(
            Input {
                id: target.id(),
                placeholder,
                value: input_state.value.clone(),
                is_disabled: false,
                is_focused: input_state.is_focused,
                cursor_index: input_state.cursor,
                selection_range: input_state.selection_range(),
                marked_range: input_state.marked_range.clone(),
                is_pointer_selecting: input_state.selection_drag.is_some(),
                is_secret,
                size: InputSize::Regular,
                leading_accessory: None,
                trailing_accessory: None,
                native_input: Some(native_input),
                scroll_handle: input_state.scroll_handle.clone(),
            },
            theme,
            move |event: &KeyDownEvent, _, cx| {
                cx.stop_propagation();
                let action = key_window.update(cx, |window_state, state_cx| {
                    let action = window_state.handle_input_key(target, &event.keystroke, state_cx);
                    state_cx.notify();
                    action
                });
                if action == ConnectionWindowInputAction::Submit {
                    submit_window.update(cx, |window_state, state_cx| {
                        window_state.submit(state_cx);
                        state_cx.notify();
                    });
                } else if action == ConnectionWindowInputAction::Close {
                    close_app.update(cx, |app, app_cx| {
                        app.close_connection_credential_manager();
                        app_cx.notify();
                    });
                }
            },
            move |_, window, cx| {
                cx.stop_propagation();
                focus_handle.focus(window);
                click_window.update(cx, |window_state, state_cx| {
                    window_state.focus_input(target);
                    state_cx.notify();
                });
            },
            move |event: &InputPointerEvent, _, cx| {
                cx.stop_propagation();
                pointer_window.update(cx, |window_state, state_cx| {
                    match event.action {
                        InputPointerAction::Begin => window_state.begin_pointer_selection(
                            target,
                            event.character_index,
                            event.granularity,
                        ),
                        InputPointerAction::Extend => {
                            window_state.update_pointer_selection(target, event.character_index)
                        }
                        InputPointerAction::Finish => window_state.finish_pointer_selection(target),
                    }
                    state_cx.notify();
                });
            },
            |_, _, _| {},
        )))
}

/// 构造凭据输入框的原生输入桥接。
fn credential_native_input(
    window_entity: Entity<ConnectionCredentialWindow>,
    target: ConnectionCredentialInputTarget,
    focus_handle: FocusHandle,
) -> NativeInput {
    NativeInput::new(focus_handle, move |edit, _, cx| {
        window_entity.update(cx, |window_state, state_cx| {
            window_state.apply_native_edit(target, edit);
            state_cx.notify();
        });
    })
}

/// 渲染协议选择标签。
fn render_credential_protocol_chip(
    id: &'static str,
    label: &'static str,
    selected: bool,
    protocol: ConnectionLinkKind,
    theme: &AppTheme,
    window_entity: &Entity<ConnectionCredentialWindow>,
) -> impl IntoElement {
    let entity = window_entity.clone();
    div()
        .id(id)
        .px_3()
        .h(px(28.0))
        .flex()
        .items_center()
        .rounded_sm()
        .border_1()
        .border_color(rgb(if selected { theme.info } else { theme.border }))
        .bg(rgb(if selected {
            theme.current_line
        } else {
            theme.content
        }))
        .text_size(px(12.0))
        .text_color(rgb(if selected {
            theme.foreground
        } else {
            theme.foreground_muted
        }))
        .cursor_pointer()
        .hover(|this| this.opacity(0.85))
        .child(label)
        .on_click(move |_, _, cx| {
            cx.stop_propagation();
            entity.update(cx, |window_state, state_cx| {
                match protocol {
                    ConnectionLinkKind::Ssh => {
                        window_state.form.allow_ssh = !window_state.form.allow_ssh
                    }
                    ConnectionLinkKind::Smb => {
                        window_state.form.allow_smb = !window_state.form.allow_smb
                    }
                    ConnectionLinkKind::Svn => {
                        window_state.form.allow_svn = !window_state.form.allow_svn
                    }
                    ConnectionLinkKind::Git => {}
                }
                window_state.form.error_message = None;
                state_cx.notify();
            });
        })
}

/// 渲染默认凭据开关。
fn render_credential_default_toggle(
    is_default: bool,
    theme: &AppTheme,
    window_entity: &Entity<ConnectionCredentialWindow>,
) -> impl IntoElement {
    let entity = window_entity.clone();
    div()
        .id("connection-credential-default")
        .flex()
        .items_center()
        .gap_2()
        .cursor_pointer()
        .on_click(move |_, _, cx| {
            cx.stop_propagation();
            entity.update(cx, |window_state, state_cx| {
                window_state.form.is_default = !window_state.form.is_default;
                state_cx.notify();
            });
        })
        .child(
            div()
                .w(px(14.0))
                .h(px(14.0))
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(3.0))
                .border_1()
                .border_color(rgb(theme.border))
                .text_size(px(10.0))
                .child(if is_default { "✓" } else { "" }),
        )
        .child(
            div()
                .text_size(px(12.0))
                .text_color(rgb(theme.foreground_muted))
                .child("设为同用户名新连接的默认凭据"),
        )
}

/// 渲染凭据管理操作按钮。
fn render_credential_action_button(
    id: &'static str,
    label: &'static str,
    is_primary: bool,
    theme: &AppTheme,
    cx: &mut Context<ConnectionCredentialWindow>,
    on_click: impl Fn(&mut ConnectionCredentialWindow, &mut Context<ConnectionCredentialWindow>)
    + 'static,
) -> impl IntoElement {
    div()
        .id(id)
        .debug_selector(move || id.to_string())
        .h(px(MODAL_ACTION_HEIGHT))
        .px_3()
        .flex()
        .items_center()
        .justify_center()
        .rounded_sm()
        .border_1()
        .border_color(rgb(theme.border))
        .bg(rgb(if is_primary {
            theme.current_line
        } else {
            theme.content
        }))
        .text_size(px(12.5))
        .text_color(rgb(theme.foreground))
        .cursor_pointer()
        .hover(|this| this.opacity(0.85))
        .child(label)
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(move |window_state, _: &MouseDownEvent, _, state_cx| {
                on_click(window_state, state_cx);
                state_cx.notify();
            }),
        )
}

#[cfg(test)]
mod tests {
    use super::{connection_directory_modal_height, preferred_connection_link_modal_height_for};
    use crate::remote::connection::ConnectionLinkKind;

    /// 目录弹窗高度应贴合内容：错误行只增加一行高度，底部不再保留固定空白。
    #[test]
    fn directory_modal_height_matches_content() {
        assert_eq!(connection_directory_modal_height(false), 157.0);
        assert_eq!(connection_directory_modal_height(true), 185.0);
    }

    /// 短的 HTTPS 仓库表单应明显低于 SSH/SMB 长表单，不再保留固定空白。
    #[test]
    fn repository_modal_height_matches_visible_fields() {
        assert_eq!(
            preferred_connection_link_modal_height_for(
                ConnectionLinkKind::Git,
                "https://host/team/repository.git",
                false,
                false,
            ),
            353.0
        );
        assert_eq!(
            preferred_connection_link_modal_height_for(
                ConnectionLinkKind::Git,
                "git@host:team/repository.git",
                false,
                false,
            ),
            367.0
        );
    }

    /// SSH 和 SMB 的高度应分别容纳其 7 行与 8 行输入项。
    #[test]
    fn server_modal_height_matches_protocol_fields() {
        assert_eq!(
            preferred_connection_link_modal_height_for(ConnectionLinkKind::Ssh, "", false, false),
            521.0
        );
        assert_eq!(
            preferred_connection_link_modal_height_for(ConnectionLinkKind::Smb, "", false, false),
            563.0
        );
    }

    /// SVN 明文传输提示和校验错误应各自增加一行高度。
    #[test]
    fn repository_modal_height_reserves_messages() {
        assert_eq!(
            preferred_connection_link_modal_height_for(
                ConnectionLinkKind::Svn,
                "http://host/svn/repository/",
                false,
                false,
            ),
            423.0
        );
        assert_eq!(
            preferred_connection_link_modal_height_for(
                ConnectionLinkKind::Svn,
                "http://host/svn/repository/",
                true,
                false,
            ),
            451.0
        );
    }
}
