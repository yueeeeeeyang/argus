//! 文件职责：渲染 SFTP、SMB、Git、SVN 通用远程文件管理标签页。
//! 创建日期：2026-06-26
//! 修改日期：2026-07-15
//! 作者：Argus 开发团队
//! 主要功能：展示通用远程目录、仓库版本控件、协议能力工具栏和虚拟化文件列表。

use std::ops::Range;
use std::sync::Arc;

use chrono::{Local, TimeZone};
use gpui::{
    Context, Entity, IntoElement, KeyDownEvent, MouseButton, MouseDownEvent, SharedString, Window,
    div, prelude::*, px, rgb, uniform_list,
};

use crate::app::{AppTextInputTarget, ArgusApp};
use crate::infra::perf::PerfSpan;
use crate::remote::remote_file::{
    RemoteFileBackend, RemoteFileEntry, RemoteFileEntryKind, RemoteFileSessionState,
    RemoteFileSortDirection, RemoteFileSortField, RemoteFileStatus,
};
use crate::theme::AppTheme;
use crate::ui::components::icon::{ArgusIcon, render_icon};
use crate::ui::components::input::{
    Input, InputAccessory, InputPointerAction, InputPointerEvent, InputSize, render_input,
};
use crate::ui::input_native::app_native_input;
use crate::utils::size_format::format_bytes;

/// 远程文件管理工具栏高度。
const REMOTE_FILE_TOOLBAR_HEIGHT: f32 = 44.0;
/// 远程文件管理表头高度。
const REMOTE_FILE_TABLE_HEADER_HEIGHT: f32 = 32.0;
/// 远程文件行高度。
const REMOTE_FILE_ROW_HEIGHT: f32 = 30.0;
/// 文件类型列宽。
const REMOTE_FILE_TYPE_COLUMN_WIDTH: f32 = 76.0;
/// 文件大小列宽。
const REMOTE_FILE_SIZE_COLUMN_WIDTH: f32 = 112.0;
/// 修改时间列宽。
const REMOTE_FILE_MTIME_COLUMN_WIDTH: f32 = 168.0;
/// 权限列宽。
const REMOTE_FILE_PERM_COLUMN_WIDTH: f32 = 96.0;

/// 渲染远程文件管理页面。
pub(crate) fn render(
    app: &ArgusApp,
    session_id: usize,
    cx: &mut Context<ArgusApp>,
) -> impl IntoElement {
    let _span = PerfSpan::new("render_remote_file_manager");
    let theme = app.theme.clone();
    let Some(session) = app.remote_file_sessions.get(&session_id) else {
        return div()
            .size_full()
            .flex()
            .items_center()
            .justify_center()
            .text_color(rgb(theme.foreground_muted))
            .child("文件管理会话不存在")
            .into_any_element();
    };
    let entries = session.sorted_entries.clone();
    let sort_field = session.sort_field;
    let sort_direction = session.sort_direction;

    div()
        .id("remote-file-manager-view")
        .size_full()
        .flex()
        .flex_col()
        .bg(rgb(theme.content))
        .child(render_toolbar(app, session, &theme, cx))
        .child(render_status_line(session, &theme))
        .child(render_table_header(
            session.id,
            sort_field,
            sort_direction,
            &theme,
            cx,
        ))
        .child(
            div()
                .flex_1()
                .min_h(px(0.0))
                .overflow_hidden()
                .child(if entries.is_empty() {
                    render_empty_state(session, &theme).into_any_element()
                } else {
                    render_file_list(session, entries, cx).into_any_element()
                }),
        )
        .into_any_element()
}

