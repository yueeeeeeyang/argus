//! 文件职责：渲染设置标签页内容。
//! 创建日期：2026-06-10
//! 修改日期：2026-06-10
//! 作者：Argus 开发团队
//! 主要功能：在单页设置视图中集中展示关于、外观和日志加载设置。

use crate::app::{ArgusApp, ThemeMode};
use crate::theme::AppTheme;
use crate::ui::components::icon::{ArgusIcon, render_icon};
use crate::ui::components::icon_button::{IconButtonSize, render_icon_button};
use gpui::{AnyElement, Context, IntoElement, div, prelude::*, px, rgb};

/// 设置页主体最大宽度，避免在大窗口中过度拉伸。
const SETTINGS_CONTENT_MAX_WIDTH: f32 = 720.0;

/// 渲染设置标签页。
///
/// 参数说明：
/// - `app`：应用状态，提供当前设置项和本地设置值。
/// - `cx`：应用上下文，用于绑定设置项交互。
///
/// 返回值：主内容区内的 GPUI 元素树；设置项修改会同步写入 `~/.argus/settings.toml`。
pub fn render(app: &ArgusApp, cx: &mut Context<ArgusApp>) -> impl IntoElement {
    let theme = app.theme.clone();
    div()
        .flex_1()
        .h_full()
        .flex()
        .flex_col()
        .overflow_hidden()
        .bg(rgb(theme.content))
        .child(
            div()
                .h(px(40.0))
                .w_full()
                .flex()
                .items_center()
                .justify_center()
                .child(
                    div()
                        .w(px(SETTINGS_CONTENT_MAX_WIDTH))
                        .max_w_full()
                        .px_4()
                        .flex()
                        .items_center()
                        .gap_2()
                        .text_size(px(13.0))
                        .text_color(rgb(theme.foreground_muted))
                        .child(render_icon(
                            ArgusIcon::Settings,
                            theme.foreground_muted,
                            14.0,
                        ))
                        .child("设置"),
                ),
        )
        .child(
            div()
                .id("settings-page-scroll")
                .flex_1()
                .overflow_y_scroll()
                .child(
                    div().w_full().flex().justify_center().child(
                        div()
                            .w(px(SETTINGS_CONTENT_MAX_WIDTH))
                            .max_w_full()
                            .px_4()
                            .pb_4()
                            .flex()
                            .flex_col()
                            .gap_5()
                            .child(settings_section(
                                "关于",
                                ArgusIcon::Info,
                                render_about_section(&theme),
                                &theme,
                            ))
                            .child(settings_section(
                                "外观",
                                ArgusIcon::Palette,
                                render_appearance_section(app, &theme, cx),
                                &theme,
                            ))
                            .child(settings_section(
                                "日志加载",
                                ArgusIcon::FolderPlus,
                                render_log_loading_section(app, &theme, cx),
                                &theme,
                            )),
                    ),
                ),
        )
}

/// 渲染单页设置中的分组标题与内容。
fn settings_section(
    title: &'static str,
    icon: ArgusIcon,
    content: AnyElement,
    theme: &AppTheme,
) -> impl IntoElement {
    div()
        .w_full()
        .flex()
        .flex_col()
        .gap_2()
        .child(
            div()
                .h(px(28.0))
                .flex()
                .items_center()
                .gap_2()
                .text_size(px(13.0))
                .text_color(rgb(theme.foreground))
                .child(render_icon(icon, theme.foreground_muted, 14.0))
                .child(title),
        )
        .child(content)
}

/// 渲染关于页面，当前只展示程序版本。
fn render_about_section(theme: &AppTheme) -> AnyElement {
    setting_group(theme)
        .child(setting_row(
            "程序版本",
            text_value(env!("CARGO_PKG_VERSION"), theme).into_any_element(),
            theme,
        ))
        .into_any_element()
}

/// 渲染外观页面，包含主题选择和日志内容字号设置。
fn render_appearance_section(
    app: &ArgusApp,
    theme: &AppTheme,
    cx: &mut Context<ArgusApp>,
) -> AnyElement {
    setting_group(theme)
        .child(setting_row(
            "主题",
            div()
                .flex()
                .items_center()
                .gap_1()
                .child(theme_segment(
                    "settings-theme-system",
                    ThemeMode::System,
                    app,
                    theme,
                    cx,
                ))
                .child(theme_segment(
                    "settings-theme-dark",
                    ThemeMode::Dark,
                    app,
                    theme,
                    cx,
                ))
                .child(theme_segment(
                    "settings-theme-light",
                    ThemeMode::Light,
                    app,
                    theme,
                    cx,
                ))
                .into_any_element(),
            theme,
        ))
        .child(setting_row(
            "日志内容字号",
            font_size_control(app, theme, cx).into_any_element(),
            theme,
        ))
        .into_any_element()
}

/// 渲染日志加载页面，提供会影响后续来源加载任务的持久化设置。
fn render_log_loading_section(
    app: &ArgusApp,
    theme: &AppTheme,
    cx: &mut Context<ArgusApp>,
) -> AnyElement {
    setting_group(theme)
        .child(setting_row(
            "嵌套压缩包深度",
            archive_depth_control(app, theme, cx).into_any_element(),
            theme,
        ))
        .child(setting_row(
            "符号链接策略",
            follow_symlink_control(app, theme, cx).into_any_element(),
            theme,
        ))
        .into_any_element()
}

/// 渲染设置详情页的紧凑设置组。
fn setting_group(_theme: &AppTheme) -> gpui::Div {
    div().w_full().flex().flex_col().gap_2().text_size(px(13.0))
}

