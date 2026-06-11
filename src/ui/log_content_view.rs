//! 文件职责：渲染日志分析工作区的主内容区域。
//! 创建日期：2026-06-09
//! 修改日期：2026-06-11
//! 作者：Argus 开发团队
//! 主要功能：按行虚拟渲染日志正文，大日志只读取当前可见页，避免整份日志进入 UI 文本节点。

use std::ops::Range;

use crate::app::{
    ArgusApp, LOG_VIEWER_TEXT_LEFT_PADDING, LOG_VIEWER_TEXT_RIGHT_PADDING, LogScrollbarAxis,
    LogScrollbarDrag, TabKind, log_viewer_display_text, log_viewer_line_number_width,
};
use crate::fonts::ARGUS_LOG_FONT_FAMILY;
use crate::highlight::{
    HighlightCache, HighlightLanguage, HighlightSpan, HighlightTokenKind, detect_highlight_language,
};
use crate::reader::log_file_reader::{
    DisplayedLogLine, LogDocument, LogOpenState, LogReaderHandle,
};
use crate::theme::AppTheme;
use crate::ui::components::icon::ArgusIcon;
use crate::ui::components::icon::render_icon;
use crate::ui::components::icon_button::{IconButtonSize, render_icon_button};
use crate::ui::components::loading_spinner::render_loading_spinner;
use crate::ui::settings_page;
use gpui::{
    AnyElement, Context, HighlightStyle, IntoElement, KeyDownEvent, ListHorizontalSizingBehavior,
    MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, ScrollWheelEvent, SharedString,
    StyledText, Window, canvas, div, point, prelude::*, px, rgb, uniform_list,
};

/// 日志正文固定行高；虚拟列表和分页窗口都依赖该值稳定换算。
const LOG_VIEWER_ROW_HEIGHT: f32 = 20.0;
/// 首帧视口未测量时的默认渲染行数。
const DEFAULT_VISIBLE_ROWS: usize = 80;
/// 自绘滚动条宽度。
const LOG_SCROLLBAR_WIDTH: f32 = 5.0;
/// 自绘滚动条边距。
const LOG_SCROLLBAR_PADDING: f32 = 4.0;
/// 自绘滚动条最小滑块长度。
const LOG_SCROLLBAR_MIN_THUMB: f32 = 32.0;

/// 滚动条渲染和拖拽所需的度量数据。
#[derive(Clone, Copy, Debug)]
struct LogScrollbarMetrics {
    /// 滑块起点。
    thumb_start: gpui::Pixels,
    /// 滑块长度。
    thumb_length: gpui::Pixels,
    /// 轨道起点。
    track_start: gpui::Pixels,
    /// 轨道长度。
    track_length: gpui::Pixels,
    /// 最大滚动距离。
    max_scroll: gpui::Pixels,
}

/// 渲染日志内容区。
///
/// 参数说明：
/// - `app`：应用状态，提供日志文档、主题和状态提示。
/// - `cx`：应用上下文，用于内容区工具按钮和日志选择事件。
///
/// 返回值：GPUI 元素树；真实来源会按读取状态展示加载、失败或逐行日志。
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
    match app.active_tab_kind() {
        TabKind::Settings => settings_page::render(app, cx).into_any_element(),
        TabKind::LogSource { source_id, path } => {
            let tab_id = app.active_tab().map(|tab| tab.id).unwrap_or_default();
            render_log_source_content(app, theme, tab_id, source_id, &path, cx)
        }
        TabKind::Empty => render_empty_state(
            "请选择日志来源",
            "左侧来源树已经接入真实结构，选择日志文件后会在此处显示真实内容。",
            app,
            theme,
        ),
    }
}

