//! 文件职责：渲染链接工作区左侧远程链接目录树。
//! 创建日期：2026-06-26
//! 修改日期：2026-09-24
//! 作者：Argus 开发团队
//! 主要功能：展示目录与多协议链接、处理选择、过滤、打开会话及链接拖放移动。

use std::ops::Range;

use gpui::{
    AnyElement, ClickEvent, Context, FontWeight, IntoElement, MouseButton, MouseDownEvent, Render,
    SharedString, Window, div, prelude::*, px, rgb, uniform_list,
};

use crate::app::ArgusApp;
use crate::remote::connection::{ConnectionTreeRow, ConnectionTreeRowKind};
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
/// 链接树选中块上下留白，与行高配合决定连线越界补偿量。
const CONNECTION_ROW_VERTICAL_INSET: f32 = 2.0;
/// 链接树连线的首层横坐标，和日志目录树保持一致。
const CONNECTION_TREE_LINE_BASE_X: f32 = 18.0;
/// 链接树每一级缩进对应的连线横向步长。
const CONNECTION_TREE_LINE_INDENT_STEP: f32 = 16.0;
/// 链接树行内容的基础左内边距；每深入一级再叠加一个缩进步长。
const CONNECTION_ROW_CONTENT_PADDING: f32 = 10.0;
/// 展开/收起箭头所在切换按钮的宽度；箭头图标在按钮内水平居中。
const CONNECTION_ROW_TOGGLE_WIDTH: f32 = 18.0;
/// 本级竖线到展开箭头按钮左沿的水平距离。
///
/// 按钮左沿 = 行内容左内边距 + 本级缩进；竖线 = 连线基准线 + 上一级缩进。
const CONNECTION_TREE_LINE_TO_TOGGLE_GAP: f32 =
    CONNECTION_ROW_CONTENT_PADDING + CONNECTION_TREE_LINE_INDENT_STEP - CONNECTION_TREE_LINE_BASE_X;
/// 链接树节点横向分支线的垂直位置（相对行内容区顶部）。
///
/// 行内容区上下各内缩 `CONNECTION_ROW_VERTICAL_INSET`，其垂直中心即整行中心，与展开箭头
/// `>` 的中心重合，避免分支线偏离箭头。
const CONNECTION_TREE_BRANCH_Y: f32 = CONNECTION_ROW_HEIGHT / 2.0 - CONNECTION_ROW_VERTICAL_INSET;
/// 链接树节点横向分支线长度：自本级竖线起接到展开箭头按钮的水平中心，
/// 保证连线真正接到箭头中央。
const CONNECTION_TREE_BRANCH_LENGTH: f32 =
    CONNECTION_TREE_LINE_TO_TOGGLE_GAP + CONNECTION_ROW_TOGGLE_WIDTH / 2.0;

/// SSH 链接悬浮提示，用于快速查看远程用户名、主机和端口。
struct ConnectionLinkTooltip {
    /// 提示文本。
    label: String,
    /// 当前主题令牌。
    theme: AppTheme,
}

/// 链接树内部拖放载荷；只允许链接叶子节点作为拖动源。
#[derive(Clone, Debug)]
struct ConnectionLinkDrag {
    /// 待移动链接节点 ID。
    link_id: crate::remote::connection::ConnectionNodeId,
    /// 拖动浮层展示名称。
    label: String,
    /// 拖动浮层使用的协议图标。
    icon: ArgusIcon,
}

/// 链接拖动时跟随指针展示的紧凑预览。
struct ConnectionLinkDragPreview {
    /// 链接名称。
    label: String,
    /// 链接协议图标。
    icon: ArgusIcon,
    /// 创建拖动时的主题快照。
    theme: AppTheme,
}

