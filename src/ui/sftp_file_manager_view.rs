//! 文件职责：渲染远程文件管理标签页。
//! 创建日期：2026-06-26
//! 修改日期：2026-06-26
//! 作者：Argus 开发团队
//! 主要功能：展示远程目录地址栏、文件操作工具栏和虚拟化文件列表。

use std::ops::Range;
use std::sync::Arc;

use chrono::{Local, TimeZone};
use gpui::{
    Context, Entity, IntoElement, KeyDownEvent, MouseButton, MouseDownEvent, SharedString, Window,
    div, prelude::*, px, rgb, uniform_list,
};

use crate::app::{AppTextInputTarget, ArgusApp};
use crate::infra::perf::PerfSpan;
use crate::remote::sftp::{
    SftpEntry, SftpEntryKind, SftpSessionState, SftpSortDirection, SftpSortField, SftpStatus,
};
use crate::theme::AppTheme;
use crate::ui::components::icon::{ArgusIcon, render_icon};
use crate::ui::components::input::{
    Input, InputAccessory, InputPointerAction, InputPointerEvent, InputSize, render_input,
};
use crate::ui::input_native::app_native_input;
use crate::utils::size_format::format_bytes;

/// 远程文件管理工具栏高度。
const SFTP_TOOLBAR_HEIGHT: f32 = 44.0;
/// 远程文件管理表头高度。
const SFTP_TABLE_HEADER_HEIGHT: f32 = 32.0;
/// 远程文件行高度。
const SFTP_ROW_HEIGHT: f32 = 30.0;
/// 文件类型列宽。
const SFTP_TYPE_COLUMN_WIDTH: f32 = 76.0;
/// 文件大小列宽。
const SFTP_SIZE_COLUMN_WIDTH: f32 = 112.0;
/// 修改时间列宽。
const SFTP_MTIME_COLUMN_WIDTH: f32 = 168.0;
/// 权限列宽。
const SFTP_PERM_COLUMN_WIDTH: f32 = 96.0;