/// 渲染当前日志 tab 对应的读取状态或真实日志文本。
fn render_log_source_content(
    app: &ArgusApp,
    theme: &AppTheme,
    tab_id: usize,
    source_id: crate::loader::SourceId,
    path: &str,
    cx: &mut Context<ArgusApp>,
) -> AnyElement {
    match app.log_read_state(source_id) {
        Some(LogOpenState::Ready(handle)) if !handle.is_empty() => {
            render_log_document(app, theme, tab_id, source_id, handle, cx)
        }
        Some(LogOpenState::Ready(handle)) => render_empty_state(
            "日志为空",
            &format!("{} 已读取完成，但没有可展示的日志行。", handle.path),
            app,
            theme,
        ),
        Some(LogOpenState::Loading { message, .. }) => {
            render_loading_state("正在读取日志", message, source_id, app, theme)
        }
        Some(LogOpenState::Failed { message, .. }) => {
            render_empty_state("日志读取失败", message, app, theme)
        }
        Some(LogOpenState::Idle) | None => render_empty_state(
            "等待读取日志",
            &format!("真实来源路径：{path}。请从左侧来源树选择该日志以启动读取。"),
            app,
            theme,
        ),
    }
}

/// 渲染日志文档；小文件走 uniform_list，大文件走分页窗口。
fn render_log_document(
    app: &ArgusApp,
    theme: &AppTheme,
    tab_id: usize,
    source_id: crate::loader::SourceId,
    handle: &LogReaderHandle,
    cx: &mut Context<ArgusApp>,
) -> AnyElement {
    let language = detect_highlight_language(&handle.label, &handle.path);
    match handle.document() {
        LogDocument::InMemory(_) => {
            render_in_memory_log(app, theme, tab_id, source_id, handle, language, cx)
        }
        LogDocument::Paged(_) => {
            render_paged_log(app, theme, tab_id, source_id, handle, language, cx)
        }
    }
}

/// 渲染小日志内存文档，使用 GPUI 虚拟列表只创建可见行元素。
fn render_in_memory_log(
    app: &ArgusApp,
    theme: &AppTheme,
    tab_id: usize,
    source_id: crate::loader::SourceId,
    handle: &LogReaderHandle,
    language: HighlightLanguage,
    cx: &mut Context<ArgusApp>,
) -> AnyElement {
    let Some(state) = app.log_tab_view_state(tab_id) else {
        return render_empty_state("日志视图未初始化", "请重新选择该日志。", app, theme);
    };
    let line_count = handle.line_count();
    let line_number_width = log_viewer_line_number_width(line_count);
    let measure_line = handle
        .document()
        .longest_line_text()
        .map(|_| longest_line_index(handle.document()))
        .unwrap_or(0);

    div()
        .id(SharedString::from(format!("log-viewer-{tab_id}")))
        .relative()
        .flex_1()
        .overflow_hidden()
        .bg(rgb(theme.content))
        .focusable()
        .on_key_down(cx.listener(|app, event: &KeyDownEvent, _, cx| {
            cx.stop_propagation();
            app.handle_log_text_key(&event.keystroke, cx);
            cx.notify();
        }))
        .on_mouse_up(
            MouseButton::Left,
            cx.listener(move |app, _: &MouseUpEvent, _, cx| {
                app.finish_log_text_selection(tab_id);
                cx.notify();
            }),
        )
        .on_mouse_up_out(
            MouseButton::Left,
            cx.listener(move |app, _: &MouseUpEvent, _, cx| {
                app.finish_log_text_selection(tab_id);
                cx.notify();
            }),
        )
        .child(
            uniform_list(
                SharedString::from(format!("log-lines-{tab_id}")),
                line_count,
                cx.processor(move |app, range: Range<usize>, _window, cx| {
                    render_log_line_range(app, source_id, tab_id, range, language, cx)
                }),
            )
            .with_width_from_item(Some(measure_line))
            .with_horizontal_sizing_behavior(ListHorizontalSizingBehavior::Unconstrained)
            .size_full()
            .track_scroll(state.scroll_handle.clone()),
        )
        .children(render_in_memory_scrollbars(
            tab_id,
            state,
            line_number_width,
            theme,
            cx,
        ))
        .into_any_element()
}