/// 渲染设置项标签。
fn setting_label(label: &'static str, theme: &AppTheme) -> impl IntoElement {
    div()
        .text_size(px(13.0))
        .text_color(rgb(theme.foreground))
        .child(label)
}

/// 渲染左右展示的设置行。
fn setting_row(label: &'static str, control: AnyElement, theme: &AppTheme) -> impl IntoElement {
    div()
        .min_h(px(42.0))
        .px_3()
        .flex()
        .items_center()
        .justify_between()
        .gap_3()
        .rounded_sm()
        .bg(rgb(theme.current_line))
        .child(setting_label(label, theme))
        .child(control)
}

/// 渲染只读文本值。
fn text_value(value: &str, theme: &AppTheme) -> impl IntoElement {
    div()
        .text_size(px(12.0))
        .text_color(rgb(theme.foreground_muted))
        .child(value.to_string())
}

/// 渲染主题模式分段按钮。
fn theme_segment(
    id: &'static str,
    mode: ThemeMode,
    app: &ArgusApp,
    theme: &AppTheme,
    cx: &mut Context<ArgusApp>,
) -> impl IntoElement {
    let is_selected = app.theme_mode == mode;
    let background = if is_selected {
        theme.selection
    } else {
        theme.content
    };

    div()
        .id(id)
        .h(px(26.0))
        .px_3()
        .flex()
        .items_center()
        .rounded_sm()
        .bg(rgb(background))
        .text_size(px(12.0))
        .text_color(rgb(theme.foreground))
        .cursor_pointer()
        .hover(move |this| {
            if is_selected {
                this.bg(rgb(theme.selection))
            } else {
                this.bg(rgb(theme.current_line))
            }
        })
        .child(mode.label())
        .on_click(cx.listener(move |app, _, _, cx| {
            app.set_theme_mode(mode, cx.window_appearance());
            cx.notify();
        }))
}

/// 渲染日志内容字号步进控件。
fn font_size_control(
    app: &ArgusApp,
    theme: &AppTheme,
    cx: &mut Context<ArgusApp>,
) -> impl IntoElement {
    div()
        .flex()
        .items_center()
        .gap_2()
        .child(render_icon_button(
            "log-font-size-minus",
            ArgusIcon::Minus,
            "减小日志字号",
            false,
            IconButtonSize::Small,
            theme,
            cx.listener(|app, _, _, cx| {
                app.adjust_log_content_font_size(-1.0);
                cx.notify();
            }),
        ))
        .child(
            div()
                .w(px(78.0))
                .h(px(26.0))
                .flex()
                .items_center()
                .justify_center()
                .rounded_sm()
                .bg(rgb(theme.content))
                .text_size(px(12.0))
                .text_color(rgb(theme.foreground))
                .child(format!("{:.0}px", app.log_content_font_size)),
        )
        .child(render_icon_button(
            "log-font-size-plus",
            ArgusIcon::Plus,
            "增大日志字号",
            false,
            IconButtonSize::Small,
            theme,
            cx.listener(|app, _, _, cx| {
                app.adjust_log_content_font_size(1.0);
                cx.notify();
            }),
        ))
}

/// 渲染嵌套压缩包深度步进控件。
fn archive_depth_control(
    app: &ArgusApp,
    theme: &AppTheme,
    cx: &mut Context<ArgusApp>,
) -> impl IntoElement {
    div()
        .flex()
        .items_center()
        .gap_2()
        .child(render_icon_button(
            "archive-depth-minus",
            ArgusIcon::Minus,
            "减少嵌套压缩包深度",
            false,
            IconButtonSize::Small,
            theme,
            cx.listener(|app, _, _, cx| {
                app.adjust_max_archive_depth(-1);
                cx.notify();
            }),
        ))
        .child(
            div()
                .w(px(78.0))
                .h(px(26.0))
                .flex()
                .items_center()
                .justify_center()
                .rounded_sm()
                .bg(rgb(theme.content))
                .text_size(px(12.0))
                .text_color(rgb(theme.foreground))
                .child(format!("{} 层", app.config.loader.max_archive_depth)),
        )
        .child(render_icon_button(
            "archive-depth-plus",
            ArgusIcon::Plus,
            "增加嵌套压缩包深度",
            false,
            IconButtonSize::Small,
            theme,
            cx.listener(|app, _, _, cx| {
                app.adjust_max_archive_depth(1);
                cx.notify();
            }),
        ))
}

/// 渲染符号链接跟随策略开关。
fn follow_symlink_control(
    app: &ArgusApp,
    theme: &AppTheme,
    cx: &mut Context<ArgusApp>,
) -> impl IntoElement {
    let toggle_icon = if app.config.loader.follow_symlinks {
        ArgusIcon::ToggleRight
    } else {
        ArgusIcon::ToggleLeft
    };
    let policy_text = if app.config.loader.follow_symlinks {
        "跟随"
    } else {
        "不跟随"
    };

    div()
        .flex()
        .items_center()
        .gap_2()
        .child(
            div()
                .w(px(58.0))
                .h(px(26.0))
                .flex()
                .items_center()
                .justify_center()
                .rounded_sm()
                .bg(rgb(theme.content))
                .text_size(px(12.0))
                .text_color(rgb(theme.foreground))
                .child(policy_text),
        )
        .child(render_icon_button(
            "follow-symlink-toggle",
            toggle_icon,
            "切换符号链接策略",
            app.config.loader.follow_symlinks,
            IconButtonSize::Small,
            theme,
            cx.listener(|app, _, _, cx| {
                app.toggle_follow_symlinks();
                cx.notify();
            }),
        ))
}