/// 渲染远程文件管理页面。
pub fn render(app: &ArgusApp, session_id: usize, cx: &mut Context<ArgusApp>) -> impl IntoElement {
    let _span = PerfSpan::new("render_sftp_file_manager");
    let theme = app.theme.clone();
    let Some(session) = app.sftp_sessions.get(&session_id) else {
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
        .id("sftp-file-manager-view")
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
    session: &SftpSessionState,
    theme: &AppTheme,
    cx: &mut Context<ArgusApp>,
) -> impl IntoElement {
    let session_id = session.id;
    let target = AppTextInputTarget::SftpAddress { session_id };
    let app_entity = cx.entity();
    let focus_handle = app
        .input_focus_handles
        .as_ref()
        .map(|handles| handles.sftp_address.clone());
    let native_input = focus_handle
        .clone()
        .map(|focus_handle| app_native_input(app_entity.clone(), target, focus_handle));
    let address_input = session.address_input.clone();
    let key_app_entity = app_entity.clone();
    let click_app_entity = app_entity.clone();
    let pointer_app_entity = app_entity.clone();

    div()
        .h(px(SFTP_TOOLBAR_HEIGHT))
        .w_full()
        .flex()
        .items_center()
        .gap_2()
        .px_3()
        .border_b_1()
        .border_color(rgb(theme.border))
        .child(toolbar_button(
            app_entity.clone(),
            "sftp-parent-dir",
            ArgusIcon::ArrowUp,
            "上级目录",
            session.status == SftpStatus::Connected,
            theme,
            move |app, _| app.open_sftp_parent_directory(session_id),
        ))
        .child(toolbar_button(
            app_entity.clone(),
            "sftp-refresh",
            ArgusIcon::Refresh,
            "刷新",
            session.status == SftpStatus::Connected,
            theme,
            move |app, _| app.refresh_sftp_directory(session_id),
        ))
        .child(div().flex_1().child(render_input(
            Input {
                id: "sftp-address-input",
                placeholder: "输入远程目录路径",
                value: address_input.value.clone(),
                is_disabled: !matches!(session.status, SftpStatus::Connected),
                is_focused: address_input.is_focused,
                cursor_index: address_input.cursor,
                selection_range: app.sftp_input_selection_range(target),
                marked_range: address_input.marked_range.clone(),
                is_pointer_selecting: address_input.selection_drag.is_some(),
                is_secret: false,
                size: InputSize::Regular,
                leading_accessory: Some(InputAccessory {
                    id: "sftp-address-leading",
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
                    app.handle_sftp_text_input_key(target, &event.keystroke);
                    app_cx.notify();
                });
            },
            move |_, window, cx| {
                cx.stop_propagation();
                if let Some(focus_handle) = focus_handle.as_ref() {
                    focus_handle.focus(window);
                }
                click_app_entity.update(cx, |app, app_cx| {
                    app.focus_sftp_text_input_target(target);
                    app_cx.notify();
                });
            },
            move |event: &InputPointerEvent, _, cx| {
                cx.stop_propagation();
                pointer_app_entity.update(cx, |app, app_cx| {
                    match event.action {
                        InputPointerAction::Begin => app.begin_sftp_input_pointer_selection(
                            target,
                            event.character_index,
                            event.granularity,
                        ),
                        InputPointerAction::Extend => {
                            app.update_sftp_input_pointer_selection(target, event.character_index)
                        }
                        InputPointerAction::Finish => {
                            app.finish_sftp_input_pointer_selection(target)
                        }
                    }
                    app_cx.notify();
                });
            },
            move |_, _, _| {},
        )))
        .child(toolbar_button(
            app_entity.clone(),
            "sftp-upload",
            ArgusIcon::Upload,
            "上传文件",
            session.status == SftpStatus::Connected,
            theme,
            move |app, cx| app.choose_sftp_upload_files(session_id, cx),
        ))
        .child(toolbar_button(
            app_entity.clone(),
            "sftp-download",
            ArgusIcon::Download,
            "下载文件",
            app.can_download_sftp_selection(session_id),
            theme,
            move |app, cx| app.choose_sftp_download_target(session_id, cx),
        ))
        .child(toolbar_button(
            app_entity.clone(),
            "sftp-rename",
            ArgusIcon::Rename,
            "重命名",
            app.can_rename_sftp_selection(session_id),
            theme,
            move |app, _| app.open_sftp_rename_dialog(session_id),
        ))
        .child(toolbar_button(
            app_entity,
            "sftp-delete",
            ArgusIcon::Trash,
            "删除",
            app.can_delete_sftp_selection(session_id),
            theme,
            move |app, _| app.request_delete_sftp_entry(session_id),
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
            cx.new(|_| SftpToolbarTooltip {
                label: tooltip.to_string(),
            })
            .into()
        })
        .child(render_icon(icon, foreground, 16.0))
        .on_click(move |_, _, cx| {
            cx.stop_propagation();
            let _ = app_entity.update(cx, |app, app_cx| {
                if enabled {
                    on_click(app, app_cx);
                } else {
                    app.placeholder_notice = format!("{tooltip} 当前不可用");
                }
                app_cx.notify();
            });
        })
}

/// SFTP 工具栏 tooltip。
struct SftpToolbarTooltip {
    /// 提示文案。
    label: String,
}

impl gpui::Render for SftpToolbarTooltip {
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

/// 渲染 SFTP 状态行。
fn render_status_line(session: &SftpSessionState, theme: &AppTheme) -> impl IntoElement {
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
    sort_field: SftpSortField,
    sort_direction: SftpSortDirection,
    theme: &AppTheme,
    cx: &mut Context<ArgusApp>,
) -> impl IntoElement {
    div()
        .h(px(SFTP_TABLE_HEADER_HEIGHT))
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
            SftpSortField::Name,
            session_id,
            sort_field,
            sort_direction,
            theme,
            cx,
        ))
        .child(header_cell(
            "类型",
            Some(SFTP_TYPE_COLUMN_WIDTH),
            SftpSortField::Type,
            session_id,
            sort_field,
            sort_direction,
            theme,
            cx,
        ))
        .child(header_cell(
            "大小",
            Some(SFTP_SIZE_COLUMN_WIDTH),
            SftpSortField::Size,
            session_id,
            sort_field,
            sort_direction,
            theme,
            cx,
        ))
        .child(header_cell(
            "修改时间",
            Some(SFTP_MTIME_COLUMN_WIDTH),
            SftpSortField::Mtime,
            session_id,
            sort_field,
            sort_direction,
            theme,
            cx,
        ))
        .child(header_cell(
            "权限",
            Some(SFTP_PERM_COLUMN_WIDTH),
            SftpSortField::Permissions,
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
    field: SftpSortField,
    session_id: usize,
    sort_field: SftpSortField,
    sort_direction: SftpSortDirection,
    theme: &AppTheme,
    cx: &mut Context<ArgusApp>,
) -> impl IntoElement {
    let is_active = sort_field == field;
    let arrow = if is_active {
        match sort_direction {
            SftpSortDirection::Asc => " ↑",
            SftpSortDirection::Desc => " ↓",
        }
    } else {
        ""
    };
    let label_text = SharedString::from(format!("{label}{arrow}"));
    div()
        .id(SharedString::from(format!(
            "sftp-header-{session_id}-{:?}",
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
            app.set_sftp_sort(session_id, field);
            cx.notify();
        }))
}

/// 渲染远程文件列表。
fn render_file_list(
    session: &SftpSessionState,
    entries: Arc<Vec<SftpEntry>>,
    cx: &mut Context<ArgusApp>,
) -> impl IntoElement {
    let session_id = session.id;
    let selected_paths = session.selected_paths.clone();
    let row_count = entries.len();
    uniform_list(
        "sftp-file-list",
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
    entry: SftpEntry,
    selected_paths: std::collections::BTreeSet<String>,
    theme: &AppTheme,
    cx: &mut Context<ArgusApp>,
) -> impl IntoElement {
    let is_selected = selected_paths.contains(&entry.path);
    let path_for_click = entry.path.clone();
    let path_for_double_click = entry.path.clone();
    let path_for_context = entry.path.clone();
    let icon = match entry.kind {
        SftpEntryKind::Directory => ArgusIcon::Folder,
        SftpEntryKind::RegularFile => ArgusIcon::File,
        SftpEntryKind::Symlink => ArgusIcon::Link,
        SftpEntryKind::Other => ArgusIcon::FileText,
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
            "sftp-entry-{session_id}-{}",
            entry.path
        )))
        .h(px(SFTP_ROW_HEIGHT))
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
                app.select_sftp_entry(
                    session_id,
                    path_for_click.clone(),
                    event.modifiers.shift || event.modifiers.secondary(),
                );
                if event.click_count >= 2 {
                    app.handle_sftp_entry_double_click(session_id, path_for_double_click.clone());
                }
                cx.notify();
            }),
        )
        .on_mouse_down(
            MouseButton::Right,
            cx.listener(move |app, event: &MouseDownEvent, _, cx| {
                cx.stop_propagation();
                app.open_sftp_entry_context_menu(
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
            SFTP_TYPE_COLUMN_WIDTH,
            theme,
        ))
        .child(row_cell(
            format_entry_size(&entry),
            SFTP_SIZE_COLUMN_WIDTH,
            theme,
        ))
        .child(row_cell(
            format_mtime(entry.mtime),
            SFTP_MTIME_COLUMN_WIDTH,
            theme,
        ))
        .child(row_cell(
            format_permissions(entry.permissions),
            SFTP_PERM_COLUMN_WIDTH,
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
fn render_empty_state(session: &SftpSessionState, theme: &AppTheme) -> impl IntoElement {
    div()
        .size_full()
        .flex()
        .items_center()
        .justify_center()
        .text_size(px(13.0))
        .text_color(rgb(theme.foreground_muted))
        .child(if session.status == SftpStatus::Connected {
            "当前目录为空".to_string()
        } else {
            session
                .message
                .clone()
                .unwrap_or_else(|| status_label(&session.status).to_string())
        })
}

/// 返回状态展示文案。
fn status_label(status: &SftpStatus) -> &'static str {
    match status {
        SftpStatus::Connecting => "连接中",
        SftpStatus::AwaitingHostKey => "等待确认指纹",
        SftpStatus::Loading => "加载中",
        SftpStatus::Connected => "已连接",
        SftpStatus::Transferring => "传输中",
        SftpStatus::Disconnected => "已断开",
        SftpStatus::Failed => "连接失败",
    }
}

/// 格式化文件大小。
fn format_entry_size(entry: &SftpEntry) -> String {
    if entry.kind == SftpEntryKind::Directory {
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
