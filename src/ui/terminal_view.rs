//! 文件职责：渲染 SSH 终端标签页的右侧内容面板。
//! 创建日期：2026-06-26
//! 修改日期：2026-06-26
//! 作者：Argus 开发团队
//! 主要功能：展示连接状态、远程终端输出，并将键盘输入转发给终端会话。

use std::ops::Range;
use std::time::Duration;

use gpui::{
    Animation, AnimationExt, AnyElement, Bounds, Context, CursorStyle, FontWeight, HighlightStyle,
    IntoElement, KeyDownEvent, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, Pixels,
    ScrollWheelEvent, SharedString, StyledText, TextRun, UnderlineStyle, Window, div, prelude::*,
    px, rgb,
};

use crate::app::ArgusApp;
use crate::fonts::ARGUS_LOG_FONT_FAMILY;
use crate::terminal::{
    TerminalCellStyle, TerminalColor, TerminalGridPosition, TerminalScreenLine,
    TerminalScreenSnapshot, TerminalSessionState, TerminalStatus, TerminalTextSelection,
};
use crate::theme::AppTheme;

/// 终端正文行高。
const TERMINAL_LINE_HEIGHT: f32 = 18.0;
/// 终端正文字号。
const TERMINAL_FONT_SIZE: f32 = 12.0;
/// 终端正文水平内边距，给光标和边缘保留轻微呼吸感。
const TERMINAL_HORIZONTAL_PADDING: f32 = 12.0;
/// 终端正文垂直内边距，避免第一行贴住状态栏。
const TERMINAL_VERTICAL_PADDING: f32 = 10.0;
/// 终端最小行数，避免极窄窗口下向远端发送不可用尺寸。
const TERMINAL_MIN_ROWS: u16 = 4;
/// 终端最小列数，避免极窄窗口下向远端发送不可用尺寸。
const TERMINAL_MIN_COLS: u16 = 20;
/// 终端最大行数，限制异常窗口尺寸导致远端 PTY 被放大过度。
const TERMINAL_MAX_ROWS: u16 = 240;
/// 终端最大列数，限制异常窗口尺寸导致远端 PTY 被放大过度。
const TERMINAL_MAX_COLS: u16 = 400;
/// 终端历史滚动条宽度，和日志查看滚动条保持一致。
const TERMINAL_SCROLLBAR_WIDTH: f32 = 5.0;
/// 终端历史滚动条距离边缘的留白。
const TERMINAL_SCROLLBAR_PADDING: f32 = 4.0;
/// 终端历史滚动条滑块最小高度，和日志查看滚动条保持一致。
const TERMINAL_SCROLLBAR_MIN_THUMB: f32 = 32.0;

/// 渲染 SSH 终端面板。
pub fn render(
    app: &ArgusApp,
    session_id: usize,
    window: &mut Window,
    cx: &mut Context<ArgusApp>,
) -> impl IntoElement {
    let theme = app.theme.clone();
    let Some(session) = app.terminal_sessions.get(&session_id) else {
        return div()
            .size_full()
            .flex()
            .items_center()
            .justify_center()
            .text_color(rgb(theme.foreground_muted))
            .child("终端会话不存在")
            .into_any_element();
    };
    let focus_handle = app
        .input_focus_handles
        .as_ref()
        .map(|handles| handles.terminal.clone());

    div()
        .id("ssh-terminal-view")
        .size_full()
        .flex()
        .flex_col()
        .bg(rgb(theme.content))
        .when_some(focus_handle.clone(), |this, focus_handle| {
            this.track_focus(&focus_handle)
        })
        .focusable()
        .on_click(cx.listener(move |app, _, window, cx| {
            if let Some(handles) = app.input_focus_handles.as_ref() {
                handles.terminal.focus(window);
            }
            cx.stop_propagation();
            cx.notify();
        }))
        .on_key_down(cx.listener(move |app, event: &KeyDownEvent, _, cx| {
            cx.stop_propagation();
            app.handle_terminal_key(&event.keystroke, cx);
            cx.notify();
        }))
        .child(render_terminal_body(session, app, window, cx))
        .into_any_element()
}

