//! 文件职责：渲染自定义跨平台日志来源选择器模态框。
//! 创建日期：2026-06-11
//! 修改日期：2026-07-14
//! 作者：Argus 开发团队
//! 主要功能：提供主窗口模态框中的目录浏览、目录/文件/压缩包多选和确认加载入口。

use std::ops::Range;
use std::path::PathBuf;

use gpui::{
    AnyElement, App, ClickEvent, Context, Entity, FocusHandle, FontWeight, IntoElement,
    KeyDownEvent, Render, SharedString, Subscription, Window, div, prelude::*, px, rgb,
    uniform_list,
};

use crate::app::{
    AppTextInputTarget, ArgusApp, SourcePickerSortDirection, SourcePickerSortKey, SourcePickerState,
};
use crate::fonts::ARGUS_UI_FONT_FAMILY;
use crate::loader::{BrowseEntry, BrowseEntryKind};
use crate::theme::AppTheme;
use crate::ui::components::icon::{ArgusIcon, render_icon};
use crate::ui::components::icon_button::{IconButtonSize, render_icon_button};
use crate::ui::components::input::{
    Input, InputAccessory, InputPointerAction, InputPointerEvent, InputSize, render_input,
};
use crate::ui::components::loading_spinner::render_loading_spinner;
use crate::ui::components::modal_dialog::{ModalDialog, render_modal_dialog};
use crate::ui::input_native::app_native_input;
use crate::utils::path::{display_name, display_path};
use crate::utils::size_format::format_bytes;
use crate::utils::time_format::format_modified_time;

/// 左侧快捷入口宽度。
const SOURCE_PICKER_LOCATION_WIDTH: f32 = 188.0;
/// 来源选择器模态框宽度，沿用改造前独立窗口尺寸。
const SOURCE_PICKER_MODAL_WIDTH: f32 = 900.0;
/// 来源选择器模态框高度，沿用改造前独立窗口尺寸。
const SOURCE_PICKER_MODAL_HEIGHT: f32 = 620.0;
/// 选择器模态框固定头部高度，和设置模态框保持一致。
const SOURCE_PICKER_HEADER_HEIGHT: f32 = 56.0;
/// 选择器模态框标题图标尺寸，和 14px 标题文字保持协调比例。
const SOURCE_PICKER_TITLE_ICON_SIZE: f32 = 16.0;
/// 选择器内容区统一内边距。
const SOURCE_PICKER_CONTENT_PADDING: f32 = 16.0;
/// 文件列表固定行高。
const SOURCE_PICKER_ROW_HEIGHT: f32 = 32.0;
/// 修改日期列宽度。
const SOURCE_PICKER_MODIFIED_WIDTH: f32 = 128.0;
/// 大小列宽度。
const SOURCE_PICKER_SIZE_WIDTH: f32 = 76.0;
/// 选择器按钮内容视觉下移量，用于抵消字体和 SVG 几何居中后的视觉偏上。
const SOURCE_PICKER_BUTTON_CONTENT_Y_OFFSET: f32 = 1.0;
/// 选择器表头内容视觉下移量，表头字号更小，使用更轻的修正避免显得下坠。
const SOURCE_PICKER_HEADER_CONTENT_Y_OFFSET: f32 = 0.5;
/// 选择器表头图标尺寸。
const SOURCE_PICKER_HEADER_ICON_SIZE: f32 = 13.0;

/// 来源选择器子视图；通过观察主应用实体获得最新选择器状态。
pub(crate) struct SourcePickerWindow {
    /// 主应用实体，选择器所有业务状态仍集中保存在 `ArgusApp`。
    app: Entity<ArgusApp>,
    /// 当前子视图自己的渲染快照，避免首次打开时读取正在更新的主应用实体。
    snapshot: SourcePickerSnapshot,
    /// 当前渲染快照的轻量签名，用于跳过主应用无关通知。
    snapshot_signature: SourcePickerSnapshotSignature,
    /// 来源选择器根区域焦点，用于点击非路径输入框区域时承接键盘焦点。
    root_focus_handle: FocusHandle,
    /// 路径输入框真实焦点句柄。
    path_focus_handle: FocusHandle,
    /// 主应用状态订阅，保持选择器模态框随后台目录读取结果刷新。
    _app_observer: Subscription,
}

/// 将来源选择器子视图包裹为主窗口模态框。
pub(crate) fn render_source_picker_modal(
    picker: Entity<SourcePickerWindow>,
    theme: &AppTheme,
    cx: &mut Context<ArgusApp>,
) -> AnyElement {
    render_modal_dialog(
        ModalDialog {
            overlay_id: "source-picker-modal-overlay",
            container_id: "source-picker-modal-container",
            width: SOURCE_PICKER_MODAL_WIDTH,
            height: SOURCE_PICKER_MODAL_HEIGHT,
            content: picker.into_any_element(),
        },
        theme.clone(),
        cx,
    )
    .into_any_element()
}

