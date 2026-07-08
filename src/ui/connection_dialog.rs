//! 文件职责：渲染链接工作区的新增目录窗口、新增 SSH 链接窗口和主机指纹确认弹窗。
//! 创建日期：2026-06-26
//! 修改日期：2026-06-26
//! 作者：Argus 开发团队
//! 主要功能：提供链接目录和 SSH 链接创建独立窗口，并在首次连接未知主机时展示指纹确认。

use std::ops::Range;

use gpui::{
    App, ClickEvent, ClipboardItem, Context, Entity, FocusHandle, FontWeight, IntoElement,
    KeyDownEvent, MouseButton, MouseDownEvent, Render, Subscription, Window, div, prelude::*, px,
    rgb,
};

use crate::app::{
    ArgusApp, ConnectionDeletePromptState, ConnectionDialogState, ConnectionDirectoryFormState,
    ConnectionHostKeyPromptState, ConnectionLinkFormState, InputTextSelectionDrag,
    SettingsTextInputState,
};
use crate::remote::connection::ConnectionLinkKind;
use crate::fonts::ARGUS_UI_FONT_FAMILY;
use crate::infra::text_selection::{
    NativeTextEdit, TextSelectionGranularity, character_count, replace_character_range,
    slice_character_range, word_range_at,
};
use crate::theme::AppTheme;
use crate::ui::components::icon::{ArgusIcon, render_icon};
use crate::ui::components::icon_button::{IconButtonSize, render_icon_button};
use crate::ui::components::input::{
    Input, InputPointerAction, InputPointerEvent, InputSize, NativeInput, render_input,
};
use crate::ui::components::modal_dialog::{ModalDialog, render_modal_dialog};

/// 新增目录窗口固定标题栏高度，和设置窗口保持一致。
const CONNECTION_WINDOW_HEADER_HEIGHT: f32 = 56.0;
/// 新增目录/链接窗口标题图标尺寸，匹配设置窗口标题视觉比例。
const CONNECTION_WINDOW_TITLE_ICON_SIZE: f32 = 16.0;
/// 主机指纹确认弹窗宽度。
const HOST_KEY_DIALOG_WIDTH: f32 = 520.0;
/// 主机指纹确认弹窗高度。
const HOST_KEY_DIALOG_HEIGHT: f32 = 280.0;
/// 删除确认弹窗宽度。
const DELETE_DIALOG_WIDTH: f32 = 420.0;
/// 删除确认弹窗高度。
const DELETE_DIALOG_HEIGHT: f32 = 210.0;

/// 目录窗口模式，区分新增和编辑已有目录。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ConnectionDirectoryWindowMode {
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
pub enum ConnectionLinkWindowMode {
    /// 新增 SSH 链接。
    Create,
    /// 编辑已有 SSH 链接。
    Edit {
        /// 正在编辑的 SSH 链接节点 ID。
        link_id: crate::remote::connection::ConnectionNodeId,
    },
}

