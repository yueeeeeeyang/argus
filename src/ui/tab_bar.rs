//! 文件职责：渲染自定义标题栏中的日志标签区域。
//! 创建日期：2026-06-09
//! 修改日期：2026-06-10
//! 作者：Argus 开发团队
//! 主要功能：展示当前日志标签、关闭按钮和本地标签切换状态。

use crate::app::ArgusApp;
use crate::theme::AppTheme;
use crate::ui::components::icon::ArgusIcon;
use crate::ui::components::icon_button::{IconButtonSize, render_icon_button};
use gpui::{Context, IntoElement, SharedString, div, prelude::*, px, rgb, svg};

/// 激活标签底部凹弧连接遮罩尺寸。
const ACTIVE_TAB_CONNECTOR_SIZE: f32 = 6.0;
/// 标签标题字号，保持标题栏紧凑密度。
const TAB_TITLE_FONT_SIZE: f32 = 12.0;

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
                render_tab(tab.id, tab.title, active_tab_id, hovered_tab_id, &theme, cx)
                    .into_any_element()
            })
            .collect::<Vec<_>>(),
    )
}

/// 渲染单个可切换、可关闭的占位标签。
fn render_tab(
    tab_id: usize,
    title: String,
    active_tab_id: usize,
    hovered_tab_id: Option<usize>,
    theme: &AppTheme,
    cx: &mut Context<ArgusApp>,
) -> impl IntoElement {
    let is_active = tab_id == active_tab_id;
    let is_hovered = hovered_tab_id == Some(tab_id);
    let should_show_close = is_active || is_hovered;
    let should_show_hover_border = !is_active && is_hovered;
    let background = if is_active {
        theme.content
    } else if is_hovered {
        theme.current_line
    } else {
        theme.title_bar
    };
    let height = if is_active { 32.0 } else { 30.0 };

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
                .min_w(px(150.0))
                .max_w(px(230.0))
                .relative()
                .px_3()
                .pb(px(1.0))
                .flex()
                .items_center()
                .justify_between()
                .rounded_t(px(8.0))
                .bg(rgb(background))
                .text_color(rgb(theme.foreground))
                .when(should_show_hover_border, |this| {
                    this.border_1().border_color(rgb(theme.border))
                })
                .child(
                    div()
                        .flex_1()
                        .truncate()
                        .text_size(px(TAB_TITLE_FONT_SIZE))
                        .child(title),
                )
                .when(should_show_close, |this| {
                    this.child(render_icon_button(
                        ("tab-close", tab_id),
                        ArgusIcon::Close,
                        "关闭标签",
                        false,
                        IconButtonSize::Small,
                        theme,
                        cx.listener(move |app, _, _, cx| {
                            cx.stop_propagation();
                            app.close_tab(tab_id);
                            cx.notify();
                        }),
                    ))
                }),
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