impl SourcePickerWindow {
    /// 创建来源选择器模态框子视图，并监听主应用状态变化。
    ///
    /// 参数说明：
    /// - `app`：主应用实体，用于读取主题、目录列表和写回交互状态。
    /// - `theme`：子视图首次绘制使用的主题快照。
    /// - `source_picker`：子视图首次绘制使用的选择器状态快照。
    /// - `cx`：选择器子视图上下文，用于注册观察订阅。
    ///
    /// 返回值：可渲染的选择器模态框子视图。
    pub(crate) fn new(
        app: Entity<ArgusApp>,
        theme: AppTheme,
        source_picker: SourcePickerState,
        cx: &mut Context<Self>,
    ) -> Self {
        let _app_observer = cx.observe(&app, |picker, app_entity, cx| {
            let next_signature = app_entity.read_with(cx, |app, _| {
                SourcePickerSnapshotSignature::from_parts(&app.theme, &app.source_picker)
            });
            if picker.snapshot_signature == next_signature {
                return;
            }

            picker.snapshot = app_entity.read_with(cx, |app, _| SourcePickerSnapshot {
                theme: app.theme.clone(),
                source_picker: app.source_picker.clone(),
            });
            picker.snapshot_signature = next_signature;
            cx.notify();
        });

        let snapshot_signature = SourcePickerSnapshotSignature::from_parts(&theme, &source_picker);
        Self {
            app,
            snapshot: SourcePickerSnapshot {
                theme,
                source_picker,
            },
            snapshot_signature,
            root_focus_handle: cx.focus_handle(),
            path_focus_handle: cx.focus_handle(),
            _app_observer,
        }
    }
}

impl Render for SourcePickerWindow {
    /// 渲染模态框内的选择器内容。
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let app_handle = self.app.clone();
        let snapshot = self.snapshot.clone();

        render_window_content(
            &snapshot,
            &app_handle,
            self.root_focus_handle.clone(),
            self.path_focus_handle.clone(),
            cx,
        )
    }
}

/// 来源选择器渲染快照；只读数据在每帧开始时复制，避免跨实体借用阻塞渲染。
#[derive(Clone)]
struct SourcePickerSnapshot {
    /// 当前主题令牌。
    theme: AppTheme,
    /// 当前选择器 UI 状态。
    source_picker: SourcePickerState,
}

/// 来源选择器快照轻量签名；用于避免每次主应用通知都深复制目录条目。
#[derive(Clone, Debug, Eq, PartialEq)]
struct SourcePickerSnapshotSignature {
    /// 当前主题令牌。
    theme: AppTheme,
    /// 当前目录。
    current_dir: PathBuf,
    /// 当前目录父级。
    parent_dir: Option<PathBuf>,
    /// 当前目录条目数量。
    entry_count: usize,
    /// 已选择路径；通常数量较小，直接纳入签名以保持选中态及时刷新。
    selected_paths: Vec<PathBuf>,
    /// 是否正在读取目录。
    is_loading: bool,
    /// 最近一次错误提示。
    error_message: Option<String>,
    /// 浏览任务 generation。
    browse_generation: usize,
    /// 路径输入框文本。
    path_input: String,
    /// 路径输入框光标。
    path_input_cursor: usize,
    /// 路径输入框选区锚点。
    path_input_selection_anchor: Option<usize>,
    /// 路径输入框焦点态。
    is_path_input_focused: bool,
    /// 当前排序字段。
    sort_key: SourcePickerSortKey,
    /// 当前排序方向。
    sort_direction: SourcePickerSortDirection,
}

impl SourcePickerSnapshotSignature {
    /// 从主应用状态生成轻量签名，不复制 `entries` 详情。
    fn from_parts(theme: &AppTheme, source_picker: &SourcePickerState) -> Self {
        Self {
            theme: theme.clone(),
            current_dir: source_picker.current_dir.clone(),
            parent_dir: source_picker.parent_dir.clone(),
            entry_count: source_picker.entries.len(),
            selected_paths: source_picker.selected_paths.clone(),
            is_loading: source_picker.is_loading,
            error_message: source_picker.error_message.clone(),
            browse_generation: source_picker.browse_generation,
            path_input: source_picker.path_input.clone(),
            path_input_cursor: source_picker.path_input_cursor,
            path_input_selection_anchor: source_picker.path_input_selection_anchor,
            is_path_input_focused: source_picker.is_path_input_focused,
            sort_key: source_picker.sort_key,
            sort_direction: source_picker.sort_direction,
        }
    }
}

