//! 文件职责：渲染独立日志搜索窗口。
//! 创建日期：2026-06-11
//! 修改日期：2026-06-16
//! 作者：Argus 开发团队
//! 主要功能：提供无标题栏搜索窗口、关键字/目录输入和搜索范围切换控件。

use gpui::{
    ClickEvent, Context, Entity, FocusHandle, FontWeight, IntoElement, KeyDownEvent, Render,
    SharedString, Subscription, Window, div, prelude::*, px, rgb,
};

use crate::app::{AppTextInputTarget, ArgusApp, LogSearchInputKind, LogSearchState};
use crate::fonts::ARGUS_UI_FONT_FAMILY;
use crate::search::search_engine::SearchScope;
use crate::theme::AppTheme;
use crate::ui::components::icon::{ArgusIcon, render_icon};
use crate::ui::components::icon_button::{IconButtonSize, render_icon_button};
use crate::ui::components::input::{
    Input, InputAccessory, InputPointerAction, InputPointerEvent, InputSize, render_input,
};
use crate::ui::components::loading_spinner::render_loading_spinner;
use crate::ui::input_native::app_native_input;

/// 搜索窗口按钮内容视觉下移量，用于修正文字和图标在按钮内略靠上的观感。
const LOG_SEARCH_BUTTON_CONTENT_Y_OFFSET: f32 = 1.0;
/// 搜索窗口标题图标尺寸，和 14px 标题文字保持协调比例。
const LOG_SEARCH_TITLE_ICON_SIZE: f32 = 16.0;

/// 搜索窗口根视图；业务状态仍保存在主应用实体中。
pub struct LogSearchWindow {
    /// 主应用实体。
    app: Entity<ArgusApp>,
    /// 搜索窗口根焦点句柄，用于窗口打开后直接接收键盘输入。
    focus_handle: FocusHandle,
    /// 关键字输入框真实焦点句柄。
    keyword_focus_handle: FocusHandle,
    /// 目录输入框真实焦点句柄。
    directory_focus_handle: FocusHandle,
    /// 是否已经执行过初始聚焦，避免每次重绘抢走输入框点击后的焦点。
    has_focused_root: bool,
    /// 当前渲染快照。
    snapshot: LogSearchSnapshot,
    /// 主应用观察订阅，确保后台搜索进度能刷新窗口。
    _app_observer: Subscription,
}

impl LogSearchWindow {
    /// 创建搜索窗口并监听主应用状态变化。
    ///
    /// 参数说明：
    /// - `app`：主应用实体。
    /// - `theme`：首次绘制使用的主题快照。
    /// - `log_search`：首次绘制使用的搜索状态快照。
    /// - `cx`：搜索窗口上下文。
    ///
    /// 返回值：可渲染搜索窗口视图。
    pub fn new(
        app: Entity<ArgusApp>,
        theme: AppTheme,
        log_search: LogSearchState,
        cx: &mut Context<Self>,
    ) -> Self {
        let _app_observer = cx.observe(&app, |window, app_entity, cx| {
            window.snapshot = app_entity.read_with(cx, |app, _| LogSearchSnapshot {
                theme: app.theme.clone(),
                log_search: app.log_search.clone(),
            });
            cx.notify();
        });

        Self {
            app,
            focus_handle: cx.focus_handle(),
            keyword_focus_handle: cx.focus_handle(),
            directory_focus_handle: cx.focus_handle(),
            has_focused_root: false,
            snapshot: LogSearchSnapshot { theme, log_search },
            _app_observer,
        }
    }
}

impl Render for LogSearchWindow {
    /// 渲染搜索窗口内容。
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if !self.has_focused_root {
            self.keyword_focus_handle.focus(window);
            self.has_focused_root = true;
        }

        render_window_content(
            &self.snapshot,
            &self.app,
            self.focus_handle.clone(),
            self.keyword_focus_handle.clone(),
            self.directory_focus_handle.clone(),
            cx,
        )
    }
}

