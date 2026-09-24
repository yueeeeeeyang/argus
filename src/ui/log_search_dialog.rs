//! 文件职责：渲染日志搜索对话框（主窗口内模态框）。
//! 创建日期：2026-06-11
//! 修改日期：2026-08-05
//! 作者：Argus 开发团队
//! 主要功能：提供主窗口内搜索模态框、关键字/目录输入和搜索范围切换控件。

use gpui::{
    AnyElement, App, Bounds, ClickEvent, Context, Entity, FocusHandle, FontWeight, IntoElement,
    KeyDownEvent, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, Pixels, Point, Render,
    ScrollHandle, SharedString, Size, Subscription, Window, canvas, deferred, div, point,
    prelude::*, px, rgb,
};

use crate::app::{
    AppTextInputTarget, ArgusApp, LogSearchInputKind, LogSearchState, TextInputState,
};
use crate::fonts::ARGUS_UI_FONT_FAMILY;
use crate::search::search_engine::{SearchProgress, SearchScope};
use crate::search::search_task::SearchTaskState;
use crate::theme::AppTheme;
use crate::ui::components::icon::{ArgusIcon, render_icon};
use crate::ui::components::icon_button::{IconButtonSize, render_icon_button};
use crate::ui::components::input::{
    Input, InputAccessory, InputPointerAction, InputPointerEvent, InputSize, render_input,
};
use crate::ui::components::loading_spinner::render_loading_spinner;
use crate::ui::components::modal_dialog::{
    ModalDialog, ModalFrame, render_modal_dialog_with_frame,
};
use crate::ui::components::scrollbar::{scrollbar_metrics, scrollbar_scroll_for_drag};
use crate::ui::input_native::app_native_input;

/// 搜索对话框模态框宽度。
const LOG_SEARCH_MODAL_WIDTH: f32 = 560.0;
/// 搜索对话框模态框高度。
///
/// 内容（标题 + 关键字 + 目录 + 模式行 + 操作行 + 内外边距）约需 244px；底部用 flex_1 占位
/// 把操作行顶到底部，300px 既容下完整内容与按钮上方留白，也为关键字历史下拉留出展开空间。
const LOG_SEARCH_MODAL_HEIGHT: f32 = 300.0;
/// 搜索对话框按钮内容视觉下移量，用于修正文字和图标在按钮内略靠上的观感。
const LOG_SEARCH_BUTTON_CONTENT_Y_OFFSET: f32 = 1.0;
/// 搜索对话框标题图标尺寸，和 14px 标题文字保持协调比例。
const LOG_SEARCH_TITLE_ICON_SIZE: f32 = 16.0;

/// 渲染主窗口内日志搜索模态框。
///
/// 参数说明：
/// - `view`：搜索对话框子视图，保存输入框焦点与历史下拉等界面状态。
/// - `theme`：当前主题令牌。
/// - `cx`：主窗口上下文，用于阻断遮罩层事件继续传递。
pub(crate) fn render_log_search_dialog(
    view: Entity<LogSearchDialog>,
    theme: &AppTheme,
    cx: &mut Context<ArgusApp>,
) -> AnyElement {
    // 拖拽偏移保存在对话框中；每次打开都会新建视图，位置自然回到居中。
    let content_offset = view.read_with(cx, |dialog, _| dialog.drag_offset);
    render_modal_dialog_with_frame(
        ModalDialog {
            overlay_id: "log-search-modal-overlay",
            container_id: "log-search-modal-container",
            width: LOG_SEARCH_MODAL_WIDTH,
            height: LOG_SEARCH_MODAL_HEIGHT,
            content: view.into_any_element(),
        },
        ModalFrame {
            border_color: Some(theme.border),
            content_offset,
        },
        theme.clone(),
        cx,
    )
    .into_any_element()
}

/// 搜索对话框根视图；业务状态仍保存在主应用实体中。
pub(crate) struct LogSearchDialog {
    /// 主应用实体。
    app: Entity<ArgusApp>,
    /// 搜索对话框根焦点句柄，用于对话框打开后直接接收键盘输入。
    focus_handle: FocusHandle,
    /// 关键字输入框真实焦点句柄。
    keyword_focus_handle: FocusHandle,
    /// 目录输入框真实焦点句柄。
    directory_focus_handle: FocusHandle,
    /// 是否已经执行过初始聚焦，避免每次重绘抢走输入框点击后的焦点。
    has_focused_root: bool,
    /// 当前渲染快照。
    snapshot: LogSearchSnapshot,
    /// 关键字输入框最近一次布局 bounds（窗口坐标），用于在根层定位历史下拉浮层。
    keyword_input_bounds: Option<Bounds<Pixels>>,
    /// 搜索根容器最近一次布局 bounds（窗口坐标），用于把浮层坐标换算成根内偏移。
    root_bounds: Option<Bounds<Pixels>>,
    /// 关键字历史下拉滚动容器的滚动句柄，用于渲染与拖拽自定义滚动条。
    keyword_history_scroll: ScrollHandle,
    /// 拖拽历史下拉滚动条时鼠标相对滑块顶部的偏移；非拖拽时为 None。
    keyword_history_drag: Option<Pixels>,
    /// 鼠标当前悬停的历史下拉条目索引；用 on_hover 显式追踪以保证逐行高亮。
    keyword_history_hover: Option<usize>,
    /// 模态框相对居中位置的拖拽偏移。
    drag_offset: Point<Pixels>,
    /// 正在进行的模态框拖拽起点；鼠标释放后清空。
    drag_start: Option<DialogDragStart>,
    /// 主应用观察订阅，确保后台搜索进度能刷新对话框。
    _app_observer: Subscription,
}

