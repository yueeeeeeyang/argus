//! 文件职责：渲染替代系统默认标题栏的 Obsidian 风格自定义标题栏。
//! 创建日期：2026-06-09
//! 修改日期：2026-06-10
//! 作者：Argus 开发团队
//! 主要功能：为原生 macOS 交通灯预留安全区，并展示左侧操作组、当前标签和贯通分割线。

use crate::app::{ArgusApp, Workspace};
use crate::theme::AppTheme;
use crate::ui::components::icon::ArgusIcon;
use crate::ui::components::icon_button::{IconButtonSize, render_icon_button};
use crate::ui::tab_bar;
use gpui::{ClickEvent, Context, IntoElement, Window, WindowControlArea, div, prelude::*, px, rgb};

/// 自定义标题栏高度，保持紧凑 Obsidian 风格。
const TITLE_BAR_HEIGHT: f32 = 40.0;
/// 标签页与来源侧栏分割线之间的视觉留白。
const TAB_LEFT_GAP: f32 = 16.0;
/// 标签栏右侧固定按钮与窗口右边缘的间距。
const TAB_RIGHT_GAP: f32 = 12.0;
/// 标题栏非激活按钮 hover 背景的视觉垂直校正值。
const TITLE_BUTTON_INACTIVE_Y_OFFSET: f32 = 1.0;

/// 渲染自定义标题栏；标题栏不包含书签入口。
///
/// 参数说明：
/// - `app`：应用状态，用于展示当前主题与占位提示行为。
/// - `_window`：当前窗口对象，保留在签名中便于后续接入更多窗口级行为。
/// - `cx`：应用上下文，用于创建标题栏按钮的占位回调。
///
/// 返回值：GPUI 元素树；当前不执行真实搜索或打开文件逻辑。
pub fn render(app: &ArgusApp, window: &mut Window, cx: &mut Context<ArgusApp>) -> impl IntoElement {
    let theme = app.theme.clone();
    let show_source_boundary = matches!(
        app.workspace,
        Workspace::LogAnalysis | Workspace::Connections
    ) && !app.is_source_panel_collapsed;

    if show_source_boundary {
        render_split_title_bar(app, window, &theme, cx).into_any_element()
    } else {
        render_compact_title_bar(app, window, &theme, cx).into_any_element()
    }
}

/// 渲染带来源侧栏贯通分割线的标题栏。
fn render_split_title_bar(
    app: &ArgusApp,
    window: &mut Window,
    theme: &AppTheme,
    cx: &mut Context<ArgusApp>,
) -> impl IntoElement {
    div()
        .id("argus-split-title-bar")
        .h(px(TITLE_BAR_HEIGHT))
        .w_full()
        .flex()
        .items_center()
        .bg(rgb(theme.title_bar))
        .on_click(cx.listener(|app, _, _, cx| {
            app.clear_log_text_focus();
            cx.notify();
        }))
        .child(
            div()
                .w(px(app.current_source_panel_width()))
                .h_full()
                .flex()
                .flex_none()
                .items_center()
                .gap_2()
                .pl_3()
                .pr_3()
                .child(title_control_group(app, theme, cx))
                .child(title_drag_area("title-left-drag-area", cx)),
        )
        .child(title_center(app, window, cx))
}

/// 渲染没有来源侧栏分割线的紧凑标题栏。
fn render_compact_title_bar(
    app: &ArgusApp,
    window: &mut Window,
    theme: &AppTheme,
    cx: &mut Context<ArgusApp>,
) -> impl IntoElement {
    div()
        .id("argus-compact-title-bar")
        .h(px(TITLE_BAR_HEIGHT))
        .w_full()
        .flex()
        .items_center()
        .bg(rgb(theme.title_bar))
        .on_click(cx.listener(|app, _, _, cx| {
            app.clear_log_text_focus();
            cx.notify();
        }))
        .px_3()
        .gap_2()
        .child(title_control_group(app, theme, cx))
        .child(title_center(app, window, cx))
}

/// 渲染标题栏中心区域，保留标签和左右拖拽空白。
fn title_center(
    app: &ArgusApp,
    window: &mut Window,
    cx: &mut Context<ArgusApp>,
) -> impl IntoElement {
    div().h_full().flex_1().flex().items_center().child(
        div()
            .h_full()
            .flex_1()
            .flex()
            .pl(px(TAB_LEFT_GAP))
            .pr(px(TAB_RIGHT_GAP))
            .child(tab_bar::render(app, window, cx)),
    )
}