/// 目录表单独立窗口视图；表单状态保存在窗口本地，提交时写回主应用配置。
pub struct ConnectionDirectoryWindow {
    /// 主应用实体，提交和关闭状态同步都写回 `ArgusApp`。
    app: Entity<ArgusApp>,
    /// 当前窗口使用的主题快照。
    theme: AppTheme,
    /// 目录表单状态。
    form: ConnectionDirectoryFormState,
    /// 当前窗口处于新增还是编辑模式。
    mode: ConnectionDirectoryWindowMode,
    /// 窗口根区域和输入框焦点句柄。
    focus_handles: ConnectionDirectoryWindowFocusHandles,
    /// 主应用状态订阅，主题切换后窗口跟随刷新。
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
    /// 创建目录表单独立窗口。
    ///
    /// 参数说明：
    /// - `app`：主应用实体。
    /// - `theme`：首次绘制使用的主题。
    /// - `form`：按当前选中目录推导出的初始表单。
    /// - `cx`：窗口上下文，用于创建焦点句柄和订阅主应用变化。
    ///
    /// 返回值：可渲染的新增目录窗口视图。
    pub fn new(
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

    /// 判断当前目录窗口是否为指定模式，用于重复点击按钮时决定置前还是替换表单。
    pub fn is_mode(&self, mode: ConnectionDirectoryWindowMode) -> bool {
        self.mode == mode
    }

    /// 替换目录窗口表单和模式，供右键编辑入口复用已经打开的窗口。
    pub fn replace_form(
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
    fn input_mut(
        &mut self,
        target: ConnectionFormInputTarget,
    ) -> Option<&mut SettingsTextInputState> {
        match target {
            ConnectionFormInputTarget::DirectoryName => Some(&mut self.form.name_input),
            _ => None,
        }
    }

    /// 处理目录窗口输入框键盘事件，并返回是否需要提交或关闭窗口。
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
    /// 渲染目录表单独立窗口主体。
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

/// SSH 链接表单独立窗口视图；表单状态保存在窗口本地，提交时写回主应用配置。
pub struct ConnectionLinkWindow {
    /// 主应用实体，提交和关闭状态同步都写回 `ArgusApp`。
    app: Entity<ArgusApp>,
    /// 当前窗口使用的主题快照。
    theme: AppTheme,
    /// SSH 链接表单状态。
    form: ConnectionLinkFormState,
    /// 当前窗口处于新增还是编辑模式。
    mode: ConnectionLinkWindowMode,
    /// 窗口根区域和输入框焦点句柄。
    focus_handles: ConnectionLinkWindowFocusHandles,
    /// 主应用状态订阅，主题切换后窗口跟随刷新。
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
    /// 创建 SSH 链接表单独立窗口。
    ///
    /// 参数说明：
    /// - `app`：主应用实体。
    /// - `theme`：首次绘制使用的主题。
    /// - `form`：按当前选中目录推导出的初始表单。
    /// - `cx`：窗口上下文，用于创建焦点句柄和订阅主应用变化。
    ///
    /// 返回值：可渲染的新增 SSH 链接窗口视图。
    pub fn new(
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
            focus_handles: ConnectionLinkWindowFocusHandles {
                root: cx.focus_handle(),
                name: cx.focus_handle(),
                host: cx.focus_handle(),
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

    /// 判断当前链接窗口是否为指定模式，用于重复点击按钮时决定置前还是替换表单。
    pub fn is_mode(&self, mode: ConnectionLinkWindowMode) -> bool {
        self.mode == mode
    }

    /// 返回当前链接窗口表单协议，用于重复打开不同协议表单时判断是否需要替换。
    pub fn link_kind(&self) -> ConnectionLinkKind {
        self.form.link_kind
    }

    /// 替换链接窗口表单和模式，供右键编辑入口复用已经打开的窗口。
    pub fn replace_form(
        &mut self,
        mut form: ConnectionLinkFormState,
        mode: ConnectionLinkWindowMode,
    ) {
        focus_input_state(&mut form.name_input);
        self.form = form;
        self.mode = mode;
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
    fn input_mut(
        &mut self,
        target: ConnectionFormInputTarget,
    ) -> Option<&mut SettingsTextInputState> {
        match target {
            ConnectionFormInputTarget::LinkName => Some(&mut self.form.name_input),
            ConnectionFormInputTarget::LinkHost => Some(&mut self.form.host_input),
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

    /// 处理链接窗口输入框键盘事件，并返回是否需要提交或关闭窗口。
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
    /// 渲染 SSH 链接表单独立窗口主体。
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        render_link_window_content(
            self.app.clone(),
            cx.entity(),
            self.theme.clone(),
            self.form.clone(),
            self.mode,
            self.focus_handles.clone(),
        )
    }
}

/// 独立窗口表单输入目标，目录和链接窗口共用输入处理函数。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ConnectionFormInputTarget {
    /// 目录名称输入框。
    DirectoryName,
    /// SSH 链接名称输入框。
    LinkName,
    /// SSH 主机输入框。
    LinkHost,
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
    /// 关闭当前窗口。
    Close,
}

/// 渲染当前链接工作区主机指纹确认弹窗。
pub fn render(app: &ArgusApp, cx: &mut Context<ArgusApp>) -> impl IntoElement {
    let theme = app.theme.clone();
    let Some(dialog) = app.connection_dialog.clone() else {
        return div().into_any_element();
    };

    match dialog {
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
        ConnectionDialogState::NewDirectory(_) | ConnectionDialogState::NewSshLink(_) => {
            div().into_any_element()
        }
    }
}

/// 渲染目录表单独立窗口内容。
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
        .bg(rgb(theme.content))
        .font_family(ARGUS_UI_FONT_FAMILY)
        .text_color(rgb(theme.foreground))
        .occlude()
        .focusable()
        .track_focus(&root_focus_for_track)
        .on_click(move |_, window, cx| {
            root_focus_for_click.focus(window);
            let _ = close_window_entity.update(cx, |window_state, state_cx| {
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
                .px_5()
                .pb_5()
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
                .when_some(form.error_message, |this, message| {
                    this.child(error_text(message, &theme))
                })
                .child(render_directory_actions(
                    &theme,
                    &app_handle,
                    &window_entity,
                    mode,
                )),
        )
}

/// 渲染 SSH 链接表单独立窗口内容。
fn render_link_window_content(
    app_handle: Entity<ArgusApp>,
    window_entity: Entity<ConnectionLinkWindow>,
    theme: AppTheme,
    form: ConnectionLinkFormState,
    mode: ConnectionLinkWindowMode,
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
        (ConnectionLinkWindowMode::Edit { .. }, ConnectionLinkKind::Ssh) => {
            ("编辑 SSH 链接", "关闭编辑链接")
        }
        (ConnectionLinkWindowMode::Edit { .. }, ConnectionLinkKind::Smb) => {
            ("编辑 SMB 链接", "关闭编辑链接")
        }
    };
    let port_placeholder = match form.link_kind {
        ConnectionLinkKind::Ssh => "22",
        ConnectionLinkKind::Smb => "445",
    };
    let password_placeholder = match form.link_kind {
        ConnectionLinkKind::Ssh => "可选",
        ConnectionLinkKind::Smb => "必填",
    };

    div()
        .id("connection-link-window-root")
        .size_full()
        .relative()
        .flex()
        .flex_col()
        .bg(rgb(theme.content))
        .font_family(ARGUS_UI_FONT_FAMILY)
        .text_color(rgb(theme.foreground))
        .occlude()
        .focusable()
        .track_focus(&root_focus_for_track)
        .on_click(move |_, window, cx| {
            root_focus_for_click.focus(window);
            let _ = close_window_entity.update(cx, |window_state, state_cx| {
                window_state.clear_input_focuses();
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
                .flex_1()
                .min_h(px(0.0))
                .overflow_y_scroll()
                .scrollbar_width(px(6.0))
                .px_5()
                .pb_5()
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
                .child(render_link_input_row(
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
                .child(render_link_input_row(
                    "密码",
                    ConnectionFormInputTarget::LinkPassword,
                    &form.password_input,
                    focus_handles.password.clone(),
                    password_placeholder,
                    true,
                    &theme,
                    &window_entity,
                    &app_handle,
                ))
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
                .when(form.link_kind == ConnectionLinkKind::Ssh, |this| {
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
                .when_some(form.error_message, |this, message| {
                    this.child(error_text(message, &theme))
                })
                .child(render_link_actions(
                    &theme,
                    &app_handle,
                    &window_entity,
                    mode,
                )),
        )
}

/// 渲染独立窗口标题栏；结构和设置窗口一致，不额外绘制分割线。
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
    input_state: &SettingsTextInputState,
    focus_handle: FocusHandle,
    placeholder: &'static str,
    is_secret: bool,
    theme: &AppTheme,
    window_entity: &Entity<ConnectionDirectoryWindow>,
    app_handle: &Entity<ArgusApp>,
) -> impl IntoElement {
    let native_input = directory_native_input(window_entity.clone(), target, focus_handle.clone());
    let key_window = window_entity.clone();
    let click_window = window_entity.clone();
    let pointer_window = window_entity.clone();
    let submit_window = window_entity.clone();
    let submit_app = app_handle.clone();
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
            },
            theme,
            move |event: &KeyDownEvent, window, cx| {
                cx.stop_propagation();
                let action = key_window.update(cx, |window_state, state_cx| {
                    let action = window_state.handle_input_key(target, &event.keystroke, state_cx);
                    state_cx.notify();
                    action
                });
                handle_directory_window_action(action, &submit_app, &submit_window, window, cx);
                if action == ConnectionWindowInputAction::Close {
                    close_directory_window(&close_app, window, cx);
                }
            },
            move |_, window, cx| {
                cx.stop_propagation();
                focus_handle.focus(window);
                let _ = click_window.update(cx, |window_state, state_cx| {
                    window_state.focus_input(target);
                    state_cx.notify();
                });
            },
            move |event: &InputPointerEvent, _, cx| {
                cx.stop_propagation();
                let _ = pointer_window.update(cx, |window_state, state_cx| {
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

/// 渲染 SSH 链接窗口输入行。
#[allow(clippy::too_many_arguments)]
fn render_link_input_row(
    label: &'static str,
    target: ConnectionFormInputTarget,
    input_state: &SettingsTextInputState,
    focus_handle: FocusHandle,
    placeholder: &'static str,
    is_secret: bool,
    theme: &AppTheme,
    window_entity: &Entity<ConnectionLinkWindow>,
    app_handle: &Entity<ArgusApp>,
) -> impl IntoElement {
    let native_input = link_native_input(window_entity.clone(), target, focus_handle.clone());
    let key_window = window_entity.clone();
    let click_window = window_entity.clone();
    let pointer_window = window_entity.clone();
    let submit_window = window_entity.clone();
    let submit_app = app_handle.clone();
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
            },
            theme,
            move |event: &KeyDownEvent, window, cx| {
                cx.stop_propagation();
                let action = key_window.update(cx, |window_state, state_cx| {
                    let action = window_state.handle_input_key(target, &event.keystroke, state_cx);
                    state_cx.notify();
                    action
                });
                handle_link_window_action(action, &submit_app, &submit_window, window, cx);
                if action == ConnectionWindowInputAction::Close {
                    close_link_window(&close_app, window, cx);
                }
            },
            move |_, window, cx| {
                cx.stop_propagation();
                focus_handle.focus(window);
                let _ = click_window.update(cx, |window_state, state_cx| {
                    window_state.focus_input(target);
                    state_cx.notify();
                });
            },
            move |event: &InputPointerEvent, _, cx| {
                cx.stop_propagation();
                let _ = pointer_window.update(cx, |window_state, state_cx| {
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
        .mt_2()
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
        .mt_2()
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

/// 渲染独立窗口操作按钮。
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
        .hover(|this| this.bg(rgb(theme.selection)))
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
) -> NativeInput {
    NativeInput::new(focus_handle, move |edit, _, cx| {
        let _ = window_entity.update(cx, |window_state, state_cx| {
            window_state.apply_native_edit(target, edit);
            state_cx.notify();
        });
    })
}

/// 创建 SSH 链接窗口的原生文本输入桥。
fn link_native_input(
    window_entity: Entity<ConnectionLinkWindow>,
    target: ConnectionFormInputTarget,
    focus_handle: FocusHandle,
) -> NativeInput {
    NativeInput::new(focus_handle, move |edit, _, cx| {
        let _ = window_entity.update(cx, |window_state, state_cx| {
            window_state.apply_native_edit(target, edit);
            state_cx.notify();
        });
    })
}

/// 根据输入目标返回稳定元素 ID。
fn input_id_for_target(target: ConnectionFormInputTarget) -> &'static str {
    match target {
        ConnectionFormInputTarget::DirectoryName => "connection-directory-name-input",
        ConnectionFormInputTarget::LinkName => "connection-link-name-input",
        ConnectionFormInputTarget::LinkHost => "connection-link-host-input",
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

/// 提交目录窗口表单，成功后关闭窗口，失败则把错误留在当前表单上。
fn submit_directory_window(
    app_handle: &Entity<ArgusApp>,
    window_entity: &Entity<ConnectionDirectoryWindow>,
    window: &mut Window,
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
            window.remove_window();
        }
        Err(message) => set_directory_window_error(window_entity, cx, message),
    }
}

/// 提交 SSH 链接窗口表单，成功后关闭窗口，失败则把错误留在当前表单上。
fn submit_link_window(
    app_handle: &Entity<ArgusApp>,
    window_entity: &Entity<ConnectionLinkWindow>,
    window: &mut Window,
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
            window.remove_window();
        }
        Err(message) => set_link_window_error(window_entity, cx, message),
    }
}

/// 关闭目录窗口并同步主应用打开状态。
fn close_directory_window(app_handle: &Entity<ArgusApp>, window: &mut Window, cx: &mut App) {
    update_connection_app(app_handle, cx, |app, _| {
        app.close_connection_directory_window()
    });
    window.remove_window();
}

/// 关闭 SSH 链接窗口并同步主应用打开状态。
fn close_link_window(app_handle: &Entity<ArgusApp>, window: &mut Window, cx: &mut App) {
    update_connection_app(app_handle, cx, |app, _| app.close_connection_link_window());
    window.remove_window();
}

/// 写入目录窗口校验错误。
fn set_directory_window_error(
    window_entity: &Entity<ConnectionDirectoryWindow>,
    cx: &mut App,
    message: String,
) {
    let _ = window_entity.update(cx, |window_state, state_cx| {
        window_state.form.error_message = Some(message);
        state_cx.notify();
    });
}

/// 写入 SSH 链接窗口校验错误。
fn set_link_window_error(
    window_entity: &Entity<ConnectionLinkWindow>,
    cx: &mut App,
    message: String,
) {
    let _ = window_entity.update(cx, |window_state, state_cx| {
        window_state.form.error_message = Some(message);
        state_cx.notify();
    });
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
                .px_5()
                .pb_5()
                .flex()
                .flex_col()
                .gap_3()
                .text_size(px(13.0))
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
                )
                .child(
                    div()
                        .mt_2()
                        .flex()
                        .justify_end()
                        .gap_2()
                        .child(action_button("拒绝", false, theme, cx, move |app, cx| {
                            app.reject_connection_host_key_prompt(reject_prompt.clone());
                            cx.notify();
                        }))
                        .child(action_button(
                            "信任并继续",
                            true,
                            theme,
                            cx,
                            move |app, cx| {
                                app.confirm_connection_host_key_prompt(confirm_prompt.clone());
                                cx.notify();
                            },
                        )),
                ),
        )
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
        "SSH 链接"
    };
    div()
        .size_full()
        .flex()
        .flex_col()
        .rounded_lg()
        .bg(rgb(theme.content))
        .child(dialog_header("确认删除", ArgusIcon::Close, theme, cx))
        .child(
            div()
                .px_5()
                .pb_5()
                .flex()
                .flex_col()
                .gap_3()
                .text_size(px(13.0))
                .text_color(rgb(theme.foreground))
                .child(format!("确定要删除{node_kind}「{}」吗？", prompt.label))
                .child(
                    div()
                        .text_size(px(12.0))
                        .text_color(rgb(theme.foreground_muted))
                        .child("此操作会立即写入本地配置。"),
                )
                .child(
                    div()
                        .mt_2()
                        .flex()
                        .justify_end()
                        .gap_2()
                        .child(action_button("取消", false, theme, cx, |app, cx| {
                            app.close_connection_dialog();
                            cx.notify();
                        }))
                        .child(action_button(
                            "确认删除",
                            true,
                            theme,
                            cx,
                            move |app, cx| {
                                app.confirm_delete_connection_node(node_id);
                                cx.notify();
                            },
                        )),
                ),
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
    label: &'static str,
    is_primary: bool,
    theme: &AppTheme,
    cx: &mut Context<ArgusApp>,
    on_click: impl Fn(&mut ArgusApp, &mut Context<ArgusApp>) + 'static,
) -> impl IntoElement {
    let background = if is_primary {
        theme.selection
    } else {
        theme.content
    };
    div()
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
        .text_color(rgb(theme.error))
        .child(message)
}

/// 聚焦输入框状态，并把光标移动到文本末尾。
fn focus_input_state(input: &mut SettingsTextInputState) {
    input.is_focused = true;
    input.cursor = character_count(&input.value);
    input.selection_anchor = None;
    input.marked_range = None;
    input.selection_drag = None;
}

/// 清理单个输入框焦点态。
fn clear_input_focus_state(input: &mut SettingsTextInputState) {
    input.is_focused = false;
    input.selection_anchor = None;
    input.marked_range = None;
    input.selection_drag = None;
}

/// 处理输入框剪贴板快捷键（粘贴/复制/剪切/全选），命中时返回对应动作，否则返回 `None`。
///
/// 这些快捷键带有平台修饰键（macOS 为 Cmd，Windows/Linux 为 Ctrl），会被
/// [`handle_text_input_key`] 的字符插入分支跳过，因此在这里单独拦截处理，
/// 和设置窗口、Runtime 过滤输入框的粘贴行为保持一致。
fn handle_text_input_clipboard(
    input: &mut SettingsTextInputState,
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
    input: &mut SettingsTextInputState,
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
    input: &mut SettingsTextInputState,
    character_index: usize,
    granularity: TextSelectionGranularity,
) {
    let range = input_range_for_granularity(input, character_index, granularity);
    input.selection_anchor = Some(range.start);
    input.cursor = range.end;
    input.selection_drag = Some(InputTextSelectionDrag {
        anchor_range: range,
        granularity,
    });
    input.marked_range = None;
}

/// 鼠标拖拽更新输入框选区。
fn update_input_pointer_selection(input: &mut SettingsTextInputState, character_index: usize) {
    let Some(drag) = input.selection_drag.clone() else {
        return;
    };
    let focus_range = input_range_for_granularity(input, character_index, drag.granularity);
    if focus_range.start < drag.anchor_range.start {
        input.selection_anchor = Some(drag.anchor_range.end);
        input.cursor = focus_range.start;
    } else {
        input.selection_anchor = Some(drag.anchor_range.start);
        input.cursor = focus_range.end;
    }
    input.marked_range = None;
}

/// 鼠标结束输入框文本选择。
fn finish_input_pointer_selection(input: &mut SettingsTextInputState) {
    input.selection_drag = None;
    if input_selection_range(input).is_none() {
        input.selection_anchor = None;
    }
}

/// 删除输入框当前选区。
fn delete_input_selection(input: &mut SettingsTextInputState) -> bool {
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
fn insert_input_text(input: &mut SettingsTextInputState, text: &str) {
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
fn delete_input_backward(input: &mut SettingsTextInputState) {
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
fn delete_input_forward(input: &mut SettingsTextInputState) {
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
fn move_input_left(input: &mut SettingsTextInputState, extend_selection: bool) {
    let cursor = input.cursor.saturating_sub(1);
    move_input_cursor(input, cursor, extend_selection);
}

/// 输入框光标右移。
fn move_input_right(input: &mut SettingsTextInputState, extend_selection: bool) {
    let cursor = (input.cursor + 1).min(character_count(&input.value));
    move_input_cursor(input, cursor, extend_selection);
}

/// 移动输入框光标，并按需扩展选区。
fn move_input_cursor(input: &mut SettingsTextInputState, cursor: usize, extend_selection: bool) {
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
fn apply_native_edit_to_input(input: &mut SettingsTextInputState, edit: &NativeTextEdit) {
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
fn input_selection_range(input: &SettingsTextInputState) -> Option<Range<usize>> {
    let anchor = input.selection_anchor?;
    if anchor == input.cursor {
        return None;
    }
    Some(anchor.min(input.cursor)..anchor.max(input.cursor))
}

/// 按鼠标点击粒度生成输入框字符范围。
fn input_range_for_granularity(
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

/// 将字符范围夹在文本长度内，并确保起止顺序稳定。
fn clamp_range(range: Range<usize>, text_length: usize) -> Range<usize> {
    let start = range.start.min(text_length);
    let end = range.end.min(text_length);
    start.min(end)..start.max(end)
}