/// 对话框拖拽开始时的鼠标位置与偏移快照。
#[derive(Clone, Copy, Debug)]
struct DialogDragStart {
    /// 鼠标按下的窗口坐标。
    pointer: Point<Pixels>,
    /// 按下时的拖拽偏移。
    offset: Point<Pixels>,
}

impl LogSearchDialog {
    /// 创建搜索对话框并监听主应用状态变化。
    ///
    /// 参数说明：
    /// - `app`：主应用实体。
    /// - `theme`：首次绘制使用的主题快照。
    /// - `log_search`：首次绘制使用的搜索状态快照。
    /// - `cx`：搜索对话框上下文。
    ///
    /// 返回值：可渲染搜索对话框视图。
    pub(crate) fn new(
        app: Entity<ArgusApp>,
        theme: AppTheme,
        log_search: LogSearchState,
        cx: &mut Context<Self>,
    ) -> Self {
        let _app_observer = cx.observe(&app, |window, app_entity, cx| {
            let (window_open, history_open) = app_entity.read_with(cx, |app, _| {
                (
                    app.log_search.is_dialog_open,
                    app.log_search.keyword_history_open,
                )
            });
            // 对话框关闭时跳过快照重建：主窗口会频繁 notify 反复触发此回调，关闭态下克隆
            // 主题/状态/历史纯属浪费，且关闭态无需渲染。
            if !window_open {
                return;
            }
            // 历史下拉由关闭转为展开时，把滚动重置回顶部，确保最新关键字优先可见，
            // 避免沿用上一次展开遗留的滚动位置。
            if history_open && !window.snapshot.log_search.keyword_history_open {
                window
                    .keyword_history_scroll
                    .set_offset(point(px(0.0), px(0.0)));
            }
            let next_snapshot = app_entity.read_with(cx, |app, _| LogSearchSnapshot::from_app(app));
            if window.snapshot != next_snapshot {
                window.snapshot = next_snapshot;
                cx.notify();
            }
        });

        Self {
            app,
            focus_handle: cx.focus_handle(),
            keyword_focus_handle: cx.focus_handle(),
            directory_focus_handle: cx.focus_handle(),
            has_focused_root: false,
            // 初始历史快照留空：new() 可能在 ArgusApp 的 update 上下文中被调用，此处对同一
            // 实体 read_with 会触发借用 panic；observe 回调会在首次应用状态变化时补齐。
            snapshot: LogSearchSnapshot::from_initial(theme, &log_search),
            keyword_input_bounds: None,
            root_bounds: None,
            keyword_history_scroll: ScrollHandle::new(),
            keyword_history_drag: None,
            keyword_history_hover: None,
            drag_offset: Point::default(),
            drag_start: None,
            _app_observer,
        }
    }
}

impl Render for LogSearchDialog {
    /// 渲染搜索对话框内容。
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if !self.has_focused_root {
            self.keyword_focus_handle.focus(window);
            self.has_focused_root = true;
        }

        // 浮层高度来自两个窗口坐标探针：根容器底部减去关键字输入框底部；
        // 起始位置在输入框容器内部以相对定位确定，无需跨容器坐标换算。
        let overlay_layout = match (self.keyword_input_bounds, self.root_bounds) {
            (Some(input), Some(root)) => Some(keyword_history_layout(input, root)),
            _ => None,
        };
        let keyword_history_scroll = self.keyword_history_scroll.clone();
        // 下拉关闭时清空悬停索引，避免下次展开时残留上次高亮。
        let keyword_history_hover = if self.snapshot.log_search.keyword_history_open {
            self.keyword_history_hover
        } else {
            if self.keyword_history_hover.is_some() {
                self.keyword_history_hover = None;
            }
            None
        };
        render_window_content(
            &self.snapshot,
            &self.app,
            self.focus_handle.clone(),
            self.keyword_focus_handle.clone(),
            self.directory_focus_handle.clone(),
            overlay_layout,
            keyword_history_scroll,
            keyword_history_hover,
            cx,
        )
    }
}