/// 渲染终端正文区域；正文文本使用 GPUI 文本元素渲染，避免 canvas 文字发虚。
fn render_terminal_body(
    session: &TerminalSessionState,
    app: &ArgusApp,
    window: &mut Window,
    cx: &mut Context<ArgusApp>,
) -> impl IntoElement {
    let theme = app.theme.clone();
    let metrics = terminal_metrics(window);
    let session_id = session.id;
    let current_rows = session.rows;
    let current_cols = session.cols;
    let viewport_handle = session.viewport_scroll.clone();
    let viewport_bounds = viewport_handle.bounds();
    let snapshot = session.screen_snapshot();
    let placeholder = terminal_placeholder(session, &snapshot);
    let scrollbar_metrics = terminal_scrollbar_metrics(viewport_bounds, &snapshot, metrics);
    let is_terminal_focused = app
        .input_focus_handles
        .as_ref()
        .is_some_and(|handles| handles.terminal.is_focused(window));
    let entity = cx.entity();

    div()
        .on_children_prepainted({
            let viewport_handle = viewport_handle.clone();
            move |_, window, _| {
                let bounds = viewport_handle.bounds();
                if bounds.size.width <= px(0.0) || bounds.size.height <= px(0.0) {
                    return;
                }
                let metrics = terminal_metrics(window);
                let desired_rows = terminal_rows_for_bounds(bounds, metrics.line_height);
                let desired_cols = terminal_cols_for_bounds(bounds, metrics.cell_width);
                if desired_rows == current_rows && desired_cols == current_cols {
                    return;
                }
                let entity = entity.clone();
                window.on_next_frame(move |_, cx| {
                    entity.update(cx, |app, cx| {
                        app.resize_terminal_session(session_id, desired_rows, desired_cols);
                        cx.notify();
                    });
                });
            }
        })
        .id("ssh-terminal-body")
        .relative()
        .flex_1()
        .min_h(px(0.0))
        .overflow_hidden()
        .bg(rgb(theme.background))
        .cursor(CursorStyle::IBeam)
        .track_scroll(&viewport_handle)
        .on_scroll_wheel(cx.listener(move |app, event: &ScrollWheelEvent, _, cx| {
            cx.stop_propagation();
            let line_delta = terminal_scroll_line_delta(event);
            if app.scroll_terminal_scrollback(session_id, line_delta) {
                cx.notify();
            }
        }))
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(move |app, event: &MouseDownEvent, window, cx| {
                cx.stop_propagation();
                if let Some(handles) = app.input_focus_handles.as_ref() {
                    handles.terminal.focus(window);
                }
                let metrics = terminal_metrics(window);
                if let Some(position) =
                    terminal_position_from_pointer(app, session_id, event.position, metrics)
                {
                    app.begin_terminal_selection(session_id, position, event.click_count);
                    cx.notify();
                }
            }),
        )
        .on_mouse_move(cx.listener(move |app, event: &MouseMoveEvent, window, cx| {
            if !event.dragging() {
                return;
            }

            let mut handled = false;
            if let Some(metrics) = scrollbar_metrics {
                let bounds = app
                    .terminal_sessions
                    .get(&session_id)
                    .map(|session| session.viewport_scroll.bounds())
                    .unwrap_or_default();
                handled |= app.drag_terminal_scrollbar(
                    session_id,
                    f32::from(event.position.y),
                    f32::from(bounds.top()),
                    f32::from(metrics.track_start),
                    f32::from(metrics.track_length),
                    f32::from(metrics.thumb_length),
                    metrics.max_scrollback_offset,
                );
            }

            if app.is_terminal_selection_drag_active(session_id) {
                let metrics = terminal_metrics(window);
                if let Some(position) =
                    terminal_position_from_pointer(app, session_id, event.position, metrics)
                {
                    handled |= app.update_terminal_selection(session_id, position);
                }
            }

            if handled {
                cx.stop_propagation();
                cx.notify();
            }
        }))
        .on_mouse_up(
            MouseButton::Left,
            cx.listener(move |app, _: &MouseUpEvent, _, cx| {
                let handled = app.finish_terminal_selection(session_id)
                    | app.finish_terminal_scrollbar_drag(session_id);
                if handled {
                    cx.stop_propagation();
                    cx.notify();
                }
            }),
        )
        .on_mouse_up_out(
            MouseButton::Left,
            cx.listener(move |app, _: &MouseUpEvent, _, cx| {
                let handled = app.finish_terminal_selection(session_id)
                    | app.finish_terminal_scrollbar_drag(session_id);
                if handled {
                    cx.stop_propagation();
                    cx.notify();
                }
            }),
        )
        .child(if let Some(placeholder) = placeholder {
            render_terminal_placeholder(placeholder, &theme).into_any_element()
        } else {
            render_terminal_screen(&snapshot, session_id, metrics, is_terminal_focused, &theme)
                .into_any_element()
        })
        .when_some(scrollbar_metrics, |this, metrics| {
            this.child(render_terminal_scrollbar(
                session_id,
                viewport_bounds,
                metrics,
                &theme,
                cx,
            ))
        })
}

