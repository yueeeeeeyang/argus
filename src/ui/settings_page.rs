//! 文件职责：保留旧设置标签页的兼容渲染入口。
//! 创建日期：2026-06-10
//! 修改日期：2026-07-14
//! 作者：Argus 开发团队
//! 主要功能：提示设置页已迁移到主窗口模态框，避免历史标签状态导致空白内容。

use crate::app::ArgusApp;
use crate::ui::components::icon::{ArgusIcon, render_icon};
use gpui::{Context, IntoElement, div, prelude::*, px, rgb};

/// 渲染旧设置标签页兼容内容。
///
/// 参数说明：
/// - `app`：应用状态，提供当前主题。
/// - `_cx`：保留上下文参数以兼容旧调用签名。
///
/// 返回值：只读提示元素；真实设置从标题栏设置按钮打开模态框。
pub(crate) fn render(app: &ArgusApp, _cx: &mut Context<ArgusApp>) -> impl IntoElement {
    let theme = app.theme.clone();

    div()
        .size_full()
        .flex()
        .items_center()
        .justify_center()
        .bg(rgb(theme.content))
        .child(
            div()
                .flex()
                .items_center()
                .gap_2()
                .text_size(px(13.0))
                .text_color(rgb(theme.foreground_muted))
                .child(render_icon(
                    ArgusIcon::Settings,
                    theme.foreground_muted,
                    16.0,
                ))
                .child("设置已迁移到模态框，请点击标题栏设置按钮打开。"),
        )
}