/// 搜索对话框只读渲染快照。
#[derive(Clone, Debug, Eq, PartialEq)]
struct LogSearchSnapshot {
    /// 当前主题。
    theme: AppTheme,
    /// 当前搜索对话框渲染所需的轻量搜索状态。
    log_search: LogSearchDialogStateSnapshot,
    /// 全部最近关键字历史（最新在前），用于下拉展示；不按当前输入过滤。
    keyword_history: Vec<String>,
}

impl LogSearchSnapshot {
    /// 从主应用状态构造搜索对话框轻量快照；避免把完整搜索结果集合复制到对话框。
    fn from_app(app: &ArgusApp) -> Self {
        Self {
            theme: app.theme.clone(),
            log_search: LogSearchDialogStateSnapshot::from_search_state(&app.log_search),
            keyword_history: app.keyword_history_items(),
        }
    }

    /// 构造对话框创建首帧使用的快照；此时不能读取同一个 `ArgusApp` 实体，因此历史先留空。
    fn from_initial(theme: AppTheme, search: &LogSearchState) -> Self {
        Self {
            theme,
            log_search: LogSearchDialogStateSnapshot::from_search_state(search),
            keyword_history: Vec::new(),
        }
    }
}

/// 搜索对话框实际渲染所需的轻量状态，避免携带全量搜索结果和快速匹配缓存。
#[derive(Clone, Debug, Eq, PartialEq)]
struct LogSearchDialogStateSnapshot {
    /// 当前搜索范围。
    scope: SearchScope,
    /// 关键字输入框状态。
    keyword_input: TextInputState,
    /// 关键字历史下拉菜单是否展开。
    keyword_history_open: bool,
    /// 关键字历史下拉菜单当前高亮项索引。
    keyword_history_highlight: Option<usize>,
    /// 目录输入框状态。
    directory_input: TextInputState,
    /// 是否区分大小写。
    case_sensitive: bool,
    /// 是否启用正则搜索。
    regex_enabled: bool,
    /// 当前搜索进度。
    progress: SearchProgress,
    /// 当前搜索任务状态。
    task_state: SearchTaskState,
    /// 当前日志快速查找提示。
    quick_match_message: Option<String>,
    /// 是否正在扫描当前日志用于计数或定位。
    is_quick_counting: bool,
    /// 全量搜索结果数量；对话框只展示数量，不需要复制结果明细。
    result_count: usize,
    /// 最近一次搜索错误或提示。
    message: Option<String>,
}

impl LogSearchDialogStateSnapshot {
    /// 从完整搜索状态抽取对话框需要的字段，过滤掉体积大的结果集合。
    fn from_search_state(search: &LogSearchState) -> Self {
        Self {
            scope: search.scope,
            keyword_input: search.keyword_input.clone(),
            keyword_history_open: search.keyword_history_open,
            keyword_history_highlight: search.keyword_history_highlight,
            directory_input: search.directory_input.clone(),
            case_sensitive: search.case_sensitive,
            regex_enabled: search.regex_enabled,
            progress: search.progress.clone(),
            task_state: search.task_state.clone(),
            quick_match_message: search.quick_match_message.clone(),
            is_quick_counting: search.is_quick_counting,
            result_count: search.results.len(),
            message: search.message.clone(),
        }
    }
}

