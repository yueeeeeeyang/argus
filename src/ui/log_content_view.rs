//! 文件职责：渲染日志分析工作区的主内容区域。
//! 创建日期：2026-06-09
//! 修改日期：2026-06-16
//! 作者：Argus 开发团队
//! 主要功能：按行虚拟渲染日志正文和 Jstack 分析页，大日志只读取当前可见页，避免整份日志进入 UI 文本节点。

use std::ops::Range;

use crate::app::{
    ArgusApp, LOG_VIEWER_TEXT_LEFT_PADDING, LOG_VIEWER_TEXT_RIGHT_PADDING, LogScrollbarAxis,
    LogScrollbarDrag, SearchResultListItem, SearchResultScrollbarAxis, SearchResultScrollbarDrag,
    SearchRunKind, TabKind, log_viewer_display_text, log_viewer_line_number_width,
};
use crate::fonts::ARGUS_LOG_FONT_FAMILY;
use crate::highlight::{
    HighlightCache, HighlightLanguage, HighlightSpan, HighlightTokenKind, detect_highlight_language,
};
use crate::reader::log_file_reader::{LogDocument, LogOpenState, LogReaderHandle};
use crate::search::search_task::SearchTaskState;
use crate::text_selection::{
    byte_index_for_character, char_column_for_byte_index, character_count, slice_character_range,
};
use crate::theme::AppTheme;
use crate::ui::components::icon::ArgusIcon;
use crate::ui::components::icon::render_icon;
use crate::ui::components::icon_button::{IconButtonSize, render_icon_button};
use crate::ui::components::loading_spinner::render_loading_spinner;
use crate::ui::jstack_analysis_view;
use crate::ui::settings_page;
use gpui::{
    AnyElement, Context, HighlightStyle, IntoElement, KeyDownEvent, ListHorizontalSizingBehavior,
    MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, ScrollWheelEvent, SharedString,
    StyledText, Window, canvas, div, point, prelude::*, px, rgb, uniform_list,
};

/// 日志正文固定行高；虚拟列表和分页窗口都依赖该值稳定换算。
const LOG_VIEWER_ROW_HEIGHT: f32 = 20.0;
/// 行号右侧打点标记尺寸；保持较小尺寸避免干扰行号读取。
const LOG_LINE_MARKER_SIZE: f32 = 5.0;
/// 行号打点距离行号列右侧的间距。
const LOG_LINE_MARKER_RIGHT: f32 = 5.0;
/// 首帧视口未测量时的默认渲染行数。
const DEFAULT_VISIBLE_ROWS: usize = 80;
/// 自绘滚动条宽度。
const LOG_SCROLLBAR_WIDTH: f32 = 5.0;
/// 自绘滚动条边距。
const LOG_SCROLLBAR_PADDING: f32 = 4.0;
/// 自绘滚动条最小滑块长度。
const LOG_SCROLLBAR_MIN_THUMB: f32 = 32.0;
/// 搜索结果面板固定行高。
const SEARCH_RESULT_ROW_HEIGHT: f32 = 28.0;
/// 搜索结果列表最小内容宽度，超出面板宽度时启用横向滚动条。
const SEARCH_RESULT_ROW_MIN_WIDTH: f32 = 760.0;
/// 搜索结果行左侧行号列宽度。
const SEARCH_RESULT_LINE_LABEL_WIDTH: f32 = 78.0;
/// 搜索结果行横向内边距总和。
const SEARCH_RESULT_ROW_HORIZONTAL_PADDING: f32 = 24.0;
/// 搜索结果行固定列间距。
const SEARCH_RESULT_ROW_GAP_WIDTH: f32 = 8.0;
/// 搜索结果中 ASCII 字符的宽度估算，用于提前撑开横向滚动内容。
const SEARCH_RESULT_ASCII_CHAR_WIDTH: f32 = 7.4;
/// 搜索结果中中文等宽字符的宽度估算，避免混排内容在面板中提前换行。
const SEARCH_RESULT_WIDE_CHAR_WIDTH: f32 = 13.0;
/// 搜索结果预览最大字符数；不截断结果数量，只限制单行预览渲染成本。
const SEARCH_RESULT_PREVIEW_MAX_CHARS: usize = 420;
/// 搜索结果预览中命中点前后的上下文字符数。
const SEARCH_RESULT_PREVIEW_CONTEXT_CHARS: usize = 160;
/// 分页日志横向切片的额外字符缓冲，避免轻微估算误差导致滚动边缘露白。
const PAGED_LOG_HORIZONTAL_OVERSCAN_COLUMNS: usize = 96;

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

/// 分页日志单行实际交给 GPUI 渲染的可见文本切片。
#[derive(Clone, Debug)]
struct LogVisibleText {
    /// 当前切片文本。
    text: String,
    /// 当前切片在完整展示文本中的字符范围。
    char_range: Range<usize>,
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
        .child(
            div()
                .flex_1()
                .min_h(px(0.0))
                .flex()
                .flex_col()
                .overflow_hidden()
                .child(render_content_body(app, &theme, cx)),
        )
        .when(app.should_show_log_search_results(), |this| {
            this.child(render_search_results_panel(app, &theme, cx))
        })
}

