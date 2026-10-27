//! 文件职责：渲染来源侧栏与主内容区之间的可拖拽分割线。
//! 创建日期：2026-06-10
//! 修改日期：2026-10-27
//! 作者：Argus 开发团队
//! 主要功能：沿内容玻璃板左缘提供不占布局宽度的透明拖拽命中区域。

use crate::app::ArgusApp;
use crate::ui::custom_title_bar::TITLE_BAR_HEIGHT;
use crate::ui::main_window::WINDOW_CONTENT_INSET;
use gpui::{Context, CursorStyle, IntoElement, MouseButton, MouseDownEvent, div, prelude::*, px};

/// 分割线拖拽命中区宽度（像素）；命中区透明，只承载悬停光标与拖拽。
const SOURCE_RESIZER_HIT_WIDTH: f32 = 6.0;

/// 渲染来源侧栏分割线。
///
/// 参数说明：
/// - `app`：应用状态，提供主题与拖拽状态。
/// - `id`：当前分割线实例 ID。
/// - `cx`：应用上下文，用于启动本地拖拽状态。
///
/// 返回值：GPUI 元素树；拖拽只调整本地 UI 宽度，不持久化配置。
///
/// 定位说明：侧栏背景色与窗口背景色相同，侧栏与内容区之间视觉上唯一的分界线是
/// 内容玻璃板的左侧边框（距侧栏右缘 `WINDOW_CONTENT_INSET`）。命中区必须以这条
/// 边框为中心，否则用户得把鼠标移到边框左侧的空白带里才能拖动。命中区本身透明，
/// 只调整位置不会改变任何外观。
///
/// 纵向范围与可见边框一致：标题栏内没有这条边框，命中区不覆盖标题栏，避免与标签
/// 点击区和窗口拖拽区抢事件。
pub(crate) fn render(
    app: &ArgusApp,
    id: &'static str,
    cx: &mut Context<ArgusApp>,
) -> impl IntoElement {
    div()
        .id(id)
        .debug_selector(|| "source-resizer-hit-area".to_string())
        .absolute()
        .top(px(TITLE_BAR_HEIGHT))
        .bottom_0()
        .left(px(app.current_source_panel_width() + WINDOW_CONTENT_INSET
            - SOURCE_RESIZER_HIT_WIDTH / 2.0))
        .w(px(SOURCE_RESIZER_HIT_WIDTH))
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
