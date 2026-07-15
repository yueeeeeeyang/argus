//! 文件职责：渲染 SFTP 文件管理相关的应用内模态弹窗。
//! 创建日期：2026-06-26
//! 修改日期：2026-06-26
//! 作者：Argus 开发团队
//! 主要功能：提供远程文件重命名和删除二次确认交互。

use gpui::{
    Context, FontWeight, IntoElement, KeyDownEvent, MouseButton, MouseDownEvent, div, prelude::*,
    px, rgb,
};

use crate::app::{AppTextInputTarget, ArgusApp, SftpDeletePromptState, SftpDialogState};
use crate::theme::AppTheme;
use crate::ui::components::icon::{ArgusIcon, render_icon};
use crate::ui::components::icon_button::{IconButtonSize, render_icon_button};
use crate::ui::components::input::{
    Input, InputPointerAction, InputPointerEvent, InputSize, render_input,
};
use crate::ui::components::modal_dialog::{ModalDialog, render_modal_dialog};
use crate::ui::input_native::app_native_input;

/// 重命名弹窗宽度。
const SFTP_RENAME_DIALOG_WIDTH: f32 = 560.0;
/// 重命名弹窗高度。
const SFTP_RENAME_DIALOG_HEIGHT: f32 = 214.0;
/// 删除确认弹窗宽度。
const SFTP_DELETE_DIALOG_WIDTH: f32 = 500.0;
/// 删除确认弹窗高度。
const SFTP_DELETE_DIALOG_HEIGHT: f32 = 188.0;

/// 渲染当前 SFTP 文件管理弹窗。
pub(crate) fn render(app: &ArgusApp, cx: &mut Context<ArgusApp>) -> impl IntoElement {
    let theme = app.theme.clone();
    let Some(dialog) = app.sftp_dialog.clone() else {
        return div().into_any_element();
    };

    match dialog {
        SftpDialogState::Rename(dialog) => render_modal_dialog(
            ModalDialog {
                overlay_id: "sftp-rename-dialog-overlay",
                container_id: "sftp-rename-dialog-container",
                width: SFTP_RENAME_DIALOG_WIDTH,
                height: SFTP_RENAME_DIALOG_HEIGHT,
                content: render_rename_dialog(app, dialog, &theme, cx).into_any_element(),
            },
            theme,
            cx,
        )
        .into_any_element(),
        SftpDialogState::ConfirmDelete(prompt) => render_modal_dialog(
            ModalDialog {
                overlay_id: "sftp-delete-dialog-overlay",
                container_id: "sftp-delete-dialog-container",
                width: SFTP_DELETE_DIALOG_WIDTH,
                height: SFTP_DELETE_DIALOG_HEIGHT,
                content: render_delete_dialog(prompt, &theme, cx).into_any_element(),
            },
            theme,
            cx,
        )
        .into_any_element(),
    }
}

/// 渲染重命名弹窗。
fn render_rename_dialog(
    app: &ArgusApp,
    dialog: crate::app::SftpRenameDialogState,
    theme: &AppTheme,
    cx: &mut Context<ArgusApp>,
) -> impl IntoElement {
    let target = AppTextInputTarget::SftpRenameName;
    let app_entity = cx.entity();
    let focus_handle = app
        .input_focus_handles
        .as_ref()
        .map(|handles| handles.sftp_rename_name.clone());
    let native_input = focus_handle
        .clone()
        .map(|focus_handle| app_native_input(app_entity.clone(), target, focus_handle));
    let input_state = dialog.name_input.clone();
    let key_app_entity = app_entity.clone();
    let click_app_entity = app_entity.clone();
    let pointer_app_entity = app_entity.clone();

    div()
        .size_full()
        .flex()
        .flex_col()
        .rounded_lg()
        .bg(rgb(theme.content))
        .child(dialog_header(
            "重命名",
            ArgusIcon::Rename,
            "关闭重命名",
            theme,
            cx,
        ))
        .child(
            div()
                .px_5()
                .pb_4()
                .flex()
                .flex_col()
                .gap_3()
                .child(
                    div()
                        .text_size(px(12.0))
                        .text_color(rgb(theme.foreground_muted))
                        .child(format!("原名称：{}", dialog.original_name)),
                )
                .child(render_input(
                    Input {
                        id: "sftp-rename-name-input",
                        placeholder: "输入新名称",
                        value: input_state.value.clone(),
                        is_disabled: false,
                        is_focused: input_state.is_focused,
                        cursor_index: input_state.cursor,
                        selection_range: app.sftp_input_selection_range(target),
                        marked_range: input_state.marked_range.clone(),
                        is_pointer_selecting: input_state.selection_drag.is_some(),
                        is_secret: false,
                        size: InputSize::Regular,
                        leading_accessory: None,
                        trailing_accessory: None,
                        native_input,
                    },
                    theme,
                    move |event: &KeyDownEvent, _, cx| {
                        cx.stop_propagation();
                        key_app_entity.update(cx, |app, app_cx| {
                            app.handle_sftp_text_input_key(target, &event.keystroke);
                            app_cx.notify();
                        });
                    },
                    move |_, window, cx| {
                        cx.stop_propagation();
                        if let Some(focus_handle) = focus_handle.as_ref() {
                            focus_handle.focus(window);
                        }
                        click_app_entity.update(cx, |app, app_cx| {
                            app.focus_sftp_text_input_target(target);
                            app_cx.notify();
                        });
                    },
                    move |event: &InputPointerEvent, _, cx| {
                        cx.stop_propagation();
                        pointer_app_entity.update(cx, |app, app_cx| {
                            match event.action {
                                InputPointerAction::Begin => app
                                    .begin_sftp_input_pointer_selection(
                                        target,
                                        event.character_index,
                                        event.granularity,
                                    ),
                                InputPointerAction::Extend => app
                                    .update_sftp_input_pointer_selection(
                                        target,
                                        event.character_index,
                                    ),
                                InputPointerAction::Finish => {
                                    app.finish_sftp_input_pointer_selection(target)
                                }
                            }
                            app_cx.notify();
                        });
                    },
                    move |_, _, _| {},
                ))
                .when_some(dialog.error_message, |this, message| {
                    this.child(
                        div()
                            .text_size(px(12.0))
                            .text_color(rgb(theme.error))
                            .child(message),
                    )
                })
                .child(
                    div()
                        .mt_1()
                        .flex()
                        .justify_end()
                        .gap_2()
                        .child(text_button("取消", false, theme, cx, |app, cx| {
                            app.close_sftp_dialog();
                            cx.notify();
                        }))
                        .child(text_button("保存", true, theme, cx, |app, cx| {
                            app.submit_sftp_dialog();
                            cx.notify();
                        })),
                ),
        )
}