/// 渲染来源选择器模态框主体。
fn render_window_content(
    snapshot: &SourcePickerSnapshot,
    app_handle: &Entity<ArgusApp>,
    root_focus_handle: FocusHandle,
    path_focus_handle: FocusHandle,
    cx: &mut Context<SourcePickerWindow>,
) -> impl IntoElement + use<> {
    let theme = snapshot.theme.clone();
    let close_app = app_handle.clone();
    let blur_app = app_handle.clone();
    let root_focus_for_track = root_focus_handle.clone();
    let root_focus_for_click = root_focus_handle.clone();

    div()
        .id("source-picker-window-root")
        .size_full()
        .relative()
        .flex()
        .rounded_lg()
        .overflow_hidden()
        .bg(rgb(theme.content))
        .border_1()
        .border_color(rgb(theme.border))
        .font_family(ARGUS_UI_FONT_FAMILY)
        .text_color(rgb(theme.foreground))
        .occlude()
        .focusable()
        .track_focus(&root_focus_for_track)
        .on_click(move |_, window, cx| {
            root_focus_for_click.focus(window);
            update_picker_app(&blur_app, cx, |app, _| {
                app.clear_all_text_input_focus();
            });
        })
        .child(
            div()
                .w(px(SOURCE_PICKER_LOCATION_WIDTH))
                .h_full()
                .flex_none()
                .flex()
                .flex_col()
                .bg(rgb(theme.side_bar))
                .child(render_sidebar_title(&theme))
                .child(render_locations(snapshot, &theme, app_handle)),
        )
        .child(
            div()
                .flex_1()
                .min_w(px(0.0))
                .h_full()
                .flex()
                .flex_col()
                .bg(rgb(theme.content))
                .child(render_browser_title_bar(&theme, &close_app))
                .child(render_browser(
                    snapshot,
                    &theme,
                    app_handle,
                    path_focus_handle,
                    cx,
                )),
        )
}

/// 渲染左侧标题，使侧栏背景从模态框顶部连续延伸到底部。
fn render_sidebar_title(theme: &AppTheme) -> impl IntoElement + use<> {
    div()
        .h(px(SOURCE_PICKER_HEADER_HEIGHT))
        .flex_none()
        .px_4()
        .flex()
        .items_center()
        .bg(rgb(theme.side_bar))
        .child(
            div()
                .flex()
                .items_center()
                .gap_2()
                .text_size(px(14.0))
                .line_height(px(18.0))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(rgb(theme.foreground))
                .child(render_icon(
                    ArgusIcon::FolderPlus,
                    theme.foreground_muted,
                    SOURCE_PICKER_TITLE_ICON_SIZE,
                ))
                .child("加载日志来源"),
        )
}

/// 渲染右侧顶部操作区；背景跟随浏览内容，不再形成独立的全宽色带。
fn render_browser_title_bar(
    theme: &AppTheme,
    close_app: &Entity<ArgusApp>,
) -> impl IntoElement + use<> {
    let close_app = close_app.clone();

    div()
        .h(px(SOURCE_PICKER_HEADER_HEIGHT))
        .flex_none()
        .px_5()
        .flex()
        .items_center()
        .justify_end()
        .bg(rgb(theme.content))
        .child(render_icon_button(
            "source-picker-window-close",
            ArgusIcon::Close,
            "关闭加载日志来源",
            false,
            IconButtonSize::Small,
            theme,
            move |_, _, cx| {
                update_picker_app(&close_app, cx, |app, _| app.close_source_picker());
            },
        ))
}

/// 渲染左侧常用位置列表。
fn render_locations(
    snapshot: &SourcePickerSnapshot,
    theme: &AppTheme,
    app_handle: &Entity<ArgusApp>,
) -> impl IntoElement + use<> {
    let locations = snapshot.source_picker.locations.clone();
    let current_dir = snapshot.source_picker.current_dir.clone();

    div()
        .w_full()
        .flex_1()
        .min_h(px(0.0))
        .flex()
        .flex_col()
        .gap_1()
        .px_4()
        .py(px(SOURCE_PICKER_CONTENT_PADDING))
        .bg(rgb(theme.side_bar))
        .child(
            div()
                .h(px(28.0))
                .flex()
                .items_center()
                .text_size(px(12.0))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(rgb(theme.foreground_muted))
                .child("常用位置"),
        )
        .children(locations.into_iter().map(|location| {
            let is_selected = current_dir == location.path;
            let label = location.label.clone();
            let path = location.path.clone();
            let app_handle = app_handle.clone();

            div()
                .id(SharedString::from(format!(
                    "source-picker-location-{}",
                    display_path(&path)
                )))
                .h(px(30.0))
                .w_full()
                .flex()
                .items_center()
                .gap_2()
                .px_2()
                .rounded_sm()
                .text_size(px(12.0))
                .text_color(rgb(if is_selected {
                    theme.foreground
                } else {
                    theme.foreground_muted
                }))
                .when(is_selected, |this| this.bg(rgb(theme.selection)))
                .hover(|this| this.bg(rgb(theme.current_line)))
                .cursor_pointer()
                .child(render_icon(ArgusIcon::Folder, theme.foreground_muted, 14.0))
                .child(div().flex_1().truncate().child(label))
                .on_click(move |_, _, cx| {
                    update_picker_app(&app_handle, cx, |app, app_cx| {
                        app.navigate_source_picker(path.clone(), app_cx);
                    });
                })
        }))
}