/// 根据当前内容状态渲染主体区域。
fn render_content_body(app: &ArgusApp, theme: &AppTheme, cx: &mut Context<ArgusApp>) -> AnyElement {
    match app.active_tab_kind() {
        TabKind::Settings => settings_page::render(app, cx).into_any_element(),
        TabKind::JstackAnalysis { analysis_id } => {
            jstack_analysis_view::render(app, analysis_id, cx).into_any_element()
        }
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
        .on_click(cx.listener(move |app, _, _, cx| {
            app.focus_log_text_view(tab_id);
            cx.notify();
        }))
        .on_scroll_wheel(cx.listener(move |app, _: &ScrollWheelEvent, _, cx| {
            if app.clear_line_marker_jump_cache(tab_id) {
                cx.notify();
            }
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
    let line_number_width = log_viewer_line_number_width(line_count);
    let (visible_char_range, horizontal_offset) = paged_visible_text_range(
        state.paged_scroll.left_px,
        viewport_bounds.size.width,
        line_number_width,
        app.log_content_font_size,
    );
    let rows = lines
        .into_iter()
        .map(|line| {
            let row_offset = line.line_number.saturating_sub(first_line_index);
            let top = row_offset as f32 * LOG_VIEWER_ROW_HEIGHT - fractional_top;
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
                    line.line_number,
                    &line.text,
                    language,
                    &state.highlight_cache,
                    horizontal_offset,
                    px(0.0),
                    line_number_width,
                    Some(visible_char_range.clone()),
                    true,
                    cx,
                ))
                .into_any_element()
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
        .on_click(cx.listener(move |app, _, _, cx| {
            app.focus_log_text_view(tab_id);
            cx.notify();
        }))
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
                line.line_number,
                &line.text,
                language,
                &highlight_cache,
                px(0.0),
                line_number_offset,
                line_number_width,
                None,
                true,
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
    line_number: usize,
    line_text: &str,
    language: HighlightLanguage,
    highlight_cache: &HighlightCache,
    horizontal_offset: gpui::Pixels,
    line_number_offset: gpui::Pixels,
    line_number_width: f32,
    visible_char_range: Option<Range<usize>>,
    enable_syntax_highlight: bool,
    cx: &mut Context<ArgusApp>,
) -> impl IntoElement {
    let selection_range =
        app.log_text_selection_byte_range_for_line(tab_id, line_number, line_text);
    let active_match = app
        .log_tab_view_state(tab_id)
        .and_then(|state| state.active_search_match.as_ref())
        .filter(|active_match| active_match.line_number == line_number);
    let has_overlay_ranges = selection_range.is_some() || active_match.is_some();
    let (visible_text, selection_range, search_ranges, active_search_range) = if has_overlay_ranges
    {
        let full_display_text = log_viewer_display_text(line_text).into_owned();
        let visible_text = visible_log_text(&full_display_text, visible_char_range.as_ref());
        let search_ranges = active_match.map(|active_match| {
            search_ranges_for_display(line_text, &full_display_text, &active_match.match_ranges)
        });
        let active_search_range = active_match.and_then(|active_match| {
            active_match.active_range.as_ref().and_then(|range| {
                search_ranges_for_display(line_text, &full_display_text, &active_match.match_ranges)
                    .into_iter()
                    .zip(active_match.match_ranges.iter())
                    .find_map(|(display_range, original_range)| {
                        if original_range == range {
                            Some(display_range)
                        } else {
                            None
                        }
                    })
            })
        });
        let selection_range = selection_range.and_then(|range| {
            clip_display_range_to_visible(&full_display_text, &visible_text, range)
        });
        let search_ranges = search_ranges.map(|ranges| {
            ranges
                .into_iter()
                .filter_map(|range| {
                    clip_display_range_to_visible(&full_display_text, &visible_text, range)
                })
                .collect::<Vec<_>>()
        });
        let active_search_range = active_search_range.and_then(|range| {
            clip_display_range_to_visible(&full_display_text, &visible_text, range)
        });

        (
            visible_text,
            selection_range,
            search_ranges,
            active_search_range,
        )
    } else {
        (
            visible_log_text_from_raw(line_text, visible_char_range.as_ref()),
            None,
            None,
            None,
        )
    };
    let syntax_spans = if enable_syntax_highlight {
        highlight_cache.highlight_line(line_number, language, &visible_text.text)
    } else {
        Vec::new()
    };
    let highlights = merge_log_line_highlights(
        syntax_spans,
        selection_range,
        search_ranges,
        active_search_range,
        theme,
    );
    let text_element = if highlights.is_empty() {
        visible_text.text.into_any_element()
    } else {
        StyledText::new(visible_text.text)
            .with_highlights(highlights)
            .into_any_element()
    };
    let has_line_marker = app
        .log_tab_view_state(tab_id)
        .is_some_and(|state| state.line_markers.contains(&line_number));
    let is_active_line_marker_jump = app
        .log_tab_view_state(tab_id)
        .is_some_and(|state| state.last_line_marker_jump == Some(line_number));
    let is_active_search_line = app
        .log_tab_view_state(tab_id)
        .and_then(|state| state.active_search_match.as_ref())
        .is_some_and(|active_match| active_match.line_number == line_number);
    let row_background = if is_active_search_line {
        Some(active_search_line_background(theme))
    } else if is_active_line_marker_jump {
        Some(theme.selection)
    } else {
        None
    };

    div()
        .id(SharedString::from(format!(
            "log-line-{}-{}",
            tab_id, line_number
        )))
        .relative()
        .h(px(LOG_VIEWER_ROW_HEIGHT))
        .text_size(px(app.log_content_font_size))
        .line_height(px(LOG_VIEWER_ROW_HEIGHT))
        .font_family(ARGUS_LOG_FONT_FAMILY)
        .text_color(rgb(theme.foreground))
        .when_some(row_background, |row, color| row.bg(rgb(color)))
        .hover(move |row| row.bg(rgb(row_background.unwrap_or(theme.current_line))))
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
                .bg(rgb(row_background.unwrap_or(theme.content)))
                .cursor_pointer()
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |app, _: &MouseDownEvent, _, cx| {
                        cx.stop_propagation();
                        app.toggle_log_line_marker(tab_id, line_number);
                        cx.notify();
                    }),
                )
                .child(
                    div()
                        .relative()
                        .size_full()
                        .pr(px(LOG_VIEWER_TEXT_LEFT_PADDING))
                        .flex()
                        .items_center()
                        .justify_end()
                        .text_color(rgb(theme.foreground_muted))
                        .child((line_number + 1).to_string())
                        .when(has_line_marker, |cell| {
                            cell.child(
                                div()
                                    .absolute()
                                    .right(px(LOG_LINE_MARKER_RIGHT))
                                    .top(px((LOG_VIEWER_ROW_HEIGHT - LOG_LINE_MARKER_SIZE) / 2.0))
                                    .w(px(LOG_LINE_MARKER_SIZE))
                                    .h(px(LOG_LINE_MARKER_SIZE))
                                    .rounded(px(LOG_LINE_MARKER_SIZE / 2.0))
                                    .bg(rgb(theme.warning)),
                            )
                        }),
                ),
        )
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(move |app, event: &MouseDownEvent, window, cx| {
                cx.stop_propagation();
                if let Some(line_text) = app.log_line_text_for_tab(tab_id, line_number) {
                    app.begin_log_text_selection_with_click_count(
                        tab_id,
                        line_number,
                        &line_text,
                        event.position.x,
                        event.click_count,
                        window,
                    );
                    cx.notify();
                }
            }),
        )
        .on_mouse_move(cx.listener(move |app, event: &MouseMoveEvent, window, cx| {
            if event.dragging() {
                if !app.is_log_text_selection_drag_active(tab_id) {
                    return;
                }
                cx.stop_propagation();
                if let Some(line_text) = app.log_line_text_for_tab(tab_id, line_number) {
                    app.update_log_text_selection(
                        tab_id,
                        line_number,
                        &line_text,
                        event.position.x,
                        window,
                    );
                    cx.notify();
                }
            }
        }))
}

