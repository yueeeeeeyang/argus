//! 文件职责：渲染来源侧栏与主内容区之间的可拖拽分割线。
//! 创建日期：2026-06-10
//! 修改日期：2026-06-10
//! 作者：Argus 开发团队
//! 主要功能：提供不占布局宽度的透明拖拽命中区域，视觉上由左右面板背景色区分。

use crate::app::ArgusApp;
use gpui::{Context, CursorStyle, IntoElement, MouseButton, MouseDownEvent, div, prelude::*, px};

/// 渲染来源侧栏分割线。
///
/// 参数说明：
/// - `app`：应用状态，提供主题与拖拽状态。
/// - `id`：当前分割线实例 ID，标题栏和内容区需要分别传入稳定 ID。
/// - `cx`：应用上下文，用于启动本地拖拽状态。
///
/// 返回值：GPUI 元素树；拖拽只调整本地 UI 宽度，不持久化配置。
pub fn render(app: &ArgusApp, id: &'static str, cx: &mut Context<ArgusApp>) -> impl IntoElement {
    div()
        .id(id)
        .absolute()
        .top_0()
        .left(px(app.current_source_panel_width() - 3.0))
        .w(px(6.0))
        .h_full()
        .flex()
        .items_center()
        .justify_center()
        .cursor(CursorStyle::ResizeColumn)
        .on_hover(cx.listener(|app, is_hovered: &bool, _, cx| {
            if app.set_source_resizer_hovered(*is_hovered) {
                cx.notify();
            }
        }))
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(|app, event: &MouseDownEvent, _, cx| {
                cx.stop_propagation();
                app.begin_source_panel_resize(event.position.x / px(1.0));
                cx.notify();
            }),
        )
}