/// 渲染右侧浏览区域。
fn render_browser(
    snapshot: &SourcePickerSnapshot,
    theme: &AppTheme,
    app_handle: &Entity<ArgusApp>,
    path_focus_handle: FocusHandle,
    cx: &mut Context<SourcePickerWindow>,
) -> impl IntoElement + use<> {
    div()
        .flex_1()
        .min_h(px(0.0))
        .flex()
        .flex_col()
        .bg(rgb(theme.content))
        .child(render_header(
            snapshot,
            theme,
            app_handle,
            path_focus_handle,
        ))
        .child(render_entry_area(snapshot, theme, app_handle, cx))
        .child(render_footer(snapshot, theme, app_handle))
}

/// 渲染路径输入、返回上级和关闭按钮区域。
fn render_header(
    snapshot: &SourcePickerSnapshot,
    theme: &AppTheme,
    app_handle: &Entity<ArgusApp>,
    path_focus_handle: FocusHandle,
) -> impl IntoElement + use<> {
    let parent_dir = snapshot.source_picker.parent_dir.clone();
    let can_go_parent = parent_dir.is_some();
    let up_app = app_handle.clone();
    let input_key_app = app_handle.clone();
    let input_click_app = app_handle.clone();
    let input_pointer_app = app_handle.clone();
    let refresh_app = app_handle.clone();
    let refresh_dir = snapshot.source_picker.current_dir.clone();
    let native_input = app_native_input(
        app_handle.clone(),
        AppTextInputTarget::SourcePickerPath,
        path_focus_handle,
    );

    div()
        .h(px(48.0))
        .flex()
        .items_center()
        .gap_2()
        .px_4()
        .bg(rgb(theme.content))
        .child(picker_icon_button(
            "source-picker-up",
            ArgusIcon::ArrowLeft,
            "上级",
            can_go_parent,
            theme,
            move |_, _, cx| {
                if let Some(parent_dir) = parent_dir.clone() {
                    update_picker_app(&up_app, cx, |app, app_cx| {
                        app.navigate_source_picker(parent_dir, app_cx);
                    });
                }
            },
        ))
        .child(div().flex_1().min_w(px(0.0)).child(render_input(
            Input {
                id: "source-picker-path-input",
                placeholder: "输入目录路径后按 Enter 跳转",
                value: snapshot.source_picker.path_input.clone(),
                is_disabled: snapshot.source_picker.is_loading,
                is_focused: snapshot.source_picker.is_path_input_focused,
                cursor_index: snapshot.source_picker.path_input_cursor,
                selection_range: snapshot.source_picker.path_input_selection_range(),
                marked_range: snapshot.source_picker.path_input_marked_range.clone(),
                is_pointer_selecting: snapshot.source_picker.path_input_selection_drag.is_some(),
                is_secret: false,
                size: InputSize::Compact,
                leading_accessory: Some(InputAccessory {
                    id: "source-picker-path-leading",
                    icon: ArgusIcon::FolderOpen,
                    tooltip: "当前目录",
                }),
                trailing_accessory: None,
                native_input: Some(native_input),
            },
            theme,
            move |event: &KeyDownEvent, _, cx| {
                cx.stop_propagation();
                update_picker_app(&input_key_app, cx, |app, app_cx| {
                    app.handle_source_picker_path_key(&event.keystroke, app_cx);
                });
            },
            move |_, _, cx| {
                cx.stop_propagation();
                update_picker_app(&input_click_app, cx, |app, _| {
                    app.set_source_picker_path_input_focused(true);
                });
            },
            move |event: &InputPointerEvent, _, cx| {
                cx.stop_propagation();
                update_picker_app(&input_pointer_app, cx, |app, _| match event.action {
                    InputPointerAction::Begin => app.begin_source_picker_path_pointer_selection(
                        event.character_index,
                        event.granularity,
                    ),
                    InputPointerAction::Extend => {
                        app.update_source_picker_path_pointer_selection(event.character_index)
                    }
                    InputPointerAction::Finish => app.finish_source_picker_path_pointer_selection(),
                });
            },
            move |_, _, cx| {
                cx.stop_propagation();
            },
        )))
        .child(picker_icon_button(
            "source-picker-refresh",
            ArgusIcon::Refresh,
            "刷新当前目录",
            true,
            theme,
            move |_, _, cx| {
                update_picker_app(&refresh_app, cx, |app, app_cx| {
                    app.navigate_source_picker(refresh_dir.clone(), app_cx);
                });
            },
        ))
}

