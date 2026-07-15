//! 文件职责：渲染窗口左侧的纵向图标导航条。
//! 创建日期：2026-06-09
//! 修改日期：2026-06-10
//! 作者：Argus 开发团队
//! 主要功能：仅保留“日志分析”和“设置”两个活动入口，并提供选中态与工作区切换。

use crate::app::{ArgusApp, Workspace};
use crate::theme::AppTheme;
use crate::ui::components::icon::ArgusIcon;
use crate::ui::components::icon_button::{IconButtonSize, render_icon_button};
use gpui::{Context, IntoElement, div, prelude::*, px, rgb};

/// 渲染左侧活动栏，活动栏宽度固定且不承载状态栏。
///
/// 参数说明：
/// - `active`：当前选中的工作区。
/// - `theme`：主题令牌，用于控制活动栏颜色。
/// - `cx`：应用上下文，用于创建工作区切换回调。
///
/// 返回值：GPUI 元素树；当前只有两个图标入口。
pub(crate) fn render(
    active: Workspace,
    theme: &AppTheme,
    cx: &mut Context<ArgusApp>,
) -> impl IntoElement {
    div()
        .w(px(48.0))
        .h_full()
        .flex()
        .flex_col()
        .items_center()
        .bg(rgb(theme.activity_bar))
        .border_r_1()
        .border_color(rgb(theme.border))
        .py_2()
        .child(activity_button(
            "activity-log-analysis",
            ArgusIcon::Logs,
            "日志分析",
            Workspace::LogAnalysis,
            active == Workspace::LogAnalysis,
            theme,
            cx,
        ))
        .child(div().flex_1())
        .child(activity_button(
            "activity-settings",
            ArgusIcon::Settings,
            "设置",
            Workspace::Settings,
            active == Workspace::Settings,
            theme,
            cx,
        ))
}

/// 渲染单个活动入口，点击后只切换本地 UI 状态。
fn activity_button(
    id: &'static str,
    icon: ArgusIcon,
    tooltip: &'static str,
    workspace: Workspace,
    is_active: bool,
    theme: &AppTheme,
    cx: &mut Context<ArgusApp>,
) -> impl IntoElement {
    render_icon_button(
        id,
        icon,
        tooltip,
        is_active,
        IconButtonSize::Medium,
        theme,
        cx.listener(move |app, _, _, cx| {
            app.switch_workspace(workspace, cx);
            cx.notify();
        }),
    )
}
