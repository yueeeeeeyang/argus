//! 文件职责：渲染日志分析工作区的主内容区域。
//! 创建日期：2026-06-09
//! 修改日期：2026-06-10
//! 作者：Argus 开发团队
//! 主要功能：展示日志行预览和真实来源未读取提示。

use crate::app::{ArgusApp, ContentState, LogLine};
use crate::theme::AppTheme;
use crate::ui::components::icon::ArgusIcon;
use crate::ui::components::icon::render_icon;
use crate::ui::components::icon_button::{IconButtonSize, render_icon_button};
use gpui::{
    AnyElement, Context, IntoElement, KeyDownEvent, SharedString, div, prelude::*, px, rgb,
};

/// 渲染日志内容区。
///
/// 参数说明：
/// - `app`：应用状态，提供日志行、主题和状态提示。
/// - `cx`：应用上下文，用于内容区工具按钮的占位回调。
///
/// 返回值：GPUI 元素树；真实来源仅展示未读取提示，不读取文件正文。
pub fn render(app: &ArgusApp, cx: &mut Context<ArgusApp>) -> impl IntoElement {
    let theme = app.theme.clone();

    div()
        .flex_1()
        .h_full()
        .flex()
        .flex_col()
        .overflow_hidden()
        .bg(rgb(theme.content))
        .when(app.is_search_panel_open, |this| {
            this.child(render_search_panel(app, cx))
        })
        .child(render_content_body(app, &theme, cx))
}

/// 根据当前内容状态渲染主体区域。
fn render_content_body(app: &ArgusApp, theme: &AppTheme, cx: &mut Context<ArgusApp>) -> AnyElement {
    match &app.content_state {
        ContentState::PlaceholderPreview => {
            let log_rows = app
                .logs
                .iter()
                .enumerate()
                .map(|(index, line)| render_log_row(index, line, app, theme, cx).into_any_element())
                .collect::<Vec<_>>();

            div()
                .flex_1()
                .bg(rgb(theme.content))
                .overflow_hidden()
                .children(log_rows)
                .into_any_element()
        }
        ContentState::SourceNotSelected => render_empty_state(
            "请选择日志来源",
            "左侧来源树已经接入真实结构，选择日志文件后会在此处显示未读取提示。",
            theme,
        ),
        ContentState::SourceNotRead { label, path, .. } => render_empty_state(
            &format!("{label} 已选中"),
            &format!("真实来源路径：{path}。日志内容读取模块尚未接入，本轮只加载来源结构。"),
            theme,
        ),
    }
}

/// 渲染内容区空状态或未读取提示。
fn render_empty_state(title: &str, detail: &str, theme: &AppTheme) -> AnyElement {
    div()
        .flex_1()
        .flex()
        .items_center()
        .justify_center()
        .overflow_hidden()
        .bg(rgb(theme.content))
        .child(
            div()
                .w(px(520.0))
                .max_w_full()
                .px_6()
                .flex()
                .flex_col()
                .items_center()
                .gap_3()
                .text_center()
                .child(
                    div()
                        .text_size(px(18.0))
                        .text_color(rgb(theme.foreground))
                        .child(title.to_string()),
                )
                .child(
                    div()
                        .text_sm()
                        .text_color(rgb(theme.foreground_muted))
                        .child(detail.to_string()),
                ),
        )
        .into_any_element()
}

/// 渲染单行日志，占位行只体现级别、行号和文本密度。
fn render_log_row(
    index: usize,
    line: &LogLine,
    app: &ArgusApp,
    theme: &AppTheme,
    cx: &mut Context<ArgusApp>,
) -> impl IntoElement {
    let is_selected = app.selected_log_line == Some(line.number);
    let background = if is_selected {
        theme.current_line
    } else {
        theme.content
    };
    let line_number = line.number;

    div()
        .id(SharedString::from(format!("log-row-{index}")))
        .h(px(28.0))
        .flex()
        .items_center()
        .border_b_1()
        .border_color(rgb(0x2b2b2b))
        .bg(rgb(background))
        .overflow_hidden()
        .text_sm()
        .cursor_pointer()
        .child(
            div()
                .w(px(64.0))
                .px_3()
                .text_color(rgb(theme.foreground_muted))
                .child(format!("{:>4}", line.number)),
        )
        .child(
            div()
                .w(px(72.0))
                .text_color(rgb(theme.color_for_level(&line.level)))
                .child(line.level.clone()),
        )
        .child(
            div()
                .flex_1()
                .truncate()
                .text_color(rgb(theme.foreground))
                .child(line.message.clone()),
        )
        .on_click(cx.listener(move |app, _, _, cx| {
            app.select_log_line(line_number);
            cx.notify();
        }))
}

/// 渲染本地可输入的搜索面板，不执行真实日志扫描。
fn render_search_panel(app: &ArgusApp, cx: &mut Context<ArgusApp>) -> impl IntoElement {
    let theme = app.theme.clone();
    let query_text = if app.search_query.is_empty() {
        "输入关键字后按 Enter 预览占位搜索".to_string()
    } else {
        app.search_query.clone()
    };
    let query_color = if app.search_query.is_empty() {
        theme.foreground_muted
    } else {
        theme.foreground
    };

    div()
        .h(px(46.0))
        .px_3()
        .flex()
        .items_center()
        .gap_2()
        .border_b_1()
        .border_color(rgb(theme.border))
        .overflow_hidden()
        .bg(rgb(0x242424))
        .child(render_icon(ArgusIcon::Search, theme.foreground_muted, 18.0))
        .child(
            div()
                .id("search-input")
                .flex_1()
                .h(px(30.0))
                .px_2()
                .flex()
                .items_center()
                .rounded_sm()
                .border_1()
                .border_color(rgb(theme.border))
                .bg(rgb(theme.content))
                .text_sm()
                .text_color(rgb(query_color))
                .focusable()
                .on_key_down(cx.listener(|app, event: &KeyDownEvent, _, cx| {
                    cx.stop_propagation();
                    app.handle_search_key(&event.keystroke);
                    cx.notify();
                }))
                .child(query_text),
        )
        .child(render_icon_button(
            "search-case",
            ArgusIcon::CaseSensitive,
            "大小写匹配",
            app.is_case_sensitive,
            IconButtonSize::Small,
            &theme,
            cx.listener(|app, _, _, cx| {
                app.toggle_search_option("case");
                cx.notify();
            }),
        ))
        .child(render_icon_button(
            "search-regex",
            ArgusIcon::Regex,
            "正则搜索",
            app.is_regex_enabled,
            IconButtonSize::Small,
            &theme,
            cx.listener(|app, _, _, cx| {
                app.toggle_search_option("regex");
                cx.notify();
            }),
        ))
        .child(render_icon_button(
            "search-whole-word",
            ArgusIcon::WholeWord,
            "全词匹配",
            app.is_whole_word_enabled,
            IconButtonSize::Small,
            &theme,
            cx.listener(|app, _, _, cx| {
                app.toggle_search_option("whole");
                cx.notify();
            }),
        ))
        .child(render_icon_button(
            "search-clear",
            ArgusIcon::Close,
            "清空搜索",
            false,
            IconButtonSize::Small,
            &theme,
            cx.listener(|app, _, _, cx| {
                app.clear_search_query();
                cx.notify();
            }),
        ))
}
