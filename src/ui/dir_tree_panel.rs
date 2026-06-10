//! 文件职责：渲染来源侧栏中的真实来源目录树。
//! 创建日期：2026-06-09
//! 修改日期：2026-06-10
//! 作者：Argus 开发团队
//! 主要功能：使用 GPUI uniform_list 虚拟渲染大目录树，并提供纵向滚动条。

use crate::app::ArgusApp;
use crate::loader::log_source::{SourceKind, SourceTreeNode};
use crate::theme::AppTheme;
use crate::ui::components::icon::{ArgusIcon, render_icon};
use gpui::{
    AnyElement, Context, FontWeight, IntoElement, MouseDownEvent, MouseMoveEvent, MouseUpEvent,
    Render, SharedString, Window, canvas, div, point, prelude::*, px, rgb, uniform_list,
};
use std::ops::Range;

use crate::utils::size_format::format_bytes;

/// 目录树连线的首层横坐标。
const TREE_LINE_BASE_X: f32 = 18.0;
/// 目录树每一级缩进对应的连线横向步长。
const TREE_LINE_INDENT_STEP: f32 = 16.0;
/// 目录树节点横向分支线的垂直位置。
const TREE_BRANCH_Y: f32 = 14.0;
/// 目录树节点横向分支线长度。
const TREE_BRANCH_LENGTH: f32 = 14.0;
/// 来源树固定行高；uniform_list 依赖所有行保持一致高度。
const SOURCE_ROW_HEIGHT: f32 = 28.0;
/// 来源树节点文字大小，保持高密度目录树的可扫描性。
const SOURCE_TREE_FONT_SIZE: f32 = 12.0;
/// 来源树节点图标大小，与 12px 文本保持紧凑比例。
const SOURCE_TREE_ICON_SIZE: f32 = 14.0;
/// 自定义滚动条宽度。
const SCROLLBAR_THUMB_WIDTH: f32 = 4.0;
/// 自定义滚动条最小高度。
const SCROLLBAR_MIN_THUMB_HEIGHT: f32 = 36.0;

/// 来源树名称悬浮提示，用于在节点文本被截断时快速查看完整名称。
struct SourceNameTooltip {
    /// 完整节点名称。
    label: String,
    /// 当前主题令牌，用于保持 tooltip 与应用暗色风格一致。
    theme: AppTheme,
}

