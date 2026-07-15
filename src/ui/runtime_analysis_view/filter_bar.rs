use super::*;

pub(crate) fn render_filter_bar(
    app: &ArgusApp,
    analysis_id: usize,
    state: &RuntimeAnalysisState,
    theme: &AppTheme,
    cx: &mut Context<ArgusApp>,
) -> impl IntoElement + use<> {
    div()
        .h(px(RUNTIME_FILTER_BAR_HEIGHT))
        .px(px(RUNTIME_VIEW_PADDING))
        .py_2()
        .flex()
        .items_center()
        .gap_2()
        .border_b_1()
        .border_color(rgb(theme.border))
        .bg(rgb(theme.content))
        .child(
            div()
                .w(px(RUNTIME_FILTER_KEYWORD_WIDTH))
                .child(render_runtime_filter_input(
                    app,
                    analysis_id,
                    RuntimeFilterInputKind::Keyword,
                    &state.filter_keyword_input,
                    "任意关键字",
                    "过滤表格内容",
                    ArgusIcon::Search,
                    false,
                    theme,
                    cx,
                )),
        )
        .child(
            div()
                .w(px(RUNTIME_FILTER_USERNAME_WIDTH))
                .child(render_runtime_filter_input(
                    app,
                    analysis_id,
                    RuntimeFilterInputKind::Username,
                    &state.filter_username_input,
                    "用户名",
                    "用户名，逗号分隔",
                    ArgusIcon::Filter,
                    false,
                    theme,
                    cx,
                )),
        )
        .child(render_runtime_time_filter_picker(
            app,
            analysis_id,
            RuntimeFilterInputKind::StartTime,
            &state.filter_start_time_input,
            "开始时间",
            "2026-06-25 00:00:00",
            theme,
            cx,
        ))
        .child(render_runtime_time_filter_picker(
            app,
            analysis_id,
            RuntimeFilterInputKind::EndTime,
            &state.filter_end_time_input,
            "结束时间",
            "2026-06-25 23:59:59",
            theme,
            cx,
        ))
        .child(
            div()
                .flex_1()
                .min_w(px(0.0))
                .text_size(px(12.0))
                .line_height(px(18.0))
                .text_color(rgb(theme.foreground_muted))
                .truncate()
                .child(runtime_filter_status_label(state)),
        )
}

/// 渲染 Runtime 时间过滤输入框和对应的日期时间选择器浮层。
pub(crate) fn render_runtime_time_filter_picker(
    app: &ArgusApp,
    analysis_id: usize,
    input_kind: RuntimeFilterInputKind,
    input_state: &TextInputState,
    title: &'static str,
    placeholder: &'static str,
    theme: &AppTheme,
    cx: &mut Context<ArgusApp>,
) -> impl IntoElement + use<> {
    div()
        .w(px(RUNTIME_FILTER_TIME_WIDTH))
        .relative()
        .child(render_runtime_filter_input(
            app,
            analysis_id,
            input_kind,
            input_state,
            title,
            placeholder,
            ArgusIcon::Filter,
            true,
            theme,
            cx,
        ))
}

/// 渲染 Runtime 页面级日期时间选择器，避免被下方表格内容覆盖。
pub(crate) fn render_runtime_time_picker_overlay(
    analysis_id: usize,
    input_kind: RuntimeFilterInputKind,
    state: &RuntimeAnalysisState,
    theme: &AppTheme,
    cx: &mut Context<ArgusApp>,
) -> AnyElement {
    let (title, input_state, is_end) = match input_kind {
        RuntimeFilterInputKind::StartTime => ("开始时间", &state.filter_start_time_input, false),
        RuntimeFilterInputKind::EndTime => ("结束时间", &state.filter_end_time_input, true),
        RuntimeFilterInputKind::Keyword | RuntimeFilterInputKind::Username => {
            return div().into_any_element();
        }
    };

    render_datetime_picker(
        analysis_id,
        input_kind,
        title,
        runtime_datetime_picker_value(input_state, is_end),
        runtime_time_picker_left(input_kind),
        RUNTIME_TIME_PICKER_TOP,
        theme,
        cx,
    )
    .into_any_element()
}

