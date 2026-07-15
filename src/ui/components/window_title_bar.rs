//! 文件职责：提供独立窗口统一标题栏组件。
//! 创建日期：2026-07-03
//! 修改日期：2026-07-03
//! 作者：Argus 开发团队
//! 主要功能：渲染带关闭按钮的窗口标题栏骨架，供独立窗口复用；标题左侧内容由调用者按各自结构提供。

use gpui::{App, ClickEvent, ElementId, IntoElement, Window, div, prelude::*, px, rgb};

use crate::theme::AppTheme;
use crate::ui::components::icon::ArgusIcon;
use crate::ui::components::icon_button::{IconButtonSize, render_icon_button};

/// 标准独立窗口标题栏高度（设置窗口、Jstack 窗口等使用）。
pub(crate) const STANDARD_TITLE_BAR_HEIGHT: f32 = 56.0;
/// 标准独立窗口标题字号。
pub(crate) const STANDARD_TITLE_BAR_FONT_SIZE: f32 = 14.0;

/// 渲染独立窗口标题栏骨架：统一高度、水平内边距、底部分隔线与右侧关闭按钮。
///
/// 说明：各独立窗口的标题左侧内容（图标 + 标题文字、文件名、附加控件）差异较大，
/// 因此左侧内容由 `left` 传入；本组件只收敛标题栏容器样式与关闭按钮，避免多窗口漂移。
///
/// 参数说明：
/// - `close_id`：关闭按钮稳定元素 ID。
/// - `close_tooltip`：关闭按钮悬停提示。
/// - `height`：标题栏高度；标准窗口用 `STANDARD_TITLE_BAR_HEIGHT`，紧凑窗口可传更小值。
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
