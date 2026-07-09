use super::*;

pub fn render_runtime_sql_text_dialog(
    analysis_id: usize,
    dialog: RuntimeSqlTextDialog,
    analysis_focus_handle: Option<FocusHandle>,
    scroll_handle: ScrollHandle,
    theme: &AppTheme,
    cx: &mut Context<ArgusApp>,
) -> AnyElement {
    let content = div()
        .size_full()
        .flex()
        .flex_col()
        .bg(rgb(theme.side_bar))
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(move |app, _, _, cx| {
                cx.stop_propagation();
                if app.clear_runtime_sql_text_selection(analysis_id) {
                    cx.notify();
                }
            }),
        )
        .child(
            div()
                .h(px(46.0))
                .px_3()
                .flex()
                .items_center()
                .justify_between()
                .border_b_1()
                .border_color(rgb(theme.border))
                .child(
                    div()
                        .min_w(px(0.0))
                        .flex()
                        .items_center()
                        .gap_2()
                        .child(render_icon(
                            ArgusIcon::Database,
                            theme.foreground_muted,
                            15.0,
                        ))
                        .child(
                            div()
                                .min_w(px(0.0))
                                .flex()
                                .flex_col()
                                .child(
                                    div()
                                        .text_size(px(13.0))
                                        .font_weight(FontWeight::SEMIBOLD)
                                        .truncate()
                                        .child("完整 SQL"),
                                )
                                .child(
                                    div()
                                        .text_size(px(11.0))
                                        .text_color(rgb(theme.foreground_muted))
                                        .truncate()
                                        .child(dialog.request_path.clone()),
                                ),
                        ),
                )
                .child(render_sql_dialog_close_button(analysis_id, theme, cx)),
        )
        .child(
            div()
                .px_3()
                .py_2()
                .flex()
                .items_center()
                .gap_2()
                .text_size(px(11.0))
                .text_color(rgb(theme.foreground_muted))
                .border_b_1()
                .border_color(rgb(theme.border))
                .child(dialog.request_time_label.clone())
                .child("·")
                .child(display_username(&dialog.username)),
        )
        .child(render_sql_dialog_code_block(
            analysis_id,
            &dialog.sql_text,
            dialog.selection.as_ref(),
            analysis_focus_handle,
            scroll_handle,
            theme,
            cx,
        ));

    render_modal_dialog(
        ModalDialog {
            overlay_id: "runtime-sql-text-dialog-overlay",
            container_id: "runtime-sql-text-dialog-container",
            width: RUNTIME_SQL_DIALOG_WIDTH,
            height: RUNTIME_SQL_DIALOG_HEIGHT,
            content: content.into_any_element(),
        },
        theme.clone(),
        cx,
    )
    .into_any_element()
}

/// 渲染 SQL 弹窗右上角关闭按钮。
pub fn render_sql_dialog_close_button(
    analysis_id: usize,
    theme: &AppTheme,
    cx: &mut Context<ArgusApp>,
) -> impl IntoElement + use<> {
    div()
        .id("runtime-sql-text-dialog-close")
        .size(px(28.0))
        .flex()
        .items_center()
        .justify_center()
        .rounded_sm()
        .cursor_pointer()
        .hover(|this| this.bg(rgb(theme.current_line)))
        .child(render_icon(ArgusIcon::Close, theme.foreground_muted, 15.0))
        .on_click(cx.listener(move |app, _, _, cx| {
            cx.stop_propagation();
            if app.close_runtime_sql_text_dialog(analysis_id) {
                cx.notify();
            }
        }))
}

