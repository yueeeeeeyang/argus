//! 文件职责：渲染替代系统默认标题栏的 Obsidian 风格自定义标题栏。
//! 创建日期：2026-06-09
//! 修改日期：2026-07-15
//! 作者：Argus 开发团队
//! 主要功能：适配原生 macOS 交通灯、Windows 窗口控制按钮，并展示操作组和当前标签。

use crate::app::{ArgusApp, Workspace};
use crate::platform::custom_titlebar;
use crate::theme::AppTheme;
use crate::ui::components::icon::ArgusIcon;
#[cfg(target_os = "windows")]
use crate::ui::components::icon::render_icon;
use crate::ui::components::icon_button::{IconButtonSize, render_icon_button};
use crate::ui::tab_bar;
#[cfg(not(target_os = "windows"))]
use gpui::WindowControlArea;
use gpui::{Context, IntoElement, MouseButton, MouseDownEvent, Window, div, prelude::*, px, rgb};

/// 自定义标题栏高度，保持紧凑 Obsidian 风格。
///
/// 公开供依赖标题栏高度的计算（如搜索结果面板保留高度）派生使用，避免各自维护
/// 易漂移的字面量。
pub(crate) const TITLE_BAR_HEIGHT: f32 = 40.0;
/// 原生交通灯及其周围不可接管连续点击的横向安全宽度。
///
/// 该值覆盖窗口左侧内边距、交通灯实际按钮和视觉占位，供原生事件命中判断复用。
pub(crate) const NATIVE_TRAFFIC_LIGHT_SAFE_WIDTH: f32 = 96.0;
/// 标题栏布局为原生交通灯或 Windows 左侧窗口按钮保留的固定宽度。
#[cfg(target_os = "macos")]
const NATIVE_TRAFFIC_LIGHT_SPACER_WIDTH: f32 = 76.0;
/// Windows 左侧单个窗口操作按钮的命中宽度；三个按钮中心位置与 macOS 交通灯接近。
#[cfg(target_os = "windows")]
const WINDOWS_WINDOW_CONTROL_BUTTON_WIDTH: f32 = 20.0;
/// Windows 左侧窗口操作按钮组宽度，与 macOS 原生交通灯布局占位一致。
#[cfg(target_os = "windows")]
const WINDOWS_WINDOW_CONTROLS_WIDTH: f32 = 76.0;
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
pub(crate) fn render(
    app: &ArgusApp,
    window: &mut Window,
    cx: &mut Context<ArgusApp>,
) -> impl IntoElement {
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
                .child(title_control_group(app, window.is_maximized(), theme, cx))
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
        .child(title_control_group(app, window.is_maximized(), theme, cx))
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
    let drag_area = div().id(id).h_full().flex_1();
    // Windows 使用普通客户区鼠标事件显式发送 HWND 拖动消息；声明非客户区控制区域会
    // 让点击再次绕过普通回调，因此只在其他平台保留 GPUI 的控制区提示。
    #[cfg(not(target_os = "windows"))]
    let drag_area = drag_area.window_control_area(WindowControlArea::Drag);

    drag_area.on_mouse_down(
        MouseButton::Left,
        cx.listener(|app, event: &MouseDownEvent, window, cx| {
            match event.click_count {
                1 => custom_titlebar::start_window_drag(window),
                2 => {
                    window.zoom_window();
                    app.placeholder_notice = "已切换窗口最大化状态".to_string();
                    cx.notify();
                }
                _ => {}
            }
            // 空白区域独占本次按下，避免根标题栏的清焦点击处理重复消费事件。
            cx.stop_propagation();
        }),
    )
}