/// 渲染顶部工具栏和地址栏。
fn render_toolbar(
    app: &ArgusApp,
    session: &RemoteFileSessionState,
    theme: &AppTheme,
    cx: &mut Context<ArgusApp>,
) -> impl IntoElement {
    let session_id = session.id;
    let target = AppTextInputTarget::RemoteFileAddress { session_id };
    let app_entity = cx.entity();
    let focus_handle = app
        .input_focus_handles
        .as_ref()
        .map(|handles| handles.remote_file_address.clone());
    let native_input = focus_handle
        .clone()
        .map(|focus_handle| app_native_input(app_entity.clone(), target, focus_handle));
    let address_input = session.address_input.clone();
    let key_app_entity = app_entity.clone();
    let click_app_entity = app_entity.clone();
    let pointer_app_entity = app_entity.clone();

    div()
        .h(px(REMOTE_FILE_TOOLBAR_HEIGHT))
        .w_full()
        .flex()
        .items_center()
        .gap_2()
        .px_3()
        .border_b_1()
        .border_color(rgb(theme.border))
        .child(toolbar_button(
            app_entity.clone(),
            "remote-file-parent-dir",
            ArgusIcon::ArrowUp,
            "上级目录",
            session.capabilities.browse && session.status == RemoteFileStatus::Connected,
            theme,
            move |app, _| app.open_remote_file_parent_directory(session_id),
        ))
        .child(toolbar_button(
            app_entity.clone(),
            "remote-file-refresh",
            ArgusIcon::Refresh,
            "刷新",
            session.capabilities.browse && session.status == RemoteFileStatus::Connected,
            theme,
            move |app, _| app.refresh_remote_file_directory(session_id),
        ))
        .when(session.backend == RemoteFileBackend::Git, |this| {
            this.child(render_git_version_selector(session, theme, cx))
        })
        .when(session.backend == RemoteFileBackend::Svn, |this| {
            this.child(render_svn_version_input(app, session, theme, cx))
        })
        .child(div().flex_1().child(render_input(
            Input {
                id: "remote-file-address-input",
                placeholder: "输入远程目录路径",
                value: address_input.value.clone(),
                is_disabled: !matches!(session.status, RemoteFileStatus::Connected),
                is_focused: address_input.is_focused,
                cursor_index: address_input.cursor,
                selection_range: app.remote_file_input_selection_range(target),
                marked_range: address_input.marked_range.clone(),
                is_pointer_selecting: address_input.selection_drag.is_some(),
                is_secret: false,
                size: InputSize::Regular,
                leading_accessory: Some(InputAccessory {
                    id: "remote-file-address-leading",
                    icon: ArgusIcon::FolderOpen,
                    tooltip: "远程目录",
                }),
                trailing_accessory: None,
                native_input,
            },
            theme,
            move |event: &KeyDownEvent, _, cx| {
                cx.stop_propagation();
                key_app_entity.update(cx, |app, app_cx| {
                    app.handle_remote_file_text_input_key(target, &event.keystroke);
                    app_cx.notify();
                });
            },
            move |_, window, cx| {
                cx.stop_propagation();
                if let Some(focus_handle) = focus_handle.as_ref() {
                    focus_handle.focus(window);
                }
                click_app_entity.update(cx, |app, app_cx| {
                    app.focus_remote_file_text_input_target(target);
                    app_cx.notify();
                });
            },
            move |event: &InputPointerEvent, _, cx| {
                cx.stop_propagation();
                pointer_app_entity.update(cx, |app, app_cx| {
                    match event.action {
                        InputPointerAction::Begin => app.begin_remote_file_input_pointer_selection(
                            target,
                            event.character_index,
                            event.granularity,
                        ),
                        InputPointerAction::Extend => app
                            .update_remote_file_input_pointer_selection(
                                target,
                                event.character_index,
                            ),
                        InputPointerAction::Finish => {
                            app.finish_remote_file_input_pointer_selection(target)
                        }
                    }
                    app_cx.notify();
                });
            },
            move |_, _, _| {},
        )))
        .when(session.capabilities.upload, |this| {
            this.child(toolbar_button(
                app_entity.clone(),
                "remote-file-upload",
                ArgusIcon::Upload,
                "上传文件",
                session.status == RemoteFileStatus::Connected,
                theme,
                move |app, cx| app.choose_remote_file_upload_files(session_id, cx),
            ))
        })
        .child(toolbar_button(
            app_entity.clone(),
            "remote-file-download",
            ArgusIcon::Download,
            "下载文件",
            app.can_download_remote_file_selection(session_id),
            theme,
            move |app, cx| app.choose_remote_file_download_target(session_id, cx),
        ))
        .when(session.capabilities.rename, |this| {
            this.child(toolbar_button(
                app_entity.clone(),
                "remote-file-rename",
                ArgusIcon::Rename,
                "重命名",
                app.can_rename_remote_file_selection(session_id),
                theme,
                move |app, _| app.open_remote_file_rename_dialog(session_id),
            ))
        })
        .when(session.capabilities.delete, |this| {
            this.child(toolbar_button(
                app_entity,
                "remote-file-delete",
                ArgusIcon::Trash,
                "删除",
                app.can_delete_remote_file_selection(session_id),
                theme,
                move |app, _| app.request_delete_remote_file_entry(session_id),
            ))
        })
}