/// 根据分页日志横向滚动位置计算当前需要渲染的字符范围。
///
/// 说明：GPUI 对超长 `StyledText` 的 shaping 成本很高。分页日志只渲染横向可视范围附近
/// 的文本片段，选择和复制仍使用完整行文本，避免切 tab 时被超长行拖慢。
fn paged_visible_text_range(
    scroll_left: f64,
    viewport_width: gpui::Pixels,
    line_number_width: f32,
    font_size: f32,
) -> (Range<usize>, gpui::Pixels) {
    let estimated_char_width = estimated_log_char_width(font_size) as f64;
    let text_viewport_width = (f32::from(viewport_width)
        - line_number_width
        - LOG_VIEWER_TEXT_LEFT_PADDING
        - LOG_VIEWER_TEXT_RIGHT_PADDING)
        .max(0.0);
    let first_visible_column = (scroll_left / estimated_char_width).floor().max(0.0) as usize;
    let start_column = first_visible_column.saturating_sub(PAGED_LOG_HORIZONTAL_OVERSCAN_COLUMNS);
    let visible_columns = (text_viewport_width / estimated_char_width as f32).ceil() as usize;
    let end_column = first_visible_column
        .saturating_add(visible_columns)
        .saturating_add(PAGED_LOG_HORIZONTAL_OVERSCAN_COLUMNS * 2)
        .max(start_column);
    let residual_offset = scroll_left - start_column as f64 * estimated_char_width;

    (start_column..end_column, px(-(residual_offset as f32)))
}

/// 返回日志字体的横向宽度估算，供分页切片和横向滚动范围共用。
fn estimated_log_char_width(font_size: f32) -> f32 {
    (font_size * 0.62).max(6.0)
}

/// 从完整展示文本中截取本次真正需要渲染的片段。
fn visible_log_text(full_text: &str, visible_char_range: Option<&Range<usize>>) -> LogVisibleText {
    let full_char_count = character_count(full_text);
    let Some(range) = visible_char_range else {
        return LogVisibleText {
            text: full_text.to_string(),
            char_range: 0..full_char_count,
        };
    };

    let start = range.start.min(full_char_count);
    let end = range.end.min(full_char_count).max(start);
    LogVisibleText {
        text: slice_character_range(full_text, start..end),
        char_range: start..end,
    }
}

/// 从原始日志行中按展示列截取可见片段，避免分页长行先展开整行再切片。
///
/// 处理原因：
/// - 分页日志常见单行很长，切 tab 或首帧显示时如果先构造整行展示文本，
///   即使最终只显示横向可见区域，也会在 UI 线程产生明显停顿。
/// - 当前日志显示规则只要求 `\t` 展开为 4 个空格，因此可以线性扫描到可见结束列后立即停止。
fn visible_log_text_from_raw(
    raw_text: &str,
    visible_char_range: Option<&Range<usize>>,
) -> LogVisibleText {
    let Some(range) = visible_char_range else {
        let display_text = log_viewer_display_text(raw_text).into_owned();
        let char_count = character_count(&display_text);
        return LogVisibleText {
            text: display_text,
            char_range: 0..char_count,
        };
    };

    let mut display_column = 0_usize;
    let mut text = String::new();
    for character in raw_text.chars() {
        if display_column >= range.end {
            break;
        }

        if character == '\t' {
            let tab_end = display_column + 4;
            let start = range.start.max(display_column);
            let end = range.end.min(tab_end);
            if start < end {
                text.extend(std::iter::repeat(' ').take(end - start));
            }
            display_column = tab_end;
            continue;
        }

        if display_column >= range.start {
            text.push(character);
        }
        display_column += 1;
    }

    let clipped_start = range.start.min(display_column);
    let clipped_end = range.end.min(display_column).max(clipped_start);
    LogVisibleText {
        text,
        char_range: clipped_start..clipped_end,
    }
}

