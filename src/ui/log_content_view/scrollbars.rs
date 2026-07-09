use super::*;

pub fn render_in_memory_scrollbars(
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
pub fn render_paged_scrollbars(
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

    let line_number_width = log_viewer_line_number_width(handle.line_count());
    let content_width =
        estimated_log_content_width(handle, line_number_width, app.log_content_font_size);
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
pub fn scrollbar_metrics(
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
pub fn render_scrollbar_thumb(
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
