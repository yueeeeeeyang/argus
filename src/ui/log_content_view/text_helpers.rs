use super::*;

pub(crate) fn render_log_line(
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
    row_min_width: Option<gpui::Pixels>,
    visible_char_range: Option<Range<usize>>,
    enable_syntax_highlight: bool,
    allow_sync_highlight: bool,
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
    let syntax_spans = if enable_syntax_highlight && allow_sync_highlight {
        highlight_cache.highlight_line(line_number, language, &visible_text.text)
    } else if enable_syntax_highlight {
        highlight_cache
            .cached_highlight_line(line_number, language, &visible_text.text)
            .unwrap_or_default()
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
        .when_some(row_min_width, |row, width| row.min_w(width))
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
pub(crate) fn paged_visible_text_range(
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
pub(crate) fn estimated_log_char_width(font_size: f32) -> f32 {
    (font_size * 0.62).max(6.0)
}

/// 估算日志正文完整横向宽度，供虚拟列表测量和自绘横向滚动条共用。
pub(crate) fn estimated_log_content_width(
    handle: &LogReaderHandle,
    line_number_width: f32,
    font_size: f32,
) -> gpui::Pixels {
    px(
        handle.estimated_longest_display_columns() as f32 * estimated_log_char_width(font_size)
            + line_number_width
            + LOG_VIEWER_TEXT_LEFT_PADDING
            + LOG_VIEWER_TEXT_RIGHT_PADDING,
    )
}

/// 从完整展示文本中截取本次真正需要渲染的片段。
pub(crate) fn visible_log_text(
    full_text: &str,
    visible_char_range: Option<&Range<usize>>,
) -> LogVisibleText {
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
pub(crate) fn visible_log_text_from_raw(
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
pub(crate) fn clip_display_range_to_visible(
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
pub(crate) fn merge_log_line_highlights(
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
            color: Some(
                rgb(crate::ui::highlight_colors::color_for_highlight_token(
                    span.kind,
                    theme,
                    crate::ui::highlight_colors::HighlightColorContext::Log,
                ))
                .into(),
            ),
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
pub(crate) fn merge_syntax_and_selection_highlights(
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
pub(crate) fn active_search_line_background(theme: &AppTheme) -> u32 {
    blend_rgb(theme.content, theme.warning, 0.18)
}

/// 按比例混合两个 RGB 颜色；只处理界面主题常用的低 24 位颜色。
pub(crate) fn blend_rgb(base: u32, overlay: u32, overlay_ratio: f32) -> u32 {
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
pub(crate) fn push_non_overlapping_highlight(
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
pub(crate) fn subtract_ranges(
    range: Range<usize>,
    protected_ranges: &[Range<usize>],
) -> Vec<Range<usize>> {
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
pub(crate) fn subtract_single_range(
    range: Range<usize>,
    protected: &Range<usize>,
) -> Vec<Range<usize>> {
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
pub(crate) fn ranges_overlap(left: &Range<usize>, right: &Range<usize>) -> bool {
    left.start < right.end && right.start < left.end
}

/// 构造搜索结果行预览；长行只截取命中附近文本，避免列表滚动时处理整条超长日志。
pub(crate) fn search_result_preview_text(
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
pub(crate) fn search_ranges_for_display(
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
pub(crate) fn display_column_for_raw_column(raw_text: &str, raw_column: usize) -> usize {
    raw_text
        .chars()
        .take(raw_column)
        .map(|ch| if ch == '\t' { 4 } else { 1 })
        .sum()
}
