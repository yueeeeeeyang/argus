//! 文件职责：渲染自动升级提示弹窗。
//! 创建日期：2026-06-16
//! 修改日期：2026-06-16
//! 作者：Argus 开发团队
//! 主要功能：展示新版本信息、升级日志、下载替换进度和失败原因，并把用户选择转回应用状态。

use crate::app::{ArgusApp, UpgradeDialogState};
use crate::theme::AppTheme;
use crate::ui::components::icon::{ArgusIcon, render_icon};
use crate::ui::components::modal_dialog::{ModalDialog, render_modal_dialog};
use crate::utils::size_format::format_bytes;
use gpui::{AnyElement, App, ClickEvent, Context, IntoElement, Window, div, prelude::*, px, rgb};

/// 升级弹窗宽度。
const UPGRADE_DIALOG_WIDTH: f32 = 560.0;
/// 升级弹窗高度。
const UPGRADE_DIALOG_HEIGHT: f32 = 420.0;

/// 渲染当前升级弹窗。
pub fn render(app: &ArgusApp, cx: &mut Context<ArgusApp>) -> impl IntoElement {
    let theme = app.theme.clone();
    let Some(dialog) = app.upgrade_dialog.clone() else {
        return div().into_any_element();
    };
    let content = match dialog {
        UpgradeDialogState::Available { upgrade } => {
            render_available_upgrade(upgrade, &theme, cx).into_any_element()
        }
        UpgradeDialogState::Progress { version, message } => {
            render_progress(version, message, &theme).into_any_element()
        }
        UpgradeDialogState::Failed { version, message } => {
            render_failure(version, message, &theme, cx).into_any_element()
        }
    };

    render_modal_dialog(
        ModalDialog {
            overlay_id: "upgrade-dialog-overlay",
            container_id: "upgrade-dialog-container",
            width: UPGRADE_DIALOG_WIDTH,
            height: UPGRADE_DIALOG_HEIGHT,
            content,
        },
        theme,
        cx,
    )
    .into_any_element()
}

/// 渲染发现新版本时的确认界面。
fn render_available_upgrade(
    upgrade: crate::updater::AvailableUpgrade,
    theme: &AppTheme,
    cx: &mut Context<ArgusApp>,
) -> impl IntoElement {
    let version = upgrade.version.clone();
    let release_notes = upgrade.release_notes.clone();
    let published_at = upgrade.published_at.clone();
    let size = format_bytes(upgrade.asset.size_bytes);

    div()
        .size_full()
        .flex()
        .flex_col()
        .bg(rgb(theme.side_bar))
        .child(dialog_header("发现新版本", ArgusIcon::Refresh, theme))
        .child(
            div()
                .flex_1()
                .px_4()
                .py_3()
                .flex()
                .flex_col()
                .gap_3()
                .child(
                    div()
                        .flex()
                        .items_center()
                        .justify_between()
                        .child(
                            div()
                                .text_size(px(16.0))
                                .text_color(rgb(theme.foreground))
                                .child(format!("Argus {version}")),
                        )
                        .child(
                            div()
                                .text_size(px(12.0))
                                .text_color(rgb(theme.foreground_muted))
                                .child(size),
                        ),
                )
                .child(
                    div()
                        .text_size(px(12.0))
                        .text_color(rgb(theme.foreground_muted))
                        .child(format!("发布时间：{published_at}")),
                )
                .child(
                    div()
                        .id("upgrade-dialog-release-notes")
                        .flex_1()
                        .overflow_y_scroll()
                        .rounded_sm()
                        .border_1()
                        .border_color(rgb(theme.border))
                        .bg(rgb(theme.content))
                        .p_3()
                        .flex()
                        .flex_col()
                        .gap_1()
                        .children(release_note_lines(&release_notes, theme)),
                ),
        )
        .child(
            div()
                .h(px(56.0))
                .px_4()
                .flex()
                .items_center()
                .justify_between()
                .border_t_1()
                .border_color(rgb(theme.border))
                .child(text_action_button(
                    "upgrade-dialog-later",
                    ArgusIcon::Close,
                    "稍后",
                    theme,
                    cx.listener(|app, _, _, cx| {
                        app.dismiss_upgrade_dialog();
                        cx.notify();
                    }),
                ))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .child(text_action_button(
                            "upgrade-dialog-skip",
                            ArgusIcon::Close,
                            "跳过此版本",
                            theme,
                            cx.listener(|app, _, _, cx| {
                                app.skip_available_upgrade();
                                cx.notify();
                            }),
                        ))
                        .child(primary_action_button(
                            "upgrade-dialog-install",
                            ArgusIcon::Refresh,
                            "立即升级",
                            theme,
                            cx.listener(|app, _, _, cx| {
                                app.install_available_upgrade(cx);
                                cx.notify();
                            }),
                        )),
                ),
        )
}