/// 渲染完整 SQL 代码块，按原始换行拆行以避开 GPUI 单文本节点换行限制。
pub fn render_sql_dialog_code_block(
    analysis_id: usize,
    sql_text: &str,
    selection: Option<&RuntimeSqlTextSelection>,
    analysis_focus_handle: Option<FocusHandle>,
    scroll_handle: ScrollHandle,
    theme: &AppTheme,
    cx: &mut Context<ArgusApp>,
) -> impl IntoElement {
    let lines = runtime_sql_dialog_lines(sql_text);
    div()
        .flex_1()
        .min_h(px(0.0))
        .m_3()
        .rounded_sm()
        .border_1()
        .border_color(rgb(theme.border))
        .bg(rgb(theme.content))
        .relative()
        .occlude()
        .child(
            div()
                .id("runtime-sql-dialog-code-scroll")
                .overflow_y_scroll()
                .scrollbar_width(px(6.0))
                .track_scroll(&scroll_handle)
                .size_full()
                .occlude()
                .font_family(ARGUS_LOG_FONT_FAMILY)
                .text_size(px(12.0))
                .line_height(px(RUNTIME_SQL_DIALOG_LINE_HEIGHT))
                .text_color(rgb(theme.foreground))
                .child(
                    div()
                        .w_full()
                        .min_w(px(0.0))
                        .p_3()
                        .flex()
                        .flex_col()
                        .children(lines.into_iter().enumerate().map(|(index, line)| {
                            let selection_range = runtime_sql_dialog_selection_range_for_line(
                                selection, index, &line,
                            );
                            render_sql_dialog_line(
                                analysis_id,
                                index,
                                line,
                                selection_range,
                                analysis_focus_handle.clone(),
                                theme,
                                cx,
                            )
                            .into_any_element()
                        })),
                ),
        )
        .child(render_sql_dialog_scrollbar(
            analysis_id,
            &scroll_handle,
            theme,
            cx,
        ))
}

/// 渲染完整 SQL 弹窗的可拖拽垂直滚动条滑块。
///
/// 复用表格滚动条的度量与拖拽逻辑，但内容高度由 `ScrollHandle::max_offset` 推算
/// （`viewport + max_offset`），以适配自动折行后动态变化的 SQL 正文高度。
///
/// 首帧渲染时滚动句柄尚未布局，`bounds` 为零导致 `scrollbar_metrics` 返回 `None`，
/// 此时返回一个透明哨兵元素：其 paint 回调在布局完成后执行，若检测到内容已溢出则触发一次重绘，
/// 下一帧即可用有效 bounds 渲染真实滑块。滑块出现后哨兵不再渲染，自然收敛，不会无限重绘。
pub fn render_sql_dialog_scrollbar(
    analysis_id: usize,
    scroll_handle: &ScrollHandle,
    theme: &AppTheme,
    cx: &mut Context<ArgusApp>,
) -> AnyElement {
    let bounds = scroll_handle.bounds();
    let max_offset = scroll_handle.max_offset();
    let offset = scroll_handle.offset();
    let content_height = bounds.size.height + max_offset.height;
    match scrollbar_metrics(
        bounds.size.height,
        content_height,
        -offset.y,
        RUNTIME_SCROLLBAR_PADDING,
        RUNTIME_SCROLLBAR_MIN_THUMB,
    ) {
        Some(metrics) => render_runtime_scrollbar_thumb(
            analysis_id,
            RuntimeScrollbarTable::SqlDialog,
            RuntimeScrollTarget::Uniform(scroll_handle.clone()),
            metrics,
            px(0.0),
            bounds,
            theme,
            cx,
        ),
        None => render_sql_dialog_scrollbar_sentinel(scroll_handle.clone(), cx),
    }
}

/// 渲染首帧哨兵：在滚动句柄完成布局前占位，布局完成后触发一次重绘以显示真实滑块。
pub fn render_sql_dialog_scrollbar_sentinel(
    scroll_handle: ScrollHandle,
    cx: &mut Context<ArgusApp>,
) -> AnyElement {
    let entity = cx.entity();
    canvas(
        |_, _, _| (),
        move |_, _, _, cx: &mut App| {
            // paint 在布局之后执行，此时 bounds 已是有效值；若内容溢出则触发下一帧渲染滑块。
            let bounds = scroll_handle.bounds();
            if bounds.size.height > px(0.0) && scroll_handle.max_offset().height > px(0.0) {
                cx.notify(entity.entity_id());
            }
        },
    )
    .absolute()
    .size_full()
    .into_any_element()
}