/// 渲染对话框主体。
fn render_window_content(
    snapshot: &LogSearchSnapshot,
    app_handle: &Entity<ArgusApp>,
    focus_handle: FocusHandle,
    keyword_focus_handle: FocusHandle,
    directory_focus_handle: FocusHandle,
    overlay_layout: Option<KeywordHistoryLayout>,
    keyword_history_scroll: ScrollHandle,
    keyword_history_hover: Option<usize>,
    cx: &mut Context<LogSearchDialog>,
) -> impl IntoElement + use<> {
    let theme = snapshot.theme.clone();
    let close_app = app_handle.clone();
    let key_app = app_handle.clone();
    let key_focus_handle = focus_handle.clone();
    let blur_app = app_handle.clone();
    let blur_focus_handle = focus_handle.clone();
    let active_input_kind = active_log_search_input_kind(&snapshot.log_search);

    // 浮层高度取根容器底部到关键字输入框底部的可用空间，避免展开时超出对话框。
    // 浮层本身挂在关键字输入框容器内部（见 render_search_input）并用 deferred 提升
    // 绘制顺序，因此不需要跨容器坐标换算，也不会被后续同级元素盖住。
    let measure_entity = cx.entity();
    let keyword_history_overlay =
        if snapshot.log_search.keyword_history_open && !snapshot.keyword_history.is_empty() {
            overlay_layout.map(|layout| KeywordHistoryOverlayState {
                input_height: layout.input_height,
                max_height: layout.max_height,
                scroll_handle: keyword_history_scroll,
                hover: keyword_history_hover,
                window_entity: measure_entity.clone(),
            })
        } else {
            None
        };

    div()
        .id("log-search-dialog-root")
        .size_full()
        .flex()
        .flex_col()
        .gap_3()
        .p_4()
        .relative()
        .bg(rgb(theme.content))
        .font_family(ARGUS_UI_FONT_FAMILY)
        .text_color(rgb(theme.foreground))
        .occlude()
        .focusable()
        .track_focus(&focus_handle)
        .on_click(move |_, window, cx| {
            blur_focus_handle.focus(window);
            update_search_app(&blur_app, cx, |app, _| {
                // 点击对话框空白处统一失焦；clear_all_text_input_focus 会一并收起关键字历史下拉。
                // 输入框与下拉项各自 stop_propagation，不会误触此回调。
                app.clear_all_text_input_focus();
            });
        })
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(|dialog, event: &MouseDownEvent, _, _| {
                // 输入框与历史下拉条目会在按下阶段阻止冒泡，这里收到的是空白、标题或按钮
                // 区域：记录拖拽起点，按住移动即可把整个模态框拖到窗口内任意位置。
                dialog.drag_start = Some(DialogDragStart {
                    pointer: event.position,
                    offset: dialog.drag_offset,
                });
            }),
        )
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
                    app.close_log_search_dialog();
                });
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
                    "log-search-dialog-close",
                    ArgusIcon::Close,
                    "关闭",
                    false,
                    IconButtonSize::Small,
                    &theme,
                    move |_, _, cx| {
                        update_search_app(&close_app, cx, |app, _| {
                            app.close_log_search_dialog();
                        });
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
            keyword_history_overlay,
            measure_entity.clone(),
        ))
        .child(render_search_input(
            snapshot,
            app_handle,
            LogSearchInputKind::Directory,
            directory_focus_handle,
            "目录",
            "来源树目录路径",
            ArgusIcon::Folder,
            None,
            measure_entity.clone(),
        ))
        .child(render_search_mode_row(snapshot, app_handle))
        // 弹性占位吸收对话框剩余高度，把操作行顶到底部，留白出现在按钮上方；
        // 用 flex_grow 而非 mt_auto，后者在本框架下不能可靠占据剩余空间。
        .child(div().flex_1().min_h(px(0.0)))
        .child(render_progress_and_actions(snapshot, app_handle))
        // 根容器尺寸探针：与关键字输入框探针同在窗口坐标系下测量，二者相减得到浮层
        // 可用高度；canvas 不参与命中测试，不影响对话框交互。
        .child(
            canvas(
                move |bounds, _, cx: &mut App| {
                    measure_entity.update(cx, |dialog, dcx| {
                        if dialog.root_bounds.as_ref() != Some(&bounds) {
                            dialog.root_bounds = Some(bounds);
                            dcx.notify();
                        }
                    });
                },
                |_, _, _, _| {},
            )
            .absolute()
            .top_0()
            .left_0()
            .size_full(),
        )
        .child(render_dialog_drag_sensor(cx))
}

/// 渲染模态框拖拽传感器。
///
/// 说明：鼠标按下后指针可能移出模态框甚至移出输入框命中范围，元素级鼠标事件不再触发；
/// 这里通过 canvas 注册窗口级监听，持续把指针位置换算成模态框偏移，并限制在窗口内。
fn render_dialog_drag_sensor(cx: &mut Context<LogSearchDialog>) -> AnyElement {
    let entity = cx.entity();
    canvas(
        |_, _, _| (),
        move |_, _, window: &mut Window, _| {
            window.on_mouse_event({
                let entity = entity.clone();
                move |event: &MouseMoveEvent, phase, window, cx| {
                    if !phase.bubble() || !event.dragging() {
                        return;
                    }
                    let entity_id = entity.entity_id();
                    let moved = entity.update(cx, |dialog, dialog_cx| {
                        let Some(start) = dialog.drag_start else {
                            return false;
                        };
                        let offset = clamp_drag_offset(
                            start.offset + (event.position - start.pointer),
                            window.viewport_size(),
                        );
                        if offset == dialog.drag_offset {
                            return false;
                        }
                        dialog.drag_offset = offset;
                        dialog_cx.notify();
                        true
                    });
                    if moved {
                        cx.stop_propagation();
                        cx.notify(entity_id);
                    }
                }
            });

            window.on_mouse_event({
                let entity = entity.clone();
                move |event: &MouseUpEvent, phase, _, cx| {
                    if !phase.bubble() || event.button != MouseButton::Left {
                        return;
                    }
                    let entity_id = entity.entity_id();
                    let handled = entity.update(cx, |dialog, _| dialog.drag_start.take().is_some());
                    if handled {
                        cx.notify(entity_id);
                    }
                }
            });
        },
    )
    .absolute()
    .top_0()
    .left_0()
    .size_full()
    .into_any_element()
}

