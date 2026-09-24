//! 文件职责：提供窗口内通用模态遮罩与容器组件。
//! 创建日期：2026-06-09
//! 修改日期：2026-06-10
//! 作者：Argus 开发团队
//! 主要功能：统一模态层遮罩、圆角容器、阴影和鼠标事件阻断能力。

use crate::theme::AppTheme;
use gpui::{
    AnyElement, Context, IntoElement, Pixels, Point, SharedString, div, prelude::*, px, rgb, rgba,
};

/// 通用模态框参数，调用方只负责提供内容元素。
pub(crate) struct ModalDialog {
    /// 模态遮罩元素 ID，便于测试定位和调试。
    pub overlay_id: &'static str,
    /// 模态容器元素 ID，便于测试定位和调试。
    pub container_id: &'static str,
    /// 模态容器宽度。
    pub width: f32,
    /// 模态容器高度。
    pub height: f32,
    /// 模态框内部内容。
    pub content: AnyElement,
}

/// 模态框可选外观与位置参数；默认无边框且保持居中。
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct ModalFrame {
    /// 容器边框颜色；`None` 表示不绘制边框。
    pub border_color: Option<u32>,
    /// 相对居中位置的拖动偏移。
    pub content_offset: Point<Pixels>,
}

/// 模态框内部内容与圆角外壳之间的安全内边距，避免子元素背景覆盖圆角。
const MODAL_CONTENT_INSET: f32 = 6.0;

/// 渲染通用模态遮罩与容器。
///
/// 参数说明：
/// - `dialog`：模态框布局参数和内容。
/// - `theme`：当前主题令牌，用于计算遮罩和容器背景。
/// - `cx`：应用上下文，用于阻断遮罩层鼠标事件继续传递。
///
/// 返回值：覆盖整个窗口的 GPUI 元素树。
pub(crate) fn render_modal_dialog<T>(
    dialog: ModalDialog,
    theme: AppTheme,
    cx: &mut Context<T>,
) -> impl IntoElement
where
    T: 'static,
{
    render_modal_dialog_with_frame(dialog, ModalFrame::default(), theme, cx)
}

/// 渲染带可选边框与拖拽偏移的通用模态遮罩与容器。
///
/// 参数说明：
/// - `dialog`：模态框布局参数和内容。
/// - `frame`：容器边框与拖动偏移；默认值等价于无边框、居中显示。
/// - `theme`：当前主题令牌，用于计算遮罩和容器背景。
/// - `cx`：应用上下文，用于阻断遮罩层鼠标事件继续传递。
pub(crate) fn render_modal_dialog_with_frame<T>(
    dialog: ModalDialog,
    frame: ModalFrame,
    theme: AppTheme,
    cx: &mut Context<T>,
) -> impl IntoElement
where
    T: 'static,
{
    div()
        .id(SharedString::from(dialog.overlay_id))
        .absolute()
        .top_0()
        .left_0()
        .size_full()
        .bg(rgba(theme.modal_overlay))
        .occlude()
        .on_click(cx.listener(|_, _, _, cx| {
            cx.stop_propagation();
        }))
        .child(
            div()
                .absolute()
                .top_0()
                .left_0()
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .id(SharedString::from(dialog.container_id))
                .occlude()
                .child(
                    div()
                        .w(px(dialog.width))
                        .h(px(dialog.height))
                        .p(px(MODAL_CONTENT_INSET))
                        .rounded_lg()
                        .bg(rgb(theme.content))
                        .shadow_lg()
                        .when_some(frame.border_color, |this, color| {
                            this.border_1().border_color(rgb(color))
                        })
                        // 相对偏移不参与居中布局，仅平移视觉位置，便于模态框在窗口内拖动。
                        .relative()
                        .left(frame.content_offset.x)
                        .top(frame.content_offset.y)
                        .occlude()
                        .child(
                            div()
                                .size_full()
                                .flex()
                                .overflow_hidden()
                                .rounded_lg()
                                .occlude()
                                .child(dialog.content),
                        ),
                ),
        )
}