/// 返回 Runtime 时间选择器浮层左侧位置，与过滤栏输入框布局保持一致。
pub(crate) fn runtime_time_picker_left(input_kind: RuntimeFilterInputKind) -> f32 {
    let start_left = RUNTIME_VIEW_PADDING
        + RUNTIME_FILTER_KEYWORD_WIDTH
        + RUNTIME_FILTER_GAP
        + RUNTIME_FILTER_USERNAME_WIDTH
        + RUNTIME_FILTER_GAP;
    match input_kind {
        RuntimeFilterInputKind::StartTime => start_left,
        RuntimeFilterInputKind::EndTime => {
            start_left + RUNTIME_FILTER_TIME_WIDTH + RUNTIME_FILTER_GAP
        }
        RuntimeFilterInputKind::Keyword | RuntimeFilterInputKind::Username => start_left,
    }
}

/// 渲染 Runtime 过滤栏中的单个输入框。
pub(crate) fn render_runtime_filter_input(
    app: &ArgusApp,
    analysis_id: usize,
    input_kind: RuntimeFilterInputKind,
    input_state: &TextInputState,
    id_suffix: &'static str,
    placeholder: &'static str,
    icon: ArgusIcon,
    opens_time_picker: bool,
    theme: &AppTheme,
    cx: &mut Context<ArgusApp>,
) -> AnyElement {
    let focus_handle = runtime_filter_focus_handle(app, input_kind);
    let native_input = focus_handle.clone().map(|focus_handle| {
        app_native_input(
            cx.entity(),
            AppTextInputTarget::RuntimeFilter {
                analysis_id,
                input_kind,
            },
            focus_handle,
        )
    });
    let input_id = runtime_filter_input_id(input_kind);
    render_input(
        Input {
            id: input_id,
            placeholder,
            value: input_state.value.clone(),
            is_disabled: false,
            is_focused: input_state.is_focused,
            cursor_index: input_state.cursor,
            selection_range: runtime_filter_input_selection_range(input_state),
            marked_range: input_state.marked_range.clone(),
            is_pointer_selecting: input_state.selection_drag.is_some(),
            is_secret: false,
            size: InputSize::Compact,
            leading_accessory: Some(InputAccessory {
                id: runtime_filter_leading_id(input_kind),
                icon,
                tooltip: id_suffix,
            }),
            trailing_accessory: Some(InputAccessory {
                id: runtime_filter_clear_id(input_kind),
                icon: ArgusIcon::Close,
                tooltip: "清空",
            }),
            native_input,
        },
        theme,
        cx.listener(move |app, event: &KeyDownEvent, _, cx| {
            cx.stop_propagation();
            app.handle_runtime_filter_input_key(analysis_id, input_kind, &event.keystroke, cx);
            cx.notify();
        }),
        cx.listener(move |app, _, _, cx| {
            cx.stop_propagation();
            app.focus_runtime_filter_input(analysis_id, input_kind);
            if opens_time_picker {
                app.open_runtime_time_picker(analysis_id, input_kind);
            }
            cx.notify();
        }),
        cx.listener(move |app, event: &InputPointerEvent, _, cx| {
            cx.stop_propagation();
            if opens_time_picker {
                app.open_runtime_time_picker(analysis_id, input_kind);
            }
            match event.action {
                InputPointerAction::Begin => app.begin_runtime_filter_input_pointer_selection(
                    analysis_id,
                    input_kind,
                    event.character_index,
                    event.granularity,
                ),
                InputPointerAction::Extend => app.update_runtime_filter_input_pointer_selection(
                    analysis_id,
                    input_kind,
                    event.character_index,
                ),
                InputPointerAction::Finish => {
                    app.finish_runtime_filter_input_pointer_selection(analysis_id, input_kind)
                }
            }
            cx.notify();
        }),
        cx.listener(move |app, _, _, cx| {
            cx.stop_propagation();
            app.clear_runtime_filter_input(analysis_id, input_kind, Some(cx));
            cx.notify();
        }),
    )
    .into_any_element()
}
