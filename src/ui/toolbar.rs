//! 文件职责：渲染侧栏和内容区的紧凑上下文工具栏。
//! 创建日期：2026-06-09
//! 修改日期：2026-06-16
//! 作者：Argus 开发团队
//! 主要功能：提供加载日志、过滤、目录树折叠、导航和更多操作的占位按钮。

use crate::app::{AppTextInputTarget, ArgusApp};
use crate::theme::AppTheme;
use crate::ui::components::icon::ArgusIcon;
use crate::ui::components::icon_button::{IconButtonSize, render_icon_button};
use crate::ui::components::input::{
    Input, InputAccessory, InputPointerAction, InputPointerEvent, InputSize, render_input,
};
use crate::ui::input_native::app_native_input;
use gpui::{
    Animation, AnimationExt, AnyElement, ClickEvent, Context, IntoElement, KeyDownEvent, div,
    prelude::*, px, rgb,
};
use std::ops::Range;
use std::time::Duration;

/// 渲染来源侧栏的局部工具按钮。
///
/// 参数说明：
/// - `app`：应用状态，提供主题令牌。
/// - `cx`：应用上下文，用于更新占位提示。
///
/// 返回值：GPUI 元素树；加载按钮会打开自定义来源选择器，其余按钮只更新本地状态。
pub(crate) fn render_source_toolbar(app: &ArgusApp, cx: &mut Context<ArgusApp>) -> AnyElement {
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
            "source-filter",
            ArgusIcon::Filter,
            "过滤",
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

/// 渲染链接工作区侧栏的局部工具按钮。
///
/// 参数说明：
/// - `app`：应用状态，提供主题和链接树搜索状态。
/// - `cx`：应用上下文，用于打开表单、过滤框或执行收起操作。
///
/// 返回值：GPUI 元素树；按钮顺序固定为新增链接、新增目录、过滤、收起全部。
pub(crate) fn render_connection_toolbar(app: &ArgusApp, cx: &mut Context<ArgusApp>) -> AnyElement {
    let theme = app.theme.clone();

    if app.is_connection_tree_search_open {
        return render_connection_search_toolbar(app, &theme, cx).into_any_element();
    }

    div()
        .flex()
        .items_center()
        .gap_1()
        .child(connection_icon_button(
            "connection-add-link",
            ArgusIcon::Link,
            "新增链接",
            &theme,
            cx,
        ))
        .child(connection_icon_button(
            "connection-add-directory",
            ArgusIcon::FolderPlus,
            "新增目录",
            &theme,
            cx,
        ))
        .child(connection_icon_button(
            "connection-filter",
            ArgusIcon::Filter,
            "过滤",
            &theme,
            cx,
        ))
        .child(connection_icon_button(
            "connection-collapse-all",
            ArgusIcon::ListCollapse,
            "全部收起",
            &theme,
            cx,
        ))
        .into_any_element()
}

/// 渲染日志内容区顶部的导航和上下文工具。
pub(crate) fn render_content_toolbar(
    app: &ArgusApp,
    cx: &mut Context<ArgusApp>,
) -> impl IntoElement {
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
    let app_entity = cx.entity();

    render_icon_button(
        id,
        icon,
        action_name,
        false,
        IconButtonSize::Small,
        theme,
        cx.listener(move |app, _event: &ClickEvent, window, cx| {
            match action_name {
                "加载日志" => app.request_load_sources(cx),
                "过滤" => {
                    app.open_source_tree_search();
                    let search_focus_handle = app
                        .ensure_input_focus_handles(cx)
                        .source_tree_search
                        .clone();
                    let app_entity = app_entity.clone();
                    window.on_next_frame(move |window, cx| {
                        search_focus_handle.focus(window);
                        // 根节点点击会在同一轮事件里清理输入焦点，这里下一帧恢复刚打开的过滤框。
                        let _ = app_entity.update(cx, |app, cx| {
                            app.set_source_tree_search_focused(true);
                            cx.notify();
                        });
                    });
                }
                "全部收起" => app.collapse_all_sources(),
                _ => app.mark_placeholder_action(action_name),
            }
            cx.notify();
        }),
    )
}

/// 渲染链接侧栏工具按钮。
fn connection_icon_button(
    id: &'static str,
    icon: ArgusIcon,
    action_name: &'static str,
    theme: &AppTheme,
    cx: &mut Context<ArgusApp>,
) -> impl IntoElement {
    let app_entity = cx.entity();

    render_icon_button(
        id,
        icon,
        action_name,
        false,
        IconButtonSize::Small,
        theme,
        cx.listener(move |app, event: &ClickEvent, window, cx| {
            match action_name {
                "新增链接" => app.open_connection_link_create_menu(event.position()),
                "新增目录" => app.open_new_connection_directory_dialog(cx),
                "过滤" => {
                    app.open_connection_tree_search();
                    let search_focus_handle = app
                        .ensure_input_focus_handles(cx)
                        .connection_tree_search
                        .clone();
                    let app_entity = app_entity.clone();
                    window.on_next_frame(move |window, cx| {
                        search_focus_handle.focus(window);
                        let _ = app_entity.update(cx, |app, cx| {
                            app.focus_connection_text_input_target(
                                AppTextInputTarget::ConnectionTreeSearch,
                            );
                            cx.notify();
                        });
                    });
                }
                "全部收起" => app.collapse_all_connections(),
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
    let native_input = app.input_focus_handles.as_ref().map(|handles| {
        app_native_input(
            cx.entity(),
            AppTextInputTarget::SourceTreeSearch,
            handles.source_tree_search.clone(),
        )
    });

    div()
        .w_full()
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
                marked_range: app.source_tree_search_marked_range.clone(),
                is_pointer_selecting: app.source_tree_search_selection_drag.is_some(),
                is_secret: false,
                size: InputSize::Compact,
                leading_accessory: Some(InputAccessory {
                    id: "source-tree-search-leading",
                    icon: ArgusIcon::Filter,
                    tooltip: "过滤",
                }),
                trailing_accessory: Some(InputAccessory {
                    id: "source-tree-search-close",
                    icon: ArgusIcon::Close,
                    tooltip: "关闭搜索",
                }),
                native_input,
            },
            theme,
            cx.listener(|app, event: &KeyDownEvent, _, cx| {
                cx.stop_propagation();
                app.handle_source_tree_search_key(&event.keystroke, cx);
                cx.notify();
            }),
            cx.listener(|app, _event: &ClickEvent, _, cx| {
                cx.stop_propagation();
                app.set_source_tree_search_focused(true);
                cx.notify();
            }),
            cx.listener(|app, event: &InputPointerEvent, _, cx| {
                match event.action {
                    InputPointerAction::Begin => app.begin_source_tree_search_pointer_selection(
                        event.character_index,
                        event.granularity,
                    ),
                    InputPointerAction::Extend => {
                        app.update_source_tree_search_pointer_selection(event.character_index)
                    }
                    InputPointerAction::Finish => app.finish_source_tree_search_pointer_selection(),
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
            |this, progress| this.opacity(progress),
        )
}

/// 渲染链接树工具栏的内联搜索输入框。
fn render_connection_search_toolbar(
    app: &ArgusApp,
    theme: &AppTheme,
    cx: &mut Context<ArgusApp>,
) -> impl IntoElement {
    let native_input = app.input_focus_handles.as_ref().map(|handles| {
        app_native_input(
            cx.entity(),
            AppTextInputTarget::ConnectionTreeSearch,
            handles.connection_tree_search.clone(),
        )
    });
    let input = &app.connection_tree_search_input;

    div()
        .w_full()
        .overflow_hidden()
        .child(render_input(
            Input {
                id: "connection-tree-search-input",
                placeholder: "过滤 SSH 链接",
                value: input.value.clone(),
                is_disabled: false,
                is_focused: input.is_focused,
                cursor_index: input.cursor,
                selection_range: input_selection_range(input),
                marked_range: input.marked_range.clone(),
                is_pointer_selecting: input.selection_drag.is_some(),
                is_secret: false,
                size: InputSize::Compact,
                leading_accessory: Some(InputAccessory {
                    id: "connection-tree-search-leading",
                    icon: ArgusIcon::Filter,
                    tooltip: "过滤",
                }),
                trailing_accessory: Some(InputAccessory {
                    id: "connection-tree-search-close",
                    icon: ArgusIcon::Close,
                    tooltip: "关闭搜索",
                }),
                native_input,
            },
            theme,
            cx.listener(|app, event: &KeyDownEvent, _, cx| {
                cx.stop_propagation();
                app.handle_connection_text_input_key(
                    AppTextInputTarget::ConnectionTreeSearch,
                    &event.keystroke,
                );
                cx.notify();
            }),
            cx.listener(|app, _event: &ClickEvent, _, cx| {
                cx.stop_propagation();
                app.focus_connection_text_input_target(AppTextInputTarget::ConnectionTreeSearch);
                cx.notify();
            }),
            cx.listener(|app, event: &InputPointerEvent, _, cx| {
                match event.action {
                    InputPointerAction::Begin => app.begin_connection_input_pointer_selection(
                        AppTextInputTarget::ConnectionTreeSearch,
                        event.character_index,
                        event.granularity,
                    ),
                    InputPointerAction::Extend => app.update_connection_input_pointer_selection(
                        AppTextInputTarget::ConnectionTreeSearch,
                        event.character_index,
                    ),
                    InputPointerAction::Finish => app.finish_connection_input_pointer_selection(
                        AppTextInputTarget::ConnectionTreeSearch,
                    ),
                }
                cx.notify();
            }),
            cx.listener(|app, _, _, cx| {
                app.close_connection_tree_search();
                cx.notify();
            }),
        ))
        .with_animation(
            ("connection-tree-search-open", input.cursor),
            Animation::new(Duration::from_millis(140)).with_easing(gpui::ease_out_quint()),
            |this, progress| this.opacity(progress),
        )
}

/// 返回输入框规范化后的非空选区。
fn input_selection_range(input: &crate::app::SettingsTextInputState) -> Option<Range<usize>> {
    let anchor = input.selection_anchor?;
    if anchor == input.cursor {
        return None;
    }
    Some(anchor.min(input.cursor)..anchor.max(input.cursor))
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