/// 渲染分页大日志；只读取当前视口附近的真实行。
fn render_paged_log(
    app: &ArgusApp,
    theme: &AppTheme,
    tab_id: usize,
    source_id: crate::loader::SourceId,
    handle: &LogReaderHandle,
    language: HighlightLanguage,
    cx: &mut Context<ArgusApp>,
) -> AnyElement {
    let Some(state) = app.log_tab_view_state(tab_id) else {
        return render_empty_state("日志视图未初始化", "请重新选择该日志。", app, theme);
    };
    let viewport_bounds = state.paged_viewport_handle.bounds();
    let viewport_height = viewport_bounds.size.height;
    let visible_rows = visible_row_capacity(viewport_height);
    let line_count = handle.line_count();
    let max_scroll_top = paged_vertical_max_scroll(line_count, viewport_height);
    let scroll_top = state.paged_scroll.top_px.clamp(0.0, max_scroll_top);
    let first_line_index = (scroll_top / LOG_VIEWER_ROW_HEIGHT as f64).floor() as usize;
    let fractional_top = (scroll_top % LOG_VIEWER_ROW_HEIGHT as f64) as f32;
    let lines = handle
        .lines(first_line_index, visible_rows)
        .unwrap_or_default();
    let horizontal_offset = px(-(state.paged_scroll.left_px as f32));
    let line_number_width = log_viewer_line_number_width(line_count);
    let rows = lines
        .into_iter()
        .filter_map(|line| {
            let row_offset = line.line_number.saturating_sub(first_line_index);
            let top = row_offset as f32 * LOG_VIEWER_ROW_HEIGHT - fractional_top;
            Some(
                div()
                    .absolute()
                    .left(px(0.0))
                    .right(px(0.0))
                    .top(px(top))
                    .h(px(LOG_VIEWER_ROW_HEIGHT))
                    .child(render_log_line(
                        app,
                        theme,
                        tab_id,
                        line,
                        language,
                        &state.highlight_cache,
                        horizontal_offset,
                        px(0.0),
                        line_number_width,
                        cx,
                    ))
                    .into_any_element(),
            )
        })
        .collect::<Vec<_>>();

    div()
        .id(SharedString::from(format!("paged-log-viewer-{tab_id}")))
        .relative()
        .flex_1()
        .overflow_hidden()
        .bg(rgb(theme.content))
        .focusable()
        .track_scroll(&state.paged_viewport_handle)
        .on_scroll_wheel(cx.listener(move |app, event: &ScrollWheelEvent, _, cx| {
            cx.stop_propagation();
            app.scroll_paged_log(tab_id, source_id, event);
            cx.notify();
        }))
        .on_key_down(cx.listener(|app, event: &KeyDownEvent, _, cx| {
            cx.stop_propagation();
            app.handle_log_text_key(&event.keystroke, cx);
            cx.notify();
        }))
        .on_mouse_up(
            MouseButton::Left,
            cx.listener(move |app, _: &MouseUpEvent, _, cx| {
                app.finish_log_text_selection(tab_id);
                cx.notify();
            }),
        )
        .on_mouse_up_out(
            MouseButton::Left,
            cx.listener(move |app, _: &MouseUpEvent, _, cx| {
                app.finish_log_text_selection(tab_id);
                cx.notify();
            }),
        )
        .children(rows)
        .children(render_paged_scrollbars(
            app, tab_id, source_id, handle, state, theme, cx,
        ))
        .into_any_element()
}

/// 通过当前 app 状态读取并渲染一个虚拟列表范围。
fn render_log_line_range(
    app: &ArgusApp,
    source_id: crate::loader::SourceId,
    tab_id: usize,
    range: Range<usize>,
    language: HighlightLanguage,
    cx: &mut Context<ArgusApp>,
) -> Vec<AnyElement> {
    let theme = app.theme.clone();
    let Some(LogOpenState::Ready(handle)) = app.log_read_state(source_id) else {
        return Vec::new();
    };
    let lines = handle
        .lines(range.start, range.end.saturating_sub(range.start))
        .unwrap_or_default();
    let line_number_width = log_viewer_line_number_width(handle.line_count());
    let Some(state) = app.log_tab_view_state(tab_id) else {
        return Vec::new();
    };
    let highlight_cache = state.highlight_cache.clone();
    let line_number_offset = app
        .log_tab_view_state(tab_id)
        .map(|state| {
            let offset = state
                .scroll_handle
                .0
                .as_ref()
                .borrow()
                .base_handle
                .offset()
                .x;
            px(0.0) - offset
        })
        .unwrap_or_else(|| px(0.0));

    lines
        .into_iter()
        .map(|line| {
            render_log_line(
                app,
                &theme,
                tab_id,
                line,
                language,
                &highlight_cache,
                px(0.0),
                line_number_offset,
                line_number_width,
                cx,
            )
            .into_any_element()
        })
        .collect()
}

