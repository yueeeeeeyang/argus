use super::*;

/// 将 Runtime 单元格内容转换为 GPUI 单行文本。
///
/// GPUI 的 `shape_line` 和普通文本节点都要求输入不包含换行；Runtime SQL 原文可能跨行，
/// 因此 UI 单元格统一折叠换行并保留原有词序，过滤和聚合仍继续使用解析层的原始 SQL。
pub fn runtime_cell_display_text(text: &str) -> String {
    if !text.contains('\n') && !text.contains('\r') {
        return text.to_string();
    }

    text.replace('\r', "\n")
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

/// 将完整 SQL 原文拆成弹窗代码区渲染行，保留空行和缩进。
pub fn runtime_sql_dialog_lines(sql_text: &str) -> Vec<String> {
    sql_text
        .replace("\r\n", "\n")
        .replace('\r', "\n")
        .split('\n')
        .map(ToString::to_string)
        .collect()
}

/// 返回 SQL 弹窗选区覆盖指定行的字符范围。
pub fn runtime_sql_dialog_selection_range_for_line(
    selection: Option<&RuntimeSqlTextSelection>,
    line_index: usize,
    line: &str,
) -> Option<Range<usize>> {
    let selection = selection?;
    let (start, end) = selection.normalized();
    if line_index < start.line_index || line_index > end.line_index {
        return None;
    }

    let line_character_count = character_count(line);
    let start_column = if line_index == start.line_index {
        start.column.min(line_character_count)
    } else {
        0
    };
    let end_column = if line_index == end.line_index {
        end.column.min(line_character_count)
    } else {
        line_character_count
    };
    (start_column < end_column).then_some(start_column..end_column)
}

/// 根据点击次数转换 SQL 弹窗文本选择粒度。
pub fn runtime_sql_dialog_granularity_for_click_count(click_count: usize) -> TextSelectionGranularity {
    match click_count {
        0 | 1 => TextSelectionGranularity::Character,
        2 => TextSelectionGranularity::Word,
        _ => TextSelectionGranularity::Line,
    }
}

/// 根据鼠标位置计算 SQL 弹窗文本行中的字符列，兼容自动换行后的视觉行命中。
pub fn runtime_sql_dialog_character_index_from_pointer(
    line: &str,
    pointer_position: gpui::Point<Pixels>,
    bounds: gpui::Bounds<Pixels>,
    window: &mut Window,
) -> usize {
    let text_relative_x = pointer_position.x - bounds.left();
    let text_relative_y = pointer_position.y - bounds.top();
    if line.is_empty() || text_relative_x <= px(0.0) {
        return 0;
    }

    let mut text_style = window.text_style();
    text_style.font_family = ARGUS_LOG_FONT_FAMILY.into();
    text_style.font_size = px(12.0).into();
    let font_size = text_style.font_size.to_pixels(window.rem_size());
    let line_height = px(RUNTIME_SQL_DIALOG_LINE_HEIGHT);
    let run = TextRun {
        len: line.len(),
        font: text_style.font(),
        color: text_style.color,
        background_color: None,
        underline: None,
        strikethrough: None,
    };
    // SQL 弹窗正文启用了自动换行，必须把 y 坐标一起交给 wrapped layout，
    // 否则点击第二条视觉行时会被当成第一条视觉行同一 x 位置。
    if let Ok(mut wrapped_lines) = window.text_system().shape_text(
        SharedString::from(line.to_string()),
        font_size,
        &[run.clone()],
        Some(bounds.size.width.max(px(1.0))),
        None,
    ) && let Some(wrapped_line) = wrapped_lines.pop()
    {
        let byte_index = wrapped_line
            .closest_index_for_position(point(text_relative_x, text_relative_y), line_height)
            .unwrap_or_else(|index| index);
        return char_column_for_byte_index(line, byte_index).min(character_count(line));
    }

    let shaped_line = window.text_system().shape_line(
        SharedString::from(line.to_string()),
        font_size,
        &[run],
        None,
    );
    let byte_index = shaped_line.closest_index_for_x(text_relative_x);
    char_column_for_byte_index(line, byte_index).min(character_count(line))
}

/// 渲染 Runtime 单元格文本并叠加当前选区高亮。
pub fn render_runtime_cell_text(
    text: String,
    selection_range: Option<Range<usize>>,
    theme: &AppTheme,
) -> AnyElement {
    let Some(selection_range) = selection_range else {
        return text.into_any_element();
    };

    let start = byte_index_for_character(&text, selection_range.start);
    let end = byte_index_for_character(&text, selection_range.end);
    if start >= end {
        return text.into_any_element();
    }

    StyledText::new(text)
        .with_highlights(vec![(
            start..end,
            HighlightStyle {
                background_color: Some(rgb(theme.selection).into()),
                color: Some(rgb(theme.foreground).into()),
                ..Default::default()
            },
        )])
        .into_any_element()
}

/// 返回当前选区在指定单元格内的字符范围。
pub fn runtime_cell_selection_range(
    selection: Option<&RuntimeTableCellSelection>,
    cell_key: &str,
) -> Option<Range<usize>> {
    selection
        .filter(|selection| selection.cell_key == cell_key)
        .and_then(RuntimeTableCellSelection::normalized_range)
}

/// 渲染 Runtime 表格单元格透明鼠标命中层，负责把拖拽选择转换成应用状态。
pub fn render_runtime_cell_pointer_layer(
    analysis_id: usize,
    cell_key: String,
    text: String,
    font_family: &'static str,
    analysis_focus_handle: Option<FocusHandle>,
    cx: &mut Context<ArgusApp>,
) -> impl IntoElement + use<> {
    let entity = cx.entity();
    div()
        .id(SharedString::from(format!(
            "runtime-cell-pointer-{analysis_id}-{cell_key}"
        )))
        .absolute()
        .left_0()
        .top_0()
        .right_0()
        .bottom_0()
        .child(
            canvas(
                |_, _, _| (),
                move |bounds, _, window: &mut Window, _| {
                    let visible_bounds = bounds.intersect(&window.content_mask().bounds);

                    window.on_mouse_event({
                        let entity = entity.clone();
                        let cell_key = cell_key.clone();
                        let text = text.clone();
                        let font_family = font_family;
                        let analysis_focus_handle = analysis_focus_handle.clone();
                        move |event: &MouseDownEvent, phase, window, cx| {
                            if !phase.bubble()
                                || event.button != MouseButton::Left
                                || !visible_bounds.contains(&event.position)
                            {
                                return;
                            }

                            let character_index = runtime_cell_character_index_from_pointer(
                                &text,
                                font_family,
                                event.position.x,
                                bounds,
                                window,
                            );
                            let granularity =
                                runtime_cell_granularity_for_click_count(event.click_count);
                            if let Some(focus_handle) = analysis_focus_handle.as_ref() {
                                focus_handle.focus(window);
                            }
                            entity.update(cx, |app, _| {
                                app.begin_runtime_cell_selection(
                                    analysis_id,
                                    cell_key.clone(),
                                    text.clone(),
                                    character_index,
                                    granularity,
                                );
                            });
                            cx.stop_propagation();
                            cx.notify(entity.entity_id());
                        }
                    });

                    window.on_mouse_event({
                        let entity = entity.clone();
                        let cell_key = cell_key.clone();
                        let text = text.clone();
                        let font_family = font_family;
                        move |event: &MouseMoveEvent, phase, window, cx| {
                            if !phase.bubble() || !event.dragging() {
                                return;
                            }

                            let character_index = runtime_cell_character_index_from_pointer(
                                &text,
                                font_family,
                                event.position.x,
                                bounds,
                                window,
                            );
                            let handled = entity.update(cx, |app, _| {
                                app.update_runtime_cell_selection(
                                    analysis_id,
                                    &cell_key,
                                    character_index,
                                )
                            });
                            if handled {
                                cx.stop_propagation();
                                cx.notify(entity.entity_id());
                            }
                        }
                    });

                    window.on_mouse_event({
                        let entity = entity.clone();
                        move |event: &MouseUpEvent, phase, _, cx| {
                            if !phase.bubble() || event.button != MouseButton::Left {
                                return;
                            }

                            let handled = entity.update(cx, |app, _| {
                                app.finish_runtime_cell_selection(analysis_id)
                            });
                            if handled {
                                cx.stop_propagation();
                                cx.notify(entity.entity_id());
                            }
                        }
                    });
                },
            )
            .size_full(),
        )
}

/// 根据点击次数转换 Runtime 单元格选择粒度；双击按需求选中整格内容。
pub fn runtime_cell_granularity_for_click_count(click_count: usize) -> TextSelectionGranularity {
    match click_count {
        0 | 1 => TextSelectionGranularity::Character,
        _ => TextSelectionGranularity::Line,
    }
}

/// 根据鼠标横坐标计算 Runtime 单元格文本中的字符列。
pub fn runtime_cell_character_index_from_pointer(
    text: &str,
    font_family: &'static str,
    pointer_x: Pixels,
    bounds: gpui::Bounds<Pixels>,
    window: &mut Window,
) -> usize {
    let text_relative_x = pointer_x - bounds.left() - px(RUNTIME_CELL_HORIZONTAL_PADDING);
    if text.is_empty() || text_relative_x <= px(0.0) {
        return 0;
    }

    let mut text_style = window.text_style();
    text_style.font_family = font_family.into();
    text_style.font_size = px(12.0).into();
    let run = TextRun {
        len: text.len(),
        font: text_style.font(),
        color: text_style.color,
        background_color: None,
        underline: None,
        strikethrough: None,
    };
    let shaped_line = window.text_system().shape_line(
        SharedString::from(text.to_string()),
        text_style.font_size.to_pixels(window.rem_size()),
        &[run],
        None,
    );
    let byte_index = shaped_line.closest_index_for_x(text_relative_x);
    char_column_for_byte_index(text, byte_index).min(character_count(text))
}

/// 生成 Runtime 普通表格单元格稳定 key。
pub fn runtime_cell_key(scope: &str, row_index: usize, column: &str) -> String {
    format!("{scope}:{row_index}:{column}")
}

/// 生成 Runtime SQL 表格单元格稳定 key。
pub fn runtime_sql_cell_key(request_index: usize, sql_index: usize, column: &str) -> String {
    format!("sql:{request_index}:{sql_index}:{column}")
}

/// 渲染操作按钮。
pub fn render_action_button(
    id: String,
    label: &'static str,
    theme: &AppTheme,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    div()
        .id(SharedString::from(id))
        .h(px(24.0))
        .px_1()
        .flex()
        .items_center()
        .justify_center()
        .rounded_sm()
        .cursor_pointer()
        .hover(|this| this.bg(rgb(theme.current_line)))
        .text_size(px(12.0))
        .line_height(px(24.0))
        .text_color(rgb(theme.info))
        .child(label)
        .on_click(on_click)
}

/// 渲染返回按钮。
pub fn render_back_button(
    id: &'static str,
    label: &'static str,
    theme: &AppTheme,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    div()
        .id(id)
        .h(px(26.0))
        .px_1()
        .flex()
        .items_center()
        .gap_1()
        .rounded_sm()
        .cursor_pointer()
        .hover(|this| this.bg(rgb(theme.current_line)))
        .text_size(px(12.0))
        .line_height(px(26.0))
        .text_color(rgb(theme.info))
        .child(render_icon(ArgusIcon::ArrowLeft, theme.info, 13.0))
        .child(label)
        .on_click(on_click)
}

/// 渲染空信息。
pub fn render_empty_message(message: &str, theme: &AppTheme) -> AnyElement {
    div()
        .flex_1()
        .flex()
        .items_center()
        .justify_center()
        .text_size(px(13.0))
        .text_color(rgb(theme.foreground_muted))
        .child(message.to_string())
        .into_any_element()
}

/// Runtime 三层表格当前过滤条件类型，具体解析逻辑复用领域层实现。
pub type RuntimeFilterCriteria = RuntimeAnalysisFilterCriteria;