/// 渲染目录列表区域。
fn render_entry_area(
    snapshot: &SourcePickerSnapshot,
    theme: &AppTheme,
    app_handle: &Entity<ArgusApp>,
    cx: &mut Context<SourcePickerWindow>,
) -> impl IntoElement + use<> {
    let entry_count = snapshot.source_picker.entries.len();

    div()
        .relative()
        .flex_1()
        .overflow_hidden()
        .flex()
        .flex_col()
        .mx_4()
        .rounded_sm()
        .bg(rgb(theme.background))
        .when(snapshot.source_picker.is_loading, |this| {
            this.child(render_loading_state(theme))
        })
        .when(
            !snapshot.source_picker.is_loading && entry_count == 0,
            |this| this.child(render_empty_state(snapshot, theme)),
        )
        .when(
            !snapshot.source_picker.is_loading && entry_count > 0,
            |this| {
                this.child(render_entry_header(snapshot, theme, app_handle))
                    .child(
                        div().flex_1().min_h(px(0.0)).child(
                            uniform_list(
                                "source-picker-entry-list",
                                entry_count,
                                cx.processor(|picker, range: Range<usize>, _window, cx| {
                                    let app_handle = picker.app.clone();
                                    let app = app_handle.read(cx);
                                    let entries = app.source_picker.entries[range].to_vec();
                                    let selected_paths = app.source_picker.selected_paths.clone();
                                    let theme = app.theme.clone();

                                    entries
                                        .into_iter()
                                        .map(|entry| {
                                            let is_selected = selected_paths
                                                .iter()
                                                .any(|selected| selected == &entry.path);
                                            render_entry_row(
                                                entry,
                                                is_selected,
                                                &theme,
                                                &app_handle,
                                            )
                                            .into_any_element()
                                        })
                                        .collect::<Vec<_>>()
                                }),
                            )
                            .size_full()
                            .block_mouse_except_scroll()
                            .track_scroll(snapshot.source_picker.entry_scroll.clone()),
                        ),
                    )
            },
        )
}

/// 渲染目录列表表头，支持按名称和修改日期切换排序。
fn render_entry_header(
    snapshot: &SourcePickerSnapshot,
    theme: &AppTheme,
    app_handle: &Entity<ArgusApp>,
) -> impl IntoElement + use<> {
    div()
        .h(px(28.0))
        .flex_none()
        .px_2()
        .border_b_1()
        .border_color(rgb(theme.border))
        .text_size(px(12.0))
        .text_color(rgb(theme.foreground_muted))
        .child(
            div()
                .h_full()
                .w_full()
                .flex()
                .items_center()
                .gap_2()
                .px_2()
                .child(div().flex_1().min_w(px(0.0)).flex().items_center().child(
                    render_sort_header_cell(
                        "名称",
                        ArgusIcon::File,
                        SourcePickerSortKey::Name,
                        snapshot.source_picker.sort_key,
                        snapshot.source_picker.sort_direction,
                        theme,
                        app_handle,
                    ),
                ))
                .child(
                    div()
                        .w(px(SOURCE_PICKER_MODIFIED_WIDTH))
                        .flex_none()
                        .flex()
                        .items_center()
                        .child(render_sort_header_cell(
                            "修改日期",
                            ArgusIcon::Refresh,
                            SourcePickerSortKey::Modified,
                            snapshot.source_picker.sort_key,
                            snapshot.source_picker.sort_direction,
                            theme,
                            app_handle,
                        )),
                )
                .child(
                    div()
                        .w(px(SOURCE_PICKER_SIZE_WIDTH))
                        .flex_none()
                        .flex()
                        .items_center()
                        .child(render_static_header_cell(
                            "大小",
                            ArgusIcon::Database,
                            theme,
                        )),
                ),
        )
}

/// 渲染可点击排序表头单元格。
fn render_sort_header_cell(
    label: &'static str,
    icon: ArgusIcon,
    sort_key: SourcePickerSortKey,
    active_key: SourcePickerSortKey,
    direction: SourcePickerSortDirection,
    theme: &AppTheme,
    app_handle: &Entity<ArgusApp>,
) -> impl IntoElement + use<> {
    let is_active = active_key == sort_key;
    let indicator = if is_active {
        match direction {
            SourcePickerSortDirection::Ascending => " ↑",
            SourcePickerSortDirection::Descending => " ↓",
        }
    } else {
        ""
    };
    let id = match sort_key {
        SourcePickerSortKey::Name => "source-picker-sort-name",
        SourcePickerSortKey::Modified => "source-picker-sort-modified",
    };
    let app_handle = app_handle.clone();

    div()
        .id(id)
        .h(px(22.0))
        .flex()
        .items_center()
        .gap_1()
        .rounded_sm()
        .px_2()
        .line_height(px(18.0))
        .text_color(rgb(if is_active {
            theme.foreground
        } else {
            theme.foreground_muted
        }))
        .hover(|this| this.bg(rgb(theme.current_line)))
        .cursor_pointer()
        .on_click(move |_, _, cx| {
            update_picker_app(&app_handle, cx, |app, _| {
                app.set_source_picker_sort(sort_key);
            });
        })
        // 表头图标和文字从列起始处左对齐，避免名称列前出现额外空白。
        .child(header_icon(
            icon,
            if is_active {
                theme.foreground
            } else {
                theme.foreground_muted
            },
        ))
        .child(header_label_text(format!("{label}{indicator}")))
}