/// 渲染单行日志文本并绑定选择事件。
fn render_log_line(
    app: &ArgusApp,
    theme: &AppTheme,
    tab_id: usize,
    line: DisplayedLogLine,
    language: HighlightLanguage,
    highlight_cache: &HighlightCache,
    horizontal_offset: gpui::Pixels,
    line_number_offset: gpui::Pixels,
    line_number_width: f32,
    cx: &mut Context<ArgusApp>,
) -> impl IntoElement {
    let line_for_down = line.text.clone();
    let line_for_move = line.text.clone();
    let display_text = log_viewer_display_text(&line.text).into_owned();
    let selection_range =
        app.log_text_selection_byte_range_for_line(tab_id, line.line_number, &line.text);
    let syntax_spans = highlight_cache.highlight_line(line.line_number, language, &display_text);
    let highlights = merge_syntax_and_selection_highlights(syntax_spans, selection_range, theme);
    let text_element = if highlights.is_empty() {
        display_text.into_any_element()
    } else {
        StyledText::new(display_text.clone())
            .with_highlights(highlights)
            .into_any_element()
    };

    div()
        .id(SharedString::from(format!(
            "log-line-{}-{}",
            tab_id, line.line_number
        )))
        .relative()
        .h(px(LOG_VIEWER_ROW_HEIGHT))
        .text_size(px(app.log_content_font_size))
        .line_height(px(LOG_VIEWER_ROW_HEIGHT))
        .font_family(ARGUS_LOG_FONT_FAMILY)
        .text_color(rgb(theme.foreground))
        .hover(|row| row.bg(rgb(theme.current_line)))
        .child(
            div()
                .relative()
                .left(horizontal_offset)
                .h_full()
                .flex()
                .items_center()
                .pl(px(line_number_width + LOG_VIEWER_TEXT_LEFT_PADDING))
                .pr(px(LOG_VIEWER_TEXT_RIGHT_PADDING))
                .whitespace_nowrap()
                .child(text_element),
        )
        .child(
            div()
                .absolute()
                .left(line_number_offset)
                .top(px(0.0))
                .h_full()
                .w(px(line_number_width + LOG_VIEWER_TEXT_LEFT_PADDING))
                .bg(rgb(theme.content))
                .pr(px(LOG_VIEWER_TEXT_LEFT_PADDING))
                .flex()
                .items_center()
                .justify_end()
                .text_color(rgb(theme.foreground_muted))
                .child((line.line_number + 1).to_string()),
        )
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(move |app, event: &MouseDownEvent, window, cx| {
                cx.stop_propagation();
                app.begin_log_text_selection_with_click_count(
                    tab_id,
                    line.line_number,
                    &line_for_down,
                    event.position.x,
                    event.click_count,
                    window,
                );
                cx.notify();
            }),
        )
        .on_mouse_move(cx.listener(move |app, event: &MouseMoveEvent, window, cx| {
            if event.dragging() {
                if !app.is_log_text_selection_drag_active(tab_id) {
                    return;
                }
                cx.stop_propagation();
                app.update_log_text_selection(
                    tab_id,
                    line.line_number,
                    &line_for_move,
                    event.position.x,
                    window,
                );
                cx.notify();
            }
        }))
}

/// 合并语法高亮和选区高亮；选区优先，避免 GPUI 收到重叠 highlight。
fn merge_syntax_and_selection_highlights(
    syntax_spans: Vec<HighlightSpan>,
    selection_range: Option<Range<usize>>,
    theme: &AppTheme,
) -> Vec<(Range<usize>, HighlightStyle)> {
    let mut highlights = Vec::new();

    for span in syntax_spans {
        let syntax_style = HighlightStyle {
            color: Some(rgb(color_for_highlight_token(span.kind, theme)).into()),
            ..Default::default()
        };

        if let Some(selection) = selection_range.as_ref()
            && ranges_overlap(&span.range, selection)
        {
            push_syntax_piece_before_selection(
                &mut highlights,
                &span.range,
                selection,
                syntax_style.clone(),
            );
            push_syntax_piece_after_selection(
                &mut highlights,
                &span.range,
                selection,
                syntax_style,
            );
            continue;
        }

        highlights.push((span.range, syntax_style));
    }

    if let Some(selection) = selection_range {
        highlights.push((
            selection,
            HighlightStyle {
                background_color: Some(rgb(theme.selection).into()),
                color: Some(rgb(theme.foreground).into()),
                ..Default::default()
            },
        ));
    }

    highlights.sort_by_key(|(range, _)| range.start);
    highlights
}

