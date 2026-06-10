//! 文件职责：渲染自定义标题栏中的日志标签区域。
//! 创建日期：2026-06-09
//! 修改日期：2026-06-10
//! 作者：Argus 开发团队
//! 主要功能：展示当前日志标签、关闭按钮和本地标签切换状态。

use crate::app::ArgusApp;
use crate::theme::AppTheme;
use crate::ui::components::icon::{ArgusIcon, render_icon};
use gpui::{Context, IntoElement, SharedString, div, prelude::*, px, rgb, svg};

/// 激活标签底部凹弧连接遮罩尺寸。
const ACTIVE_TAB_CONNECTOR_SIZE: f32 = 6.0;
/// 标签标题字号，保持标题栏紧凑密度。
const TAB_TITLE_FONT_SIZE: f32 = 12.0;
/// 激活和悬停标签高度，确保 hover 时与当前标签保持一致。
const TAB_ACTIVE_HEIGHT: f32 = 32.0;
/// 静止未激活标签高度，保留标题栏层次。
const TAB_INACTIVE_HEIGHT: f32 = 30.0;
/// 普通标签最小宽度。
const TAB_MIN_WIDTH: f32 = 150.0;
/// 普通标签最大宽度。
const TAB_MAX_WIDTH: f32 = 230.0;
/// 关闭按钮固定占位宽度，避免 hover 时插入按钮撑宽标签。
const TAB_CLOSE_SLOT_WIDTH: f32 = 18.0;
/// 标签关闭按钮命中区尺寸，比通用标题栏按钮更紧凑。
const TAB_CLOSE_BUTTON_SIZE: f32 = 18.0;
/// 标签关闭图标尺寸，匹配 12px 标题文本。
const TAB_CLOSE_ICON_SIZE: f32 = 13.0;

/// 激活标签凹弧连接件方向。
#[derive(Clone, Copy)]
enum TabConnectorSide {
    /// 标签左侧连接件。
    Left,
    /// 标签右侧连接件。
    Right,
}

/// 渲染标题栏中的当前标签区域。
///
/// 参数说明：
/// - `app`：应用状态，用于读取主题。
/// - `cx`：应用上下文，用于绑定切换、关闭和悬停状态。
///
/// 返回值：GPUI 元素树；当前不包含新增标签页入口。
pub fn render(app: &ArgusApp, cx: &mut Context<ArgusApp>) -> impl IntoElement {
    let theme = app.theme.clone();
    let tabs = app.tabs.clone();
    let active_tab_id = app.active_tab_id;
    let hovered_tab_id = app.hovered_tab_id;

    div().h_full().flex().items_end().children(
        tabs.into_iter()
            .map(|tab| {
                render_tab(tab, active_tab_id, hovered_tab_id, &theme, cx).into_any_element()
            })
            .collect::<Vec<_>>(),
    )
}

/// 渲染单个可切换、可关闭的标签。
fn render_tab(
    tab: crate::app::ArgusTab,
    active_tab_id: usize,
    hovered_tab_id: Option<usize>,
    theme: &AppTheme,
    cx: &mut Context<ArgusApp>,
) -> impl IntoElement {
    let tab_id = tab.id;
    let is_active = tab_id == active_tab_id;
    let is_hovered = hovered_tab_id == Some(tab_id);
    let should_show_close = is_active || is_hovered;
    let background = if is_active {
        theme.content
    } else if is_hovered {
        theme.current_line
    } else {
        theme.title_bar
    };
    let height = if is_active || is_hovered {
        TAB_ACTIVE_HEIGHT
    } else {
        TAB_INACTIVE_HEIGHT
    };
    let foreground = if is_active || is_hovered {
        theme.foreground
    } else {
        theme.foreground_muted
    };

    div()
        .id(SharedString::from(format!("tab-{tab_id}")))
        .h(px(height))
        .flex()
        .items_end()
        .cursor_pointer()
        .when(is_active, |this| {
            this.child(active_tab_connector(TabConnectorSide::Left, theme))
        })
        .child(
            div()
                .h(px(height))
                .min_w(px(TAB_MIN_WIDTH))
                .max_w(px(TAB_MAX_WIDTH))
                .relative()
                .pl_3()
                .pr_1()
                .pb(px(1.0))
                .flex()
                .items_center()
                .gap_2()
                .rounded_t(px(8.0))
                .bg(rgb(background))
                .text_color(rgb(foreground))
                .child(
                    div()
                        .flex_1()
                        .truncate()
                        .text_size(px(TAB_TITLE_FONT_SIZE))
                        .child(tab.title),
                )
                .child(render_tab_close_slot(tab_id, should_show_close, theme, cx)),
        )
        .when(is_active, |this| {
            this.child(active_tab_connector(TabConnectorSide::Right, theme))
        })
        .on_hover(cx.listener(move |app, is_hovered: &bool, _, cx| {
            app.set_hovered_tab(tab_id, *is_hovered);
            cx.notify();
        }))
        .on_click(cx.listener(move |app, _, _, cx| {
            app.activate_tab(tab_id);
            cx.notify();
        }))
}

/// 渲染固定宽度的关闭按钮槽；按钮显隐不改变标签整体宽度。
fn render_tab_close_slot(
    tab_id: usize,
    should_show_close: bool,
    theme: &AppTheme,
    cx: &mut Context<ArgusApp>,
) -> impl IntoElement {
    let close_hover_background = theme.border;
    let close_foreground = theme.foreground_muted;

    div()
        .w(px(TAB_CLOSE_SLOT_WIDTH))
        .h(px(TAB_CLOSE_BUTTON_SIZE))
        .flex_none()
        .flex()
        .items_center()
        .justify_end()
        .when(should_show_close, |this| {
            this.child(
                div()
                    .id(SharedString::from(format!("tab-close-{tab_id}")))
                    .w(px(TAB_CLOSE_BUTTON_SIZE))
                    .h(px(TAB_CLOSE_BUTTON_SIZE))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded_sm()
                    .hover(move |this| this.bg(rgb(close_hover_background)))
                    .child(render_icon(
                        ArgusIcon::Close,
                        close_foreground,
                        TAB_CLOSE_ICON_SIZE,
                    ))
                    .on_click(cx.listener(move |app, _, _, cx| {
                        cx.stop_propagation();
                        app.close_tab(tab_id);
                        cx.notify();
                    })),
            )
        })
}

/// 渲染激活标签与内容区衔接处的单侧凹弧连接件。
fn active_tab_connector(side: TabConnectorSide, theme: &AppTheme) -> impl IntoElement {
    let path = match side {
        TabConnectorSide::Left => "chrome/tab-connector-left.svg",
        TabConnectorSide::Right => "chrome/tab-connector-right.svg",
    };

    div()
        .w(px(ACTIVE_TAB_CONNECTOR_SIZE))
        .h(px(ACTIVE_TAB_CONNECTOR_SIZE))
        .flex_none()
        .bg(rgb(theme.content))
        .child(
            svg()
                .path(path)
                .size(px(ACTIVE_TAB_CONNECTOR_SIZE))
                .text_color(rgb(theme.title_bar)),
        )
}