/// 搜索窗口只读渲染快照。
#[derive(Clone)]
struct LogSearchSnapshot {
    /// 当前主题。
    theme: AppTheme,
    /// 当前搜索状态。
    log_search: LogSearchState,
}

/// 渲染窗口主体。
fn render_window_content(
    snapshot: &LogSearchSnapshot,
    app_handle: &Entity<ArgusApp>,
    focus_handle: FocusHandle,
    keyword_focus_handle: FocusHandle,
    directory_focus_handle: FocusHandle,
    _cx: &mut Context<LogSearchWindow>,
) -> impl IntoElement + use<> {
    let theme = snapshot.theme.clone();
    let close_app = app_handle.clone();
    let key_app = app_handle.clone();
    let key_focus_handle = focus_handle.clone();
    let blur_app = app_handle.clone();
    let blur_focus_handle = focus_handle.clone();
    let active_input_kind = active_log_search_input_kind(&snapshot.log_search);

    div()
        .id("log-search-window-root")
        .size_full()
        .flex()
        .flex_col()
        .gap_3()
        .p_4()
        .bg(rgb(theme.content))
        .font_family(ARGUS_UI_FONT_FAMILY)
        .text_color(rgb(theme.foreground))
        .occlude()
        .focusable()
        .track_focus(&focus_handle)
        .on_click(move |_, window, cx| {
            blur_focus_handle.focus(window);
            update_search_app(&blur_app, cx, |app, _| {
                app.clear_all_text_input_focus();
            });
        })
        .on_key_down(move |event: &KeyDownEvent, window, cx| {
            if !key_focus_handle.is_focused(window) {
                return;
            }

            let should_close = event.keystroke.key.eq_ignore_ascii_case("escape");
            if let Some(active_input_kind) = active_input_kind {
                update_search_app(&key_app, cx, |app, app_cx| {
                    app.handle_log_search_input_key(active_input_kind, &event.keystroke, app_cx);
                });
            } else if should_close {
                update_search_app(&key_app, cx, |app, _| {
                    app.close_log_search_window();
                });
            }
            if should_close {
                window.remove_window();
            }
        })
        .child(
            div()
                .h(px(28.0))
                .flex()
                .items_center()
                .child(
                    div()
                        .flex_1()
                        .flex()
                        .items_center()
                        .gap_2()
                        .text_size(px(14.0))
                        .line_height(px(18.0))
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(rgb(theme.foreground))
                        .child(render_icon(
                            ArgusIcon::Search,
                            theme.foreground_muted,
                            LOG_SEARCH_TITLE_ICON_SIZE,
                        ))
                        .child("日志搜索"),
                )
                .child(render_icon_button(
                    "log-search-window-close",
                    ArgusIcon::Close,
                    "关闭",
                    false,
                    IconButtonSize::Small,
                    &theme,
                    move |_, window, cx| {
                        update_search_app(&close_app, cx, |app, _| {
                            app.close_log_search_window();
                        });
                        window.remove_window();
                    },
                )),
        )
        .child(render_search_input(
            snapshot,
            app_handle,
            LogSearchInputKind::Keyword,
            keyword_focus_handle,
            "关键字",
            "输入关键字后按 Enter 搜索",
            ArgusIcon::Search,
        ))
        .child(render_search_input(
            snapshot,
            app_handle,
            LogSearchInputKind::Directory,
            directory_focus_handle,
            "目录",
            "来源树目录路径",
            ArgusIcon::Folder,
        ))
        .child(render_search_mode_row(snapshot, app_handle))
        .child(render_progress_and_actions(snapshot, app_handle))
}

/// 返回当前搜索窗口逻辑聚焦的输入框；没有显式焦点时不接收文本输入。
fn active_log_search_input_kind(search: &LogSearchState) -> Option<LogSearchInputKind> {
    if search.keyword_input.is_focused {
        Some(LogSearchInputKind::Keyword)
    } else if search.directory_input.is_focused {
        Some(LogSearchInputKind::Directory)
    } else {
        None
    }
}