/// 保留选区左侧未被覆盖的语法颜色片段。
fn push_syntax_piece_before_selection(
    highlights: &mut Vec<(Range<usize>, HighlightStyle)>,
    syntax_range: &Range<usize>,
    selection_range: &Range<usize>,
    syntax_style: HighlightStyle,
) {
    let end = syntax_range.end.min(selection_range.start);
    if syntax_range.start < end {
        highlights.push((syntax_range.start..end, syntax_style));
    }
}

/// 保留选区右侧未被覆盖的语法颜色片段。
fn push_syntax_piece_after_selection(
    highlights: &mut Vec<(Range<usize>, HighlightStyle)>,
    syntax_range: &Range<usize>,
    selection_range: &Range<usize>,
    syntax_style: HighlightStyle,
) {
    let start = syntax_range.start.max(selection_range.end);
    if start < syntax_range.end {
        highlights.push((start..syntax_range.end, syntax_style));
    }
}

/// 判断两个半开区间是否重叠。
fn ranges_overlap(left: &Range<usize>, right: &Range<usize>) -> bool {
    left.start < right.end && right.start < left.end
}

/// 根据高亮 token 返回当前主题下的显示颜色。
fn color_for_highlight_token(kind: HighlightTokenKind, theme: &AppTheme) -> u32 {
    match kind {
        HighlightTokenKind::Trace => theme.foreground_muted,
        HighlightTokenKind::Debug => theme.debug,
        HighlightTokenKind::Info => theme.info,
        HighlightTokenKind::Warning => theme.warning,
        HighlightTokenKind::Error | HighlightTokenKind::Fatal => theme.error,
        HighlightTokenKind::Timestamp => theme.syntax.timestamp,
        HighlightTokenKind::Comment => theme.syntax.comment,
        HighlightTokenKind::Key => theme.syntax.key,
        HighlightTokenKind::Value => theme.syntax.string,
        HighlightTokenKind::String => theme.syntax.string,
        HighlightTokenKind::Number => theme.syntax.number,
        HighlightTokenKind::Boolean => theme.syntax.boolean,
        HighlightTokenKind::Punctuation => theme.syntax.punctuation,
        HighlightTokenKind::Tag => theme.syntax.tag,
        HighlightTokenKind::Attribute => theme.syntax.attribute,
        HighlightTokenKind::ThreadName => theme.syntax.thread,
        HighlightTokenKind::ThreadState => theme.warning,
        HighlightTokenKind::StackClass => theme.syntax.class,
        HighlightTokenKind::StackMethod => theme.syntax.method,
        HighlightTokenKind::StackLocation => theme.foreground_muted,
        HighlightTokenKind::Lock => theme.syntax.lock,
        HighlightTokenKind::Exception => theme.syntax.exception,
    }
}

/// 渲染小日志虚拟列表的横纵滚动条。
fn render_in_memory_scrollbars(
    tab_id: usize,
    state: &crate::app::LogTabViewState,
    line_number_width: f32,
    theme: &AppTheme,
    cx: &mut Context<ArgusApp>,
) -> Vec<AnyElement> {
    let scroll_state = state.scroll_handle.0.as_ref().borrow();
    let bounds = scroll_state.base_handle.bounds();
    let Some(size) = scroll_state.last_item_size else {
        return Vec::new();
    };
    let offset = scroll_state.base_handle.offset();
    let scroll_handle = scroll_state.base_handle.clone();
    drop(scroll_state);

    let mut scrollbars = Vec::new();
    if let Some(metrics) = scrollbar_metrics(
        size.item.height,
        size.contents.height,
        -offset.y,
        false,
        px(0.0),
    ) {
        scrollbars.push(render_scrollbar_thumb(
            tab_id,
            LogScrollbarAxis::Vertical,
            metrics,
            bounds,
            scroll_handle.clone(),
            None,
            theme,
            cx,
        ));
    }
    if let Some(metrics) = scrollbar_metrics(
        size.item.width,
        size.contents.width,
        -offset.x,
        true,
        px(line_number_width),
    ) {
        scrollbars.push(render_scrollbar_thumb(
            tab_id,
            LogScrollbarAxis::Horizontal,
            metrics,
            bounds,
            scroll_handle,
            None,
            theme,
            cx,
        ));
    }

    scrollbars
}

