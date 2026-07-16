//! 文件职责：提供统一的图标按钮与悬停提示组件。
//! 创建日期：2026-06-09
//! 修改日期：2026-07-15
//! 作者：Argus 开发团队
//! 主要功能：复用 Lucide 图标、按钮状态、点击交互和 tooltip 视觉样式。

use crate::theme::AppTheme;
use crate::ui::components::icon::{ArgusIcon, render_icon};
use gpui::{
    App, AppContext, ClickEvent, Context, ElementId, IntoElement, Render, Window, div, prelude::*,
    px, rgb,
};

/// 图标按钮尺寸。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum IconButtonSize {
    /// 标题栏和内容工具栏中的小按钮。
    Small,
    /// 来源侧栏工具区中的 14px 图标按钮。
    Tiny,
}

/// 图标按钮外形；常规工具栏维持小圆角，独立悬浮操作使用正圆形。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum IconButtonShape {
    /// 项目现有工具栏使用的小圆角矩形。
    RoundedRectangle,
    /// 独立悬浮操作使用的圆形。
    Circle,
}

/// 图标按钮内容的视觉下移量。
///
/// 说明：当前 UI 字体和 Lucide SVG 在 GPUI 中按几何中心对齐时会显得略靠上，
/// 这里仅移动内容层，不改变按钮尺寸和命中区域。
const ICON_BUTTON_CONTENT_Y_OFFSET: f32 = 1.0;

/// tooltip 视图状态，仅保存需要展示的说明文案。
struct TooltipView {
    /// tooltip 展示文本。
    label: String,
    /// tooltip 背景色。
    background: u32,
    /// tooltip 边框色。
    border: u32,
    /// tooltip 文本色。
    foreground: u32,
}

impl Render for TooltipView {
    /// 渲染紧凑 tooltip，颜色跟随当前主题令牌。
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .px_2()
            .py_1()
            .rounded_sm()
            .bg(rgb(self.background))
            .border_1()
            .border_color(rgb(self.border))
            .text_xs()
            .text_color(rgb(self.foreground))
            .child(self.label.clone())
    }
}

/// 渲染统一图标按钮。
///
/// 参数说明：
/// - `id`：稳定元素 ID，用于 GPUI 状态和后续测试定位。
/// - `icon`：按钮内展示的 Lucide 图标。
/// - `tooltip`：悬停提示文案。
/// - `is_selected`：是否展示选中态。
/// - `size`：按钮尺寸规格。
/// - `theme`：主题令牌。
/// - `on_click`：点击回调，可操作应用状态或窗口 API。
///
/// 返回值：GPUI 元素树；不会直接触发真实业务功能。
pub(crate) fn render_icon_button(
    id: impl Into<ElementId>,
    icon: ArgusIcon,
    tooltip: &'static str,
    is_selected: bool,
    size: IconButtonSize,
    theme: &AppTheme,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    render_icon_button_with_shape(
        id,
        icon,
        tooltip,
        is_selected,
        size,
        IconButtonShape::RoundedRectangle,
        theme,
        on_click,
    )
}

/// 渲染圆形图标按钮，悬停和按下背景始终沿用同一圆形命中区域。
///
/// 参数与 [`render_icon_button`] 一致，仅按钮外形固定为正圆，适用于悬浮跳转等独立操作。
pub(crate) fn render_round_icon_button(
    id: impl Into<ElementId>,
    icon: ArgusIcon,
    tooltip: &'static str,
    is_selected: bool,
    size: IconButtonSize,
    theme: &AppTheme,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    render_icon_button_with_shape(
        id,
        icon,
        tooltip,
        is_selected,
        size,
        IconButtonShape::Circle,
        theme,
        on_click,
    )
}

/// 根据指定外形渲染图标按钮的共享视觉和交互实现。
#[allow(clippy::too_many_arguments)]
fn render_icon_button_with_shape(
    id: impl Into<ElementId>,
    icon: ArgusIcon,
    tooltip: &'static str,
    is_selected: bool,
    size: IconButtonSize,
    shape: IconButtonShape,
    theme: &AppTheme,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    let (button_size, icon_size) = match size {
        IconButtonSize::Small => (28.0, 16.0),
        IconButtonSize::Tiny => (24.0, 14.0),
    };
    let selected_background = theme.selection;
    let hover_background = if is_selected {
        theme.selection
    } else {
        theme.current_line
    };
    let foreground = if is_selected {
        theme.foreground
    } else {
        theme.foreground_muted
    };
    let tooltip_background = theme.current_line;
    let tooltip_border = theme.border;
    let tooltip_foreground = theme.foreground;

    div()
        .id(id)
        .w(px(button_size))
        .h(px(button_size))
        .flex()
        .items_center()
        .justify_center()
        .when(shape == IconButtonShape::RoundedRectangle, |this| {
            this.rounded_sm()
        })
        .when(shape == IconButtonShape::Circle, |this| this.rounded_full())
        .when(is_selected, |this| this.bg(rgb(selected_background)))
        .hover(move |this| this.bg(rgb(hover_background)))
        .cursor_pointer()
        .active(|this| this.opacity(0.82))
        .tooltip(move |_, cx| {
            cx.new(|_| TooltipView {
                label: tooltip.to_string(),
                background: tooltip_background,
                border: tooltip_border,
                foreground: tooltip_foreground,
            })
            .into()
        })
        .child(
            div()
                .relative()
                .top(px(ICON_BUTTON_CONTENT_Y_OFFSET))
                .child(render_icon(icon, foreground, icon_size)),
        )
        .on_click(on_click)
}
