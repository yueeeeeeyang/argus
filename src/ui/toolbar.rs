//! 文件职责：渲染侧栏和内容区的紧凑上下文工具栏。
//! 创建日期：2026-06-09
//! 修改日期：2026-06-10
//! 作者：Argus 开发团队
//! 主要功能：提供加载日志、搜索、目录树折叠、导航和更多操作的占位按钮。

use crate::app::ArgusApp;
use crate::theme::AppTheme;
use crate::ui::components::icon::ArgusIcon;
use crate::ui::components::icon_button::{IconButtonSize, render_icon_button};
use crate::ui::components::input::{Input, InputAccessory, InputSize, render_input};
use gpui::{
    Animation, AnimationExt, AnyElement, ClickEvent, Context, IntoElement, KeyDownEvent, div,
    prelude::*, px, rgb,
};
use std::time::Duration;

/// 渲染来源侧栏的局部工具按钮。
///
/// 参数说明：
/// - `app`：应用状态，提供主题令牌。
/// - `cx`：应用上下文，用于更新占位提示。
///
/// 返回值：GPUI 元素树；加载按钮会打开系统路径选择器，其余按钮只更新本地状态。
pub fn render_source_toolbar(app: &ArgusApp, cx: &mut Context<ArgusApp>) -> AnyElement {
    let theme = app.theme.clone();

    if app.is_source_tree_search_open {
        return render_source_search_toolbar(app, &theme, cx).into_any_element();
    }

    div()
        .flex()
        .items_center()
        .gap_1()
        .child(source_icon_button(
            "source-load-log",
            ArgusIcon::FolderPlus,
            "加载日志",
            &theme,
            cx,
        ))
        .child(source_icon_button(
            "source-search",
            ArgusIcon::Search,
            "搜索",
            &theme,
            cx,
        ))
        .child(source_icon_button(
            "source-collapse-all",
            ArgusIcon::ListCollapse,
            "全部收起",
            &theme,
            cx,
        ))
        .into_any_element()
}

/// 渲染日志内容区顶部的导航和上下文工具。
pub fn render_content_toolbar(app: &ArgusApp, cx: &mut Context<ArgusApp>) -> impl IntoElement {
    let theme = app.theme.clone();

    div()
        .h(px(42.0))
        .flex()
        .items_center()
        .justify_between()
        .px_3()
        .border_b_1()
        .border_color(rgb(theme.border))
        .bg(rgb(theme.content))
        .child(
            div()
                .flex()
                .items_center()
                .gap_2()
                .child(toolbar_icon_button(
                    "content-back",
                    ArgusIcon::ArrowLeft,
                    "后退",
                    &theme,
                    cx,
                ))
                .child(toolbar_icon_button(
                    "content-forward",
                    ArgusIcon::ArrowRight,
                    "前进",
                    &theme,
                    cx,
                ))
                .child(
                    div()
                        .text_sm()
                        .text_color(rgb(theme.foreground_muted))
                        .child(app.content_path_label()),
                ),
        )
        .child(
            div()
                .flex()
                .items_center()
                .gap_1()
                .child(toolbar_icon_button(
                    "content-wrap",
                    ArgusIcon::Wrap,
                    "自动换行",
                    &theme,
                    cx,
                ))
                .child(toolbar_icon_button(
                    "content-more",
                    ArgusIcon::More,
                    "更多操作",
                    &theme,
                    cx,
                )),
        )
}

/// 渲染来源侧栏工具按钮，尺寸与标题栏图标按钮保持一致。
fn source_icon_button(
    id: &'static str,
    icon: ArgusIcon,
    action_name: &'static str,
    theme: &AppTheme,
    cx: &mut Context<ArgusApp>,
) -> impl IntoElement {
    render_icon_button(
        id,
        icon,
        action_name,
        false,
        IconButtonSize::Small,
        theme,
        cx.listener(move |app, _, window, cx| {
            match action_name {
                "加载日志" => app.request_load_sources(cx),
                "搜索" => {
                    app.open_source_tree_search();
                    window.on_next_frame(|window, _| window.focus_next());
                }
                "全部收起" => app.collapse_all_sources(),
                _ => app.mark_placeholder_action(action_name),
            }
            cx.notify();
        }),
    )
}

/// 渲染来源树工具栏的内联搜索输入框，过滤当前已加载的日志节点。
fn render_source_search_toolbar(
    app: &ArgusApp,
    theme: &AppTheme,
    cx: &mut Context<ArgusApp>,
) -> impl IntoElement {
    div()
        .overflow_hidden()
        .child(render_input(
            Input {
                id: "source-tree-search-input",
                placeholder: "过滤已加载日志",
                value: app.source_tree_search_query.clone(),
                is_disabled: false,
                is_focused: app.is_source_tree_search_focused,
                cursor_index: app.source_tree_search_cursor,
                selection_range: app.source_tree_search_selection_range(),
                size: InputSize::Compact,
                leading_accessory: Some(InputAccessory {
                    id: "source-tree-search-leading",
                    icon: ArgusIcon::Search,
                    tooltip: "搜索",
                }),
                trailing_accessory: Some(InputAccessory {
                    id: "source-tree-search-close",
                    icon: ArgusIcon::Close,
                    tooltip: "关闭搜索",
                }),
            },
            theme,
            cx.listener(|app, event: &KeyDownEvent, _, cx| {
                cx.stop_propagation();
                app.handle_source_tree_search_key(&event.keystroke, cx);
                cx.notify();
            }),
            cx.listener(|app, event: &ClickEvent, _, cx| {
                app.set_source_tree_search_focused(true);
                if let ClickEvent::Mouse(mouse_event) = event
                    && mouse_event.up.click_count >= 2
                {
                    app.select_all_source_tree_search();
                }
                cx.notify();
            }),
            cx.listener(|app, _, _, cx| {
                app.close_source_tree_search();
                cx.notify();
            }),
        ))
        .with_animation(
            (
                "source-tree-search-open",
                app.source_tree_search_animation_generation,
            ),
            Animation::new(Duration::from_millis(140)).with_easing(gpui::ease_out_quint()),
            |this, progress| {
                this.opacity(progress)
                    .w(px(44.0 + (216.0 - 44.0) * progress))
            },
        )
}

/// 渲染内容区常规小型工具按钮，统一更新应用状态提示。
fn toolbar_icon_button(
    id: &'static str,
    icon: ArgusIcon,
    action_name: &'static str,
    theme: &AppTheme,
    cx: &mut Context<ArgusApp>,
) -> impl IntoElement {
    render_icon_button(
        id,
        icon,
        action_name,
        false,
        IconButtonSize::Small,
        theme,
        cx.listener(move |app, _, _, cx| {
            app.mark_placeholder_action(action_name);
            cx.notify();
        }),
    )
}