/// 将完整展示文本上的 byte range 裁剪并平移到当前可见切片上。
fn clip_display_range_to_visible(
    full_text: &str,
    visible_text: &LogVisibleText,
    range: Range<usize>,
) -> Option<Range<usize>> {
    if range.start >= range.end {
        return None;
    }

    let start_column = char_column_for_byte_index(full_text, range.start);
    let end_column = char_column_for_byte_index(full_text, range.end);
    let clipped_start = start_column.max(visible_text.char_range.start);
    let clipped_end = end_column.min(visible_text.char_range.end);
    if clipped_start >= clipped_end {
        return None;
    }

    let local_start = clipped_start - visible_text.char_range.start;
    let local_end = clipped_end - visible_text.char_range.start;
    Some(
        byte_index_for_character(&visible_text.text, local_start)
            ..byte_index_for_character(&visible_text.text, local_end),
    )
}

/// 合并语法高亮、搜索命中和选区高亮；选区优先，避免 GPUI 收到重叠 highlight。
fn merge_log_line_highlights(
    syntax_spans: Vec<HighlightSpan>,
    selection_range: Option<Range<usize>>,
    search_ranges: Option<Vec<Range<usize>>>,
    active_search_range: Option<Range<usize>>,
    theme: &AppTheme,
) -> Vec<(Range<usize>, HighlightStyle)> {
    let mut highlights = Vec::new();
    let search_ranges = search_ranges.unwrap_or_default();
    let mut protected_ranges = search_ranges.clone();
    if let Some(active_range) = active_search_range.clone() {
        protected_ranges.push(active_range);
    }
    if let Some(selection) = selection_range.clone() {
        protected_ranges.push(selection);
    }

    for span in syntax_spans {
        let syntax_style = HighlightStyle {
            color: Some(rgb(color_for_highlight_token(span.kind, theme)).into()),
            ..Default::default()
        };

        for visible_range in subtract_ranges(span.range, &protected_ranges) {
            highlights.push((visible_range, syntax_style.clone()));
        }
    }

    for range in search_ranges
        .into_iter()
        .filter(|range| range.start < range.end)
    {
        if active_search_range.as_ref() == Some(&range) {
            continue;
        }
        push_non_overlapping_highlight(
            &mut highlights,
            range,
            selection_range.as_ref(),
            HighlightStyle {
                background_color: Some(rgb(theme.warning).into()),
                color: Some(rgb(theme.background).into()),
                ..Default::default()
            },
        );
    }

    if let Some(active_range) = active_search_range.filter(|range| range.start < range.end) {
        push_non_overlapping_highlight(
            &mut highlights,
            active_range,
            selection_range.as_ref(),
            HighlightStyle {
                background_color: Some(rgb(theme.warning).into()),
                color: Some(rgb(theme.background).into()),
                ..Default::default()
            },
        );
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

/// 保持旧单元测试可读性的二元合并入口。
#[cfg(test)]
fn merge_syntax_and_selection_highlights(
    syntax_spans: Vec<HighlightSpan>,
    selection_range: Option<Range<usize>>,
    theme: &AppTheme,
) -> Vec<(Range<usize>, HighlightStyle)> {
    merge_log_line_highlights(syntax_spans, selection_range, None, None, theme)
}

/// 生成搜索跳转当前行背景色。
///
/// 搜索命中行需要有整行定位感，但不能复用 `theme.selection`，否则用户在当前行上
/// 选择文本时，行背景和选区背景会混在一起。这里用 WARN 色少量混入内容底色，
/// 让行背景和搜索词高亮保持同一语义，同时把视觉层级让给真正的文本选区。
fn active_search_line_background(theme: &AppTheme) -> u32 {
    blend_rgb(theme.content, theme.warning, 0.18)
}

/// 按比例混合两个 RGB 颜色；只处理界面主题常用的低 24 位颜色。
fn blend_rgb(base: u32, overlay: u32, overlay_ratio: f32) -> u32 {
    let ratio = overlay_ratio.clamp(0.0, 1.0);
    let inverse_ratio = 1.0 - ratio;
    let base_r = ((base >> 16) & 0xff) as f32;
    let base_g = ((base >> 8) & 0xff) as f32;
    let base_b = (base & 0xff) as f32;
    let overlay_r = ((overlay >> 16) & 0xff) as f32;
    let overlay_g = ((overlay >> 8) & 0xff) as f32;
    let overlay_b = (overlay & 0xff) as f32;

    let red = (base_r * inverse_ratio + overlay_r * ratio).round() as u32;
    let green = (base_g * inverse_ratio + overlay_g * ratio).round() as u32;
    let blue = (base_b * inverse_ratio + overlay_b * ratio).round() as u32;
    (red << 16) | (green << 8) | blue
}

/// 将一段搜索高亮加入现有集合，并避开选区范围。
fn push_non_overlapping_highlight(
    highlights: &mut Vec<(Range<usize>, HighlightStyle)>,
    range: Range<usize>,
    selection_range: Option<&Range<usize>>,
    style: HighlightStyle,
) {
    if let Some(selection) = selection_range
        && ranges_overlap(&range, selection)
    {
        if range.start < selection.start {
            highlights.push((range.start..range.end.min(selection.start), style.clone()));
        }
        if selection.end < range.end {
            highlights.push((range.start.max(selection.end)..range.end, style));
        }
        return;
    }

    highlights.push((range, style));
}

/// 从基础范围中扣除保护范围，返回可以继续使用语法色的非重叠片段。
fn subtract_ranges(range: Range<usize>, protected_ranges: &[Range<usize>]) -> Vec<Range<usize>> {
    let mut pieces = vec![range];
    for protected in protected_ranges {
        pieces = pieces
            .into_iter()
            .flat_map(|piece| subtract_single_range(piece, protected))
            .collect();
    }
    pieces
}

/// 从单个范围中扣除一个保护范围。
fn subtract_single_range(range: Range<usize>, protected: &Range<usize>) -> Vec<Range<usize>> {
    if !ranges_overlap(&range, protected) {
        return vec![range];
    }

    let mut pieces = Vec::new();
    if range.start < protected.start {
        pieces.push(range.start..range.end.min(protected.start));
    }
    if protected.end < range.end {
        pieces.push(range.start.max(protected.end)..range.end);
    }
    pieces
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

/// 构造搜索结果行预览；长行只截取命中附近文本，避免列表滚动时处理整条超长日志。
fn search_result_preview_text(
    result: &crate::search::search_engine::SearchResult,
) -> (String, Vec<Range<usize>>) {
    let total_chars = character_count(&result.line_text);
    if total_chars <= SEARCH_RESULT_PREVIEW_MAX_CHARS {
        return (result.line_text.clone(), result.match_ranges.clone());
    }

    let first_match_column = result
        .match_ranges
        .first()
        .map(|range| char_column_for_byte_index(&result.line_text, range.start))
        .unwrap_or(0);
    let mut preview_start = first_match_column.saturating_sub(SEARCH_RESULT_PREVIEW_CONTEXT_CHARS);
    let mut preview_end = (preview_start + SEARCH_RESULT_PREVIEW_MAX_CHARS).min(total_chars);
    if preview_end == total_chars {
        preview_start = total_chars.saturating_sub(SEARCH_RESULT_PREVIEW_MAX_CHARS);
        preview_end = total_chars;
    }

    let has_prefix = preview_start > 0;
    let has_suffix = preview_end < total_chars;
    let prefix = if has_prefix { "..." } else { "" };
    let suffix = if has_suffix { "..." } else { "" };
    let prefix_len = character_count(prefix);
    let mut preview_text = String::new();
    preview_text.push_str(prefix);
    preview_text.push_str(&slice_character_range(
        &result.line_text,
        preview_start..preview_end,
    ));
    preview_text.push_str(suffix);

    let preview_ranges = result
        .match_ranges
        .iter()
        .filter_map(|range| {
            let start_column = char_column_for_byte_index(&result.line_text, range.start);
            let end_column = char_column_for_byte_index(&result.line_text, range.end);
            let clipped_start = start_column.max(preview_start);
            let clipped_end = end_column.min(preview_end);
            if clipped_start >= clipped_end {
                return None;
            }

            let local_start = clipped_start - preview_start + prefix_len;
            let local_end = clipped_end - preview_start + prefix_len;
            let start = byte_index_for_character(&preview_text, local_start);
            let end = byte_index_for_character(&preview_text, local_end);
            (start < end).then_some(start..end)
        })
        .collect();

    (preview_text, preview_ranges)
}

/// 将基于原始日志行的搜索字节范围转换为展示文本范围；制表符展开后需要重新映射。
fn search_ranges_for_display(
    raw_text: &str,
    display_text: &str,
    ranges: &[Range<usize>],
) -> Vec<Range<usize>> {
    ranges
        .iter()
        .filter_map(|range| {
            let raw_start_column = char_column_for_byte_index(raw_text, range.start);
            let raw_end_column = char_column_for_byte_index(raw_text, range.end);
            let display_start_column = display_column_for_raw_column(raw_text, raw_start_column);
            let display_end_column = display_column_for_raw_column(raw_text, raw_end_column);
            let start = byte_index_for_character(display_text, display_start_column);
            let end = byte_index_for_character(display_text, display_end_column);
            (start < end).then_some(start..end)
        })
        .collect()
}

/// 根据原始行字符列计算制表符展开后的展示列。
fn display_column_for_raw_column(raw_text: &str, raw_column: usize) -> usize {
    raw_text
        .chars()
        .take(raw_column)
        .map(|ch| if ch == '\t' { 4 } else { 1 })
        .sum()
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

    let estimated_char_width = estimated_log_char_width(app.log_content_font_size);
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
                                app.clear_line_marker_jump_cache(tab_id);
                                let _ = source_id;
                            } else {
                                app.clear_line_marker_jump_cache(tab_id);
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

/// 渲染日志搜索结果底部面板。
fn render_search_results_panel(
    app: &ArgusApp,
    theme: &AppTheme,
    cx: &mut Context<ArgusApp>,
) -> impl IntoElement {
    let result_count = app.log_search.results.len();
    let visible_item_count = app.log_search.visible_result_items.len();
    let is_running = app.log_search.task_state.is_running();
    let panel_title = match app.log_search.task_state {
        SearchTaskState::Idle => "搜索结果".to_string(),
        SearchTaskState::Running => {
            if app.log_search.run_kind == SearchRunKind::QuickKeywords {
                "正在快搜".to_string()
            } else {
                "正在搜索".to_string()
            }
        }
        SearchTaskState::Finished => {
            if app.log_search.run_kind == SearchRunKind::QuickKeywords {
                "快搜完成".to_string()
            } else {
                "搜索完成".to_string()
            }
        }
        SearchTaskState::Cancelled => "搜索已取消".to_string(),
        SearchTaskState::Failed(ref message) => format!("搜索失败：{message}"),
    };
    let status_text = if is_running {
        search_progress_text(app)
    } else {
        format!("共 {result_count} 条结果")
    };

    div()
        .id("log-search-results-panel")
        .relative()
        .h(px(app.log_search.result_panel_height))
        .flex_none()
        .flex()
        .flex_col()
        .border_t_1()
        .border_color(rgb(theme.border))
        .bg(rgb(theme.status_bar))
        .occlude()
        .on_mouse_up(
            MouseButton::Right,
            cx.listener(|app, event: &MouseUpEvent, _, cx| {
                app.open_search_results_context_menu(event.position);
                cx.stop_propagation();
                cx.notify();
            }),
        )
        .child(render_search_results_resize_handle(theme, cx))
        .child(
            div()
                .h(px(36.0))
                .px_3()
                .flex()
                .items_center()
                .gap_2()
                .text_size(px(12.0))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .child(if is_running {
                            render_loading_spinner(
                                ("log-search-results-spinner", 0),
                                theme.foreground_muted,
                                14.0,
                            )
                            .into_any_element()
                        } else {
                            render_icon(ArgusIcon::Search, theme.foreground_muted, 14.0)
                                .into_any_element()
                        })
                        .child(panel_title),
                )
                .child(
                    div()
                        .flex_1()
                        .truncate()
                        .text_color(rgb(theme.foreground_muted))
                        .child(status_text),
                )
                .child(render_icon_button(
                    "log-search-results-close",
                    ArgusIcon::Close,
                    "关闭结果",
                    false,
                    IconButtonSize::Small,
                    theme,
                    cx.listener(|app, _, _, cx| {
                        app.close_log_search_results_panel();
                        cx.notify();
                    }),
                )),
        )
        .when(result_count == 0, |this| {
            this.child(
                div()
                    .flex_1()
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_size(px(12.0))
                    .text_color(rgb(theme.foreground_muted))
                    .child(
                        app.log_search
                            .message
                            .clone()
                            .unwrap_or_else(|| "暂无搜索结果".to_string()),
                    ),
            )
        })
        .when(result_count > 0 && visible_item_count > 0, |this| {
            this.child(
                div()
                    .relative()
                    .flex_1()
                    .min_h(px(0.0))
                    .overflow_hidden()
                    .child(
                        uniform_list(
                            "log-search-result-list",
                            visible_item_count,
                            cx.processor(|app, range: Range<usize>, _window, cx| {
                                let theme = app.theme.clone();
                                let items = app.log_search.visible_result_items[range].to_vec();
                                items
                                    .iter()
                                    .map(|item| render_search_result_item(app, *item, &theme, cx))
                                    .collect::<Vec<_>>()
                            }),
                        )
                        .with_width_from_item(Some(0))
                        .with_horizontal_sizing_behavior(
                            ListHorizontalSizingBehavior::Unconstrained,
                        )
                        .size_full()
                        .block_mouse_except_scroll()
                        .track_scroll(app.log_search.result_scroll.clone()),
                    )
                    .children(render_search_results_scrollbars(app, theme, cx)),
            )
        })
}

/// 渲染搜索结果面板顶部拖拽条，用于调整底部面板高度。
fn render_search_results_resize_handle(
    theme: &AppTheme,
    cx: &mut Context<ArgusApp>,
) -> impl IntoElement {
    let entity = cx.entity();

    div()
        .id("log-search-results-resize-handle")
        .absolute()
        .top(px(0.0))
        .left(px(0.0))
        .right(px(0.0))
        .h(px(6.0))
        .cursor_pointer()
        .occlude()
        .hover(|this| this.bg(rgb(theme.border)))
        .child(
            canvas(
                |_, _, _| (),
                move |bounds, _, window: &mut Window, _| {
                    window.on_mouse_event({
                        let entity = entity.clone();
                        move |event: &MouseDownEvent, phase, _, cx| {
                            if !phase.bubble()
                                || event.button != MouseButton::Left
                                || !bounds.contains(&event.position)
                            {
                                return;
                            }
                            cx.stop_propagation();
                            entity.update(cx, |app, _| {
                                app.begin_search_result_panel_resize(event.position.y);
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
                            let handled =
                                entity.update(cx, |app, _| app.finish_search_result_panel_resize());
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
                            app.resize_search_result_panel(event.position.y)
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
}

/// 根据虚拟列表行类型分派渲染搜索分组或命中结果。
fn render_search_result_item(
    app: &ArgusApp,
    item: SearchResultListItem,
    theme: &AppTheme,
    cx: &mut Context<ArgusApp>,
) -> AnyElement {
    match item {
        SearchResultListItem::Group(group_index) => {
            render_search_result_group_row(app, group_index, theme, cx).into_any_element()
        }
        SearchResultListItem::Result(result_index) => {
            let Some(result) = app.log_search.results.get(result_index) else {
                return div().h(px(SEARCH_RESULT_ROW_HEIGHT)).into_any_element();
            };
            render_search_result_row(app, result_index, result, theme, cx).into_any_element()
        }
    }
}

/// 渲染搜索结果文件分组行。
fn render_search_result_group_row(
    app: &ArgusApp,
    group_index: usize,
    theme: &AppTheme,
    cx: &mut Context<ArgusApp>,
) -> impl IntoElement {
    let Some(group) = app.log_search.result_groups.get(group_index) else {
        return div().h(px(SEARCH_RESULT_ROW_HEIGHT)).into_any_element();
    };
    let is_collapsed = app
        .log_search
        .collapsed_result_groups
        .contains(&group.source_id);
    let result_count = group.end_index.saturating_sub(group.start_index);
    let group_label = format!("{} ({result_count})", group.label);
    let group_intrinsic_width = SEARCH_RESULT_ROW_HORIZONTAL_PADDING
        + 14.0
        + 14.0
        + SEARCH_RESULT_ROW_GAP_WIDTH * 2.0
        + estimated_search_result_text_width(&group_label)
        + estimated_search_result_text_width(&group.path);
    let row_width = search_result_row_width(app, group_intrinsic_width);

    div()
        .id(SharedString::from(format!(
            "log-search-result-group-{group_index}"
        )))
        .h(px(SEARCH_RESULT_ROW_HEIGHT))
        .w(px(row_width))
        .px_3()
        .flex()
        .items_center()
        .gap_2()
        .whitespace_nowrap()
        .text_size(px(12.0))
        .font_weight(gpui::FontWeight::SEMIBOLD)
        .text_color(rgb(theme.foreground))
        .bg(rgb(theme.current_line))
        .hover(|this| this.bg(rgb(theme.selection)))
        .cursor_pointer()
        .child(render_icon(
            if is_collapsed {
                ArgusIcon::Expand
            } else {
                ArgusIcon::Collapse
            },
            theme.foreground_muted,
            14.0,
        ))
        .child(render_icon(
            ArgusIcon::FileText,
            theme.foreground_muted,
            14.0,
        ))
        .child(
            div()
                .flex_none()
                .max_w(px(220.0))
                .truncate()
                .child(group_label),
        )
        .child(
            div()
                .flex_1()
                .min_w(px(0.0))
                .truncate()
                .text_color(rgb(theme.foreground_muted))
                .child(group.path.clone()),
        )
        .on_click(cx.listener(move |app, _, _, cx| {
            app.toggle_search_result_group(group_index);
            cx.notify();
        }))
        .on_mouse_up(
            MouseButton::Right,
            cx.listener(|app, event: &MouseUpEvent, _, cx| {
                app.open_search_results_context_menu(event.position);
                cx.stop_propagation();
                cx.notify();
            }),
        )
        .into_any_element()
}

/// 渲染搜索结果中的一行。
fn render_search_result_row(
    app: &ArgusApp,
    index: usize,
    result: &crate::search::search_engine::SearchResult,
    theme: &AppTheme,
    cx: &mut Context<ArgusApp>,
) -> impl IntoElement {
    let is_active = app.log_search.active_result_index == Some(index);
    let hover_background = if is_active {
        theme.selection
    } else {
        theme.content
    };
    let (preview_text, preview_match_ranges) = search_result_preview_text(result);
    let display_text = log_viewer_display_text(&preview_text).into_owned();
    let match_ranges =
        search_ranges_for_display(&preview_text, &display_text, &preview_match_ranges);
    let text_width = estimated_search_result_text_width(&display_text);
    let keyword_badges_width = search_result_keyword_badges_width(result);
    let row_width = search_result_row_width(
        app,
        SEARCH_RESULT_ROW_HORIZONTAL_PADDING
            + SEARCH_RESULT_LINE_LABEL_WIDTH
            + SEARCH_RESULT_ROW_GAP_WIDTH
            + keyword_badges_width
            + SEARCH_RESULT_ROW_GAP_WIDTH
            + text_width,
    );
    let text_element = StyledText::new(display_text)
        .with_highlights(
            match_ranges
                .into_iter()
                .map(|range| {
                    (
                        range,
                        HighlightStyle {
                            background_color: Some(rgb(theme.warning).into()),
                            color: Some(rgb(theme.background).into()),
                            ..Default::default()
                        },
                    )
                })
                .collect::<Vec<_>>(),
        )
        .into_any_element();

    div()
        .id(SharedString::from(format!("log-search-result-{index}")))
        .h(px(SEARCH_RESULT_ROW_HEIGHT))
        .w(px(row_width))
        .px_3()
        .flex()
        .items_center()
        .gap_2()
        .whitespace_nowrap()
        .text_size(px(12.0))
        .cursor_pointer()
        .when(is_active, |this| this.bg(rgb(theme.selection)))
        .hover(move |this| this.bg(rgb(hover_background)))
        .child(
            div()
                .w(px(78.0))
                .flex_none()
                .text_color(rgb(theme.foreground_muted))
                .child(format!("第 {} 行", result.line_number + 1)),
        )
        .child(render_search_result_keyword_badges(result, theme))
        .child(
            div()
                .flex_none()
                .w(px(text_width.max(1.0)))
                .whitespace_nowrap()
                .font_family(ARGUS_LOG_FONT_FAMILY)
                .text_color(rgb(theme.foreground))
                .child(text_element),
        )
        .on_click(cx.listener(move |app, _, _, cx| {
            app.activate_search_result(index, cx);
            cx.notify();
        }))
        .on_mouse_up(
            MouseButton::Right,
            cx.listener(|app, event: &MouseUpEvent, _, cx| {
                app.open_search_results_context_menu(event.position);
                cx.stop_propagation();
                cx.notify();
            }),
        )
}

/// 渲染搜索结果行前的命中关键字徽标。
fn render_search_result_keyword_badges(
    result: &crate::search::search_engine::SearchResult,
    theme: &AppTheme,
) -> impl IntoElement {
    div()
        .flex_none()
        .flex()
        .items_center()
        .gap_1()
        .children(result.matched_keywords.iter().take(4).map(|keyword| {
            div()
                .h(px(18.0))
                .px_1()
                .flex()
                .items_center()
                .rounded_sm()
                .bg(rgb(theme.selection))
                .text_size(px(11.0))
                .line_height(px(18.0))
                .text_color(rgb(theme.foreground))
                .child(keyword.clone())
        }))
        .when(result.matched_keywords.len() > 4, |this| {
            this.child(
                div()
                    .h(px(18.0))
                    .px_1()
                    .flex()
                    .items_center()
                    .rounded_sm()
                    .bg(rgb(theme.current_line))
                    .text_size(px(11.0))
                    .line_height(px(18.0))
                    .text_color(rgb(theme.foreground_muted))
                    .child(format!("+{}", result.matched_keywords.len() - 4)),
            )
        })
}

/// 估算搜索结果关键字徽标宽度，用于横向滚动范围。
fn search_result_keyword_badges_width(result: &crate::search::search_engine::SearchResult) -> f32 {
    let visible_keywords_width = result
        .matched_keywords
        .iter()
        .take(4)
        .map(|keyword| estimated_search_result_text_width(keyword) + 14.0)
        .sum::<f32>();
    let overflow_width = if result.matched_keywords.len() > 4 {
        28.0
    } else {
        0.0
    };
    visible_keywords_width + overflow_width
}

/// 返回搜索结果行实际渲染宽度：至少撑满当前视口，内容更宽时交给横向滚动条。
fn search_result_row_width(app: &ArgusApp, intrinsic_width: f32) -> f32 {
    let viewport_width = search_result_viewport_width(app);
    app.log_search
        .result_list_content_width
        .max(SEARCH_RESULT_ROW_MIN_WIDTH)
        .max(viewport_width)
        .max(intrinsic_width)
}

/// 读取搜索结果列表当前可视宽度；首帧尚未测量时返回 0，由最小宽度兜底。
fn search_result_viewport_width(app: &ArgusApp) -> f32 {
    let scroll_state = app.log_search.result_scroll.0.borrow();
    f32::from(scroll_state.base_handle.bounds().size.width)
}

/// 估算搜索结果单行文本宽度，中文和其它非 ASCII 字符按更宽字形处理，避免提前换行。
fn estimated_search_result_text_width(text: &str) -> f32 {
    text.chars()
        .map(|character| {
            if character.is_ascii() {
                SEARCH_RESULT_ASCII_CHAR_WIDTH
            } else {
                SEARCH_RESULT_WIDE_CHAR_WIDTH
            }
        })
        .sum()
}

/// 渲染搜索结果面板自定义横纵滚动条。
fn render_search_results_scrollbars(
    app: &ArgusApp,
    theme: &AppTheme,
    cx: &mut Context<ArgusApp>,
) -> Vec<AnyElement> {
    let scroll_state = app.log_search.result_scroll.0.borrow();
    let bounds = scroll_state.base_handle.bounds();
    let offset = scroll_state.base_handle.offset();
    let scroll_handle = scroll_state.base_handle.clone();
    let size = scroll_state.last_item_size;
    drop(scroll_state);

    let Some(size) = size else {
        return Vec::new();
    };

    let mut scrollbars = Vec::new();
    if let Some(metrics) = scrollbar_metrics(
        size.item.height,
        size.contents.height,
        -offset.y,
        false,
        px(0.0),
    ) {
        scrollbars.push(render_search_result_scrollbar_thumb(
            SearchResultScrollbarAxis::Vertical,
            metrics,
            bounds,
            scroll_handle.clone(),
            theme,
            cx,
        ));
    }

    let content_width = size
        .contents
        .width
        .max(px(app.log_search.result_list_content_width));
    if let Some(metrics) =
        scrollbar_metrics(bounds.size.width, content_width, -offset.x, true, px(0.0))
    {
        scrollbars.push(render_search_result_scrollbar_thumb(
            SearchResultScrollbarAxis::Horizontal,
            metrics,
            bounds,
            scroll_handle,
            theme,
            cx,
        ));
    }

    scrollbars
}

/// 渲染搜索结果面板单个滚动条。
fn render_search_result_scrollbar_thumb(
    axis: SearchResultScrollbarAxis,
    metrics: LogScrollbarMetrics,
    viewport_bounds: gpui::Bounds<gpui::Pixels>,
    scroll_handle: gpui::ScrollHandle,
    theme: &AppTheme,
    cx: &mut Context<ArgusApp>,
) -> AnyElement {
    let entity = cx.entity();
    let is_horizontal = axis == SearchResultScrollbarAxis::Horizontal;
    let mut thumb = div()
        .id(SharedString::from(format!(
            "log-search-result-scrollbar-{axis:?}"
        )))
        .absolute()
        .rounded_lg()
        .bg(rgb(theme.foreground_muted))
        .opacity(0.42)
        .hover(|this| this.opacity(0.76))
        .occlude();

    thumb = if is_horizontal {
        thumb
            .left(metrics.thumb_start)
            .bottom(px(3.0))
            .w(metrics.thumb_length)
            .h(px(LOG_SCROLLBAR_WIDTH))
    } else {
        thumb
            .top(metrics.thumb_start)
            .right(px(3.0))
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
                            entity.update(cx, |app, _| {
                                let pointer = if is_horizontal {
                                    event.position.x
                                } else {
                                    event.position.y
                                };
                                let thumb_start = if is_horizontal {
                                    thumb_bounds.origin.x
                                } else {
                                    thumb_bounds.origin.y
                                };
                                app.log_search.result_scrollbar_drag =
                                    Some(SearchResultScrollbarDrag {
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
                                let handled = app.log_search.result_scrollbar_drag.is_some();
                                app.log_search.result_scrollbar_drag = None;
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
                            let Some(drag) = app.log_search.result_scrollbar_drag else {
                                return false;
                            };
                            if drag.axis != axis {
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
                            let current = scroll_handle.offset();
                            if is_horizontal {
                                scroll_handle.set_offset(point(-scroll, current.y));
                            } else {
                                scroll_handle.set_offset(point(current.x, -scroll));
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

/// 返回搜索结果面板进度文案。
fn search_progress_text(app: &ArgusApp) -> String {
    let progress = &app.log_search.progress;
    let scope = app.log_search.scope;
    let progress_part = match scope {
        crate::search::search_engine::SearchScope::CurrentFile => {
            format!("行进度 {}/{}", progress.scanned_lines, progress.total_lines)
        }
        crate::search::search_engine::SearchScope::Directory
        | crate::search::search_engine::SearchScope::SelectedFiles => {
            format!(
                "文件进度 {}/{}",
                progress.scanned_files, progress.total_files
            )
        }
    };
    let current = progress
        .current_path
        .as_ref()
        .map(|path| format!("，当前：{path}"))
        .unwrap_or_default();
    format!(
        "{}，{}，结果 {} 条{}",
        scope.label(),
        progress_part,
        app.log_search.results.len(),
        current
    )
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

    /// 验证搜索跳转行背景不再复用文本选区色，避免选中当前行时视觉混淆。
    #[test]
    fn active_search_line_background_differs_from_selection() {
        let theme = AppTheme::dark();
        let background = active_search_line_background(&theme);

        assert_ne!(background, theme.selection);
        assert_ne!(background, theme.content);
        assert_eq!(blend_rgb(0x000000, 0xffffff, 0.5), 0x808080);
    }

    /// 验证分页长行可按展示列直接截取，并保持 tab 展开为 4 个空格。
    #[test]
    fn paged_visible_text_slices_raw_line_without_full_expansion() {
        let visible = visible_log_text_from_raw("ab\tcdef", Some(&(2..8)));

        assert_eq!(visible.text, "    cd");
        assert_eq!(visible.char_range, 2..8);
    }
}