/// 渲染不可排序的静态表头单元格。
fn render_static_header_cell(
    label: &'static str,
    icon: ArgusIcon,
    theme: &AppTheme,
) -> impl IntoElement + use<> {
    div()
        .h(px(22.0))
        .flex()
        .items_center()
        .gap_1()
        .px_2()
        .line_height(px(18.0))
        .text_color(rgb(theme.foreground_muted))
        .child(header_icon(icon, theme.foreground_muted))
        .child(header_label_text(label.to_string()))
}

/// 渲染加载状态。
fn render_loading_state(theme: &AppTheme) -> impl IntoElement + use<> {
    div()
        .size_full()
        .flex()
        .items_center()
        .justify_center()
        .gap_2()
        .text_size(px(12.0))
        .text_color(rgb(theme.foreground_muted))
        .child(render_loading_spinner(
            ("source-picker-loading", 0),
            theme.foreground_muted,
            16.0,
        ))
        .child("正在读取目录...")
}

/// 渲染空目录或错误状态。
fn render_empty_state(
    snapshot: &SourcePickerSnapshot,
    theme: &AppTheme,
) -> impl IntoElement + use<> {
    let message = snapshot
        .source_picker
        .error_message
        .clone()
        .unwrap_or_else(|| "当前目录为空".to_string());

    div()
        .size_full()
        .flex()
        .flex_col()
        .items_center()
        .justify_center()
        .gap_2()
        .text_size(px(12.0))
        .text_color(rgb(theme.foreground_muted))
        .child(render_icon(
            ArgusIcon::FolderOpen,
            theme.foreground_muted,
            28.0,
        ))
        .child(message)
}

/// 渲染单个目录或文件行。
fn render_entry_row(
    entry: BrowseEntry,
    is_selected: bool,
    theme: &AppTheme,
    app_handle: &Entity<ArgusApp>,
) -> impl IntoElement + use<> {
    let path = entry.path.clone();
    let selected_path = path.clone();
    let is_directory = matches!(entry.kind, BrowseEntryKind::Directory);
    let icon = icon_for_entry(&entry.kind);
    let modified_text = entry_modified_text(&entry);
    let size_text = entry_size_text(&entry);
    let disabled_reason = entry.disabled_reason.clone();
    let row_id = format!("source-picker-entry-{}", display_path(&entry.path));
    let app_handle = app_handle.clone();

    div()
        .id(SharedString::from(row_id))
        .h(px(SOURCE_PICKER_ROW_HEIGHT))
        .w_full()
        .px_2()
        .child(
            div()
                .id(SharedString::from(format!(
                    "source-picker-entry-content-{}",
                    display_path(&entry.path)
                )))
                .h_full()
                .w_full()
                .flex()
                .items_center()
                .gap_2()
                .px_2()
                .rounded_sm()
                .text_size(px(12.0))
                .text_color(rgb(if entry.is_selectable || is_directory {
                    theme.foreground
                } else {
                    theme.foreground_muted
                }))
                .when(is_selected, |this| this.bg(rgb(theme.selection)))
                .hover(|this| this.bg(rgb(theme.current_line)))
                .cursor_pointer()
                .child(render_icon(icon, theme.foreground_muted, 15.0))
                .child(div().flex_1().min_w(px(0.0)).truncate().child(entry.name))
                .child(
                    div()
                        .w(px(SOURCE_PICKER_MODIFIED_WIDTH))
                        .flex_none()
                        .truncate()
                        .text_color(rgb(theme.foreground_muted))
                        .child(modified_text),
                )
                .child(
                    div()
                        .w(px(SOURCE_PICKER_SIZE_WIDTH))
                        .flex_none()
                        .text_right()
                        .text_color(rgb(theme.foreground_muted))
                        .child(size_text),
                )
                .when_some(disabled_reason, |this, reason| {
                    let tooltip_theme = theme.clone();
                    let tooltip_message = reason.clone();
                    this.tooltip(move |_, cx| {
                        let message = tooltip_message.clone();
                        let theme = tooltip_theme.clone();
                        cx.new(move |_| SourcePickerTooltip { message, theme })
                            .into()
                    })
                })
                .on_click(move |event, _, cx| {
                    update_picker_app(&app_handle, cx, |app, app_cx| {
                        if is_directory && event.click_count() >= 2 {
                            app.navigate_source_picker(path.clone(), app_cx);
                        } else if is_directory {
                            app.toggle_source_picker_directory(selected_path.clone());
                        } else {
                            app.toggle_source_picker_file(selected_path.clone());
                        }
                    });
                }),
        )
}

/// 选择器不可选条目 tooltip。
struct SourcePickerTooltip {
    /// 展示消息。
    message: String,
    /// 当前主题令牌。
    theme: AppTheme,
}

impl Render for SourcePickerTooltip {
    /// 渲染选择器 tooltip。
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .max_w(px(360.0))
            .px_2()
            .py_1()
            .rounded_sm()
            .bg(rgb(self.theme.title_bar))
            .border_1()
            .border_color(rgb(self.theme.border))
            .text_size(px(12.0))
            .text_color(rgb(self.theme.foreground))
            .child(self.message.clone())
    }
}

