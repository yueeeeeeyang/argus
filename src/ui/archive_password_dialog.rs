//! 文件职责：渲染压缩包密码输入弹窗。
//! 创建日期：2026-07-08
//! 修改日期：2026-07-08
//! 作者：Argus 开发团队
//! 主要功能：在用户主动打开或展开加密压缩包时收集密码，并触发原操作重试。

use crate::app::{AppTextInputTarget, ArgusApp};
use crate::loader::archive::ArchivePasswordErrorKind;
use crate::theme::AppTheme;
use crate::ui::components::icon::{ArgusIcon, render_icon};
use crate::ui::components::icon_button::{IconButtonSize, render_icon_button};
use crate::ui::components::input::{
    Input, InputPointerAction, InputPointerEvent, InputSize, render_input,
};
use crate::ui::components::modal_dialog::{ModalDialog, render_modal_dialog};
use crate::ui::input_native::app_native_input;
use gpui::{
    AnyElement, Context, IntoElement, KeyDownEvent, MouseButton, MouseDownEvent, div, prelude::*,
    px, rgb,
};

/// 渲染当前压缩包密码输入弹窗。
pub(crate) fn render(app: &ArgusApp, cx: &mut Context<ArgusApp>) -> impl IntoElement {
    let theme = app.theme.clone();
    render_modal_dialog(
        ModalDialog {
            overlay_id: "archive-password-dialog-overlay",
            container_id: "archive-password-dialog-container",
            width: 460.0,
            height: 260.0,
            content: render_dialog_content(app, &theme, cx).into_any_element(),
        },
        theme,
        cx,
    )
}

/// 渲染弹窗主体内容。
fn render_dialog_content(
    app: &ArgusApp,
    theme: &AppTheme,
    cx: &mut Context<ArgusApp>,
) -> AnyElement {
    let Some(prompt) = app.archive_password_prompt.clone() else {
        return div().into_any_element();
    };
    let target = AppTextInputTarget::ArchivePassword;
    let app_entity = cx.entity();
    let focus_handle = app
        .input_focus_handles
        .as_ref()
        .map(|handles| handles.archive_password.clone());
    let native_input = focus_handle
        .clone()
        .map(|focus_handle| app_native_input(app_entity.clone(), target, focus_handle));
    let input_state = prompt.input.clone();
    let source_label = prompt.error.source_label.clone();
    let description = match prompt.error.kind {
        ArchivePasswordErrorKind::Required => "该压缩包已加密，需要输入密码后继续。",
        ArchivePasswordErrorKind::Invalid => "上一次密码无法解锁该压缩包，请重新输入。",
        ArchivePasswordErrorKind::Unsupported => "该压缩包使用了暂不支持的加密方式。",
    };
    let key_app_entity = app_entity.clone();
    let click_app_entity = app_entity.clone();
    let pointer_app_entity = app_entity.clone();

    div()
        .size_full()
        .flex()
        .flex_col()
        .rounded_lg()
        .bg(rgb(theme.content))
        .child(dialog_header(theme, cx))
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
                        .child(description),
                )
                .child(
                    div()
                        .text_size(px(12.0))
                        .text_color(rgb(theme.foreground_muted))
                        .line_height(px(18.0))
                        .child(source_label),
                )
                .child(render_input(
                    Input {
                        id: "archive-password-input",
                        placeholder: "输入压缩包密码",
                        value: input_state.value.clone(),
                        is_disabled: false,
                        is_focused: input_state.is_focused,
                        cursor_index: input_state.cursor,
                        selection_range: app.archive_password_input_selection_range(),
                        marked_range: input_state.marked_range.clone(),
                        is_pointer_selecting: input_state.selection_drag.is_some(),
                        is_secret: true,
                        size: InputSize::Regular,
                        leading_accessory: None,
                        trailing_accessory: None,
                        native_input,
                    },
                    theme,
                    move |event: &KeyDownEvent, _, cx| {
                        cx.stop_propagation();
                        key_app_entity.update(cx, |app, app_cx| {
                            app.handle_archive_password_key(&event.keystroke, app_cx);
                            app_cx.notify();
                        });
                    },
                    move |_, window, cx| {
                        cx.stop_propagation();
                        if let Some(focus_handle) = focus_handle.as_ref() {
                            focus_handle.focus(window);
                        }
                        click_app_entity.update(cx, |app, app_cx| {
                            app.focus_archive_password_input();
                            app_cx.notify();
                        });
                    },
                    move |event: &InputPointerEvent, _, cx| {
                        cx.stop_propagation();
                        pointer_app_entity.update(cx, |app, app_cx| {
                            match event.action {
                                InputPointerAction::Begin => app
                                    .begin_archive_password_pointer_selection(
                                        event.character_index,
                                        event.granularity,
                                    ),
                                InputPointerAction::Extend => app
                                    .update_archive_password_pointer_selection(
                                        event.character_index,
                                    ),
                                InputPointerAction::Finish => {
                                    app.finish_archive_password_pointer_selection()
                                }
                            }
                            app_cx.notify();
                        });
                    },
                    move |_, _, _| {},
                ))
                .when_some(prompt.message, |this, message| {
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
                            app.cancel_archive_password_prompt();
                            cx.notify();
                        }))
                        .child(text_button("继续", true, theme, cx, |app, cx| {
                            app.submit_archive_password_prompt(cx);
                            cx.notify();
                        })),
                ),
        )
        .into_any_element()
}

/// 渲染弹窗标题栏。
fn dialog_header(theme: &AppTheme, cx: &mut Context<ArgusApp>) -> impl IntoElement {
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
                .child(render_icon(ArgusIcon::Key, theme.foreground, 18.0))
                .child(
                    div()
                        .text_size(px(16.0))
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .text_color(rgb(theme.foreground))
                        .child("输入压缩包密码"),
                ),
        )
        .child(render_icon_button(
            "archive-password-close",
            ArgusIcon::Close,
            "关闭密码弹窗",
            false,
            IconButtonSize::Small,
            theme,
            cx.listener(|app, _, _, cx| {
                app.cancel_archive_password_prompt();
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
    let hover_background = theme.selection;

    div()
        .h(px(30.0))
        .px_4()
        .flex()
        .items_center()
        .justify_center()
        .rounded_sm()
        .border_1()
        .border_color(rgb(theme.border))
        .bg(rgb(background))
        .hover(move |this| this.bg(rgb(hover_background)))
        .text_size(px(13.0))
        .text_color(rgb(theme.foreground))
        .cursor_pointer()
        .child(label)
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(move |app, _: &MouseDownEvent, _, cx| {
                cx.stop_propagation();
                on_click(app, cx);
            }),
        )
}