/// 渲染分页日志的横纵滚动条。
fn render_paged_scrollbars(
    app: &ArgusApp,
    tab_id: usize,
    source_id: crate::loader::SourceId,
    handle: &LogReaderHandle,
    state: &crate::app::LogTabViewState,
    theme: &AppTheme,
    cx: &mut Context<ArgusApp>,
) -> Vec<AnyElement> {
    let bounds = state.paged_viewport_handle.bounds();
    let viewport = bounds.size;
    if viewport.width <= px(0.0) || viewport.height <= px(0.0) {
        return Vec::new();
    }

    let estimated_char_width = (app.log_content_font_size * 0.62).max(6.0);
    let line_number_width = log_viewer_line_number_width(handle.line_count());
    let content_width = px(handle.estimated_longest_display_columns() as f32
        * estimated_char_width
        + line_number_width
        + LOG_VIEWER_TEXT_LEFT_PADDING
        + LOG_VIEWER_TEXT_RIGHT_PADDING);
    let content_height = px(handle.line_count() as f32 * LOG_VIEWER_ROW_HEIGHT);
    let mut scrollbars = Vec::new();

    if let Some(metrics) = scrollbar_metrics(
        viewport.height,
        content_height,
        px(state.paged_scroll.top_px as f32),
        false,
        px(0.0),
    ) {
        scrollbars.push(render_scrollbar_thumb(
            tab_id,
            LogScrollbarAxis::Vertical,
            metrics,
            bounds,
            state.paged_viewport_handle.clone(),
            Some(source_id),
            theme,
            cx,
        ));
    }
    if let Some(metrics) = scrollbar_metrics(
        viewport.width,
        content_width,
        px(state.paged_scroll.left_px as f32),
        true,
        px(line_number_width),
    ) {
        scrollbars.push(render_scrollbar_thumb(
            tab_id,
            LogScrollbarAxis::Horizontal,
            metrics,
            bounds,
            state.paged_viewport_handle.clone(),
            Some(source_id),
            theme,
            cx,
        ));
    }

    scrollbars
}

/// 计算滚动条滑块位置。
fn scrollbar_metrics(
    viewport_len: gpui::Pixels,
    content_len: gpui::Pixels,
    scroll_offset: gpui::Pixels,
    horizontal: bool,
    leading_gutter: gpui::Pixels,
) -> Option<LogScrollbarMetrics> {
    if viewport_len <= px(0.0) || content_len <= viewport_len {
        return None;
    }
    let max_scroll = content_len - viewport_len;
    let track_start = if horizontal {
        leading_gutter + px(LOG_SCROLLBAR_PADDING)
    } else {
        px(LOG_SCROLLBAR_PADDING)
    };
    let reserved = track_start + px(LOG_SCROLLBAR_PADDING);
    let track_length = (viewport_len - reserved).max(px(1.0));
    let min_thumb = px(LOG_SCROLLBAR_MIN_THUMB).min(track_length);
    let thumb_length = (viewport_len * (viewport_len / content_len)).clamp(min_thumb, track_length);
    let movable = (track_length - thumb_length).max(px(0.0));
    let scroll_ratio = (scroll_offset / max_scroll).clamp(0.0, 1.0);
    let thumb_start = track_start + movable * scroll_ratio;

    Some(LogScrollbarMetrics {
        thumb_start,
        thumb_length,
        track_start,
        track_length,
        max_scroll,
    })
}

