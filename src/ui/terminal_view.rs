//! 文件职责：渲染 SSH 终端标签页的右侧内容面板。
//! 创建日期：2026-06-26
//! 修改日期：2026-06-26
//! 作者：Argus 开发团队
//! 主要功能：展示连接状态、远程终端输出，并将键盘输入转发给终端会话。

use gpui::{
    App, Bounds, Context, Entity, FontWeight, IntoElement, KeyDownEvent, MouseButton,
    MouseDownEvent, MouseMoveEvent, MouseUpEvent, Pixels, ScrollWheelEvent, SharedString, TextRun,
    UnderlineStyle, Window, canvas, div, fill, point, prelude::*, px, rgb, size,
};

use crate::app::ArgusApp;
use crate::fonts::ARGUS_LOG_FONT_FAMILY;
use crate::terminal::{
    TerminalCellRun, TerminalCellStyle, TerminalColor, TerminalScreenSnapshot,
    TerminalSessionState, TerminalStatus,
};
use crate::ui::components::icon::{ArgusIcon, render_icon};

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
pub fn render(app: &ArgusApp, session_id: usize, cx: &mut Context<ArgusApp>) -> impl IntoElement {
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
            app.handle_terminal_key(&event.keystroke);
            cx.notify();
        }))
        .child(render_terminal_header(session, &theme))
        .child(render_terminal_body(session, app, cx))
        .into_any_element()
}

/// 渲染终端顶部状态条。
fn render_terminal_header(
    session: &TerminalSessionState,
    theme: &crate::theme::AppTheme,
) -> impl IntoElement {
    let status_text = match session.status {
        TerminalStatus::Connecting => "连接中",
        TerminalStatus::AwaitingHostKey => "等待确认指纹",
        TerminalStatus::Connected => "已连接",
        TerminalStatus::Disconnected => "已断开",
        TerminalStatus::Failed => "连接失败",
    };
    div()
        .h(px(38.0))
        .px_3()
        .flex()
        .items_center()
        .justify_between()
        .border_b_1()
        .border_color(rgb(theme.border))
        .bg(rgb(theme.content))
        .child(
            div()
                .flex()
                .items_center()
                .gap_2()
                .text_size(px(12.5))
                .text_color(rgb(theme.foreground))
                .child(render_icon(
                    ArgusIcon::Terminal,
                    theme.foreground_muted,
                    16.0,
                ))
                .child(session.address.clone()),
        )
        .child(
            div()
                .text_size(px(12.0))
                .text_color(rgb(theme.foreground_muted))
                .child(status_text),
        )
}

/// 渲染终端正文区域。
fn render_terminal_body(
    session: &TerminalSessionState,
    app: &ArgusApp,
    cx: &mut Context<ArgusApp>,
) -> impl IntoElement {
    let theme = app.theme.clone();
    let session_id = session.id;
    let current_rows = session.rows;
    let current_cols = session.cols;
    let snapshot = session.screen_snapshot();
    let placeholder = terminal_placeholder(session, &snapshot);
    let resize_entity = cx.entity();
    let scrollbar_event_entity = resize_entity.clone();

    div()
        .id("ssh-terminal-body")
        .relative()
        .flex_1()
        .min_h(px(0.0))
        .overflow_hidden()
        .bg(rgb(theme.background))
        .on_scroll_wheel(cx.listener(move |app, event: &ScrollWheelEvent, _, cx| {
            cx.stop_propagation();
            let line_delta = terminal_scroll_line_delta(event);
            if app.scroll_terminal_scrollback(session_id, line_delta) {
                cx.notify();
            }
        }))
        .child(
            canvas(
                move |bounds, window: &mut Window, _| {
                    let metrics = terminal_metrics(window);
                    let desired_rows = terminal_rows_for_bounds(bounds, metrics.line_height);
                    let desired_cols = terminal_cols_for_bounds(bounds, metrics.cell_width);
                    let scrollbar_metrics = terminal_scrollbar_metrics(bounds, &snapshot, metrics);
                    if desired_rows != current_rows || desired_cols != current_cols {
                        let resize_entity = resize_entity.clone();
                        window.on_next_frame(move |_, cx| {
                            resize_entity.update(cx, |app, cx| {
                                app.resize_terminal_session(session_id, desired_rows, desired_cols);
                                cx.notify();
                            });
                        });
                    }
                    TerminalPaintState {
                        metrics,
                        snapshot,
                        placeholder,
                        scrollbar_metrics,
                    }
                },
                move |bounds, paint_state, window: &mut Window, cx| {
                    if let Some(scrollbar_metrics) = paint_state.scrollbar_metrics {
                        register_terminal_scrollbar_events(
                            session_id,
                            bounds,
                            scrollbar_metrics,
                            scrollbar_event_entity.clone(),
                            window,
                        );
                    }
                    paint_terminal_body(bounds, paint_state, &theme, window, cx);
                },
            )
            .size_full(),
        )
}