/// 渲染删除确认弹窗。
fn render_delete_dialog(
    prompt: SftpDeletePromptState,
    theme: &AppTheme,
    cx: &mut Context<ArgusApp>,
) -> impl IntoElement {
    let description = if prompt.is_directory {
        format!(
            "确定删除空目录「{}」吗？非空目录会被服务器拒绝。",
            prompt.name
        )
    } else {
        format!("确定删除文件「{}」吗？此操作不可撤销。", prompt.name)
    };
    let confirm_prompt = prompt.clone();

    div()
        .size_full()
        .flex()
        .flex_col()
        .rounded_lg()
        .bg(rgb(theme.content))
        .child(dialog_header(
            "删除确认",
            ArgusIcon::Trash,
            "关闭删除确认",
            theme,
            cx,
        ))
        .child(
            div()
                .px_5()
                .pb_4()
                .flex()
                .flex_col()
                .gap_4()
                .text_size(px(13.0))
                .text_color(rgb(theme.foreground))
                .child(description)
                .child(
                    div()
                        .flex()
                        .justify_end()
                        .gap_2()
                        .child(text_button("取消", false, theme, cx, |app, cx| {
                            app.close_sftp_dialog();
                            cx.notify();
                        }))
                        .child(text_button("删除", true, theme, cx, move |app, cx| {
                            app.confirm_delete_sftp_entry(confirm_prompt.clone());
                            cx.notify();
                        })),
                ),
        )
}

/// 渲染弹窗标题栏。
fn dialog_header(
    title: &'static str,
    icon: ArgusIcon,
    close_tooltip: &'static str,
    theme: &AppTheme,
    cx: &mut Context<ArgusApp>,
) -> impl IntoElement {
    div()
        .h(px(58.0))
        .px_5()
        .flex()
        .items_center()
        .justify_between()
        .child(
            div()
                .flex()
                .items_center()
                .gap_2()
                .text_size(px(20.0))
                .font_weight(FontWeight::MEDIUM)
                .text_color(rgb(theme.foreground))
                .child(render_icon(icon, theme.foreground_muted, 20.0))
                .child(title),
        )
        .child(render_icon_button(
            "sftp-dialog-close",
            ArgusIcon::Close,
            close_tooltip,
            false,
            IconButtonSize::Small,
            theme,
            cx.listener(|app, _, _, cx| {
                app.close_sftp_dialog();
                cx.notify();
            }),
        ))
}

/// 渲染弹窗底部文字按钮。
fn text_button(
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
    let border = theme.border;
    let hover_background = theme.selection;

    div()
        .h(px(32.0))
        .px_4()
        .flex()
        .items_center()
        .justify_center()
        .rounded_sm()
        .border_1()
        .border_color(rgb(border))
        .bg(rgb(background))
        .hover(move |this| this.bg(rgb(hover_background)))
        .cursor_pointer()
        .text_size(px(13.0))
        .text_color(rgb(theme.foreground))
        .child(label)
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(move |app, _: &MouseDownEvent, _, cx| {
                cx.stop_propagation();
                on_click(app, cx);
            }),
        )
}