/// 渲染 SQL 弹窗中的一行，支持拖拽选中文本。
///
/// 文本外层套一层 `min_w(0)+w_full` 容器以限定宽度：长 SQL 行会在弹窗宽度内自动折行，
/// 配合代码块的 `overflow_y_scroll` 实现完整展示与垂直滚动；选区命中测试同样按折行后的
/// 视觉布局计算（见 `runtime_sql_dialog_character_index_from_pointer`）。
pub fn render_sql_dialog_line(
    analysis_id: usize,
    line_index: usize,
    line: String,
    selection_range: Option<Range<usize>>,
    analysis_focus_handle: Option<FocusHandle>,
    theme: &AppTheme,
    cx: &mut Context<ArgusApp>,
) -> impl IntoElement + use<> {
    div()
        .id(SharedString::from(format!(
            "runtime-sql-dialog-line-{line_index}"
        )))
        .min_h(px(RUNTIME_SQL_DIALOG_LINE_HEIGHT))
        .w_full()
        .flex_none()
        .relative()
        .flex()
        .items_center()
        .whitespace_normal()
        .line_height(px(RUNTIME_SQL_DIALOG_LINE_HEIGHT))
        .child(
            div()
                .min_w(px(0.0))
                .w_full()
                .child(render_runtime_cell_text(
                    line.clone(),
                    selection_range,
                    theme,
                )),
        )
        .child(render_sql_dialog_line_pointer_layer(
            analysis_id,
            line_index,
            line,
            analysis_focus_handle,
            cx,
        ))
}

/// 渲染 SQL 弹窗单行透明命中层，将鼠标拖拽转换成跨行文本选区。
pub fn render_sql_dialog_line_pointer_layer(
    analysis_id: usize,
    line_index: usize,
    line: String,
    analysis_focus_handle: Option<FocusHandle>,
    cx: &mut Context<ArgusApp>,
) -> impl IntoElement + use<> {
    let entity = cx.entity();
    div()
        .id(SharedString::from(format!(
            "runtime-sql-dialog-line-hitbox-{line_index}"
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
                        let line = line.clone();
                        let analysis_focus_handle = analysis_focus_handle.clone();
                        move |event: &MouseDownEvent, phase, window, cx| {
                            if !phase.bubble()
                                || event.button != MouseButton::Left
                                || !visible_bounds.contains(&event.position)
                            {
                                return;
                            }

                            let character_index = runtime_sql_dialog_character_index_from_pointer(
                                &line,
                                event.position,
                                bounds,
                                window,
                            );
                            let granularity =
                                runtime_sql_dialog_granularity_for_click_count(event.click_count);
                            if let Some(focus_handle) = analysis_focus_handle.as_ref() {
                                focus_handle.focus(window);
                            }
                            entity.update(cx, |app, _| {
                                app.begin_runtime_sql_text_selection(
                                    analysis_id,
                                    line_index,
                                    line.clone(),
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
                        let line = line.clone();
                        move |event: &MouseMoveEvent, phase, window, cx| {
                            if !phase.bubble()
                                || !event.dragging()
                                || !visible_bounds.contains(&event.position)
                            {
                                return;
                            }
                            let character_index = runtime_sql_dialog_character_index_from_pointer(
                                &line,
                                event.position,
                                bounds,
                                window,
                            );
                            let handled = entity.update(cx, |app, _| {
                                app.update_runtime_sql_text_selection(
                                    analysis_id,
                                    line_index,
                                    line.clone(),
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
                                app.finish_runtime_sql_text_selection(analysis_id)
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