/// 终端 canvas 绘制需要的预计算状态。
struct TerminalPaintState {
    /// 当前字体和单元格尺寸。
    metrics: TerminalMetrics,
    /// vt100 屏幕快照。
    snapshot: TerminalScreenSnapshot,
    /// 连接中/失败等空屏提示。
    placeholder: Option<String>,
    /// 当前终端历史滚动条指标。
    scrollbar_metrics: Option<TerminalScrollbarMetrics>,
}

/// 终端单元格测量结果。
#[derive(Clone, Copy)]
struct TerminalMetrics {
    /// 单个终端列宽。
    cell_width: Pixels,
    /// 单行高度。
    line_height: Pixels,
    /// 文本字号。
    font_size: Pixels,
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
        font_size,
    }
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

/// 注册终端历史滚动条拖拽事件。
fn register_terminal_scrollbar_events(
    session_id: usize,
    viewport_bounds: Bounds<Pixels>,
    metrics: TerminalScrollbarMetrics,
    entity: Entity<ArgusApp>,
    window: &mut Window,
) {
    let thumb_bounds = terminal_scrollbar_thumb_bounds(viewport_bounds, metrics);
    window.on_mouse_event({
        let entity = entity.clone();
        move |event: &MouseDownEvent, phase, _, cx| {
            if !phase.bubble()
                || event.button != MouseButton::Left
                || !thumb_bounds.contains(&event.position)
            {
                return;
            }
            let cursor_offset = f32::from(event.position.y - thumb_bounds.top());
            entity.update(cx, |app, _| {
                app.begin_terminal_scrollbar_drag(session_id, cursor_offset);
            });
            cx.stop_propagation();
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
                entity.update(cx, |app, _| app.finish_terminal_scrollbar_drag(session_id));
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
            app.drag_terminal_scrollbar(
                session_id,
                f32::from(event.position.y),
                f32::from(viewport_bounds.top()),
                f32::from(metrics.track_start),
                f32::from(metrics.track_length),
                f32::from(metrics.thumb_length),
                metrics.max_scrollback_offset,
            )
        });
        if handled {
            cx.stop_propagation();
            cx.notify(entity.entity_id());
        }
    });
}