/// 渲染标题栏可拖拽空白区域，并在双击时执行系统级最大化/还原。
fn title_drag_area(id: &'static str, cx: &mut Context<ArgusApp>) -> impl IntoElement {
    div()
        .id(id)
        .h_full()
        .flex_1()
        .window_control_area(WindowControlArea::Drag)
        .on_click(cx.listener(|app, event: &ClickEvent, window, cx| {
            if let ClickEvent::Mouse(mouse_event) = event
                && mouse_event.up.click_count >= 2
            {
                window.zoom_window();
                app.placeholder_notice = "已切换窗口最大化状态".to_string();
                cx.stop_propagation();
                cx.notify();
            }
        }))
}

/// 渲染左侧标题栏操作组。
fn title_control_group(
    app: &ArgusApp,
    theme: &AppTheme,
    cx: &mut Context<ArgusApp>,
) -> impl IntoElement {
    if app.is_source_panel_collapsed {
        return collapsed_title_control_group(theme, cx).into_any_element();
    }

    let source_panel_action = if app.is_source_panel_collapsed {
        "展开左侧菜单"
    } else {
        "收起左侧菜单"
    };

    div()
        .h_full()
        .flex()
        .items_center()
        .gap_2()
        .child(native_traffic_light_spacer())
        .child(title_action_button(
            "title-log-analysis",
            ArgusIcon::Logs,
            "日志分析",
            app.workspace == Workspace::LogAnalysis,
            theme,
            cx,
        ))
        .child(title_action_button(
            "title-connection",
            ArgusIcon::Connection,
            "链接",
            app.workspace == Workspace::Connections,
            theme,
            cx,
        ))
        .child(settings_button(app, theme, cx))
        .child(title_action_button(
            "title-source-toggle",
            ArgusIcon::Layout,
            source_panel_action,
            app.is_source_panel_collapsed,
            theme,
            cx,
        ))
        .into_any_element()
}

/// 渲染来源侧栏折叠后的标题栏控制组，仅保留展开入口。
fn collapsed_title_control_group(theme: &AppTheme, cx: &mut Context<ArgusApp>) -> impl IntoElement {
    div()
        .h_full()
        .flex()
        .items_center()
        .gap_2()
        .child(native_traffic_light_spacer())
        .child(title_action_button(
            "title-source-expand",
            ArgusIcon::Layout,
            "展开左侧菜单",
            false,
            theme,
            cx,
        ))
}

/// 渲染原生 macOS 交通灯按钮的安全占位区。
fn native_traffic_light_spacer() -> impl IntoElement {
    div().w(px(76.0)).h_full()
}

/// 渲染标题栏占位操作按钮，点击后只更新占位提示。
fn title_action_button(
    id: &'static str,
    icon: ArgusIcon,
    action_name: &'static str,
    is_selected: bool,
    theme: &AppTheme,
    cx: &mut Context<ArgusApp>,
) -> impl IntoElement {
    div().h_full().flex().items_center().child(
        div()
            .when(!is_selected, |this| {
                this.relative().top(px(TITLE_BUTTON_INACTIVE_Y_OFFSET))
            })
            .child(render_icon_button(
                id,
                icon,
                action_name,
                is_selected,
                IconButtonSize::Small,
                theme,
                cx.listener(move |app, _, _, cx| {
                    match action_name {
                        "日志分析" => app.switch_workspace(Workspace::LogAnalysis, cx),
                        "链接" => app.switch_workspace(Workspace::Connections, cx),
                        "收起左侧菜单" | "展开左侧菜单" => app.toggle_source_panel(),
                        _ => app.mark_placeholder_action(action_name),
                    }
                    cx.notify();
                }),
            )),
    )
}

/// 渲染标题栏右侧设置入口，点击后打开或聚焦独立设置窗口。
fn settings_button(
    app: &ArgusApp,
    theme: &AppTheme,
    cx: &mut Context<ArgusApp>,
) -> impl IntoElement {
    let is_selected = app.is_settings_window_open;
    div().h_full().flex().items_center().child(
        div()
            .when(!is_selected, |this| {
                this.relative().top(px(TITLE_BUTTON_INACTIVE_Y_OFFSET))
            })
            .child(render_icon_button(
                "title-settings",
                ArgusIcon::Settings,
                "设置",
                is_selected,
                IconButtonSize::Small,
                theme,
                cx.listener(|app, _, _, cx| {
                    app.open_settings_window(cx);
                    cx.notify();
                }),
            )),
    )
}