/// 根据当前窗口字体环境测量终端等宽单元格。
fn terminal_metrics(window: &mut Window) -> TerminalMetrics {
    let mut text_style = window.text_style();
    text_style.font_family = ARGUS_LOG_FONT_FAMILY.into();
    text_style.font_size = px(TERMINAL_FONT_SIZE).into();
    text_style.font_weight = FontWeight::NORMAL;
    let font_size = text_style.font_size.to_pixels(window.rem_size());
    let run = TextRun {
        len: 1,
        font: text_style.font(),
        color: rgb(0xffffff).into(),
        background_color: None,
        underline: None,
        strikethrough: None,
    };
    let shaped = window
        .text_system()
        .shape_line(SharedString::from("W"), font_size, &[run], None);

    TerminalMetrics {
        cell_width: shaped.width.max(px(1.0)),
        line_height: px(TERMINAL_LINE_HEIGHT),
    }
}

/// 终端单元格测量结果。
#[derive(Clone, Copy)]
struct TerminalMetrics {
    /// 单个终端列宽。
    cell_width: Pixels,
    /// 单行高度。
    line_height: Pixels,
}

/// 终端历史滚动条几何指标。
#[derive(Clone, Copy)]
struct TerminalScrollbarMetrics {
    /// 滑块相对正文顶部的起始位置。
    thumb_start: Pixels,
    /// 滑块长度。
    thumb_length: Pixels,
    /// 滚动轨道相对正文顶部的起始位置。
    track_start: Pixels,
    /// 滚动轨道长度。
    track_length: Pixels,
    /// 最大 scrollback 偏移。
    max_scrollback_offset: usize,
}

/// 根据正文区域高度计算远程 PTY 行数。
fn terminal_rows_for_bounds(bounds: Bounds<Pixels>, line_height: Pixels) -> u16 {
    let available_height = (f32::from(bounds.size.height) - TERMINAL_VERTICAL_PADDING * 2.0)
        .max(f32::from(line_height));
    ((available_height / f32::from(line_height)).floor() as u16)
        .clamp(TERMINAL_MIN_ROWS, TERMINAL_MAX_ROWS)
}

/// 根据正文区域宽度计算远程 PTY 列数。
fn terminal_cols_for_bounds(bounds: Bounds<Pixels>, cell_width: Pixels) -> u16 {
    let available_width = (f32::from(bounds.size.width) - TERMINAL_HORIZONTAL_PADDING * 2.0)
        .max(f32::from(cell_width));
    ((available_width / f32::from(cell_width)).floor() as u16)
        .clamp(TERMINAL_MIN_COLS, TERMINAL_MAX_COLS)
}

/// 根据终端历史量和当前偏移计算滚动条位置。
fn terminal_scrollbar_metrics(
    bounds: Bounds<Pixels>,
    snapshot: &TerminalScreenSnapshot,
    terminal_metrics: TerminalMetrics,
) -> Option<TerminalScrollbarMetrics> {
    let max_offset = snapshot.max_scrollback_offset;
    if max_offset == 0 || bounds.size.height <= px(0.0) {
        return None;
    }

    let viewport_len = bounds.size.height;
    let max_scroll = terminal_metrics.line_height * max_offset as f32;
    let content_len = viewport_len + max_scroll;
    let track_start = px(TERMINAL_SCROLLBAR_PADDING);
    let track_length = (viewport_len - px(TERMINAL_SCROLLBAR_PADDING * 2.0)).max(px(1.0));
    let min_thumb = px(TERMINAL_SCROLLBAR_MIN_THUMB).min(track_length);
    let thumb_length = (viewport_len * (viewport_len / content_len)).clamp(min_thumb, track_length);
    let movable = (track_length - thumb_length).max(px(0.0));
    let offset_from_top = max_offset.saturating_sub(snapshot.scrollback_offset.min(max_offset));
    let scroll_ratio =
        ((terminal_metrics.line_height * offset_from_top as f32) / max_scroll).clamp(0.0, 1.0);
    let thumb_start = track_start + movable * scroll_ratio;

    Some(TerminalScrollbarMetrics {
        thumb_start,
        thumb_length,
        track_start,
        track_length,
        max_scrollback_offset: max_offset,
    })
}

