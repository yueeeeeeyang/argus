//! 文件职责：渲染日志分析工作区的来源侧栏。
//! 创建日期：2026-06-09
//! 修改日期：2026-06-10
//! 作者：Argus 开发团队
//! 主要功能：展示局部工具区和来源目录树，不访问真实文件系统。

use crate::app::{ArgusApp, Workspace};
use crate::ui::{connection_tree_panel, dir_tree_panel, toolbar};
use gpui::{Context, IntoElement, div, prelude::*, px, rgb};

/// 渲染来源侧栏。
///
/// 参数说明：
/// - `app`：应用状态，提供主题和来源树占位数据。
/// - `cx`：应用上下文，用于局部工具栏占位按钮回调。
///
/// 返回值：GPUI 元素树；当前只展示固定来源数据。
pub fn render(app: &ArgusApp, cx: &mut Context<ArgusApp>) -> impl IntoElement {
    let theme = app.theme.clone();

    div()
        .id("argus-source-panel")
        .w(px(app.current_source_panel_width()))
        .h_full()
        .flex()
        .flex_none()
        .flex_col()
        .bg(rgb(theme.side_bar))
        .on_click(cx.listener(|app, _, _, cx| {
            app.clear_log_text_focus();
            cx.notify();
        }))
        .child(
            div()
                .h(px(40.0))
                .flex()
                .items_center()
                .justify_center()
                .px_3()
                .child(if app.workspace == Workspace::Connections {
                    toolbar::render_connection_toolbar(app, cx)
                } else {
                    toolbar::render_source_toolbar(app, cx)
                }),
        )
        .child(if app.workspace == Workspace::Connections {
            connection_tree_panel::render(app, cx).into_any_element()
        } else {
            dir_tree_panel::render(app, cx).into_any_element()
        })
}