/// 渲染 Git 分支/标签选择器；菜单项由版本类型分组排序并标记当前引用。
fn render_git_version_selector(
    session: &RemoteFileSessionState,
    theme: &AppTheme,
    cx: &mut Context<ArgusApp>,
) -> impl IntoElement {
    let session_id = session.id;
    let label = session.version_input.value.clone();
    div()
        .id("git-repository-version-selector")
        .h(px(28.0))
        .w(px(210.0))
        .px_2()
        .flex()
        .items_center()
        .gap_2()
        .rounded_sm()
        .border_1()
        .border_color(rgb(theme.border))
        .text_sm()
        .text_color(rgb(theme.foreground))
        .cursor_pointer()
        .hover(|this| this.bg(rgb(theme.current_line)))
        .child(render_icon(
            ArgusIcon::GitBranch,
            theme.foreground_muted,
            15.0,
        ))
        .child(
            div()
                .flex_1()
                .overflow_hidden()
                .whitespace_nowrap()
                .child(if label.is_empty() {
                    "加载版本中...".to_string()
                } else {
                    label
                }),
        )
        .child(render_icon(
            ArgusIcon::Collapse,
            theme.foreground_muted,
            13.0,
        ))
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(move |app, event: &MouseDownEvent, _, cx| {
                cx.stop_propagation();
                app.open_repository_version_menu(session_id, event.position);
                cx.notify();
            }),
        )
}

/// 渲染 SVN HEAD/数字修订输入框，按 Enter 后由 worker 校验并原子切换版本。
fn render_svn_version_input(
    app: &ArgusApp,
    session: &RemoteFileSessionState,
    theme: &AppTheme,
    cx: &mut Context<ArgusApp>,
) -> impl IntoElement {
    let session_id = session.id;
    let target = AppTextInputTarget::RemoteFileVersion { session_id };
    let app_entity = cx.entity();
    let focus_handle = app
        .input_focus_handles
        .as_ref()
        .map(|handles| handles.remote_file_address.clone());
    let native_input = focus_handle
        .clone()
        .map(|focus_handle| app_native_input(app_entity.clone(), target, focus_handle));
    let version_input = session.version_input.clone();
    let key_app_entity = app_entity.clone();
    let click_app_entity = app_entity.clone();
    let pointer_app_entity = app_entity;

    div().w(px(178.0)).child(render_input(
        Input {
            id: "svn-repository-version-input",
            placeholder: "HEAD 或修订号",
            value: version_input.value,
            is_disabled: session.status != RemoteFileStatus::Connected,
            is_focused: version_input.is_focused,
            cursor_index: version_input.cursor,
            selection_range: app.remote_file_input_selection_range(target),
            marked_range: version_input.marked_range,
            is_pointer_selecting: version_input.selection_drag.is_some(),
            is_secret: false,
            size: InputSize::Regular,
            leading_accessory: Some(InputAccessory {
                id: "svn-version-leading",
                icon: ArgusIcon::History,
                tooltip: "输入 HEAD 或非负修订号后按 Enter",
            }),
            trailing_accessory: None,
            native_input,
        },
        theme,
        move |event: &KeyDownEvent, _, cx| {
            cx.stop_propagation();
            key_app_entity.update(cx, |app, app_cx| {
                app.handle_remote_file_text_input_key(target, &event.keystroke);
                app_cx.notify();
            });
        },
        move |_, window, cx| {
            cx.stop_propagation();
            if let Some(focus_handle) = focus_handle.as_ref() {
                focus_handle.focus(window);
            }
            click_app_entity.update(cx, |app, app_cx| {
                app.focus_remote_file_text_input_target(target);
                app_cx.notify();
            });
        },
        move |event: &InputPointerEvent, _, cx| {
            cx.stop_propagation();
            pointer_app_entity.update(cx, |app, app_cx| {
                match event.action {
                    InputPointerAction::Begin => app.begin_remote_file_input_pointer_selection(
                        target,
                        event.character_index,
                        event.granularity,
                    ),
                    InputPointerAction::Extend => app
                        .update_remote_file_input_pointer_selection(target, event.character_index),
                    InputPointerAction::Finish => {
                        app.finish_remote_file_input_pointer_selection(target)
                    }
                }
                app_cx.notify();
            });
        },
        move |_, _, _| {},
    ))
}