/// 计算终端历史滚动条滑块区域。
fn terminal_scrollbar_thumb_bounds(
    bounds: Bounds<Pixels>,
    metrics: TerminalScrollbarMetrics,
) -> Bounds<Pixels> {
    Bounds::new(
        point(
            bounds.right() - px(TERMINAL_SCROLLBAR_PADDING + TERMINAL_SCROLLBAR_WIDTH),
            bounds.top() + metrics.thumb_start,
        ),
        size(px(TERMINAL_SCROLLBAR_WIDTH), metrics.thumb_length),
    )
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

/// 绘制终端正文、颜色背景和光标。
fn paint_terminal_body(
    bounds: Bounds<Pixels>,
    state: TerminalPaintState,
    theme: &crate::theme::AppTheme,
    window: &mut Window,
    cx: &mut App,
) {
    let origin = point(
        bounds.left() + px(TERMINAL_HORIZONTAL_PADDING),
        bounds.top() + px(TERMINAL_VERTICAL_PADDING),
    );

    if let Some(placeholder) = state.placeholder {
        paint_terminal_placeholder(placeholder, origin, state.metrics, theme, window, cx);
        return;
    }

    for run in &state.snapshot.runs {
        paint_terminal_run_background(run, origin, state.metrics, theme, window);
    }
    for run in &state.snapshot.runs {
        paint_terminal_run_text(run, origin, state.metrics, theme, window, cx);
    }
    paint_terminal_cursor(&state.snapshot, origin, state.metrics, theme, window);
    if let Some(scrollbar_metrics) = state.scrollbar_metrics {
        paint_terminal_scrollbar(bounds, scrollbar_metrics, theme, window);
    }
}

/// 绘制空终端状态提示。
fn paint_terminal_placeholder(
    text: String,
    origin: gpui::Point<Pixels>,
    metrics: TerminalMetrics,
    theme: &crate::theme::AppTheme,
    window: &mut Window,
    cx: &mut App,
) {
    let run = terminal_text_run(
        &text,
        TerminalCellStyle {
            fg: TerminalColor::Default,
            bg: TerminalColor::Default,
            is_bold: false,
            is_dim: false,
            is_underline: false,
            is_inverse: false,
        },
        theme.foreground_muted,
        None,
        window,
    );
    let shaped =
        window
            .text_system()
            .shape_line(SharedString::from(text), metrics.font_size, &[run], None);
    let _ = shaped.paint(origin, metrics.line_height, window, cx);
}

/// 绘制终端片段背景色。
fn paint_terminal_run_background(
    run: &TerminalCellRun,
    origin: gpui::Point<Pixels>,
    metrics: TerminalMetrics,
    theme: &crate::theme::AppTheme,
    window: &mut Window,
) {
    let (_, background) = resolved_terminal_colors(run.style, theme);
    if background == theme.background {
        return;
    }
    let run_origin = terminal_cell_origin(origin, run.row, run.start_col, metrics);
    window.paint_quad(fill(
        Bounds::new(
            run_origin,
            size(metrics.cell_width * run.cols as f32, metrics.line_height),
        ),
        rgb(background),
    ));
}

/// 绘制终端片段文本。
fn paint_terminal_run_text(
    run: &TerminalCellRun,
    origin: gpui::Point<Pixels>,
    metrics: TerminalMetrics,
    theme: &crate::theme::AppTheme,
    window: &mut Window,
    cx: &mut App,
) {
    let (foreground, background) = resolved_terminal_colors(run.style, theme);
    let text_run = terminal_text_run(&run.text, run.style, foreground, Some(background), window);
    let shaped = window.text_system().shape_line(
        SharedString::from(run.text.clone()),
        metrics.font_size,
        &[text_run],
        None,
    );
    let _ = shaped.paint(
        terminal_cell_origin(origin, run.row, run.start_col, metrics),
        metrics.line_height,
        window,
        cx,
    );
}

/// 绘制终端块状光标。
fn paint_terminal_cursor(
    snapshot: &TerminalScreenSnapshot,
    origin: gpui::Point<Pixels>,
    metrics: TerminalMetrics,
    theme: &crate::theme::AppTheme,
    window: &mut Window,
) {
    if snapshot.is_cursor_hidden || snapshot.rows == 0 || snapshot.cols == 0 {
        return;
    }
    let cursor_row = snapshot.cursor_row.min(snapshot.rows.saturating_sub(1));
    let cursor_col = snapshot.cursor_col.min(snapshot.cols.saturating_sub(1));
    window.paint_quad(fill(
        Bounds::new(
            terminal_cell_origin(origin, cursor_row, cursor_col, metrics),
            size(metrics.cell_width, metrics.line_height),
        ),
        rgb(theme.selection),
    ));
}

/// 绘制终端历史滚动条滑块；终端和日志查看一样不绘制背景轨道。
fn paint_terminal_scrollbar(
    bounds: Bounds<Pixels>,
    metrics: TerminalScrollbarMetrics,
    theme: &crate::theme::AppTheme,
    window: &mut Window,
) {
    let mut thumb_color = rgb(theme.foreground_muted);
    thumb_color.a = 0.38;
    window.paint_quad(
        fill(
            terminal_scrollbar_thumb_bounds(bounds, metrics),
            thumb_color,
        )
        .corner_radii(px(TERMINAL_SCROLLBAR_WIDTH / 2.0)),
    );
}

/// 生成终端文本片段的 GPUI TextRun。
fn terminal_text_run(
    text: &str,
    style: TerminalCellStyle,
    foreground: u32,
    background: Option<u32>,
    window: &mut Window,
) -> TextRun {
    let mut text_style = window.text_style();
    text_style.font_family = ARGUS_LOG_FONT_FAMILY.into();
    text_style.font_size = px(TERMINAL_FONT_SIZE).into();
    text_style.font_weight = if style.is_bold {
        FontWeight::BOLD
    } else {
        FontWeight::NORMAL
    };
    TextRun {
        len: text.len(),
        font: text_style.font(),
        color: rgb(foreground).into(),
        background_color: background.map(|color| rgb(color).into()),
        underline: style.is_underline.then_some(UnderlineStyle {
            color: Some(rgb(foreground).into()),
            thickness: px(1.0),
            wavy: false,
        }),
        strikethrough: None,
    }
}

/// 计算终端单元格在 canvas 中的左上角。
fn terminal_cell_origin(
    origin: gpui::Point<Pixels>,
    row: u16,
    col: u16,
    metrics: TerminalMetrics,
) -> gpui::Point<Pixels> {
    point(
        origin.x + metrics.cell_width * col as f32,
        origin.y + metrics.line_height * row as f32,
    )
}

/// 根据终端样式和主题解析最终前景/背景色。
fn resolved_terminal_colors(
    style: TerminalCellStyle,
    theme: &crate::theme::AppTheme,
) -> (u32, u32) {
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
