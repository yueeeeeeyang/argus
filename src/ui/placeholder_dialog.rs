//! 文件职责：渲染窗口级占位弹窗。
//! 创建日期：2026-06-09
//! 修改日期：2026-06-10
//! 作者：Argus 开发团队
//! 主要功能：提供打开来源等真实弹窗交互外观，但不调用系统文件选择器或业务能力。

use crate::app::{ArgusApp, PlaceholderSourceKind};
use crate::ui::components::icon::{ArgusIcon, render_icon};
use crate::ui::components::icon_button::{IconButtonSize, render_icon_button};
use gpui::{Context, IntoElement, SharedString, div, prelude::*, px, rgb};

/// 渲染当前活动占位弹窗。
///
/// 参数说明：
/// - `app`：应用状态，提供选中的占位来源类型。
/// - `cx`：应用上下文，用于更新弹窗内本地状态。
///
/// 返回值：覆盖全窗口的 GPUI 元素树；不执行真实文件选择。
pub fn render(app: &ArgusApp, cx: &mut Context<ArgusApp>) -> impl IntoElement {
    let theme = app.theme.clone();

    div()
        .size_full()
        .absolute()
        .top_0()
        .left_0()
        .flex()
        .items_center()
        .justify_center()
        .bg(rgb(0x101010))
        .opacity(0.98)
        .child(
            div()
                .w(px(420.0))
                .rounded_sm()
                .border_1()
                .border_color(rgb(theme.border))
                .bg(rgb(theme.side_bar))
                .shadow_lg()
                .child(
                    div()
                        .h(px(44.0))
                        .px_3()
                        .flex()
                        .items_center()
                        .justify_between()
                        .border_b_1()
                        .border_color(rgb(theme.border))
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap_2()
                                .child(render_icon(ArgusIcon::Open, theme.foreground, 18.0))
                                .child("打开来源"),
                        )
                        .child(render_icon_button(
                            "dialog-close",
                            ArgusIcon::Close,
                            "关闭弹窗",
                            false,
                            IconButtonSize::Small,
                            &theme,
                            cx.listener(|app, _, _, cx| {
                                app.close_dialog();
                                cx.notify();
                            }),
                        )),
                )
                .child(
                    div()
                        .p_4()
                        .flex()
                        .flex_col()
                        .gap_3()
                        .child(source_option(
                            PlaceholderSourceKind::File,
                            ArgusIcon::FileText,
                            "选择日志文件",
                            "占位展示文件打开入口，不调用系统文件面板。",
                            app,
                            cx,
                        ))
                        .child(source_option(
                            PlaceholderSourceKind::Directory,
                            ArgusIcon::FolderOpen,
                            "选择目录",
                            "占位展示目录扫描入口，不访问真实文件系统。",
                            app,
                            cx,
                        ))
                        .child(source_option(
                            PlaceholderSourceKind::Archive,
                            ArgusIcon::Archive,
                            "选择压缩包",
                            "占位展示压缩包入口，不解压或枚举条目。",
                            app,
                            cx,
                        ))
                        .child(
                            div()
                                .id("dialog-confirm")
                                .mt_2()
                                .h(px(34.0))
                                .flex()
                                .items_center()
                                .justify_center()
                                .rounded_sm()
                                .border_1()
                                .border_color(rgb(theme.border))
                                .text_sm()
                                .text_color(rgb(theme.foreground))
                                .cursor_pointer()
                                .child("确认占位选择")
                                .on_click(cx.listener(|app, _, _, cx| {
                                    app.mark_placeholder_action("打开来源确认");
                                    app.close_dialog();
                                    cx.notify();
                                })),
                        ),
                ),
        )
}

/// 渲染打开来源弹窗中的单个来源类型选项。
fn source_option(
    source_kind: PlaceholderSourceKind,
    icon: ArgusIcon,
    title: &'static str,
    description: &'static str,
    app: &ArgusApp,
    cx: &mut Context<ArgusApp>,
) -> impl IntoElement {
    let theme = app.theme.clone();
    let is_selected = app.selected_placeholder_source == source_kind;
    let background = if is_selected {
        theme.selection
    } else {
        theme.content
    };

    div()
        .id(SharedString::from(format!("source-kind-{source_kind:?}")))
        .p_3()
        .flex()
        .items_center()
        .gap_3()
        .rounded_sm()
        .border_1()
        .border_color(rgb(theme.border))
        .bg(rgb(background))
        .cursor_pointer()
        .child(render_icon(icon, theme.foreground, 20.0))
        .child(
            div()
                .flex_1()
                .flex()
                .flex_col()
                .gap_1()
                .child(
                    div()
                        .text_sm()
                        .text_color(rgb(theme.foreground))
                        .child(title),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(rgb(theme.foreground_muted))
                        .child(description),
                ),
        )
        .on_click(cx.listener(move |app, _, _, cx| {
            app.select_placeholder_source(source_kind);
            cx.notify();
        }))
}