/// 渲染搜索范围和匹配选项行。
fn render_search_mode_row(
    snapshot: &LogSearchSnapshot,
    app_handle: &Entity<ArgusApp>,
) -> impl IntoElement + use<> {
    let case_app = app_handle.clone();
    let regex_app = app_handle.clone();

    div()
        .h(px(30.0))
        .flex()
        .items_center()
        .justify_between()
        .gap_3()
        .child(render_scope_segment(snapshot, app_handle))
        .child(
            div()
                .flex()
                .items_center()
                .gap_1()
                .child(render_search_option_toggle(
                    "log-search-case-sensitive",
                    "Aa",
                    "区分大小写",
                    snapshot.log_search.case_sensitive,
                    &snapshot.theme,
                    move |_, _, cx| {
                        update_search_app(&case_app, cx, |app, _| {
                            app.toggle_log_search_case_sensitive();
                        });
                    },
                ))
                .child(render_search_option_toggle(
                    "log-search-regex",
                    ".*",
                    "正则搜索",
                    snapshot.log_search.regex_enabled,
                    &snapshot.theme,
                    move |_, _, cx| {
                        update_search_app(&regex_app, cx, |app, _| {
                            app.toggle_log_search_regex_enabled();
                        });
                    },
                )),
        )
}

/// 渲染搜索范围分段控件。
fn render_scope_segment(
    snapshot: &LogSearchSnapshot,
    app_handle: &Entity<ArgusApp>,
) -> impl IntoElement + use<> {
    let theme = snapshot.theme.clone();
    let active_scope = snapshot.log_search.scope;

    div().flex().items_center().gap_1().children(
        [
            SearchScope::CurrentFile,
            SearchScope::Directory,
            SearchScope::SelectedFiles,
        ]
        .into_iter()
        .map(|scope| {
            let is_active = scope == active_scope;
            let app = app_handle.clone();
            let icon = icon_for_search_scope(scope);
            div()
                .id(SharedString::from(format!("log-search-scope-{scope:?}")))
                .h(px(26.0))
                .px_3()
                .flex()
                .items_center()
                .gap_1()
                .rounded_sm()
                .text_size(px(12.0))
                .line_height(px(26.0))
                .cursor_pointer()
                .text_color(rgb(if is_active {
                    theme.foreground
                } else {
                    theme.foreground_muted
                }))
                .when(is_active, |this| this.bg(rgb(theme.selection)))
                .hover(|this| this.bg(rgb(theme.current_line)))
                .on_click(move |_, _, cx| {
                    update_search_app(&app, cx, |app, _| {
                        app.set_log_search_scope(scope);
                    });
                })
                .child(render_button_icon(icon, is_active, &theme))
                .child(button_label(scope.label()))
        }),
    )
}

/// 返回搜索范围按钮前置图标，便于用户快速区分搜索范围。
fn icon_for_search_scope(scope: SearchScope) -> ArgusIcon {
    match scope {
        SearchScope::CurrentFile => ArgusIcon::FileText,
        SearchScope::Directory => ArgusIcon::Folder,
        SearchScope::SelectedFiles => ArgusIcon::Logs,
    }
}

/// 渲染和文字按钮状态一致的紧凑图标。
fn render_button_icon(icon: ArgusIcon, is_active: bool, theme: &AppTheme) -> impl IntoElement {
    let color = if is_active {
        theme.foreground
    } else {
        theme.foreground_muted
    };
    button_icon(icon, color, 13.0)
}