/// 渲染底部已选择路径和操作按钮。
fn render_footer(
    snapshot: &SourcePickerSnapshot,
    theme: &AppTheme,
    app_handle: &Entity<ArgusApp>,
) -> impl IntoElement + use<> {
    let selected_paths = snapshot.source_picker.selected_paths.clone();
    let selected_count = selected_paths.len();
    let error_message = snapshot.source_picker.error_message.clone();
    let clear_app = app_handle.clone();
    let cancel_app = app_handle.clone();
    let confirm_app = app_handle.clone();

    div()
        .h(px(108.0))
        .flex()
        .flex_col()
        .gap_2()
        .px_4()
        .py_3()
        .bg(rgb(theme.content))
        .child(
            div()
                .h(px(28.0))
                .flex()
                .items_center()
                .gap_2()
                .child(
                    div()
                        .text_size(px(12.0))
                        .text_color(rgb(theme.foreground_muted))
                        .child(if selected_count == 0 {
                            "未选择来源".to_string()
                        } else {
                            format!("已选择 {selected_count} 个来源")
                        }),
                )
                .children(selected_paths.iter().take(3).map(|path| {
                    render_selected_chip(path.clone(), theme, app_handle).into_any_element()
                }))
                .when(selected_count > 3, |this| {
                    this.child(
                        div()
                            .text_size(px(12.0))
                            .text_color(rgb(theme.foreground_muted))
                            .child(format!("+{} 个", selected_count - 3)),
                    )
                }),
        )
        .when_some(error_message, |this, message| {
            this.child(
                div()
                    .h(px(18.0))
                    .text_size(px(12.0))
                    .text_color(rgb(theme.error))
                    .truncate()
                    .child(message),
            )
        })
        .child(
            div()
                .flex()
                .items_center()
                .justify_between()
                .child(div().flex().items_center().gap_2().child(text_button(
                    "source-picker-clear",
                    ArgusIcon::Close,
                    "清空选择",
                    theme,
                    move |_, _, cx| {
                        update_picker_app(&clear_app, cx, |app, _| {
                            app.clear_source_picker_selection();
                        });
                    },
                )))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .child(text_button(
                            "source-picker-cancel",
                            ArgusIcon::Close,
                            "取消",
                            theme,
                            move |_, _, cx| {
                                update_picker_app(&cancel_app, cx, |app, _| {
                                    app.close_source_picker();
                                });
                            },
                        ))
                        .child(primary_button(
                            "source-picker-confirm",
                            ArgusIcon::FolderPlus,
                            "加载",
                            selected_count > 0,
                            theme,
                            move |_, _, cx| {
                                confirm_app.update(cx, |app, app_cx| {
                                    app.confirm_source_picker_selection(app_cx);
                                    app_cx.notify();
                                });
                            },
                        )),
                ),
        )
}

/// 渲染一个已选择路径 chip，点击可移除。
fn render_selected_chip(
    path: PathBuf,
    theme: &AppTheme,
    app_handle: &Entity<ArgusApp>,
) -> impl IntoElement + use<> {
    let label = display_name(&path);
    let remove_path = path.clone();
    let app_handle = app_handle.clone();

    div()
        .id(SharedString::from(format!(
            "source-picker-selected-chip-{}",
            display_path(&path)
        )))
        .max_w(px(150.0))
        .h(px(24.0))
        .flex()
        .items_center()
        .gap_1()
        .px_2()
        .rounded_sm()
        .bg(rgb(theme.current_line))
        .text_size(px(12.0))
        .text_color(rgb(theme.foreground))
        .cursor_pointer()
        .child(div().truncate().child(label))
        .child(render_icon(ArgusIcon::Close, theme.foreground_muted, 12.0))
        .on_click(move |_, _, cx| {
            update_picker_app(&app_handle, cx, |app, _| {
                app.remove_source_picker_path(&remove_path);
            });
        })
}

/// 根据文件系统条目类型选择图标。
fn icon_for_entry(kind: &BrowseEntryKind) -> ArgusIcon {
    match kind {
        BrowseEntryKind::Directory => ArgusIcon::Folder,
        BrowseEntryKind::LogFile => ArgusIcon::FileText,
        BrowseEntryKind::Archive(_) => ArgusIcon::Archive,
        BrowseEntryKind::UnsupportedArchive(_) | BrowseEntryKind::OtherUnsupported => {
            ArgusIcon::File
        }
    }
}

/// 返回文件系统条目的修改日期展示文本。
fn entry_modified_text(entry: &BrowseEntry) -> String {
    entry
        .modified
        .map(format_modified_time)
        .unwrap_or_else(|| "-".to_string())
}

/// 返回文件系统条目的大小或类型展示文本。
fn entry_size_text(entry: &BrowseEntry) -> String {
    match entry.kind {
        BrowseEntryKind::Directory => "目录".to_string(),
        BrowseEntryKind::LogFile | BrowseEntryKind::Archive(_) => {
            entry.size.map(format_bytes).unwrap_or_default()
        }
        BrowseEntryKind::UnsupportedArchive(_) | BrowseEntryKind::OtherUnsupported => {
            entry.kind.label()
        }
    }
}

