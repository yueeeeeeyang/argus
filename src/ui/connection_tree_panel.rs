//! 文件职责：渲染链接工作区左侧 SSH 链接目录树。
//! 创建日期：2026-06-26
//! 修改日期：2026-06-26
//! 作者：Argus 开发团队
//! 主要功能：展示目录、SSH 链接、过滤结果和节点点击打开终端交互。

use std::ops::Range;

use gpui::{
    AnyElement, Context, FontWeight, IntoElement, MouseButton, MouseDownEvent, Render,
    SharedString, Window, div, prelude::*, px, rgb, uniform_list,
};

use crate::app::ArgusApp;
use crate::connections::{ConnectionTreeRow, ConnectionTreeRowKind};
use crate::theme::AppTheme;
use crate::ui::components::icon::{ArgusIcon, render_icon};

/// 链接树固定行高，保证虚拟列表滚动稳定。
const CONNECTION_ROW_HEIGHT: f32 = 28.0;
/// 链接树节点文字大小。
const CONNECTION_TREE_FONT_SIZE: f32 = 12.0;
/// 链接树节点图标大小。
const CONNECTION_TREE_ICON_SIZE: f32 = 14.0;
/// 链接树节点选中背景圆角。
const CONNECTION_ROW_RADIUS: f32 = 5.0;
/// 链接树连线的首层横坐标，和日志目录树保持一致。
const CONNECTION_TREE_LINE_BASE_X: f32 = 18.0;
/// 链接树每一级缩进对应的连线横向步长。
const CONNECTION_TREE_LINE_INDENT_STEP: f32 = 16.0;
/// 链接树节点横向分支线的垂直位置。
const CONNECTION_TREE_BRANCH_Y: f32 = 14.0;
/// 链接树节点横向分支线长度。
const CONNECTION_TREE_BRANCH_LENGTH: f32 = 14.0;

/// SSH 链接悬浮提示，用于快速查看远程用户名、主机和端口。
struct ConnectionLinkTooltip {
    /// 提示文本。
    label: String,
    /// 当前主题令牌。
    theme: AppTheme,
}

impl Render for ConnectionLinkTooltip {
    /// 渲染紧凑 tooltip 内容。
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .max_w(px(360.0))
            .px_2()
            .py_1()
            .rounded_sm()
            .bg(rgb(self.theme.title_bar))
            .border_1()
            .border_color(rgb(self.theme.border))
            .text_size(px(12.0))
            .font_weight(FontWeight::NORMAL)
            .text_color(rgb(self.theme.foreground))
            .child(self.label.clone())
    }
}

/// 渲染链接目录树面板。
pub fn render(app: &ArgusApp, cx: &mut Context<ArgusApp>) -> impl IntoElement {
    let theme = app.theme.clone();
    let rows = app.visible_connection_rows();
    let empty_message = if app.is_connection_tree_filtering() {
        "未找到匹配链接"
    } else {
        "暂无链接"
    };
    let empty_icon = if app.is_connection_tree_filtering() {
        ArgusIcon::Filter
    } else {
        ArgusIcon::Connection
    };

    div()
        .relative()
        .flex_1()
        .overflow_hidden()
        .pt(px(1.0))
        .pb_2()
        .when(rows.is_empty(), |this| {
            this.child(
                div()
                    .h_full()
                    .flex()
                    .flex_col()
                    .items_center()
                    .justify_center()
                    .gap_2()
                    .text_xs()
                    .text_color(rgb(theme.foreground_muted))
                    .child(render_icon(empty_icon, theme.foreground_muted, 28.0))
                    .child(empty_message),
            )
        })
        .when(!rows.is_empty(), |this| {
            let row_count = rows.len();
            this.child(
                uniform_list(
                    "connection-tree-list",
                    row_count,
                    cx.processor(|app, range: Range<usize>, _window, cx| {
                        let rows = app.visible_connection_rows();
                        let theme = app.theme.clone();
                        rows[range]
                            .iter()
                            .cloned()
                            .map(|row| render_row(row, &theme, cx).into_any_element())
                            .collect::<Vec<_>>()
                    }),
                )
                .size_full()
                .track_scroll(app.connection_tree_scroll.clone()),
            )
        })
}

