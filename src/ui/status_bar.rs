//! 文件职责：渲染主内容区底部状态栏。
//! 创建日期：2026-06-09
//! 修改日期：2026-06-10
//! 作者：Argus 开发团队
//! 主要功能：显示编码、来源树统计、内容读取状态和用户操作提示，且只属于内容区宽度。

use crate::app::{ArgusApp, TabKind};
use gpui::{IntoElement, div, prelude::*, px, rgb};

/// 渲染内容区状态栏。
///
/// 参数说明：
/// - `app`：应用状态，提供主题、日志行数和占位提示。
///
/// 返回值：GPUI 元素树；调用方应将其放在内容区内部，避免横跨活动栏和来源侧栏。
pub fn render(app: &ArgusApp) -> impl IntoElement {
    let theme = app.theme.clone();
    let content_status = match app.active_tab_kind() {
        TabKind::Empty if !app.logs.is_empty() => format!("{} 行样例", app.logs.len()),
        TabKind::Empty => "未选择日志".to_string(),
        TabKind::LogSource { .. } => "内容未读取".to_string(),
        TabKind::Settings => "设置".to_string(),
    };

    div()
        .h(px(26.0))
        .w_full()
        .flex()
        .items_center()
        .justify_between()
        .px_3()
        .border_t_1()
        .border_color(rgb(theme.border))
        .bg(rgb(theme.status_bar))
        .text_xs()
        .child(
            div()
                .flex()
                .items_center()
                .gap_3()
                .text_color(rgb(theme.foreground_muted))
                .child(app.selected_encoding.clone())
                .child(format!("{} 个来源节点", app.source_registry.node_count()))
                .child(format!("{} 个可见", app.visible_source_ids().len()))
                .child(content_status),
        )
        .child(
            div()
                .truncate()
                .text_color(rgb(theme.success))
                .child(app.placeholder_notice.clone()),
        )
}