/// 渲染搜索选项切换按钮。
fn render_search_option_toggle(
    id: &'static str,
    label: &'static str,
    tooltip: &'static str,
    is_active: bool,
    theme: &AppTheme,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut gpui::App) + 'static,
) -> impl IntoElement {
    let tooltip_background = theme.current_line;
    let tooltip_border = theme.border;
    let tooltip_foreground = theme.foreground;

    div()
        .id(id)
        .h(px(26.0))
        .min_w(px(34.0))
        .px_2()
        .flex()
        .items_center()
        .justify_center()
        .rounded_sm()
        .text_size(px(12.0))
        .line_height(px(26.0))
        .font_weight(FontWeight::SEMIBOLD)
        .cursor_pointer()
        .text_color(rgb(if is_active {
            theme.foreground
        } else {
            theme.foreground_muted
        }))
        .when(is_active, |this| this.bg(rgb(theme.selection)))
        .hover(|this| this.bg(rgb(theme.current_line)))
        .tooltip(move |_, cx| {
            cx.new(|_| LogSearchTooltip {
                label: tooltip.to_string(),
                background: tooltip_background,
                border: tooltip_border,
                foreground: tooltip_foreground,
            })
            .into()
        })
        .child(button_label(label))
        .on_click(on_click)
}

/// 渲染关键字或目录输入框。
fn render_search_input(
    snapshot: &LogSearchSnapshot,
    app_handle: &Entity<ArgusApp>,
    input_kind: LogSearchInputKind,
    focus_handle: FocusHandle,
    label: &'static str,
    placeholder: &'static str,
    icon: ArgusIcon,
) -> impl IntoElement + use<> {
    let theme = snapshot.theme.clone();
    let input_state = match input_kind {
        LogSearchInputKind::Keyword => snapshot.log_search.keyword_input.clone(),
        LogSearchInputKind::Directory => snapshot.log_search.directory_input.clone(),
    };
    let key_app = app_handle.clone();
    let click_app = app_handle.clone();
    let pointer_app = app_handle.clone();
    let clear_app = app_handle.clone();
    let native_input = app_native_input(
        app_handle.clone(),
        AppTextInputTarget::LogSearch(input_kind),
        focus_handle,
    );

    div()
        .flex()
        .items_center()
        .gap_3()
        .child(
            div()
                .w(px(48.0))
                .text_size(px(12.0))
                .text_color(rgb(theme.foreground_muted))
                .child(label),
        )
        .child(div().flex_1().min_w(px(0.0)).child(render_input(
            Input {
                id: match input_kind {
                    LogSearchInputKind::Keyword => "log-search-keyword-input",
                    LogSearchInputKind::Directory => "log-search-directory-input",
                },
                placeholder,
                value: input_state.value.clone(),
                is_disabled: false,
                is_focused: input_state.is_focused,
                cursor_index: input_state.cursor,
                selection_range: selection_range_for_input(&input_state),
                marked_range: input_state.marked_range.clone(),
                is_pointer_selecting: input_state.selection_drag.is_some(),
                size: InputSize::Regular,
                leading_accessory: Some(InputAccessory {
                    id: "log-search-leading",
                    icon,
                    tooltip: label,
                }),
                trailing_accessory: Some(InputAccessory {
                    id: match input_kind {
                        LogSearchInputKind::Keyword => "log-search-clear-keyword",
                        LogSearchInputKind::Directory => "log-search-clear-directory",
                    },
                    icon: ArgusIcon::Close,
                    tooltip: "清空",
                }),
                native_input: Some(native_input),
            },
            &theme,
            move |event: &KeyDownEvent, window, cx| {
                let should_close = event.keystroke.key.eq_ignore_ascii_case("escape");
                update_search_app(&key_app, cx, |app, app_cx| {
                    app.handle_log_search_input_key(input_kind, &event.keystroke, app_cx);
                });
                if should_close {
                    window.remove_window();
                }
            },
            move |_, _, cx| {
                cx.stop_propagation();
                update_search_app(&click_app, cx, |app, _| {
                    app.focus_log_search_input(input_kind);
                });
            },
            move |event: &InputPointerEvent, _, cx| {
                cx.stop_propagation();
                update_search_app(&pointer_app, cx, |app, _| match event.action {
                    InputPointerAction::Begin => app.begin_log_search_input_pointer_selection(
                        input_kind,
                        event.character_index,
                        event.granularity,
                    ),
                    InputPointerAction::Extend => app.update_log_search_input_pointer_selection(
                        input_kind,
                        event.character_index,
                    ),
                    InputPointerAction::Finish => {
                        app.finish_log_search_input_pointer_selection(input_kind)
                    }
                });
            },
            move |_, _, cx| {
                cx.stop_propagation();
                update_search_app(&clear_app, cx, |app, _| {
                    app.clear_log_search_input(input_kind);
                });
            },
        )))
}

