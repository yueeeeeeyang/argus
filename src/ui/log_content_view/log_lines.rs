use super::*;

pub(crate) fn render_content_body(
    app: &mut ArgusApp,
    theme: &AppTheme,
    window: &mut Window,
    cx: &mut Context<ArgusApp>,
) -> AnyElement {
    match app.active_tab_kind() {
        TabKind::JstackAnalysis { analysis_id } => {
            jstack_analysis_view::render(app, analysis_id, cx).into_any_element()
        }
        TabKind::RuntimeAnalysis { analysis_id } => {
            runtime_analysis_view::render(app, analysis_id, cx).into_any_element()
        }
        TabKind::SshTerminal { session_id } => {
            terminal_view::render(app, session_id, window, cx).into_any_element()
        }
        TabKind::RemoteFileManager { session_id } => {
            remote_file_manager_view::render(app, session_id, cx).into_any_element()
        }
        TabKind::LogSource { source_id, path } => {
            let tab_id = app.active_tab().map(|tab| tab.id).unwrap_or_default();
            render_log_source_content(app, theme, tab_id, source_id, &path, cx)
        }
        TabKind::Empty if app.workspace == crate::app::Workspace::Connections => {
            render_empty_state(
                "请选择 SSH 链接",
                "左侧链接目录树中选择 SSH 链接后会在此处打开终端。",
                app,
                theme,
            )
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
pub(crate) fn render_log_source_content(
    app: &mut ArgusApp,
    theme: &AppTheme,
    tab_id: usize,
    source_id: crate::loader::SourceId,
    path: &str,
    cx: &mut Context<ArgusApp>,
) -> AnyElement {
    match app.log_read_state(source_id) {
        Some(LogOpenState::Ready(handle)) if !handle.is_empty() => {
            let handle = handle.clone();
            render_log_document(app, theme, tab_id, source_id, &handle, cx)
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
pub(crate) fn render_log_document(
    app: &mut ArgusApp,
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
pub(crate) fn render_in_memory_log(
    app: &ArgusApp,
    theme: &AppTheme,
    tab_id: usize,
    source_id: crate::loader::SourceId,
    handle: &LogReaderHandle,
    language: HighlightLanguage,
    cx: &mut Context<ArgusApp>,
) -> AnyElement {
    let _span = PerfSpan::new("render_paged_log");
    let Some(state) = app.log_tab_view_state(tab_id) else {
        return render_empty_state("日志视图未初始化", "请重新选择该日志。", app, theme);
    };
    let line_count = handle.line_count();
    let line_number_width = log_viewer_line_number_width(line_count);

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
            .with_width_from_item(Some(0))
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
pub(crate) fn render_paged_log(
    app: &mut ArgusApp,
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
    let horizontal_scroll_left = state.paged_scroll.left_px;
    let highlight_cache = state.highlight_cache.clone();
    let paged_viewport_handle = state.paged_viewport_handle.clone();
    let cached_lines = handle.cached_lines(first_line_index, visible_rows);

    app.request_paged_log_prefetch(
        tab_id,
        source_id,
        handle.clone(),
        first_line_index,
        visible_rows,
        cx,
    );

    let line_number_width = log_viewer_line_number_width(line_count);
    let (visible_char_range, horizontal_offset) = paged_visible_text_range(
        horizontal_scroll_left,
        viewport_bounds.size.width,
        line_number_width,
        app.log_content_font_size,
    );
    let highlight_prefetch_lines = cached_lines
        .iter()
        .filter_map(|line| {
            let line = line.as_ref()?;
            let visible_text = visible_log_text_from_raw(&line.text, Some(&visible_char_range));
            highlight_cache
                .cached_highlight_line(line.line_number, language, &visible_text.text)
                .is_none()
                .then_some((line.line_number, visible_text.text))
        })
        .collect::<Vec<_>>();
    app.request_log_highlight_prefetch(tab_id, source_id, language, highlight_prefetch_lines, cx);

    let rows = cached_lines
        .into_iter()
        .enumerate()
        .map(|(slot_offset, line)| {
            let line_number = first_line_index.saturating_add(slot_offset);
            let row_offset = line_number.saturating_sub(first_line_index);
            let top = row_offset as f32 * LOG_VIEWER_ROW_HEIGHT - fractional_top;
            div()
                .absolute()
                .left(px(0.0))
                .right(px(0.0))
                .top(px(top))
                .h(px(LOG_VIEWER_ROW_HEIGHT))
                .child(match line {
                    Some(line) => render_log_line(
                        app,
                        theme,
                        tab_id,
                        line.line_number,
                        &line.text,
                        language,
                        &highlight_cache,
                        horizontal_offset,
                        px(0.0),
                        line_number_width,
                        None,
                        Some(visible_char_range.clone()),
                        true,
                        false,
                        cx,
                    )
                    .into_any_element(),
                    None => {
                        render_paged_log_placeholder_line(theme, line_number, line_number_width)
                            .into_any_element()
                    }
                })
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
        .track_scroll(&paged_viewport_handle)
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
        .children(
            app.log_tab_view_state(tab_id)
                .map(|state| {
                    render_paged_scrollbars(app, tab_id, source_id, handle, state, theme, cx)
                })
                .unwrap_or_default(),
        )
        .into_any_element()
}

/// 渲染分页日志缓存缺失行的轻量占位，避免首帧为了读取磁盘阻塞 UI。
pub(crate) fn render_paged_log_placeholder_line(
    theme: &AppTheme,
    line_number: usize,
    line_number_width: f32,
) -> impl IntoElement + use<> {
    div()
        .h(px(LOG_VIEWER_ROW_HEIGHT))
        .w_full()
        .flex()
        .items_center()
        .text_size(px(12.0))
        .line_height(px(LOG_VIEWER_ROW_HEIGHT))
        .text_color(rgb(theme.foreground_muted))
        .child(
            div()
                .w(px(line_number_width))
                .flex_none()
                .pr_2()
                .flex()
                .justify_end()
                .child(format!("{}", line_number + 1)),
        )
        .child(div().flex_1().truncate().child("正在加载..."))
}

/// 通过当前 app 状态读取并渲染一个虚拟列表范围。
pub(crate) fn render_log_line_range(
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
    let row_min_width =
        estimated_log_content_width(handle, line_number_width, app.log_content_font_size);
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
                Some(row_min_width),
                None,
                true,
                true,
                cx,
            )
            .into_any_element()
        })
        .collect()
}
