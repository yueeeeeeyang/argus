use super::*;

pub fn render_search_results_panel(
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
    let keyword_summary = app.log_search.result_keyword_summary.clone();
    let header_status_text = match keyword_summary {
        Some(summary) => format!("{status_text}，{summary}"),
        None => status_text,
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
                        .child(header_status_text),
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
pub fn render_search_results_resize_handle(
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

                    window.on_mouse_event(
                        move |event: &MouseMoveEvent, phase, window: &mut Window, cx| {
                            if !phase.bubble() || !event.dragging() {
                                return;
                            }
                            // 按窗口视口高度动态计算面板上限，使其可近乎撑满窗口；
                            // 预留上方标题栏与最小日志可见区，并保证不小于最小高度。
                            let viewport_height = f32::from(window.viewport_size().height);
                            let max_height = (viewport_height
                                - SEARCH_RESULT_PANEL_RESERVED_HEIGHT)
                                .max(SEARCH_RESULT_PANEL_HEIGHT_MIN);
                            let handled = entity.update(cx, |app, _| {
                                app.resize_search_result_panel(event.position.y, max_height)
                            });
                            if handled {
                                cx.stop_propagation();
                                cx.notify(entity.entity_id());
                            }
                        },
                    );
                },
            )
            .size_full(),
        )
}

/// 根据虚拟列表行类型分派渲染搜索分组或命中结果。
pub fn render_search_result_item(
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
pub fn render_search_result_group_row(
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
pub fn render_search_result_row(
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
    let row_width = search_result_row_width(
        app,
        SEARCH_RESULT_ROW_HORIZONTAL_PADDING
            + SEARCH_RESULT_LINE_LABEL_WIDTH
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

/// 返回搜索结果行实际渲染宽度：至少撑满当前视口，内容更宽时交给横向滚动条。
pub fn search_result_row_width(app: &ArgusApp, intrinsic_width: f32) -> f32 {
    let viewport_width = search_result_viewport_width(app);
    app.log_search
        .result_list_content_width
        .max(SEARCH_RESULT_ROW_MIN_WIDTH)
        .max(viewport_width)
        .max(intrinsic_width)
}

/// 读取搜索结果列表当前可视宽度；首帧尚未测量时返回 0，由最小宽度兜底。
pub fn search_result_viewport_width(app: &ArgusApp) -> f32 {
    let scroll_state = app.log_search.result_scroll.0.borrow();
    f32::from(scroll_state.base_handle.bounds().size.width)
}

/// 估算搜索结果单行文本宽度，中文和其它非 ASCII 字符按更宽字形处理，避免提前换行。
pub fn estimated_search_result_text_width(text: &str) -> f32 {
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
pub fn render_search_results_scrollbars(
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
pub fn render_search_result_scrollbar_thumb(
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

