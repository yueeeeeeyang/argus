//! 文件职责：渲染设置工作区的占位界面。
//! 创建日期：2026-06-09
//! 修改日期：2026-06-10
//! 作者：Argus 开发团队
//! 主要功能：展示主题、编码、缓存和快捷键设置分组，但不持久化任何配置。

use crate::app::{ArgusApp, ThemeMode};
use crate::ui::components::icon::{ArgusIcon, render_icon};
use crate::ui::components::icon_button::{IconButtonSize, render_icon_button};
use crate::ui::status_bar;
use gpui::{Context, IntoElement, SharedString, div, prelude::*, px, rgb};

/// 渲染设置页占位内容。
///
/// 参数说明：
/// - `app`：应用状态，提供主题和状态栏提示。
/// - `cx`：应用上下文，用于绑定设置项的本地状态变更。
///
/// 返回值：GPUI 元素树；所有设置项均为静态占位。
pub fn render(app: &ArgusApp, cx: &mut Context<ArgusApp>) -> impl IntoElement {
    let theme = app.theme.clone();

    div()
        .flex_1()
        .h_full()
        .flex()
        .flex_col()
        .bg(rgb(theme.content))
        .child(
            div()
                .h(px(42.0))
                .flex()
                .items_center()
                .px_4()
                .border_b_1()
                .border_color(rgb(theme.border))
                .text_color(rgb(theme.foreground_muted))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .child(render_icon(
                            ArgusIcon::Settings,
                            theme.foreground_muted,
                            18.0,
                        ))
                        .child("Argus / 设置"),
                ),
        )
        .child(
            div()
                .flex_1()
                .flex()
                .flex_col()
                .p_4()
                .gap_3()
                .child(theme_group(app, cx))
                .child(encoding_group(app, cx))
                .child(cache_group(app, cx))
                .child(shortcut_group(app)),
        )
        .child(status_bar::render(app))
}

/// 渲染设置分组容器。
fn setting_group(
    icon: ArgusIcon,
    title: &'static str,
    description: &'static str,
    app: &ArgusApp,
    content: impl IntoElement,
) -> impl IntoElement {
    let theme = app.theme.clone();

    div()
        .w_full()
        .mb_3()
        .p_3()
        .rounded_sm()
        .border_1()
        .border_color(rgb(theme.border))
        .bg(rgb(0x242424))
        .child(
            div()
                .flex()
                .items_center()
                .justify_between()
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .child(render_icon(icon, theme.foreground_muted, 18.0))
                        .child(div().text_color(rgb(theme.foreground)).child(title)),
                )
                .child(content),
        )
        .child(
            div()
                .mt_2()
                .text_sm()
                .text_color(rgb(theme.foreground_muted))
                .child(description),
        )
}

/// 渲染主题模式分段控件。
fn theme_group(app: &ArgusApp, cx: &mut Context<ArgusApp>) -> impl IntoElement {
    setting_group(
        ArgusIcon::Palette,
        "主题",
        "当前只改变设置页本地状态，不切换真实主题令牌。",
        app,
        div()
            .flex()
            .items_center()
            .gap_1()
            .child(theme_segment("theme-system", ThemeMode::System, app, cx))
            .child(theme_segment("theme-dark", ThemeMode::Dark, app, cx))
            .child(theme_segment("theme-light", ThemeMode::Light, app, cx)),
    )
}

/// 渲染单个主题分段按钮。
fn theme_segment(
    id: &'static str,
    mode: ThemeMode,
    app: &ArgusApp,
    cx: &mut Context<ArgusApp>,
) -> impl IntoElement {
    let theme = app.theme.clone();
    let is_selected = app.theme_mode == mode;
    let background = if is_selected {
        theme.selection
    } else {
        theme.content
    };

    div()
        .id(id)
        .h(px(26.0))
        .px_2()
        .flex()
        .items_center()
        .rounded_sm()
        .border_1()
        .border_color(rgb(theme.border))
        .bg(rgb(background))
        .text_xs()
        .text_color(rgb(theme.foreground))
        .cursor_pointer()
        .child(mode.label())
        .on_click(cx.listener(move |app, _, _, cx| {
            app.set_theme_mode(mode);
            cx.notify();
        }))
}