impl Render for ConnectionLinkDragPreview {
    /// 渲染不会参与点击命中的拖动预览条目。
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .h(px(CONNECTION_ROW_HEIGHT))
            .max_w(px(240.0))
            .px_3()
            .flex()
            .items_center()
            .gap_2()
            .rounded(px(CONNECTION_ROW_RADIUS))
            .border_1()
            .border_color(rgb(self.theme.border))
            .bg(rgb(self.theme.current_line))
            .shadow_md()
            .text_size(px(CONNECTION_TREE_FONT_SIZE))
            .font_weight(FontWeight::MEDIUM)
            .text_color(rgb(self.theme.foreground))
            .child(render_icon(
                self.icon,
                self.theme.foreground_muted,
                CONNECTION_TREE_ICON_SIZE,
            ))
            .child(div().truncate().child(self.label.clone()))
    }
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
pub(crate) fn render(app: &ArgusApp, cx: &mut Context<ArgusApp>) -> impl IntoElement {
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
        .id("connection-tree-panel")
        .relative()
        .flex_1()
        .overflow_hidden()
        .pt(px(1.0))
        .pb_2()
        .on_click(cx.listener(|app, _: &ClickEvent, _, cx| {
            if app.clear_connection_tree_selection() {
                cx.notify();
            }
        }))
        .on_drop(cx.listener(|app, drag: &ConnectionLinkDrag, _, cx| {
            // 目录行会在更内层处理并阻止冒泡；落在列表空白处则移动到根层级。
            cx.stop_propagation();
            app.move_connection_link(drag.link_id, None);
            cx.notify();
        }))
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
        ConnectionTreeRowKind::GitLink => ArgusIcon::GitBranch,
        ConnectionTreeRowKind::SvnLink => ArgusIcon::History,
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
        ConnectionTreeRowKind::GitLink => "git",
        ConnectionTreeRowKind::SvnLink => "svn",
    };
    let tooltip = row.tooltip.clone();
    let tooltip_theme = theme.clone();
    let drag_label = row.label.clone();
    let drag_theme = theme.clone();
    let drop_background = theme.selection;
    let drop_border = theme.foreground_muted;

    div()
        .id(SharedString::from(format!("connection-node-{node_id}")))
        .h(px(CONNECTION_ROW_HEIGHT))
        .w_full()
        .px_2()
        .py(px(CONNECTION_ROW_VERTICAL_INSET))
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
                .pl(px(CONNECTION_ROW_CONTENT_PADDING
                    + row.depth as f32 * CONNECTION_TREE_LINE_INDENT_STEP))
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
                .when(row.is_selected, |this| this.bg(rgb(theme.current_line)))
                .when(!row.is_selected, |this| {
                    this.hover(|this| this.bg(rgb(theme.current_line)))
                })
                .when(row.kind == ConnectionTreeRowKind::Directory, |this| {
                    this.drag_over::<ConnectionLinkDrag>(move |style, _drag, _window, _cx| {
                        style
                            .bg(rgb(drop_background))
                            .border_1()
                            .border_color(rgb(drop_border))
                    })
                    .on_drop(cx.listener(
                        move |app, drag: &ConnectionLinkDrag, _, cx| {
                            cx.stop_propagation();
                            app.move_connection_link(drag.link_id, Some(node_id));
                            cx.notify();
                        },
                    ))
                })
                .when(row.kind != ConnectionTreeRowKind::Directory, |this| {
                    let drag = ConnectionLinkDrag {
                        link_id: node_id,
                        label: drag_label,
                        icon,
                    };
                    this.cursor_move()
                        .on_drag(drag, move |drag, _position, _window, cx| {
                            cx.new(|_| ConnectionLinkDragPreview {
                                label: drag.label.clone(),
                                icon: drag.icon,
                                theme: drag_theme.clone(),
                            })
                        })
                        // 放到另一个链接行不是有效目录目标，必须阻止事件落到根面板。
                        .on_drop(cx.listener(|_app, _drag: &ConnectionLinkDrag, _, cx| {
                            cx.stop_propagation();
                        }))
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
                        .w(px(CONNECTION_ROW_TOGGLE_WIDTH))
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
                .on_click(cx.listener(move |app, _: &ClickEvent, _, cx| {
                    cx.stop_propagation();
                    app.handle_connection_tree_click(node_id, cx);
                    cx.notify();
                })),
        )
}