/// 把拖拽偏移限制在当前窗口内，保证模态框不会被拖出可视区域。
///
/// 参数说明：
/// - `offset`：期望的拖拽偏移（相对居中位置）。
/// - `viewport`：当前窗口可视区域尺寸。
///
/// 返回值：窗口放得下时限制在可移动范围内；窗口小于模态框时收敛为居中。
fn clamp_drag_offset(offset: Point<Pixels>, viewport: Size<Pixels>) -> Point<Pixels> {
    let max_x = (f32::from(viewport.width - px(LOG_SEARCH_MODAL_WIDTH)) / 2.0).max(0.0);
    let max_y = (f32::from(viewport.height - px(LOG_SEARCH_MODAL_HEIGHT)) / 2.0).max(0.0);
    point(
        px(f32::from(offset.x).clamp(-max_x, max_x)),
        px(f32::from(offset.y).clamp(-max_y, max_y)),
    )
}

/// 关键字历史下拉最大高度，避免历史很多时占满对话框。
const LOG_SEARCH_HISTORY_MAX_HEIGHT: f32 = 220.0;

/// 关键字历史下拉的布局约束；位置由输入框容器内部相对定位决定，只保留尺寸信息。
#[derive(Clone, Copy, Debug, PartialEq)]
struct KeywordHistoryLayout {
    /// 关键字输入框高度，浮层从其下边缘开始。
    input_height: Pixels,
    /// 浮层最大高度（关键字输入框底部到根容器底部）。
    max_height: Pixels,
}

/// 根据关键字输入框与对话框根容器的窗口坐标 bounds 计算历史下拉布局约束。
///
/// 说明：输入框容器与根容器各有一个布局探针，二者相减即可得到浮层可用高度；
/// 探针未完成布局或输入框已越出根容器时，最大高度收敛为 0。
fn keyword_history_layout(input: Bounds<Pixels>, root: Bounds<Pixels>) -> KeywordHistoryLayout {
    KeywordHistoryLayout {
        input_height: input.size.height,
        max_height: (root.bottom() - input.bottom() - px(8.0))
            .max(px(0.0))
            .min(px(LOG_SEARCH_HISTORY_MAX_HEIGHT)),
    }
}

/// 关键字输入框下方历史下拉的渲染状态。
struct KeywordHistoryOverlayState {
    /// 关键字输入框高度。
    input_height: Pixels,
    /// 浮层最大高度。
    max_height: Pixels,
    /// 历史下拉滚动容器句柄。
    scroll_handle: ScrollHandle,
    /// 当前悬停条目索引。
    hover: Option<usize>,
    /// 视图实体，供滚动条拖拽回调使用。
    window_entity: Entity<LogSearchDialog>,
}