/// 渲染升级执行中的进度界面。
fn render_progress(version: String, message: String, theme: &AppTheme) -> impl IntoElement {
    div()
        .size_full()
        .flex()
        .flex_col()
        .bg(rgb(theme.side_bar))
        .child(dialog_header("正在升级", ArgusIcon::Refresh, theme))
        .child(
            div()
                .flex_1()
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .gap_3()
                .child(render_icon(
                    ArgusIcon::Refresh,
                    theme.foreground_muted,
                    32.0,
                ))
                .child(
                    div()
                        .text_size(px(15.0))
                        .text_color(rgb(theme.foreground))
                        .child(format!("Argus {version}")),
                )
                .child(
                    div()
                        .text_size(px(12.0))
                        .text_color(rgb(theme.foreground_muted))
                        .child(message),
                ),
        )
}

/// 渲染升级失败界面。
fn render_failure(
    version: Option<String>,
    message: String,
    theme: &AppTheme,
    cx: &mut Context<ArgusApp>,
) -> impl IntoElement {
    let title = version
        .map(|version| format!("版本 {version} 升级失败"))
        .unwrap_or_else(|| "升级检查失败".to_string());

    div()
        .size_full()
        .flex()
        .flex_col()
        .bg(rgb(theme.side_bar))
        .child(dialog_header("升级失败", ArgusIcon::Info, theme))
        .child(
            div()
                .flex_1()
                .px_4()
                .py_3()
                .flex()
                .flex_col()
                .gap_3()
                .child(
                    div()
                        .text_size(px(15.0))
                        .text_color(rgb(theme.foreground))
                        .child(title),
                )
                .child(
                    div()
                        .rounded_sm()
                        .border_1()
                        .border_color(rgb(theme.border))
                        .bg(rgb(theme.content))
                        .p_3()
                        .text_size(px(12.0))
                        .line_height(px(18.0))
                        .text_color(rgb(theme.foreground_muted))
                        .child(message),
                ),
        )
        .child(
            div()
                .h(px(56.0))
                .px_4()
                .flex()
                .items_center()
                .justify_end()
                .border_t_1()
                .border_color(rgb(theme.border))
                .child(primary_action_button(
                    "upgrade-dialog-close-failure",
                    ArgusIcon::Close,
                    "关闭",
                    theme,
                    cx.listener(|app, _, _, cx| {
                        app.dismiss_upgrade_dialog();
                        cx.notify();
                    }),
                )),
        )
}

/// 渲染统一弹窗头部。
fn dialog_header(title: &'static str, icon: ArgusIcon, theme: &AppTheme) -> impl IntoElement {
    div()
        .h(px(52.0))
        .px_4()
        .flex()
        .items_center()
        .gap_2()
        .border_b_1()
        .border_color(rgb(theme.border))
        .child(render_icon(icon, theme.foreground_muted, 17.0))
        .child(
            div()
                .text_size(px(14.0))
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .text_color(rgb(theme.foreground))
                .child(title),
        )
}

/// 把升级日志按行渲染，保留基本换行结构。
fn release_note_lines(release_notes: &str, theme: &AppTheme) -> Vec<AnyElement> {
    let lines = if release_notes.trim().is_empty() {
        vec!["暂无升级日志"]
    } else {
        release_notes.lines().collect::<Vec<_>>()
    };

    lines
        .into_iter()
        .map(|line| {
            div()
                .text_size(px(12.0))
                .line_height(px(18.0))
                .text_color(rgb(theme.foreground_muted))
                .child(line.to_string())
                .into_any_element()
        })
        .collect()
}

/// 渲染普通文本操作按钮。
fn text_action_button(
    id: &'static str,
    icon: ArgusIcon,
    label: &'static str,
    theme: &AppTheme,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    action_button(
        id,
        icon,
        label,
        theme.current_line,
        theme.foreground,
        theme,
        on_click,
    )
}

/// 渲染主操作按钮。
fn primary_action_button(
    id: &'static str,
    icon: ArgusIcon,
    label: &'static str,
    theme: &AppTheme,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    action_button(
        id,
        icon,
        label,
        theme.selection,
        theme.foreground,
        theme,
        on_click,
    )
}

/// 渲染升级弹窗底部操作按钮。
fn action_button(
    id: &'static str,
    icon: ArgusIcon,
    label: &'static str,
    background: u32,
    foreground: u32,
    theme: &AppTheme,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    let hover_background = theme.selection;

    div()
        .id(id)
        .h(px(30.0))
        .px_3()
        .flex()
        .items_center()
        .justify_center()
        .gap_1()
        .rounded_sm()
        .bg(rgb(background))
        .hover(move |this| this.bg(rgb(hover_background)))
        .cursor_pointer()
        .text_size(px(12.0))
        .text_color(rgb(foreground))
        .child(render_icon(icon, foreground, 13.0))
        .child(label)
        .on_click(on_click)
}