/// 渲染左侧标题栏操作组。
fn title_control_group(
    app: &ArgusApp,
    is_window_maximized: bool,
    theme: &AppTheme,
    cx: &mut Context<ArgusApp>,
) -> impl IntoElement {
    if app.is_source_panel_collapsed {
        return collapsed_title_control_group(is_window_maximized, theme, cx).into_any_element();
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
        .child(platform_window_controls(is_window_maximized, theme))
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
fn collapsed_title_control_group(
    is_window_maximized: bool,
    theme: &AppTheme,
    cx: &mut Context<ArgusApp>,
) -> impl IntoElement {
    div()
        .h_full()
        .flex()
        .items_center()
        .gap_2()
        .child(platform_window_controls(is_window_maximized, theme))
        .child(title_action_button(
            "title-source-expand",
            ArgusIcon::Layout,
            "展开左侧菜单",
            false,
            theme,
            cx,
        ))
}

/// macOS 使用原生交通灯，只渲染等宽安全占位区。
#[cfg(target_os = "macos")]
fn platform_window_controls(_is_window_maximized: bool, _theme: &AppTheme) -> impl IntoElement {
    div().w(px(NATIVE_TRAFFIC_LIGHT_SPACER_WIDTH)).h_full()
}

/// 在 Windows 透明标题栏左侧渲染可直接操作当前 GPUI 窗口的按钮组。
///
/// 按钮组与 macOS 交通灯使用相同布局宽度，并采用普通客户区点击回调；这样不依赖当前
/// GPUI Windows 后端未能稳定触发的非客户区 `WindowControlArea` 分发。
#[cfg(target_os = "windows")]
fn platform_window_controls(is_window_maximized: bool, theme: &AppTheme) -> impl IntoElement {
    let maximize_icon = if is_window_maximized {
        ArgusIcon::WindowRestore
    } else {
        ArgusIcon::WindowMaximize
    };

    div()
        .id("windows-window-controls")
        .h_full()
        .w(px(WINDOWS_WINDOW_CONTROLS_WIDTH))
        .flex_none()
        .flex()
        .items_center()
        .child(windows_window_control_button(
            "windows-window-close",
            ArgusIcon::Close,
            WindowsWindowAction::Close,
            true,
            theme,
        ))
        .child(windows_window_control_button(
            "windows-window-minimize",
            ArgusIcon::Minus,
            WindowsWindowAction::Minimize,
            false,
            theme,
        ))
        .child(windows_window_control_button(
            "windows-window-maximize",
            maximize_icon,
            WindowsWindowAction::ToggleMaximize,
            false,
            theme,
        ))
}

/// Linux/BSD 由桌面环境负责窗口操作，不额外占用标题栏左侧空间。
#[cfg(not(any(target_os = "macos", target_os = "windows")))]
fn platform_window_controls(_is_window_maximized: bool, _theme: &AppTheme) -> impl IntoElement {
    div()
}

/// Windows 左侧窗口按钮对应的显式操作。
#[cfg(target_os = "windows")]
#[derive(Clone, Copy)]
enum WindowsWindowAction {
    /// 关闭当前窗口；最后一个窗口销毁后 GPUI 会结束消息循环。
    Close,
    /// 最小化当前窗口。
    Minimize,
    /// 在最大化与还原状态之间切换。
    ToggleMaximize,
}

/// 渲染单个 Windows 窗口控制按钮，并通过普通点击事件执行对应窗口操作。
#[cfg(target_os = "windows")]
fn windows_window_control_button(
    id: &'static str,
    icon: ArgusIcon,
    action: WindowsWindowAction,
    is_close: bool,
    theme: &AppTheme,
) -> impl IntoElement {
    let foreground = theme.foreground_muted;
    let hover_background = if is_close {
        0xc4_2b_1c
    } else {
        theme.current_line
    };

    div()
        .id(id)
        .h_full()
        .w(px(WINDOWS_WINDOW_CONTROL_BUTTON_WIDTH))
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .hover(move |this| this.bg(rgb(hover_background)))
        .cursor_pointer()
        .active(|this| this.opacity(0.82))
        .child(render_icon(icon, foreground, 12.0))
        .on_click(move |_, window, cx| {
            cx.stop_propagation();
            match action {
                WindowsWindowAction::Close => window.remove_window(),
                WindowsWindowAction::Minimize => window.minimize_window(),
                WindowsWindowAction::ToggleMaximize => window.zoom_window(),
            }
        })
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
                        "日志分析" => app.switch_workspace(Workspace::LogAnalysis),
                        "链接" => app.switch_workspace(Workspace::Connections),
                        "收起左侧菜单" | "展开左侧菜单" => app.toggle_source_panel(),
                        _ => app.mark_placeholder_action(action_name),
                    }
                    cx.notify();
                }),
            )),
    )
}

/// 渲染标题栏右侧设置入口，点击后打开主窗口设置模态框。
fn settings_button(
    app: &ArgusApp,
    theme: &AppTheme,
    cx: &mut Context<ArgusApp>,
) -> impl IntoElement {
    let is_selected = app.is_settings_modal_open;
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
                    app.open_settings_modal(cx);
                    cx.notify();
                }),
            )),
    )
}