/// 渲染选择器小型图标按钮。
fn picker_icon_button<F>(
    id: &'static str,
    icon: ArgusIcon,
    _tooltip: &'static str,
    is_enabled: bool,
    theme: &AppTheme,
    on_click: F,
) -> impl IntoElement + use<F>
where
    F: Fn(&ClickEvent, &mut Window, &mut App) + 'static,
{
    div()
        .id(id)
        .w(px(28.0))
        .h(px(28.0))
        .flex()
        .items_center()
        .justify_center()
        .rounded_sm()
        .text_color(rgb(theme.foreground_muted))
        .when(is_enabled, move |this| {
            this.cursor_pointer()
                .hover(|this| this.bg(rgb(theme.current_line)))
                .on_click(on_click)
        })
        .when(!is_enabled, |this| this.opacity(0.45))
        .child(
            div()
                .relative()
                .top(px(SOURCE_PICKER_BUTTON_CONTENT_Y_OFFSET))
                .child(render_icon(icon, theme.foreground_muted, 16.0)),
        )
}

/// 渲染普通文本按钮。
fn text_button<F>(
    id: &'static str,
    icon: ArgusIcon,
    label: &'static str,
    theme: &AppTheme,
    on_click: F,
) -> impl IntoElement + use<F>
where
    F: Fn(&ClickEvent, &mut Window, &mut App) + 'static,
{
    div()
        .id(id)
        .h(px(30.0))
        .px_3()
        .flex()
        .items_center()
        .justify_center()
        .gap_1()
        .rounded_sm()
        .text_size(px(12.0))
        .line_height(px(30.0))
        .text_color(rgb(theme.foreground))
        .bg(rgb(theme.current_line))
        .hover(|this| this.bg(rgb(theme.selection)))
        .cursor_pointer()
        .on_click(on_click)
        .child(button_icon(icon, theme.foreground_muted, 13.0))
        .child(button_label(label))
}

/// 渲染主操作按钮。
fn primary_button<F>(
    id: &'static str,
    icon: ArgusIcon,
    label: &'static str,
    is_enabled: bool,
    theme: &AppTheme,
    on_click: F,
) -> impl IntoElement + use<F>
where
    F: Fn(&ClickEvent, &mut Window, &mut App) + 'static,
{
    div()
        .id(id)
        .h(px(30.0))
        .px_4()
        .flex()
        .items_center()
        .justify_center()
        .gap_1()
        .rounded_sm()
        .text_size(px(12.0))
        .line_height(px(30.0))
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(rgb(theme.foreground))
        .bg(rgb(if is_enabled {
            theme.selection
        } else {
            theme.current_line
        }))
        .when(is_enabled, move |this| {
            this.cursor_pointer()
                .hover(|this| this.opacity(0.9))
                .on_click(on_click)
        })
        .when(!is_enabled, |this| this.opacity(0.55))
        .child(button_icon(
            icon,
            if is_enabled {
                theme.foreground
            } else {
                theme.foreground_muted
            },
            13.0,
        ))
        .child(button_label(label))
}

/// 渲染选择器按钮中的图标内容，统一修正视觉垂直居中。
fn button_icon(icon: ArgusIcon, color: u32, size: f32) -> impl IntoElement {
    div()
        .relative()
        .top(px(SOURCE_PICKER_BUTTON_CONTENT_Y_OFFSET))
        .child(render_icon(icon, color, size))
}

/// 渲染选择器按钮中的文字内容，统一修正视觉垂直居中。
fn button_label(label: &'static str) -> impl IntoElement {
    div()
        .relative()
        .top(px(SOURCE_PICKER_BUTTON_CONTENT_Y_OFFSET))
        .child(label)
}

/// 渲染表头文字内容，和按钮文字使用同一套视觉垂直居中修正。
fn header_label_text(label: String) -> impl IntoElement {
    div()
        .relative()
        .top(px(SOURCE_PICKER_HEADER_CONTENT_Y_OFFSET))
        .child(label)
}

/// 渲染表头图标内容，和表头文字使用同一套视觉垂直居中修正。
fn header_icon(icon: ArgusIcon, color: u32) -> impl IntoElement {
    div()
        .relative()
        .top(px(SOURCE_PICKER_HEADER_CONTENT_Y_OFFSET))
        .child(render_icon(icon, color, SOURCE_PICKER_HEADER_ICON_SIZE))
}

/// 统一更新主应用状态；选择器窗口只负责表现，不直接保存业务状态。
fn update_picker_app(
    app_handle: &Entity<ArgusApp>,
    cx: &mut App,
    update: impl FnOnce(&mut ArgusApp, &mut Context<ArgusApp>),
) {
    let _ = app_handle.update(cx, |app, app_cx| {
        update(app, app_cx);
        app_cx.notify();
    });
}