/// 返回当前搜索对话框逻辑聚焦的输入框；没有显式焦点时不接收文本输入。
fn active_log_search_input_kind(
    search: &LogSearchDialogStateSnapshot,
) -> Option<LogSearchInputKind> {
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
                .when(is_active, |this| this.bg(rgb(theme.current_line)))
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
        .when(is_active, |this| this.bg(rgb(theme.current_line)))
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
    keyword_history: Option<KeywordHistoryOverlayState>,
    measure_entity: Entity<LogSearchDialog>,
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
        .child(
            div()
                .flex_1()
                .min_w(px(0.0))
                .relative()
                .child(render_input(
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
                        is_secret: false,
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
                    move |event: &KeyDownEvent, _, cx| {
                        key_app.update(cx, |app, app_cx| {
                            app.handle_log_search_input_key(input_kind, &event.keystroke, app_cx);
                            // 与 update_search_app 一致：Entity::update 不会自动 notify，导航键
                            // （上下移动历史高亮、回车选中）改了状态后必须显式通知。
                            app_cx.notify();
                        });
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
                            InputPointerAction::Begin => app
                                .begin_log_search_input_pointer_selection(
                                    input_kind,
                                    event.character_index,
                                    event.granularity,
                                ),
                            InputPointerAction::Extend => app
                                .update_log_search_input_pointer_selection(
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
                ))
                .when(input_kind == LogSearchInputKind::Keyword, |this| {
                    // 探针元素：absolute + top_0/left_0 + size_full 覆盖关键字输入框容器原点，
                    // prepaint 阶段拿到容器 bounds 并写回视图状态，供浮层高度与起始位置使用；仅在
                    // bounds 变化时 notify，避免重绘死循环。必须显式 top_0/left_0，否则绝对元素
                    // 会落到其静态位置（input 下方），导致测得的 bounds 偏低、浮层离输入框过远。
                    this.child(
                        canvas(
                            move |bounds, _, cx: &mut App| {
                                measure_entity.update(cx, |dialog, dcx| {
                                    if dialog.keyword_input_bounds.as_ref() != Some(&bounds) {
                                        dialog.keyword_input_bounds = Some(bounds);
                                        dcx.notify();
                                    }
                                });
                            },
                            |_, _, _, _| {},
                        )
                        .absolute()
                        .top_0()
                        .left_0()
                        .size_full(),
                    )
                    // 历史下拉挂在输入框容器内部：纵向从其下边缘开始，横向铺满容器宽度；
                    // deferred 让它在所有祖先之后绘制，从而压在目录输入框等后续同级元素之上。
                    .when_some(keyword_history, |this, history| {
                        this.child(deferred(render_keyword_history_overlay(
                            snapshot, app_handle, history,
                        )))
                    })
                }),
        )
}

/// 渲染关键字历史下拉浮层；点击条目回填输入框并触发搜索。
///
/// 浮层由调用方挂在关键字输入框容器内部，纵向上从输入框下边缘开始、横向铺满容器宽度，
/// 并用 deferred 在所有祖先之后绘制，从而压在目录输入框等后续同级元素之上。
fn render_keyword_history_overlay(
    snapshot: &LogSearchSnapshot,
    app_handle: &Entity<ArgusApp>,
    history_state: KeywordHistoryOverlayState,
) -> impl IntoElement + use<> {
    let theme = snapshot.theme.clone();
    let input_height = history_state.input_height;
    let max_height = history_state.max_height;
    let scroll_handle = history_state.scroll_handle;
    let keyword_history_hover = history_state.hover;
    let window_entity = history_state.window_entity;
    // 借用而非克隆：渲染每帧都会进入此函数，整 Vec 克隆纯属浪费。
    let history = &snapshot.keyword_history;
    let highlight = snapshot.log_search.keyword_history_highlight;
    let mut list = div().flex().flex_col().py_1();
    for (index, keyword) in history.iter().enumerate() {
        let is_active = highlight == Some(index);
        let is_hovered = keyword_history_hover == Some(index);
        let select_app = app_handle.clone();
        let hover_entity = window_entity.clone();
        list = list.child(
            div()
                // 用 (&'static str, usize) 元组作 id，避免每帧 format! 分配字符串。
                .id(("log-search-keyword-history", index))
                .h(px(30.0))
                .w_full()
                .px_3()
                .flex()
                .items_center()
                .text_size(px(12.0))
                .text_color(rgb(theme.foreground))
                .cursor_pointer()
                // 默认透明背景（沿用浮层容器底色，避免整块灰底）；键盘激活用 selection，
                // 鼠标悬停用 current_line。悬停态由 on_hover 显式写入视图状态，保证逐行高亮。
                .when(is_active, |this| this.bg(rgb(theme.current_line)))
                .when(!is_active && is_hovered, |this| {
                    this.bg(rgb(theme.current_line))
                })
                .on_hover(move |hovered, _, cx| {
                    hover_entity.update(cx, |window, wcx| {
                        let changed = if *hovered && window.keyword_history_hover != Some(index) {
                            window.keyword_history_hover = Some(index);
                            true
                        } else if !*hovered && window.keyword_history_hover == Some(index) {
                            window.keyword_history_hover = None;
                            true
                        } else {
                            false
                        };
                        if changed {
                            wcx.notify();
                        }
                    });
                })
                .child(keyword.clone())
                .on_click(move |_, _, cx| {
                    cx.stop_propagation();
                    update_search_app(&select_app, cx, |app, app_cx| {
                        app.select_keyword_history(index, app_cx);
                    });
                }),
        );
    }
    // 外层 absolute 容器同时作为滚动条的 containing block（thumb 以 absolute 定位其上）。
    div()
        .absolute()
        .top(input_height)
        .left(px(0.0))
        .right(px(0.0))
        .mt_1()
        .child(
            div()
                .id("log-search-keyword-history-scroll")
                .max_h(max_height)
                .overflow_y_scroll()
                .track_scroll(&scroll_handle)
                .bg(rgb(theme.content))
                .border_1()
                .border_color(rgb(theme.border))
                .rounded(px(6.0))
                .occlude()
                // 输入框的透明指针层使用原始 bounds 处理 MouseDown，不会受 occlude 的
                // 命中过滤约束。因此必须在按下阶段就终止事件，避免条目覆盖的目录
                // 输入框先开始光标选择；仅在 on_click 阶段拦截已经太晚。
                .on_mouse_down(MouseButton::Left, move |_, _, cx| {
                    cx.stop_propagation();
                })
                // 点击下拉内部空白（非条目）不冒泡到根，避免误关下拉；条目自带 stop_propagation。
                .on_click(move |_, _, cx| {
                    cx.stop_propagation();
                })
                .child(list),
        )
        .child(render_keyword_history_scrollbar(
            &scroll_handle,
            window_entity,
            &theme,
        ))
}

/// 历史下拉滚动条内边距（轨道上下留白，避免滑块贴边）。
const KEYWORD_HISTORY_SCROLLBAR_PADDING: f32 = 4.0;
/// 历史下拉滚动条滑块最小长度，保证内容远超视口时仍可抓取。
const KEYWORD_HISTORY_SCROLLBAR_MIN_THUMB: f32 = 28.0;
/// 历史下拉滚动条滑块宽度。
const KEYWORD_HISTORY_SCROLLBAR_THUMB_SIZE: f32 = 5.0;

/// 渲染历史下拉可拖拽滚动条；首帧滚动句柄尚未布局时返回透明哨兵，布局完成后触发重绘显示真实滑块。
fn render_keyword_history_scrollbar(
    scroll_handle: &ScrollHandle,
    window_entity: Entity<LogSearchDialog>,
    theme: &AppTheme,
) -> AnyElement {
    let bounds = scroll_handle.bounds();
    let max_offset = scroll_handle.max_offset();
    let offset = scroll_handle.offset();
    let content_height = bounds.size.height + max_offset.height;
    match scrollbar_metrics(
        bounds.size.height,
        content_height,
        -offset.y,
        KEYWORD_HISTORY_SCROLLBAR_PADDING,
        KEYWORD_HISTORY_SCROLLBAR_MIN_THUMB,
    ) {
        Some(metrics) => {
            let scroll_handle = scroll_handle.clone();
            div()
                .absolute()
                .top(metrics.thumb_start)
                .right(px(KEYWORD_HISTORY_SCROLLBAR_PADDING))
                .w(px(KEYWORD_HISTORY_SCROLLBAR_THUMB_SIZE))
                .h(metrics.thumb_length)
                .rounded_lg()
                .bg(rgb(theme.foreground_muted))
                .opacity(0.48)
                .hover(|this| this.opacity(0.78))
                .cursor_pointer()
                .occlude()
                .child(
                    canvas(
                        |_, _, _| (),
                        move |thumb_bounds, _, window: &mut Window, _| {
                            window.on_mouse_event({
                                let entity = window_entity.clone();
                                move |event: &MouseDownEvent, phase, _, cx| {
                                    if !phase.bubble()
                                        || event.button != MouseButton::Left
                                        || !thumb_bounds.contains(&event.position)
                                    {
                                        return;
                                    }
                                    let cursor_offset = event.position.y - thumb_bounds.top();
                                    entity.update(cx, |window, _| {
                                        window.keyword_history_drag = Some(cursor_offset);
                                    });
                                    cx.stop_propagation();
                                    cx.notify(entity.entity_id());
                                }
                            });

                            window.on_mouse_event({
                                let entity = window_entity.clone();
                                move |event: &MouseUpEvent, phase, _, cx| {
                                    if !phase.bubble() || event.button != MouseButton::Left {
                                        return;
                                    }
                                    let handled = entity.update(cx, |window, _| {
                                        let was_dragging = window.keyword_history_drag.is_some();
                                        window.keyword_history_drag = None;
                                        was_dragging
                                    });
                                    if handled {
                                        cx.stop_propagation();
                                        cx.notify(entity.entity_id());
                                    }
                                }
                            });

                            window.on_mouse_event({
                                let entity = window_entity.clone();
                                let scroll_handle = scroll_handle.clone();
                                move |event: &MouseMoveEvent, phase, _, cx| {
                                    if !phase.bubble() || !event.dragging() {
                                        return;
                                    }
                                    let handled = entity.update(cx, |window, _| {
                                        let Some(cursor_offset) = window.keyword_history_drag
                                        else {
                                            return false;
                                        };
                                        let pointer = event.position.y - bounds.top();
                                        let scroll = scrollbar_scroll_for_drag(
                                            pointer,
                                            cursor_offset,
                                            &metrics,
                                        );
                                        let current = scroll_handle.offset();
                                        scroll_handle.set_offset(point(current.x, -scroll));
                                        true
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
                .into_any_element()
        }
        None => {
            // 首帧哨兵：滚动句柄完成布局前占位，布局完成后若已溢出则触发一次重绘以显示滑块。
            // 故意用 1×1 尺寸：哨兵只需被绘制以触发 paint 回调（回调读取的是滚动句柄的 bounds，
            // 而非自身 bounds），避免全尺寸画布覆盖在条目之上干扰逐行 hover 命中。
            let scroll_handle = scroll_handle.clone();
            canvas(
                |_, _, _| (),
                move |_, _, _, cx: &mut App| {
                    let bounds = scroll_handle.bounds();
                    if bounds.size.height > px(0.0) && scroll_handle.max_offset().height > px(0.0) {
                        cx.notify(window_entity.entity_id());
                    }
                },
            )
            .absolute()
            .w(px(1.0))
            .h(px(1.0))
            .into_any_element()
        }
    }
}

/// 渲染进度和搜索/取消按钮。
fn render_progress_and_actions(
    snapshot: &LogSearchSnapshot,
    app_handle: &Entity<ArgusApp>,
) -> impl IntoElement + use<> {
    let theme = snapshot.theme.clone();
    let search = &snapshot.log_search;
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
                    let progress_text = progress_label(search);
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

/// 渲染搜索对话框操作按钮。
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
        .hover(|this| this.bg(rgb(theme.current_line)))
        .text_size(px(12.0))
        .line_height(px(28.0))
        .text_color(rgb(theme.foreground))
        .cursor_pointer()
        .child(button_icon(icon, theme.foreground_muted, 13.0))
        .when(!label.is_empty(), |this| this.child(button_label(label)))
        .on_click(on_click)
}

/// 渲染搜索对话框按钮中的图标，统一修正视觉垂直居中。
fn button_icon(icon: ArgusIcon, color: u32, size: f32) -> impl IntoElement {
    div()
        .relative()
        .top(px(LOG_SEARCH_BUTTON_CONTENT_Y_OFFSET))
        .child(crate::ui::components::icon::render_icon(icon, color, size))
}

/// 渲染搜索对话框按钮中的文字，统一修正视觉垂直居中。
fn button_label(label: &'static str) -> impl IntoElement {
    div()
        .relative()
        .top(px(LOG_SEARCH_BUTTON_CONTENT_Y_OFFSET))
        .child(label)
}

/// 计算输入框选区范围。
fn selection_range_for_input(input: &crate::app::TextInputState) -> Option<std::ops::Range<usize>> {
    let anchor = input.selection_anchor?;
    if anchor == input.cursor {
        return None;
    }

    Some(anchor.min(input.cursor)..anchor.max(input.cursor))
}

/// 返回搜索进度文案。
fn progress_label(search: &LogSearchDialogStateSnapshot) -> String {
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
        format!("{prefix}，结果 {} 条", search.result_count)
    } else {
        format!("{prefix}，结果 {} 条，{message}", search.result_count)
    }
}

/// 将搜索对话框中的交互写回主应用实体。
fn update_search_app(
    app_handle: &Entity<ArgusApp>,
    cx: &mut gpui::App,
    update: impl FnOnce(&mut ArgusApp, &mut Context<ArgusApp>),
) {
    app_handle.update(cx, |app, app_cx| {
        update(app, app_cx);
        app_cx.notify();
    });
}

/// 搜索对话框内的紧凑悬停提示。
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

#[cfg(test)]
mod tests {
    use super::*;

    /// 验证历史下拉高度取输入框底部到根容器底部的可用空间，并受最大高度约束。
    #[test]
    fn keyword_history_layout_follows_available_space() {
        let input = Bounds {
            origin: point(px(40.0), px(100.0)),
            size: gpui::size(px(300.0), px(34.0)),
        };
        let root = Bounds {
            origin: point(px(0.0), px(0.0)),
            size: gpui::size(px(548.0), px(288.0)),
        };

        let layout = keyword_history_layout(input, root);
        assert_eq!(layout.input_height, px(34.0));
        // 输入框底部 134 + 8px 间隙，剩余 146px 不足上限。
        assert_eq!(layout.max_height, px(146.0));

        // 根容器很高时收敛到最大高度。
        let tall_root = Bounds {
            origin: point(px(0.0), px(0.0)),
            size: gpui::size(px(548.0), px(600.0)),
        };
        assert_eq!(
            keyword_history_layout(input, tall_root).max_height,
            px(LOG_SEARCH_HISTORY_MAX_HEIGHT)
        );

        // 输入框已越出根容器底部时不给可用高度。
        let short_root = Bounds {
            origin: point(px(0.0), px(0.0)),
            size: gpui::size(px(548.0), px(120.0)),
        };
        assert_eq!(
            keyword_history_layout(input, short_root).max_height,
            px(0.0)
        );
    }

    /// 验证模态框拖拽偏移被限制在窗口内；窗口小于模态框时固定居中。
    #[test]
    fn clamp_drag_offset_keeps_dialog_inside_viewport() {
        let viewport = gpui::size(px(1330.0), px(820.0));
        // 窗口足够大：偏移夹到可移动范围。
        assert_eq!(
            clamp_drag_offset(point(px(1000.0), px(-900.0)), viewport),
            point(
                px((1330.0 - LOG_SEARCH_MODAL_WIDTH) / 2.0),
                px(-(820.0 - LOG_SEARCH_MODAL_HEIGHT) / 2.0)
            )
        );
        // 小偏移原样保留。
        assert_eq!(
            clamp_drag_offset(point(px(12.0), px(-8.0)), viewport),
            point(px(12.0), px(-8.0))
        );
        // 窗口小于模态框：收敛为居中（偏移为 0）。
        let tiny = gpui::size(px(400.0), px(200.0));
        assert_eq!(
            clamp_drag_offset(point(px(50.0), px(50.0)), tiny),
            point(px(0.0), px(0.0))
        );
    }
}