impl Render for SourceNameTooltip {
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

/// 渲染目录树面板。
///
/// 参数说明：
/// - `app`：应用状态，提供来源注册表、可见节点索引和主题令牌。
/// - `cx`：应用上下文，用于虚拟列表范围处理和节点事件回调。
///
/// 返回值：GPUI 元素树；只渲染当前视口范围内的来源节点。
pub fn render(app: &ArgusApp, cx: &mut Context<ArgusApp>) -> impl IntoElement {
    let theme = app.theme.clone();
    let visible_count = app.visible_source_ids().len();
    let empty_message = if app.is_source_loading {
        "正在加载来源..."
    } else if app.is_source_tree_filtering() {
        "未找到匹配日志"
    } else {
        "暂无日志来源"
    };
    let empty_icon = if app.is_source_loading {
        ArgusIcon::Refresh
    } else if app.is_source_tree_filtering() {
        ArgusIcon::Search
    } else {
        ArgusIcon::Folder
    };

    div()
        .relative()
        .flex_1()
        .overflow_hidden()
        .pt(px(6.0))
        .pb_2()
        .when(visible_count == 0, |this| {
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
        .when(visible_count > 0, |this| {
            this.child(
                uniform_list(
                    "source-tree-list",
                    visible_count,
                    cx.processor(|app, range: Range<usize>, _window, cx| {
                        let visible_ids = app.visible_source_ids()[range].to_vec();
                        let theme = app.theme.clone();
                        let mut rows = Vec::with_capacity(visible_ids.len());

                        for source_id in visible_ids {
                            if let Some(source) = app.source_registry.node(source_id).cloned() {
                                let row_meta = app.source_registry.row_meta(source_id);
                                rows.push(
                                    render_node(
                                        &source,
                                        row_meta.child_count,
                                        &row_meta.ancestor_continuation_levels,
                                        row_meta.has_next_sibling,
                                        &theme,
                                        cx,
                                    )
                                    .into_any_element(),
                                );
                            }
                        }

                        rows
                    }),
                )
                .size_full()
                .track_scroll(app.source_tree_scroll.clone()),
            )
            .child(render_scrollbar(app, cx))
        })
}

/// 渲染单个来源节点，缩进根据真实来源深度计算。
fn render_node(
    source: &SourceTreeNode,
    child_count: usize,
    ancestor_continuation_levels: &[usize],
    has_next_sibling: bool,
    theme: &AppTheme,
    cx: &mut Context<ArgusApp>,
) -> impl IntoElement {
    let node_icon = icon_for_source(source);
    let expand_icon = if source.expanded {
        ArgusIcon::Collapse
    } else {
        ArgusIcon::Expand
    };
    let source_id = source.id;
    let can_expand = source.kind.can_expand();
    let meta_text = source_meta_text(source, child_count);
    let tooltip_label = source.label.clone();
    let tooltip_theme = theme.clone();

    div()
        .id(SharedString::from(format!("source-node-{source_id}")))
        .h(px(SOURCE_ROW_HEIGHT))
        .w_full()
        .px_2()
        .child(
            div()
                .id(SharedString::from(format!(
                    "source-node-content-{source_id}"
                )))
                .relative()
                .h_full()
                .w_full()
                .flex()
                .items_center()
                .gap_2()
                .pl(px(10.0 + source.depth as f32 * 16.0))
                .pr_2()
                .rounded_sm()
                .when(source.selected, |this| this.bg(rgb(theme.selection)))
                .hover(|this| this.bg(rgb(theme.current_line)))
                .text_size(px(SOURCE_TREE_FONT_SIZE))
                .font_weight(FontWeight::MEDIUM)
                .text_color(rgb(theme.foreground))
                .cursor_pointer()
                .when(source.depth > 0, |this| {
                    this.children(tree_connection_lines(
                        source.depth,
                        ancestor_continuation_levels,
                        has_next_sibling,
                        theme,
                    ))
                })
                .child(
                    div()
                        .id(SharedString::from(format!("source-toggle-{source_id}")))
                        .w(px(18.0))
                        .h(px(20.0))
                        .flex()
                        .items_center()
                        .justify_center()
                        .when(can_expand, |this| {
                            let icon = if source.metadata.is_loading {
                                ArgusIcon::Refresh
                            } else {
                                expand_icon
                            };
                            this.child(render_icon(
                                icon,
                                theme.foreground_muted,
                                SOURCE_TREE_ICON_SIZE,
                            ))
                        }),
                )
                .child(render_icon(
                    node_icon,
                    theme.foreground_muted,
                    SOURCE_TREE_ICON_SIZE,
                ))
                .child(
                    div()
                        .id(SharedString::from(format!("source-label-{source_id}")))
                        .flex_1()
                        .truncate()
                        .tooltip(move |_, cx| {
                            cx.new(|_| SourceNameTooltip {
                                label: tooltip_label.clone(),
                                theme: tooltip_theme.clone(),
                            })
                            .into()
                        })
                        .child(source.label.clone()),
                )
                .when(!meta_text.is_empty(), |this| {
                    this.child(
                        div()
                            .ml_2()
                            .flex_none()
                            .text_size(px(SOURCE_TREE_FONT_SIZE))
                            .font_weight(FontWeight::NORMAL)
                            .text_color(rgb(theme.foreground_muted))
                            .child(meta_text.clone()),
                    )
                })
                .when_some(source.metadata.message.clone(), |this, message| {
                    this.child(
                        div()
                            .max_w(px(80.0))
                            .text_size(px(12.0))
                            .font_weight(FontWeight::NORMAL)
                            .text_color(rgb(theme.warning))
                            .truncate()
                            .child(message),
                    )
                })
                .on_click(cx.listener(move |app, _, _, cx| {
                    if can_expand {
                        app.toggle_source_expanded(source_id, cx);
                    } else {
                        app.select_source(source_id);
                        app.scroll_source_into_view(source_id);
                    }
                    cx.notify();
                })),
        )
}

/// 返回来源节点右侧元信息；目录显示子级数量，文件显示大小。
fn source_meta_text(source: &SourceTreeNode, child_count: usize) -> String {
    match source.kind {
        SourceKind::Directory | SourceKind::ArchiveDirectory => {
            if source.metadata.children_loaded {
                format!("{child_count} 项")
            } else {
                "…".to_string()
            }
        }
        SourceKind::LogFile
        | SourceKind::Archive(_)
        | SourceKind::ArchiveFile
        | SourceKind::Unsupported(_)
        | SourceKind::Error => source.metadata.size.map(format_bytes).unwrap_or_default(),
    }
}
/// 根据节点层级和兄弟关系渲染目录树连线，避免最后一个子节点下方残留无连接竖线。
fn tree_connection_lines(
    depth: usize,
    ancestor_continuation_levels: &[usize],
    has_next_sibling: bool,
    theme: &AppTheme,
) -> Vec<AnyElement> {
    let mut lines = Vec::new();

    for level in ancestor_continuation_levels.iter().copied() {
        let x = TREE_LINE_BASE_X + level as f32 * TREE_LINE_INDENT_STEP;
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

    let branch_x = TREE_LINE_BASE_X + (depth - 1) as f32 * TREE_LINE_INDENT_STEP;
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
            current_line.h(px(TREE_BRANCH_Y + 1.0))
        }
        .into_any_element(),
    );

    lines.push(
        div()
            .absolute()
            .left(px(branch_x))
            .top(px(TREE_BRANCH_Y))
            .w(px(TREE_BRANCH_LENGTH))
            .h(px(1.0))
            .bg(rgb(theme.border))
            .opacity(0.55)
            .into_any_element(),
    );

    lines
}

/// 渲染来源树自定义纵向滚动条。
fn render_scrollbar(app: &ArgusApp, cx: &mut Context<ArgusApp>) -> impl IntoElement {
    let scroll_state = app.source_tree_scroll.0.borrow();
    let bounds = scroll_state.base_handle.bounds();
    let scroll_offset = scroll_state.base_handle.offset();
    let content_height = scroll_state
        .last_item_size
        .unwrap_or_default()
        .contents
        .height;
    let scroll_handle = scroll_state.base_handle.clone();
    drop(scroll_state);

    let viewport_height = bounds.size.height;
    if viewport_height == px(0.0) || content_height <= viewport_height {
        return div().id("source-tree-scrollbar");
    }

    let track_padding = px(4.0);
    let track_height = (viewport_height - track_padding * 2.0).max(px(1.0));
    let thumb_height = ((viewport_height / content_height) * track_height)
        .clamp(px(SCROLLBAR_MIN_THUMB_HEIGHT), track_height);
    let max_scroll = (content_height - viewport_height).max(px(1.0));
    let scroll_ratio = (-scroll_offset.y / max_scroll).clamp(0.0, 1.0);
    let thumb_top = track_padding + (track_height - thumb_height) * scroll_ratio;
    let entity = cx.entity();

    div()
        .id("source-tree-scrollbar")
        .absolute()
        .top(thumb_top)
        .right(px(3.0))
        .w(px(SCROLLBAR_THUMB_WIDTH))
        .h(thumb_height)
        .rounded_lg()
        .bg(rgb(app.theme.foreground_muted))
        .opacity(0.45)
        .hover(|this| this.opacity(0.75))
        .child(
            canvas(
                |_, _, _| (),
                move |thumb_bounds, _, window: &mut Window, _| {
                    window.on_mouse_event({
                        let entity = entity.clone();
                        move |event: &MouseDownEvent, _, _, cx| {
                            if !thumb_bounds.contains(&event.position) {
                                return;
                            }
                            entity.update(cx, |app, _| {
                                app.source_scrollbar_drag_position =
                                    Some(event.position - thumb_bounds.origin);
                            });
                        }
                    });

                    window.on_mouse_event({
                        let entity = entity.clone();
                        move |_: &MouseUpEvent, _, _, cx| {
                            entity.update(cx, |app, _| {
                                app.source_scrollbar_drag_position = None;
                            });
                        }
                    });

                    window.on_mouse_event(move |event: &MouseMoveEvent, _, _, cx| {
                        if !event.dragging() {
                            return;
                        }

                        let Some(drag_position) = entity.read(cx).source_scrollbar_drag_position
                        else {
                            return;
                        };
                        let usable_track = (track_height - thumb_height).max(px(1.0));
                        let thumb_offset =
                            (event.position.y - bounds.origin.y - track_padding - drag_position.y)
                                .clamp(px(0.0), usable_track);
                        let percentage = thumb_offset / usable_track;
                        scroll_handle.set_offset(point(px(0.0), -(max_scroll * percentage)));
                        cx.notify(entity.entity_id());
                    });
                },
            )
            .size_full(),
        )
}

/// 根据来源节点类型返回对应 Lucide 图标。
fn icon_for_source(source: &SourceTreeNode) -> ArgusIcon {
    match &source.kind {
        SourceKind::Directory if source.expanded => ArgusIcon::FolderOpen,
        SourceKind::Directory => ArgusIcon::Folder,
        SourceKind::Archive(_) => ArgusIcon::Archive,
        SourceKind::ArchiveDirectory if source.expanded => ArgusIcon::FolderOpen,
        SourceKind::ArchiveDirectory => ArgusIcon::Folder,
        SourceKind::ArchiveFile | SourceKind::LogFile => ArgusIcon::FileText,
        SourceKind::Unsupported(_) | SourceKind::Error => ArgusIcon::File,
    }
}