/// 渲染进度和搜索/取消按钮。
fn render_progress_and_actions(
    snapshot: &LogSearchSnapshot,
    app_handle: &Entity<ArgusApp>,
) -> impl IntoElement + use<> {
    let theme = snapshot.theme.clone();
    let search = snapshot.log_search.clone();
    let start_app = app_handle.clone();
    let quick_app = app_handle.clone();
    let cancel_app = app_handle.clone();
    let count_app = app_handle.clone();
    let previous_app = app_handle.clone();
    let next_app = app_handle.clone();

    div()
        .h(px(34.0))
        .flex()
        .items_center()
        .gap_3()
        .when(!search.task_state.is_running(), |this| {
            this.child(action_button(
                "log-search-quick-keywords",
                ArgusIcon::QuickSearch,
                "快搜",
                &theme,
                move |_, _, cx| {
                    update_search_app(&quick_app, cx, |app, app_cx| {
                        app.start_quick_keyword_search(app_cx);
                    });
                },
            ))
        })
        .child(
            div()
                .flex_1()
                .flex()
                .items_center()
                .gap_2()
                .text_size(px(12.0))
                .text_color(rgb(theme.foreground_muted))
                .when(search.task_state.is_running(), |this| {
                    let progress_text = progress_label(&search);
                    this.child(render_loading_spinner(
                        ("log-search-progress-spinner", 0),
                        theme.foreground_muted,
                        14.0,
                    ))
                    .child(progress_text)
                })
                .when(
                    !search.task_state.is_running() && search.is_quick_counting,
                    |this| {
                        let message = search
                            .quick_match_message
                            .clone()
                            .unwrap_or_else(|| "处理中...".to_string());
                        this.child(render_loading_spinner(
                            ("log-search-quick-spinner", 0),
                            theme.foreground_muted,
                            14.0,
                        ))
                        .child(message)
                    },
                )
                .when(
                    !search.task_state.is_running()
                        && !search.is_quick_counting
                        && search.quick_match_message.is_some(),
                    |this| this.child(search.quick_match_message.clone().unwrap_or_default()),
                ),
        )
        .when(search.task_state.is_running(), |this| {
            this.child(action_button(
                "log-search-cancel",
                ArgusIcon::Close,
                "取消",
                &theme,
                move |_, _, cx| {
                    update_search_app(&cancel_app, cx, |app, _| {
                        app.cancel_log_search();
                    });
                },
            ))
        })
        .when(!search.task_state.is_running(), |this| {
            this.child(action_button(
                "log-search-count",
                ArgusIcon::Search,
                "计数",
                &theme,
                move |_, _, cx| {
                    update_search_app(&count_app, cx, |app, app_cx| {
                        app.count_current_log_matches(app_cx);
                    });
                },
            ))
            .child(action_button(
                "log-search-previous",
                ArgusIcon::ArrowUp,
                "",
                &theme,
                move |_, _, cx| {
                    update_search_app(&previous_app, cx, |app, app_cx| {
                        app.activate_previous_current_log_match(app_cx);
                    });
                },
            ))
            .child(action_button(
                "log-search-next",
                ArgusIcon::ArrowDown,
                "下一个",
                &theme,
                move |_, _, cx| {
                    update_search_app(&next_app, cx, |app, app_cx| {
                        app.activate_next_current_log_match(app_cx);
                    });
                },
            ))
            .child(action_button(
                "log-search-start",
                ArgusIcon::Search,
                "搜索",
                &theme,
                move |_, _, cx| {
                    update_search_app(&start_app, cx, |app, app_cx| {
                        app.start_log_search(app.log_search.scope, app_cx);
                    });
                },
            ))
        })
}

