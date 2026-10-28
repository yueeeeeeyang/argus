//! 文件职责：渲染日志分析工作区的来源侧栏。
//! 创建日期：2026-06-09
//! 修改日期：2026-10-28
//! 作者：Argus 开发团队
//! 主要功能：展示局部工具区和来源目录树，不访问真实文件系统。

use crate::app::{ArgusApp, Workspace};
use crate::theme::AppTheme;
use crate::ui::components::icon::{ArgusIcon, render_icon};
use crate::ui::components::icon_button::{IconButtonSize, render_icon_button};
use crate::ui::{connection_tree_panel, dir_tree_panel, toolbar};
use gpui::{Context, IntoElement, div, prelude::*, px, rgb};

/// 渲染来源侧栏。
///
/// 参数说明：
/// - `app`：应用状态，提供主题和来源树占位数据。
/// - `cx`：应用上下文，用于局部工具栏占位按钮回调。
///
/// 返回值：GPUI 元素树；当前只展示固定来源数据。
pub(crate) fn render(app: &ArgusApp, cx: &mut Context<ArgusApp>) -> impl IntoElement {
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
        // 欢迎态（无标签、侧栏收起）的加载提示由内容区欢迎面板展示，避免重复。
        .when(!app.is_log_welcome(), |this| {
            this.when_some(app.source_load_notice.clone(), |this, notice| {
                this.child(render_source_load_notice(notice, &theme, cx))
            })
        })
}

/// 渲染来源加载提示条。
///
/// 加载失败或部分文件被跳过时展示具体原因，避免用户只看到“暂无日志来源”而误以为
/// 没有任何响应；提示条可手动关闭，新的加载请求开始时由应用状态清空。
fn render_source_load_notice(
    notice: String,
    theme: &AppTheme,
    cx: &mut Context<ArgusApp>,
) -> impl IntoElement + use<> {
    let close_entity = cx.entity();
    div()
        .debug_selector(|| "source-load-notice".to_string())
        .flex_none()
        .max_h(px(96.0))
        .px_3()
        .py_2()
        .flex()
        .items_start()
        .gap_2()
        .overflow_hidden()
        .border_t_1()
        .border_color(rgb(theme.border))
        .text_size(px(11.0))
        .line_height(px(16.0))
        .text_color(rgb(theme.foreground_muted))
        .child(render_icon(ArgusIcon::Info, theme.warning, 13.0))
        .child(div().flex_1().min_w(px(0.0)).child(notice))
        .child(render_icon_button(
            "source-load-notice-close",
            ArgusIcon::Close,
            "关闭来源加载提示",
            false,
            IconButtonSize::Tiny,
            theme,
            move |_, _, app_cx| {
                app_cx.stop_propagation();
                close_entity.update(app_cx, |app, cx| {
                    app.source_load_notice = None;
                    cx.notify();
                });
            },
        ))
}