/// 渲染工具栏图标按钮。
fn toolbar_button(
    app_entity: Entity<ArgusApp>,
    id: &'static str,
    icon: ArgusIcon,
    tooltip: &'static str,
    enabled: bool,
    theme: &AppTheme,
    on_click: impl Fn(&mut ArgusApp, &mut Context<ArgusApp>) + 'static,
) -> impl IntoElement {
    let foreground = if enabled {
        theme.foreground_muted
    } else {
        theme.border
    };
    let hover_background = theme.current_line;

    div()
        .id(id)
        .w(px(28.0))
        .h(px(28.0))
        .flex()
        .items_center()
        .justify_center()
        .rounded_sm()
        .cursor_pointer()
        .hover(move |this| this.bg(rgb(hover_background)))
        .tooltip(move |_, cx| {
            cx.new(|_| RemoteFileToolbarTooltip {
                label: tooltip.to_string(),
            })
            .into()
        })
        .child(render_icon(icon, foreground, 16.0))
        .on_click(move |_, _, cx| {
            cx.stop_propagation();
            app_entity.update(cx, |app, app_cx| {
                if enabled {
                    on_click(app, app_cx);
                } else {
                    app.placeholder_notice = format!("{tooltip} 当前不可用");
                }
                app_cx.notify();
            });
        })
}

/// 远程文件管理工具栏 tooltip。
struct RemoteFileToolbarTooltip {
    /// 提示文案。
    label: String,
}

impl gpui::Render for RemoteFileToolbarTooltip {
    /// 渲染紧凑提示气泡。
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .px_2()
            .py_1()
            .rounded_sm()
            .text_xs()
            .child(self.label.clone())
    }
}

/// 渲染远程文件管理状态行。
fn render_status_line(session: &RemoteFileSessionState, theme: &AppTheme) -> impl IntoElement {
    div()
        .h(px(28.0))
        .w_full()
        .flex()
        .items_center()
        .justify_between()
        .px_3()
        .text_size(px(12.0))
        .text_color(rgb(theme.foreground_muted))
        .child(
            session
                .message
                .clone()
                .unwrap_or_else(|| status_label(&session.status).to_string()),
        )
        .child(status_label(&session.status))
}

/// 渲染文件列表表头。
fn render_table_header(
    session_id: usize,
    sort_field: RemoteFileSortField,
    sort_direction: RemoteFileSortDirection,
    theme: &AppTheme,
    cx: &mut Context<ArgusApp>,
) -> impl IntoElement {
    div()
        .h(px(REMOTE_FILE_TABLE_HEADER_HEIGHT))
        .w_full()
        .flex()
        .items_center()
        .border_b_1()
        .border_color(rgb(theme.border))
        .bg(rgb(theme.side_bar))
        .text_size(px(12.0))
        .text_color(rgb(theme.foreground_muted))
        .child(header_cell(
            "名称",
            None,
            RemoteFileSortField::Name,
            session_id,
            sort_field,
            sort_direction,
            theme,
            cx,
        ))
        .child(header_cell(
            "类型",
            Some(REMOTE_FILE_TYPE_COLUMN_WIDTH),
            RemoteFileSortField::Type,
            session_id,
            sort_field,
            sort_direction,
            theme,
            cx,
        ))
        .child(header_cell(
            "大小",
            Some(REMOTE_FILE_SIZE_COLUMN_WIDTH),
            RemoteFileSortField::Size,
            session_id,
            sort_field,
            sort_direction,
            theme,
            cx,
        ))
        .child(header_cell(
            "修改时间",
            Some(REMOTE_FILE_MTIME_COLUMN_WIDTH),
            RemoteFileSortField::Mtime,
            session_id,
            sort_field,
            sort_direction,
            theme,
            cx,
        ))
        .child(header_cell(
            "权限",
            Some(REMOTE_FILE_PERM_COLUMN_WIDTH),
            RemoteFileSortField::Permissions,
            session_id,
            sort_field,
            sort_direction,
            theme,
            cx,
        ))
}