/// 把平台滚轮事件转换为终端 scrollback 行数；正数表示查看更早的输出。
fn terminal_scroll_line_delta(event: &ScrollWheelEvent) -> f32 {
    let pixel_delta = event.delta.pixel_delta(px(TERMINAL_LINE_HEIGHT));
    f32::from(pixel_delta.y) / TERMINAL_LINE_HEIGHT
}

/// 连接中或失败时，如果屏幕没有远端内容，则展示状态提示。
fn terminal_placeholder(
    session: &TerminalSessionState,
    snapshot: &TerminalScreenSnapshot,
) -> Option<String> {
    if !snapshot.runs.is_empty() || session.status == TerminalStatus::Connected {
        return None;
    }
    Some(
        session
            .message
            .clone()
            .unwrap_or_else(|| "等待远程终端输出...".to_string()),
    )
}

/// 渲染空终端状态提示。
fn render_terminal_placeholder(text: String, theme: &AppTheme) -> impl IntoElement {
    div()
        .absolute()
        .left(px(TERMINAL_HORIZONTAL_PADDING))
        .top(px(TERMINAL_VERTICAL_PADDING))
        .font_family(ARGUS_LOG_FONT_FAMILY)
        .text_size(px(TERMINAL_FONT_SIZE))
        .line_height(px(TERMINAL_LINE_HEIGHT))
        .text_color(rgb(theme.foreground_muted))
        .child(text)
}

/// 渲染当前终端屏幕文本、颜色、选区和光标。
fn render_terminal_screen(
    snapshot: &TerminalScreenSnapshot,
    session_id: usize,
    metrics: TerminalMetrics,
    is_terminal_focused: bool,
    theme: &AppTheme,
) -> impl IntoElement {
    let content_width =
        px(TERMINAL_HORIZONTAL_PADDING * 2.0) + metrics.cell_width * snapshot.cols as f32;
    let content_height =
        px(TERMINAL_VERTICAL_PADDING * 2.0) + metrics.line_height * snapshot.rows as f32;

    div()
        .relative()
        .size_full()
        .overflow_hidden()
        .child(
            div()
                .relative()
                .w(content_width)
                .h(content_height)
                .pt(px(TERMINAL_VERTICAL_PADDING))
                .pl(px(TERMINAL_HORIZONTAL_PADDING))
                .children(
                    snapshot
                        .lines
                        .iter()
                        .enumerate()
                        .map(|(row, line)| {
                            render_terminal_line(row as u16, line, snapshot, theme)
                                .into_any_element()
                        })
                        .collect::<Vec<_>>(),
                ),
        )
        .when(!snapshot.is_cursor_hidden && is_terminal_focused, |this| {
            this.child(render_terminal_cursor(snapshot, session_id, metrics, theme))
        })
}

/// 渲染一行终端文本，并合成 ANSI 颜色、背景色和当前选区高亮。
fn render_terminal_line(
    row: u16,
    line: &TerminalScreenLine,
    snapshot: &TerminalScreenSnapshot,
    theme: &AppTheme,
) -> impl IntoElement {
    let highlights = terminal_line_highlights(row, line, snapshot, theme);
    let text_element = if highlights.is_empty() {
        line.text.clone().into_any_element()
    } else {
        StyledText::new(line.text.clone())
            .with_highlights(highlights)
            .into_any_element()
    };

    div()
        .h(px(TERMINAL_LINE_HEIGHT))
        .line_height(px(TERMINAL_LINE_HEIGHT))
        .font_family(ARGUS_LOG_FONT_FAMILY)
        .text_size(px(TERMINAL_FONT_SIZE))
        .text_color(rgb(theme.foreground))
        .whitespace_nowrap()
        .child(text_element)
}

