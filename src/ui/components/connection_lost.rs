//! 文件职责：渲染连接断开或连接失败时覆盖内容区的状态面板。
//! 创建日期：2026-10-15
//! 修改日期：2026-10-15
//! 作者：Argus 开发团队
//! 主要功能：在终端或远程文件管理会话断开后整窗提示状态与原因，并提供重连按钮。

use gpui::{ClickEvent, FontWeight, IntoElement, SharedString, Window, div, prelude::*, px, rgb};

use crate::theme::AppTheme;
use crate::ui::components::icon::{ArgusIcon, render_icon};

/// 渲染连接断开/失败面板：整块内容区显示状态与原因，并提供重连按钮。
///
/// 参数说明：
/// - `selector`：面板 debug 选择器前缀，用于区分终端与远程文件管理状态；
/// - `status_label`：状态标题，例如「SSH 连接已断开」「连接失败」；
/// - `message`：断开或失败原因；为空时退回状态提示，避免空白面板；
/// - `on_reconnect`：点击重连按钮后的回调。
///
/// 返回值：铺满内容区的状态面板元素。
pub(crate) fn render_connection_lost_panel(
    selector: &'static str,
    status_label: &str,
    message: Option<String>,
    theme: &AppTheme,
    on_reconnect: impl Fn(&ClickEvent, &mut Window, &mut gpui::App) + 'static,
) -> impl IntoElement {
    let detail = message.unwrap_or_else(|| format!("{status_label}，请重连后继续操作"));

    div()
        .id(selector)
        .debug_selector(move || selector.to_string())
        .size_full()
        .flex()
        .flex_col()
        .items_center()
        .justify_center()
        .gap_2()
        .bg(rgb(theme.content))
        .text_color(rgb(theme.foreground))
        .child(render_icon(
            ArgusIcon::Connection,
            theme.foreground_muted,
            28.0,
        ))
        .child(
            div()
                .text_size(px(14.0))
                .line_height(px(20.0))
                .font_weight(FontWeight::SEMIBOLD)
                .child(status_label.to_string()),
        )
        .child(
            div()
                .max_w(px(420.0))
                .text_size(px(12.0))
                .line_height(px(18.0))
                .text_color(rgb(theme.foreground_muted))
                .child(detail),
        )
        .child(
            div()
                .id(SharedString::from(format!("{selector}-reconnect")))
                .debug_selector(move || format!("{selector}-reconnect"))
                .h(px(28.0))
                .px_3()
                .flex()
                .items_center()
                .gap_1()
                .rounded_sm()
                .cursor_pointer()
                .border_1()
                .border_color(rgb(theme.border))
                .text_color(rgb(theme.info))
                .text_size(px(12.0))
                .line_height(px(18.0))
                .hover(|this| this.bg(rgb(theme.current_line)))
                .child(render_icon(ArgusIcon::Refresh, theme.info, 13.0))
                .child("重连")
                .on_click(on_reconnect),
        )
}
