//! 文件职责：提供独立窗口统一标题栏组件。
//! 创建日期：2026-07-03
//! 修改日期：2026-07-15
//! 作者：Argus 开发团队
//! 主要功能：使用主窗口标题栏主题色渲染带关闭按钮的独立窗口标题栏骨架。

use gpui::{App, ClickEvent, ElementId, IntoElement, Window, div, prelude::*, px, rgb};

use crate::theme::AppTheme;
use crate::ui::components::icon::ArgusIcon;
use crate::ui::components::icon_button::{IconButtonSize, render_icon_button};

/// 渲染独立窗口标题栏骨架：统一高度、水平内边距、底部分隔线与右侧关闭按钮。
///
/// 说明：各独立窗口的标题左侧内容（图标 + 标题文字、文件名、附加控件）差异较大，
/// 因此左侧内容由 `left` 传入；本组件只收敛标题栏容器样式与关闭按钮，避免多窗口漂移。
///
/// 参数说明：
/// - `close_id`：关闭按钮稳定元素 ID。
/// - `close_tooltip`：关闭按钮悬停提示。
/// - `height`：标题栏高度；由各窗口按自身结构传入。
/// - `show_border`：是否渲染底部分隔线。
/// - `left`：标题栏左侧内容，由调用者构造以适配不同标题结构。
/// - `theme`：主题令牌。
/// - `on_close`：关闭按钮点击回调。
pub(crate) fn render_window_title_bar(
    close_id: impl Into<ElementId>,
    close_tooltip: &'static str,
    height: f32,
    show_border: bool,
    theme: &AppTheme,
    left: impl IntoElement,
    on_close: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    let mut bar = div()
        .h(px(height))
        .flex_none()
        .px_5()
        .flex()
        .items_center()
        .justify_between()
        .bg(rgb(theme.title_bar))
        .occlude();
    if show_border {
        bar = bar.border_b_1().border_color(rgb(theme.border));
    }
    bar.child(left).child(render_icon_button(
        close_id,
        ArgusIcon::Close,
        close_tooltip,
        false,
        IconButtonSize::Small,
        theme,
        on_close,
    ))
}
