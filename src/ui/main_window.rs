//! 文件职责：组合 Argus 主窗口的整体布局。
//! 创建日期：2026-06-09
//! 修改日期：2026-06-16
//! 作者：Argus 开发团队
//! 主要功能：渲染自定义标题栏、来源侧栏、日志内容区、升级弹窗和设置页占位界面。

use crate::app::ArgusApp;
use crate::fonts::ARGUS_UI_FONT_FAMILY;
use crate::ui::{
    components::context_menu, custom_title_bar, log_content_view, placeholder_dialog, source_panel,
    source_resizer, upgrade_dialog,
};
use gpui::{
    Animation, AnimationExt, AnyElement, Context, IntoElement, MouseButton, MouseMoveEvent,
    MouseUpEvent, Window, div, prelude::*, px, rgb,
};
use std::time::Duration;

/// 渲染 Argus 根布局。
///
/// 参数说明：
/// - `app`：应用状态，包含当前工作区和占位数据。
/// - `window`：GPUI 窗口对象，用于自定义窗口按钮。
/// - `cx`：应用上下文，用于为子组件创建状态更新回调。
///
/// 返回值：GPUI 元素树；当前不会抛出业务异常。
pub fn render(
    app: &mut ArgusApp,
    window: &mut Window,
    cx: &mut Context<ArgusApp>,
) -> impl IntoElement {
    app.sync_window_appearance_theme(window);
    let theme = app.theme.clone();

    div()
        .id("argus-root")
        .relative()
        .size_full()
        .flex()
        .flex_col()
        .bg(rgb(theme.background))
        .font_family(ARGUS_UI_FONT_FAMILY)
        .text_color(rgb(theme.foreground))
        .on_mouse_move(cx.listener(|app, event: &MouseMoveEvent, _window, cx| {
            let pointer_x = event.position.x / px(1.0);
            if app.is_source_panel_resizing && app.resize_source_panel(pointer_x) {
                cx.notify();
            }
        }))
        .on_mouse_up(
            MouseButton::Left,
            cx.listener(|app, _: &MouseUpEvent, _, cx| {
                if app.finish_source_panel_resize() {
                    cx.notify();
                }
            }),
        )
        .on_mouse_up_out(
            MouseButton::Left,
            cx.listener(|app, _: &MouseUpEvent, _, cx| {
                if app.finish_source_panel_resize() {
                    cx.notify();
                }
            }),
        )
        .child(custom_title_bar::render(app, window, cx))
        .child(
            div()
                .flex()
                .flex_1()
                .overflow_hidden()
                .bg(rgb(theme.side_bar))
                .child(animated_source_panel(app, cx))
                .child(log_content_view::render(app, cx)),
        )
        .when(!app.is_source_panel_collapsed, |this| {
            this.child(source_resizer::render(app, "source-resizer", cx))
        })
        .when(app.active_dialog.is_some(), |this| {
            this.child(placeholder_dialog::render(app, cx))
        })
        .when(app.upgrade_dialog.is_some(), |this| {
            this.child(upgrade_dialog::render(app, cx))
        })
        .when(app.active_menu.is_some(), |this| {
            this.child(context_menu::render_active_menu(app, cx))
        })
}

/// 渲染可动画宽度的来源侧栏容器；内容保持原宽度，外层负责裁剪。
fn animated_source_panel(app: &ArgusApp, cx: &mut Context<ArgusApp>) -> AnyElement {
    let from_width = app.source_panel_animation_from_width;
    let to_width = app.source_panel_animation_to_width;
    let panel = div()
        .id("animated-source-panel")
        .h_full()
        .flex_none()
        .overflow_hidden()
        .child(source_panel::render(app, cx));

    if app.is_source_panel_resizing {
        return panel
            .w(px(app.source_panel_width.max(0.0)))
            .opacity(1.0)
            .into_any_element();
    }

    panel
        .with_animation(
            ("source-panel-width", app.source_panel_animation_generation),
            Animation::new(Duration::from_millis(170)).with_easing(gpui::ease_out_quint()),
            move |this, progress| {
                let width = from_width + (to_width - from_width) * progress;
                this.w(px(width.max(0.0))).opacity(if to_width == 0.0 {
                    1.0 - progress * 0.12
                } else {
                    0.88 + progress * 0.12
                })
            },
        )
        .into_any_element()
}
