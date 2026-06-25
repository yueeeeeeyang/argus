//! 文件职责：渲染主内容区底部状态栏。
//! 创建日期：2026-06-09
//! 修改日期：2026-06-16
//! 作者：Argus 开发团队
//! 主要功能：显示编码、来源树统计、日志或 Jstack 内容状态和用户操作提示，且只属于内容区宽度。

use crate::app::{ArgusApp, TabKind};
use crate::reader::log_file_reader::LogOpenState;
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
        TabKind::LogSource { source_id, .. } => app
            .log_read_state(source_id)
            .map(LogOpenState::status_label)
            .unwrap_or_else(|| "未读取".to_string()),
        TabKind::JstackAnalysis { analysis_id } => app
            .jstack_analysis_state(analysis_id)
            .map(|state| state.title.clone())
            .unwrap_or_else(|| "Jstack分析".to_string()),
        TabKind::RuntimeAnalysis { analysis_id } => app
            .runtime_analysis_state(analysis_id)
            .map(|state| state.title.clone())
            .unwrap_or_else(|| "Runtime分析".to_string()),
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