/// 渲染编码选择占位控件。
fn encoding_group(app: &ArgusApp, cx: &mut Context<ArgusApp>) -> impl IntoElement {
    let theme = app.theme.clone();

    setting_group(
        ArgusIcon::Type,
        "编码",
        "编码选择只保存在内存中，不触发日志重新解码。",
        app,
        div()
            .flex()
            .items_center()
            .gap_2()
            .child(
                div()
                    .h(px(26.0))
                    .px_2()
                    .flex()
                    .items_center()
                    .rounded_sm()
                    .border_1()
                    .border_color(rgb(theme.border))
                    .bg(rgb(theme.content))
                    .text_xs()
                    .text_color(rgb(theme.foreground))
                    .child(app.selected_encoding.clone()),
            )
            .child(render_icon_button(
                "encoding-cycle",
                ArgusIcon::Refresh,
                "切换编码",
                false,
                IconButtonSize::Small,
                &theme,
                cx.listener(|app, _, _, cx| {
                    app.cycle_encoding();
                    cx.notify();
                }),
            )),
    )
}

/// 渲染缓存开关与上限步进控件。
fn cache_group(app: &ArgusApp, cx: &mut Context<ArgusApp>) -> impl IntoElement {
    let theme = app.theme.clone();
    let toggle_icon = if app.is_cache_enabled {
        ArgusIcon::ToggleRight
    } else {
        ArgusIcon::ToggleLeft
    };

    setting_group(
        ArgusIcon::Database,
        "缓存",
        "临时缓存设置只影响本地展示状态，不创建临时文件。",
        app,
        div()
            .flex()
            .items_center()
            .gap_2()
            .child(render_icon_button(
                "cache-toggle",
                toggle_icon,
                "切换缓存",
                app.is_cache_enabled,
                IconButtonSize::Small,
                &theme,
                cx.listener(|app, _, _, cx| {
                    app.toggle_cache_enabled();
                    cx.notify();
                }),
            ))
            .child(render_icon_button(
                "cache-minus",
                ArgusIcon::Minus,
                "减少缓存上限",
                false,
                IconButtonSize::Small,
                &theme,
                cx.listener(|app, _, _, cx| {
                    app.adjust_cache_limit(-128);
                    cx.notify();
                }),
            ))
            .child(
                div()
                    .h(px(26.0))
                    .px_2()
                    .flex()
                    .items_center()
                    .rounded_sm()
                    .bg(rgb(theme.content))
                    .text_xs()
                    .text_color(rgb(theme.foreground))
                    .child(format!("{} MB", app.cache_limit_mb)),
            )
            .child(render_icon_button(
                "cache-plus",
                ArgusIcon::Plus,
                "增加缓存上限",
                false,
                IconButtonSize::Small,
                &theme,
                cx.listener(|app, _, _, cx| {
                    app.adjust_cache_limit(128);
                    cx.notify();
                }),
            )),
    )
}

/// 渲染快捷键分组，当前仅展示可扫描的真实列表样式。
fn shortcut_group(app: &ArgusApp) -> impl IntoElement {
    setting_group(
        ArgusIcon::Keyboard,
        "快捷键",
        "快捷键注册将在功能模块实现阶段接入；当前仅展示占位映射。",
        app,
        div()
            .flex()
            .items_center()
            .gap_2()
            .child(shortcut_badge("Cmd+O", app))
            .child(shortcut_badge("Cmd+F", app))
            .child(shortcut_badge("Cmd+G", app)),
    )
}

/// 渲染单个快捷键徽标。
fn shortcut_badge(label: &'static str, app: &ArgusApp) -> impl IntoElement {
    let theme = app.theme.clone();

    div()
        .id(SharedString::from(format!("shortcut-{label}")))
        .h(px(24.0))
        .px_2()
        .flex()
        .items_center()
        .rounded_sm()
        .border_1()
        .border_color(rgb(theme.border))
        .bg(rgb(theme.content))
        .text_xs()
        .text_color(rgb(theme.foreground_muted))
        .child(label)
}