/// 根据节点层级和兄弟关系渲染链接目录树连线，避免最后一个子节点下方残留无连接竖线。
fn tree_connection_lines(
    depth: usize,
    ancestor_continuation_levels: &[usize],
    has_next_sibling: bool,
    theme: &AppTheme,
) -> Vec<AnyElement> {
    let mut lines = Vec::new();

    // 竖线越过行内容区上下内缩、覆盖整行高度：相邻行的竖线首尾相接，行与行之间不再出现断缝。
    let vertical_overhang = -CONNECTION_ROW_VERTICAL_INSET;

    for level in ancestor_continuation_levels.iter().copied() {
        let x = CONNECTION_TREE_LINE_BASE_X + level as f32 * CONNECTION_TREE_LINE_INDENT_STEP;
        lines.push(
            div()
                .absolute()
                .left(px(x))
                .top(px(vertical_overhang))
                .bottom(px(vertical_overhang))
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
        .top(px(vertical_overhang))
        .w(px(1.0))
        .bg(rgb(theme.border))
        .opacity(0.55);

    lines.push(
        if has_next_sibling {
            current_line.bottom(px(vertical_overhang))
        } else {
            // 最后一个子节点：竖线止于本级分支线下方 1px，形成 └ 形收尾。
            current_line.h(px(CONNECTION_TREE_BRANCH_Y
                + CONNECTION_ROW_VERTICAL_INSET
                + 1.0))
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::hint::black_box;

    /// 回归：链接树连线必须覆盖整行、垂直居中于展开箭头，并横向接到箭头按钮中心。
    ///
    /// 与日志目录树同一实现口径：竖线只覆盖行内容区会让每行之间出现断缝；分支线必须
    /// 落在箭头垂直中心并接到箭头水平中心。
    #[test]
    fn tree_connector_geometry_centers_on_expand_arrow() {
        // 竖线在行内容区上下各外扩一个内缩量，总高恰好等于整行高度 → 相邻行首尾相接。
        let connector_height = (CONNECTION_ROW_HEIGHT - 2.0 * CONNECTION_ROW_VERTICAL_INSET)
            + 2.0 * CONNECTION_ROW_VERTICAL_INSET;
        assert_eq!(
            black_box(connector_height),
            black_box(CONNECTION_ROW_HEIGHT),
            "竖线应覆盖整行高度，行与行之间不留断缝"
        );

        // 分支线落在行内容区垂直中心，即整行中心、也是展开箭头的垂直中心。
        assert_eq!(
            black_box(CONNECTION_TREE_BRANCH_Y),
            black_box(CONNECTION_ROW_HEIGHT / 2.0 - CONNECTION_ROW_VERTICAL_INSET),
            "分支线应位于行内容区垂直中心"
        );
        assert_eq!(
            black_box(CONNECTION_TREE_BRANCH_Y),
            12.0,
            "分支线应落在整行 y=14 处"
        );

        // 分支线从本级竖线延伸「竖线到按钮左沿 8px + 按钮半宽 9px」，正好落在箭头按钮中心。
        assert_eq!(
            black_box(CONNECTION_TREE_BRANCH_LENGTH),
            black_box(CONNECTION_TREE_LINE_TO_TOGGLE_GAP + CONNECTION_ROW_TOGGLE_WIDTH / 2.0),
            "分支线应接到展开箭头按钮水平中心"
        );
        assert_eq!(
            black_box(CONNECTION_TREE_BRANCH_LENGTH),
            17.0,
            "分支线长度应为 17px（8px 间距 + 18px 按钮的一半）"
        );

        // 行内容左内边距与切换按钮宽度必须与连线几何使用同一组常量。
        assert_eq!(black_box(CONNECTION_TREE_LINE_TO_TOGGLE_GAP), 8.0);
    }
}