/// 渲染表头单元格；点击切换排序，活动列展示方向箭头。
#[allow(clippy::too_many_arguments)]
fn header_cell(
    label: &'static str,
    width: Option<f32>,
    field: RemoteFileSortField,
    session_id: usize,
    sort_field: RemoteFileSortField,
    sort_direction: RemoteFileSortDirection,
    theme: &AppTheme,
    cx: &mut Context<ArgusApp>,
) -> impl IntoElement {
    let is_active = sort_field == field;
    let arrow = if is_active {
        match sort_direction {
            RemoteFileSortDirection::Asc => " ↑",
            RemoteFileSortDirection::Desc => " ↓",
        }
    } else {
        ""
    };
    let label_text = SharedString::from(format!("{label}{arrow}"));
    div()
        .id(SharedString::from(format!(
            "remote-file-header-{session_id}-{:?}",
            field
        )))
        .when_some(width, |this, width| this.w(px(width)).flex_none())
        .when(width.is_none(), |this| this.flex_1().min_w(px(0.0)))
        .h_full()
        .flex()
        .items_center()
        .px_3()
        .cursor_pointer()
        .hover(|this| this.bg(rgb(theme.current_line)))
        .text_color(rgb(if is_active {
            theme.foreground
        } else {
            theme.foreground_muted
        }))
        .child(label_text)
        .on_click(cx.listener(move |app, _, _, cx| {
            app.set_remote_file_sort(session_id, field);
            cx.notify();
        }))
}

/// 渲染远程文件列表。
fn render_file_list(
    session: &RemoteFileSessionState,
    entries: Arc<Vec<RemoteFileEntry>>,
    cx: &mut Context<ArgusApp>,
) -> impl IntoElement {
    let session_id = session.id;
    let selected_paths = session.selected_paths.clone();
    let row_count = entries.len();
    uniform_list(
        "remote-file-file-list",
        row_count,
        cx.processor(move |app, range: Range<usize>, _, cx| {
            let theme = app.theme.clone();
            entries[range]
                .iter()
                .cloned()
                .map(|entry| {
                    render_file_row(session_id, entry, selected_paths.clone(), &theme, cx)
                        .into_any_element()
                })
                .collect::<Vec<_>>()
        }),
    )
    .size_full()
    .track_scroll(session.list_scroll.clone())
}