/// 渲染搜索窗口操作按钮。
fn action_button(
    id: &'static str,
    icon: ArgusIcon,
    label: &'static str,
    theme: &AppTheme,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut gpui::App) + 'static,
) -> impl IntoElement {
    div()
        .id(id)
        .h(px(28.0))
        .when(label.is_empty(), |this| this.w(px(38.0)))
        .when(!label.is_empty(), |this| this.px_3())
        .flex()
        .items_center()
        .justify_center()
        .gap_1()
        .rounded_sm()
        .bg(rgb(theme.current_line))
        .hover(|this| this.bg(rgb(theme.selection)))
        .text_size(px(12.0))
        .line_height(px(28.0))
        .text_color(rgb(theme.foreground))
        .cursor_pointer()
        .child(button_icon(icon, theme.foreground_muted, 13.0))
        .when(!label.is_empty(), |this| this.child(button_label(label)))
        .on_click(on_click)
}

/// 渲染搜索窗口按钮中的图标，统一修正视觉垂直居中。
fn button_icon(icon: ArgusIcon, color: u32, size: f32) -> impl IntoElement {
    div()
        .relative()
        .top(px(LOG_SEARCH_BUTTON_CONTENT_Y_OFFSET))
        .child(crate::ui::components::icon::render_icon(icon, color, size))
}

/// 渲染搜索窗口按钮中的文字，统一修正视觉垂直居中。
fn button_label(label: &'static str) -> impl IntoElement {
    div()
        .relative()
        .top(px(LOG_SEARCH_BUTTON_CONTENT_Y_OFFSET))
        .child(label)
}

/// 计算输入框选区范围。
fn selection_range_for_input(
    input: &crate::app::LogSearchInputState,
) -> Option<std::ops::Range<usize>> {
    let anchor = input.selection_anchor?;
    if anchor == input.cursor {
        return None;
    }

    Some(anchor.min(input.cursor)..anchor.max(input.cursor))
}

/// 返回搜索进度文案。
fn progress_label(search: &LogSearchState) -> String {
    let message = search.message.clone().unwrap_or_default();
    let prefix = match search.scope {
        SearchScope::CurrentFile => format!(
            "行进度 {}/{}",
            search.progress.scanned_lines, search.progress.total_lines
        ),
        SearchScope::Directory | SearchScope::SelectedFiles => format!(
            "文件进度 {}/{}",
            search.progress.scanned_files, search.progress.total_files
        ),
    };

    if message.is_empty() {
        format!("{prefix}，结果 {} 条", search.results.len())
    } else {
        format!("{prefix}，结果 {} 条，{message}", search.results.len())
    }
}

/// 将搜索窗口中的交互写回主应用实体。
fn update_search_app(
    app_handle: &Entity<ArgusApp>,
    cx: &mut gpui::App,
    update: impl FnOnce(&mut ArgusApp, &mut Context<ArgusApp>),
) {
    let _ = app_handle.update(cx, |app, app_cx| {
        update(app, app_cx);
        app_cx.notify();
    });
}

/// 搜索窗口内的紧凑悬停提示。
struct LogSearchTooltip {
    /// tooltip 展示文本。
    label: String,
    /// tooltip 背景色。
    background: u32,
    /// tooltip 边框色。
    border: u32,
    /// tooltip 文本色。
    foreground: u32,
}

impl Render for LogSearchTooltip {
    /// 渲染单行说明提示。
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .px_2()
            .py_1()
            .rounded_sm()
            .border_1()
            .border_color(rgb(self.border))
            .bg(rgb(self.background))
            .text_color(rgb(self.foreground))
            .text_size(px(11.0))
            .child(self.label.clone())
    }
}