/// 渲染单个滚动条滑块，并在拖动时写回对应滚动状态。
fn render_scrollbar_thumb(
    tab_id: usize,
    axis: LogScrollbarAxis,
    metrics: LogScrollbarMetrics,
    viewport_bounds: gpui::Bounds<gpui::Pixels>,
    scroll_handle: gpui::ScrollHandle,
    paged_source_id: Option<crate::loader::SourceId>,
    theme: &AppTheme,
    cx: &mut Context<ArgusApp>,
) -> AnyElement {
    let entity = cx.entity();
    let is_horizontal = axis == LogScrollbarAxis::Horizontal;
    let mut thumb = div()
        .id(SharedString::from(format!(
            "log-scrollbar-{tab_id}-{axis:?}"
        )))
        .absolute()
        .occlude()
        .rounded_lg()
        .bg(rgb(theme.foreground_muted))
        .opacity(0.38)
        .hover(|this| this.opacity(0.72));

    thumb = if is_horizontal {
        thumb
            .left(metrics.thumb_start)
            .bottom(px(LOG_SCROLLBAR_PADDING))
            .w(metrics.thumb_length)
            .h(px(LOG_SCROLLBAR_WIDTH))
    } else {
        thumb
            .top(metrics.thumb_start)
            .right(px(LOG_SCROLLBAR_PADDING))
            .w(px(LOG_SCROLLBAR_WIDTH))
            .h(metrics.thumb_length)
    };

    thumb
        .child(
            canvas(
                |_, _, _| (),
                move |thumb_bounds, _, window: &mut Window, _| {
                    window.on_mouse_event({
                        let entity = entity.clone();
                        move |event: &MouseDownEvent, phase, _, cx| {
                            if !phase.bubble()
                                || event.button != MouseButton::Left
                                || !thumb_bounds.contains(&event.position)
                            {
                                return;
                            }
                            cx.stop_propagation();
                            let pointer = if is_horizontal {
                                event.position.x
                            } else {
                                event.position.y
                            };
                            let thumb_start = if is_horizontal {
                                thumb_bounds.left()
                            } else {
                                thumb_bounds.top()
                            };
                            entity.update(cx, |app, _| {
                                app.log_scrollbar_drag = Some(LogScrollbarDrag {
                                    tab_id,
                                    axis,
                                    cursor_offset: pointer - thumb_start,
                                });
                            });
                            cx.notify(entity.entity_id());
                        }
                    });

                    window.on_mouse_event({
                        let entity = entity.clone();
                        move |event: &MouseUpEvent, phase, _, cx| {
                            if !phase.bubble() || event.button != MouseButton::Left {
                                return;
                            }
                            let handled = entity.update(cx, |app, _| {
                                let handled = app
                                    .log_scrollbar_drag
                                    .is_some_and(|drag| drag.tab_id == tab_id && drag.axis == axis);
                                if handled {
                                    app.log_scrollbar_drag = None;
                                }
                                handled
                            });
                            if handled {
                                cx.stop_propagation();
                                cx.notify(entity.entity_id());
                            }
                        }
                    });

                    window.on_mouse_event(move |event: &MouseMoveEvent, phase, _, cx| {
                        if !phase.bubble() || !event.dragging() {
                            return;
                        }
                        let handled = entity.update(cx, |app, _| {
                            let Some(drag) = app.log_scrollbar_drag else {
                                return false;
                            };
                            if drag.tab_id != tab_id || drag.axis != axis {
                                return false;
                            }
                            let pointer = if is_horizontal {
                                event.position.x - viewport_bounds.left()
                            } else {
                                event.position.y - viewport_bounds.top()
                            };
                            let movable =
                                (metrics.track_length - metrics.thumb_length).max(px(1.0));
                            let thumb_start = (pointer - drag.cursor_offset)
                                .clamp(metrics.track_start, metrics.track_start + movable);
                            let ratio = (thumb_start - metrics.track_start) / movable;
                            let scroll = metrics.max_scroll * ratio;

                            if let Some(source_id) = paged_source_id {
                                if let Some(state) = app.log_tab_view_state_mut(tab_id) {
                                    match axis {
                                        LogScrollbarAxis::Vertical => {
                                            state.paged_scroll.top_px = f64::from(scroll);
                                        }
                                        LogScrollbarAxis::Horizontal => {
                                            state.paged_scroll.left_px = f64::from(scroll);
                                        }
                                    }
                                }
                                let _ = source_id;
                            } else {
                                let current = scroll_handle.offset();
                                match axis {
                                    LogScrollbarAxis::Vertical => {
                                        scroll_handle.set_offset(point(current.x, -scroll));
                                    }
                                    LogScrollbarAxis::Horizontal => {
                                        scroll_handle.set_offset(point(-scroll, current.y));
                                    }
                                }
                            }
                            true
                        });
                        if handled {
                            cx.stop_propagation();
                            cx.notify(entity.entity_id());
                        }
                    });
                },
            )
            .size_full(),
        )
        .into_any_element()
}