/// 渲染单个链接树节点。
fn render_row(
    row: ConnectionTreeRow,
    theme: &AppTheme,
    cx: &mut Context<ArgusApp>,
) -> impl IntoElement {
    let node_id = row.id;
    let icon = match row.kind {
        ConnectionTreeRowKind::Directory if row.expanded => ArgusIcon::FolderOpen,
        ConnectionTreeRowKind::Directory => ArgusIcon::Folder,
        ConnectionTreeRowKind::SshLink => ArgusIcon::Link,
        ConnectionTreeRowKind::SmbLink => ArgusIcon::Database,
    };
    let expand_icon = if row.expanded {
        ArgusIcon::Collapse
    } else {
        ArgusIcon::Expand
    };
    let meta_text = match row.kind {
        ConnectionTreeRowKind::Directory => "",
        ConnectionTreeRowKind::SshLink => "ssh",
        ConnectionTreeRowKind::SmbLink => "smb",
    };
    let tooltip = row.tooltip.clone();
    let tooltip_theme = theme.clone();

    div()
        .id(SharedString::from(format!("connection-node-{node_id}")))
        .h(px(CONNECTION_ROW_HEIGHT))
        .w_full()
        .px_2()
        .py(px(2.0))
        .on_mouse_down(
            MouseButton::Right,
            cx.listener(move |app, event: &MouseDownEvent, _, cx| {
                cx.stop_propagation();
                app.open_connection_tree_context_menu(node_id, event.position);
                cx.notify();
            }),
        )
        .child(
            div()
                .id(SharedString::from(format!(
                    "connection-node-content-{node_id}"
                )))
                .relative()
                .h_full()
                .w_full()
                .flex()
                .items_center()
                .gap_2()
                .pl(px(10.0 + row.depth as f32 * 16.0))
                .pr_2()
                .rounded(px(CONNECTION_ROW_RADIUS))
                .cursor_pointer()
                .when_some(tooltip, |this, tooltip| {
                    this.tooltip(move |_, cx| {
                        cx.new(|_| ConnectionLinkTooltip {
                            label: tooltip.clone(),
                            theme: tooltip_theme.clone(),
                        })
                        .into()
                    })
                })
                .text_size(px(CONNECTION_TREE_FONT_SIZE))
                .font_weight(FontWeight::MEDIUM)
                .text_color(rgb(theme.foreground))
                .when(row.is_selected, |this| this.bg(rgb(theme.selection)))
                .when(!row.is_selected, |this| {
                    this.hover(|this| this.bg(rgb(theme.current_line)))
                })
                .on_mouse_down(
                    MouseButton::Right,
                    cx.listener(move |app, event: &MouseDownEvent, _, cx| {
                        cx.stop_propagation();
                        app.open_connection_tree_context_menu(node_id, event.position);
                        cx.notify();
                    }),
                )
                .when(row.depth > 0, |this| {
                    this.children(tree_connection_lines(
                        row.depth,
                        &row.ancestor_continuation_levels,
                        row.has_next_sibling,
                        theme,
                    ))
                })
                .child(
                    div()
                        .w(px(18.0))
                        .h(px(20.0))
                        .flex()
                        .items_center()
                        .justify_center()
                        .when(
                            row.kind == ConnectionTreeRowKind::Directory && row.has_children,
                            |this| {
                                this.child(render_icon(
                                    expand_icon,
                                    theme.foreground_muted,
                                    CONNECTION_TREE_ICON_SIZE,
                                ))
                            },
                        ),
                )
                .child(render_icon(
                    icon,
                    theme.foreground_muted,
                    CONNECTION_TREE_ICON_SIZE,
                ))
                .child(
                    div()
                        .id(SharedString::from(format!("connection-label-{node_id}")))
                        .flex_1()
                        .truncate()
                        .child(row.label),
                )
                .when(!meta_text.is_empty(), |this| {
                    this.child(
                        div()
                            .text_size(px(CONNECTION_TREE_FONT_SIZE))
                            .font_weight(FontWeight::NORMAL)
                            .text_color(rgb(theme.foreground_muted))
                            .child(meta_text),
                    )
                })
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |app, _: &MouseDownEvent, _, cx| {
                        cx.stop_propagation();
                        app.handle_connection_tree_click(node_id, cx);
                        cx.notify();
                    }),
                ),
        )
}

/// 根据节点层级和兄弟关系渲染链接目录树连线。
fn tree_connection_lines(
    depth: usize,
    ancestor_continuation_levels: &[usize],
    has_next_sibling: bool,
    theme: &AppTheme,
) -> Vec<AnyElement> {
    let mut lines = Vec::new();

    for level in ancestor_continuation_levels.iter().copied() {
        let x = CONNECTION_TREE_LINE_BASE_X + level as f32 * CONNECTION_TREE_LINE_INDENT_STEP;
        lines.push(
            div()
                .absolute()
                .left(px(x))
                .top_0()
                .bottom_0()
                .w(px(1.0))
                .bg(rgb(theme.border))
                .opacity(0.55)
                .into_any_element(),
        );
    }

    let branch_x =
        CONNECTION_TREE_LINE_BASE_X + (depth - 1) as f32 * CONNECTION_TREE_LINE_INDENT_STEP;
    let current_line = div()
        .absolute()
        .left(px(branch_x))
        .top_0()
        .w(px(1.0))
        .bg(rgb(theme.border))
        .opacity(0.55);

    lines.push(
        if has_next_sibling {
            current_line.bottom_0()
        } else {
            current_line.h(px(CONNECTION_TREE_BRANCH_Y + 1.0))
        }
        .into_any_element(),
    );

    lines.push(
        div()
            .absolute()
            .left(px(branch_x))
            .top(px(CONNECTION_TREE_BRANCH_Y))
            .w(px(CONNECTION_TREE_BRANCH_LENGTH))
            .h(px(1.0))
            .bg(rgb(theme.border))
            .opacity(0.55)
            .into_any_element(),
    );

    lines
}