/// 合成终端行内所有非重叠高亮范围，选区覆盖 ANSI 背景。
fn terminal_line_highlights(
    row: u16,
    line: &TerminalScreenLine,
    snapshot: &TerminalScreenSnapshot,
    theme: &AppTheme,
) -> Vec<(Range<usize>, HighlightStyle)> {
    let selection_range =
        terminal_selection_column_range_for_row(snapshot.selection, row, snapshot.cols)
            .map(|range| line.byte_range_for_columns(range))
            .filter(|range| range.start < range.end);
    let mut highlights = Vec::new();

    for run in snapshot.runs.iter().filter(|run| run.row == row) {
        let run_range = line.byte_range_for_columns(run.start_col..run.start_col + run.cols);
        if run_range.start >= run_range.end {
            continue;
        }
        for visible_range in subtract_optional_range(run_range, selection_range.as_ref()) {
            highlights.push((visible_range, terminal_highlight_style(run.style, theme)));
        }
    }

    if let Some(selection_range) = selection_range {
        highlights.push((
            selection_range,
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

/// 返回当前行被终端选区覆盖的列范围。
fn terminal_selection_column_range_for_row(
    selection: Option<TerminalTextSelection>,
    row: u16,
    cols: u16,
) -> Option<Range<u16>> {
    let selection = selection?;
    let (start, end) = selection.normalized();
    if row < start.row || row > end.row {
        return None;
    }

    let range = if start.row == end.row {
        start.col..end.col
    } else if row == start.row {
        start.col..cols
    } else if row == end.row {
        0..end.col
    } else {
        0..cols
    };

    (range.start < range.end).then_some(range)
}

/// 从基础高亮范围中扣除选区范围，避免 StyledText 收到重叠 highlight。
fn subtract_optional_range(
    range: Range<usize>,
    protected: Option<&Range<usize>>,
) -> Vec<Range<usize>> {
    let Some(protected) = protected else {
        return vec![range];
    };
    if protected.end <= range.start || protected.start >= range.end {
        return vec![range];
    }

    let mut ranges = Vec::new();
    if range.start < protected.start {
        ranges.push(range.start..protected.start.min(range.end));
    }
    if protected.end < range.end {
        ranges.push(protected.end.max(range.start)..range.end);
    }
    ranges
        .into_iter()
        .filter(|range| range.start < range.end)
        .collect()
}

/// 将终端样式转换为 GPUI 文本高亮样式。
fn terminal_highlight_style(style: TerminalCellStyle, theme: &AppTheme) -> HighlightStyle {
    let (foreground, background) = resolved_terminal_colors(style, theme);
    HighlightStyle {
        color: Some(rgb(foreground).into()),
        background_color: (background != theme.background).then_some(rgb(background).into()),
        font_weight: style.is_bold.then_some(FontWeight::BOLD),
        underline: style.is_underline.then_some(UnderlineStyle {
            color: Some(rgb(foreground).into()),
            thickness: px(1.0),
            wavy: false,
        }),
        ..Default::default()
    }
}

/// 渲染终端线条光标；样式、宽度、高度和闪烁频率与输入框光标保持一致。
fn render_terminal_cursor(
    snapshot: &TerminalScreenSnapshot,
    session_id: usize,
    metrics: TerminalMetrics,
    theme: &AppTheme,
) -> impl IntoElement {
    let cursor_row = snapshot.cursor_row.min(snapshot.rows.saturating_sub(1));
    let cursor_col = snapshot.cursor_col.min(snapshot.cols);
    div()
        .id(SharedString::from(format!("terminal-cursor-{session_id}")))
        .absolute()
        .left(px(TERMINAL_HORIZONTAL_PADDING) + metrics.cell_width * cursor_col as f32)
        .top(px(TERMINAL_VERTICAL_PADDING) + metrics.line_height * cursor_row as f32 + px(1.0))
        .w(px(1.0))
        .h((metrics.line_height - px(2.0)).max(px(1.0)))
        .bg(rgb(theme.foreground))
        .with_animation(
            ("terminal-cursor-blink", session_id),
            Animation::new(Duration::from_millis(900))
                .repeat()
                .with_easing(gpui::pulsating_between(0.08, 1.0)),
            |this, opacity| this.opacity(opacity),
        )
}

/// 渲染终端历史滚动条滑块；终端和日志查看一样不绘制背景轨道。
fn render_terminal_scrollbar(
    session_id: usize,
    viewport_bounds: Bounds<Pixels>,
    metrics: TerminalScrollbarMetrics,
    theme: &AppTheme,
    cx: &mut Context<ArgusApp>,
) -> AnyElement {
    div()
        .id(SharedString::from(format!(
            "terminal-scrollbar-{session_id}"
        )))
        .absolute()
        .occlude()
        .top(metrics.thumb_start)
        .right(px(TERMINAL_SCROLLBAR_PADDING))
        .w(px(TERMINAL_SCROLLBAR_WIDTH))
        .h(metrics.thumb_length)
        .rounded_lg()
        .bg(rgb(theme.foreground_muted))
        .opacity(0.38)
        .hover(|this| this.opacity(0.72))
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(move |app, event: &MouseDownEvent, _, cx| {
                cx.stop_propagation();
                let cursor_offset =
                    f32::from(event.position.y - viewport_bounds.top() - metrics.thumb_start);
                app.begin_terminal_scrollbar_drag(session_id, cursor_offset);
                cx.notify();
            }),
        )
        .into_any_element()
}

/// 把鼠标位置转换为当前终端屏幕行列。
fn terminal_position_from_pointer(
    app: &ArgusApp,
    session_id: usize,
    position: gpui::Point<Pixels>,
    metrics: TerminalMetrics,
) -> Option<TerminalGridPosition> {
    let session = app.terminal_sessions.get(&session_id)?;
    if session.rows == 0 || session.cols == 0 {
        return None;
    }
    let bounds = session.viewport_scroll.bounds();
    if bounds.size.width <= px(0.0) || bounds.size.height <= px(0.0) {
        return None;
    }

    let local_x = f32::from(position.x - bounds.left()) - TERMINAL_HORIZONTAL_PADDING;
    let local_y = f32::from(position.y - bounds.top()) - TERMINAL_VERTICAL_PADDING;
    let col = if local_x <= 0.0 {
        0
    } else {
        (local_x / f32::from(metrics.cell_width)).floor() as u16
    }
    .min(session.cols);
    let row = if local_y <= 0.0 {
        0
    } else {
        (local_y / f32::from(metrics.line_height)).floor() as u16
    }
    .min(session.rows.saturating_sub(1));

    Some(TerminalGridPosition { row, col })
}

/// 根据终端样式和主题解析最终前景/背景色。
fn resolved_terminal_colors(style: TerminalCellStyle, theme: &AppTheme) -> (u32, u32) {
    let mut foreground = terminal_color_to_rgb(style.fg, theme.foreground, style.is_bold);
    let mut background = terminal_color_to_rgb(style.bg, theme.background, false);
    if style.is_inverse {
        std::mem::swap(&mut foreground, &mut background);
    }
    if style.is_dim {
        foreground = dim_rgb(foreground);
    }
    (foreground, background)
}

/// 将终端颜色转换为 RGB 数值。
fn terminal_color_to_rgb(color: TerminalColor, default_color: u32, is_bold: bool) -> u32 {
    match color {
        TerminalColor::Default => default_color,
        TerminalColor::Rgb(red, green, blue) => rgb_value(red, green, blue),
        TerminalColor::Indexed(index) => terminal_indexed_color(index, is_bold),
    }
}

/// 解析 ANSI 16 色和 xterm 256 色索引。
fn terminal_indexed_color(index: u8, is_bold: bool) -> u32 {
    const ANSI_COLORS: [u32; 16] = [
        0x000000, 0xcd3131, 0x0dbc79, 0xe5e510, 0x2472c8, 0xbc3fbc, 0x11a8cd, 0xe5e5e5, 0x666666,
        0xf14c4c, 0x23d18b, 0xf5f543, 0x3b8eea, 0xd670d6, 0x29b8db, 0xffffff,
    ];
    if index < 16 {
        let adjusted_index = if is_bold && index < 8 {
            index + 8
        } else {
            index
        };
        return ANSI_COLORS[adjusted_index as usize];
    }
    if index <= 231 {
        let cube_index = index - 16;
        let red = color_cube_component(cube_index / 36);
        let green = color_cube_component((cube_index / 6) % 6);
        let blue = color_cube_component(cube_index % 6);
        return rgb_value(red, green, blue);
    }
    let level = 8 + (index - 232) * 10;
    rgb_value(level, level, level)
}

/// xterm 6x6x6 色块单通道值。
fn color_cube_component(value: u8) -> u8 {
    if value == 0 { 0 } else { 55 + value * 40 }
}

/// 合成 RGB 数值。
fn rgb_value(red: u8, green: u8, blue: u8) -> u32 {
    ((red as u32) << 16) | ((green as u32) << 8) | blue as u32
}

/// 将暗淡文本压低亮度，模拟 SGR dim 效果。
fn dim_rgb(color: u32) -> u32 {
    let red = ((color >> 16) & 0xff) as u8;
    let green = ((color >> 8) & 0xff) as u8;
    let blue = (color & 0xff) as u8;
    rgb_value(
        (red as f32 * 0.62) as u8,
        (green as f32 * 0.62) as u8,
        (blue as f32 * 0.62) as u8,
    )
}