/// 渲染单个远程文件行。
fn render_file_row(
    session_id: usize,
    entry: RemoteFileEntry,
    selected_paths: std::collections::BTreeSet<String>,
    theme: &AppTheme,
    cx: &mut Context<ArgusApp>,
) -> impl IntoElement {
    let is_selected = selected_paths.contains(&entry.path);
    let path_for_click = entry.path.clone();
    let path_for_double_click = entry.path.clone();
    let path_for_context = entry.path.clone();
    let icon = match entry.kind {
        RemoteFileEntryKind::Directory => ArgusIcon::Folder,
        RemoteFileEntryKind::RegularFile => ArgusIcon::File,
        RemoteFileEntryKind::Symlink => ArgusIcon::Link,
        RemoteFileEntryKind::Other => ArgusIcon::FileText,
    };
    let background = if is_selected {
        theme.selection
    } else {
        theme.content
    };
    let hover_background = if is_selected {
        theme.selection
    } else {
        theme.current_line
    };

    div()
        .id(SharedString::from(format!(
            "remote-file-entry-{session_id}-{}",
            entry.path
        )))
        .h(px(REMOTE_FILE_ROW_HEIGHT))
        .w_full()
        .flex()
        .items_center()
        .bg(rgb(background))
        .hover(move |this| this.bg(rgb(hover_background)))
        .text_size(px(12.5))
        .text_color(rgb(theme.foreground))
        .cursor_pointer()
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(move |app, event: &MouseDownEvent, _, cx| {
                cx.stop_propagation();
                app.select_remote_file_entry(
                    session_id,
                    path_for_click.clone(),
                    event.modifiers.shift || event.modifiers.secondary(),
                );
                if event.click_count >= 2 {
                    app.handle_remote_file_entry_double_click(
                        session_id,
                        path_for_double_click.clone(),
                    );
                }
                cx.notify();
            }),
        )
        .on_mouse_down(
            MouseButton::Right,
            cx.listener(move |app, event: &MouseDownEvent, _, cx| {
                cx.stop_propagation();
                app.open_remote_file_entry_context_menu(
                    session_id,
                    path_for_context.clone(),
                    event.position,
                );
                cx.notify();
            }),
        )
        .child(
            div()
                .flex_1()
                .min_w(px(0.0))
                .px_3()
                .flex()
                .items_center()
                .gap_2()
                .child(render_icon(icon, theme.foreground_muted, 14.0))
                .child(div().truncate().child(entry.name.clone())),
        )
        .child(row_cell(
            entry.kind.label().to_string(),
            REMOTE_FILE_TYPE_COLUMN_WIDTH,
            theme,
        ))
        .child(row_cell(
            format_entry_size(&entry),
            REMOTE_FILE_SIZE_COLUMN_WIDTH,
            theme,
        ))
        .child(row_cell(
            format_mtime(entry.mtime),
            REMOTE_FILE_MTIME_COLUMN_WIDTH,
            theme,
        ))
        .child(row_cell(
            format_permissions(entry.permissions),
            REMOTE_FILE_PERM_COLUMN_WIDTH,
            theme,
        ))
}

/// 渲染表格普通单元格。
fn row_cell(text: String, width: f32, theme: &AppTheme) -> impl IntoElement {
    div()
        .w(px(width))
        .flex_none()
        .px_3()
        .truncate()
        .text_color(rgb(theme.foreground_muted))
        .child(text)
}

/// 渲染空目录或未连接状态。
fn render_empty_state(session: &RemoteFileSessionState, theme: &AppTheme) -> impl IntoElement {
    div()
        .size_full()
        .flex()
        .items_center()
        .justify_center()
        .text_size(px(13.0))
        .text_color(rgb(theme.foreground_muted))
        .child(if session.status == RemoteFileStatus::Connected {
            "当前目录为空".to_string()
        } else {
            session
                .message
                .clone()
                .unwrap_or_else(|| status_label(&session.status).to_string())
        })
}

/// 返回状态展示文案。
fn status_label(status: &RemoteFileStatus) -> &'static str {
    match status {
        RemoteFileStatus::Connecting => "连接中",
        RemoteFileStatus::AwaitingHostKey => "等待确认指纹",
        RemoteFileStatus::Loading => "加载中",
        RemoteFileStatus::Connected => "已连接",
        RemoteFileStatus::Transferring => "传输中",
        RemoteFileStatus::Disconnected => "已断开",
        RemoteFileStatus::Failed => "连接失败",
    }
}

/// 格式化文件大小。
fn format_entry_size(entry: &RemoteFileEntry) -> String {
    if entry.kind == RemoteFileEntryKind::Directory {
        "-".to_string()
    } else {
        entry
            .size
            .map(format_bytes)
            .unwrap_or_else(|| "-".to_string())
    }
}

/// 格式化 Unix 修改时间。
fn format_mtime(mtime: Option<u64>) -> String {
    let Some(mtime) = mtime else {
        return "-".to_string();
    };
    Local
        .timestamp_opt(mtime as i64, 0)
        .single()
        .map(|time| time.format("%Y-%m-%d %H:%M").to_string())
        .unwrap_or_else(|| "-".to_string())
}

/// 格式化 Unix 权限为三位八进制。
fn format_permissions(permissions: Option<u32>) -> String {
    permissions
        .map(|permissions| format!("{:03o}", permissions & 0o777))
        .unwrap_or_else(|| "-".to_string())
}