/// 渲染内容区空状态或未读取提示。
fn render_empty_state(title: &str, detail: &str, app: &ArgusApp, theme: &AppTheme) -> AnyElement {
    render_empty_state_with_leading(title, detail, None, app, theme)
}

/// 渲染带加载图标的内容区提示。
fn render_loading_state(
    title: &str,
    detail: &str,
    source_id: crate::loader::SourceId,
    app: &ArgusApp,
    theme: &AppTheme,
) -> AnyElement {
    render_empty_state_with_leading(
        title,
        detail,
        Some(render_loading_spinner(
            ("log-reading-spinner", source_id.0),
            theme.foreground_muted,
            16.0,
        )),
        app,
        theme,
    )
}

/// 渲染内容区居中提示，可选在标题前追加一个状态图标。
fn render_empty_state_with_leading(
    title: &str,
    detail: &str,
    leading: Option<AnyElement>,
    app: &ArgusApp,
    theme: &AppTheme,
) -> AnyElement {
    let detail_font_size = app.log_content_font_size;
    let title_font_size = detail_font_size + 4.0;
    let title_row = div()
        .flex()
        .items_center()
        .justify_center()
        .gap_2()
        .text_size(px(title_font_size))
        .text_color(rgb(theme.foreground))
        .children(leading)
        .child(title.to_string());

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
                .child(title_row)
                .child(
                    div()
                        .text_size(px(detail_font_size))
                        .text_color(rgb(theme.foreground_muted))
                        .child(detail.to_string()),
                ),
        )
        .into_any_element()
}

/// 计算分页视口当前应该渲染的行数。
fn visible_row_capacity(viewport_height: gpui::Pixels) -> usize {
    if viewport_height <= px(0.0) {
        return DEFAULT_VISIBLE_ROWS;
    }

    ((f32::from(viewport_height) / LOG_VIEWER_ROW_HEIGHT).ceil() as usize + 2)
        .max(1)
        .min(400)
}

/// 计算分页日志最大纵向滚动像素。
fn paged_vertical_max_scroll(line_count: usize, viewport_height: gpui::Pixels) -> f64 {
    let content_height = line_count as f64 * LOG_VIEWER_ROW_HEIGHT as f64;
    let viewport_height = f64::from(viewport_height).max(0.0);
    (content_height - viewport_height).max(0.0)
}

/// 返回用于横向测量的最长行索引。
fn longest_line_index(document: &LogDocument) -> usize {
    match document {
        LogDocument::InMemory(document) => document.longest_line_index,
        LogDocument::Paged(document) => document.longest_line_index(),
    }
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
        .bg(rgb(theme.current_line))
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
            "正则表达式",
            app.is_regex_enabled,
            IconButtonSize::Small,
            &theme,
            cx.listener(|app, _, _, cx| {
                app.toggle_search_option("regex");
                cx.notify();
            }),
        ))
        .child(render_icon_button(
            "search-word",
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

#[cfg(test)]
mod tests {
    use super::*;

    /// 验证选区覆盖语法高亮中段时，只覆盖被选中的范围，两侧语法颜色仍会保留。
    #[test]
    fn selection_splits_overlapping_syntax_highlight() {
        let theme = AppTheme::dark();
        let highlights = merge_syntax_and_selection_highlights(
            vec![HighlightSpan {
                range: 0..15,
                kind: HighlightTokenKind::StackClass,
            }],
            Some(5..9),
            &theme,
        );
        let ranges = highlights
            .iter()
            .map(|(range, _)| range.clone())
            .collect::<Vec<_>>();

        assert_eq!(ranges, vec![0..5, 5..9, 9..15]);
    }
}
