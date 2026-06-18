//! 文件职责：渲染 Argus 独立设置窗口。
//! 创建日期：2026-06-12
//! 修改日期：2026-06-18
//! 作者：Argus 开发团队
//! 主要功能：以无系统标题栏窗口展示关于、外观、日志显示、日志搜索、升级和日志加载设置，并通过主应用状态持久化配置。

use std::sync::Arc;

use gpui::{
    App, Context, Entity, FocusHandle, FontWeight, IntoElement, KeyDownEvent, Render, SharedString,
    Subscription, Window, div, prelude::*, px, rgb,
};

use crate::app::{AppTextInputTarget, ArgusApp, SettingsTextInputState};
use crate::fonts::ARGUS_UI_FONT_FAMILY;
use crate::platform::open_with_registration::RegistrationStatus;
use crate::theme::{AppTheme, ThemeOption};
use crate::ui::components::dropdown::{Dropdown, DropdownItem, render_dropdown};
use crate::ui::components::icon::{ArgusIcon, render_icon};
use crate::ui::components::icon_button::{IconButtonSize, render_icon_button};
use crate::ui::components::input::{
    Input, InputAccessory, InputPointerAction, InputPointerEvent, InputSize, Textarea,
    render_input, render_textarea,
};
use crate::ui::input_native::app_native_input;

/// 设置行右侧内边距，用于让浮层菜单和选择框左边缘对齐。
const SETTINGS_ROW_HORIZONTAL_PADDING: f32 = 12.0;
/// 设置窗口标题图标尺寸，和 14px 标题文字保持协调比例。
const SETTINGS_WINDOW_TITLE_ICON_SIZE: f32 = 16.0;
/// 设置窗口主内容滚动条宽度；GPUI 需要显式宽度才会绘制滚动条。
const SETTINGS_WINDOW_SCROLLBAR_WIDTH: f32 = 8.0;
/// 主题下拉框固定宽度，需和通用下拉框按钮宽度保持一致。
const SETTINGS_THEME_DROPDOWN_WIDTH: f32 = 260.0;
/// 主题下拉菜单单行高度。
const SETTINGS_THEME_DROPDOWN_ROW_HEIGHT: f32 = 30.0;
/// 主题下拉菜单最大高度，用户主题较多时在菜单内部滚动。
const SETTINGS_THEME_DROPDOWN_MAX_HEIGHT: f32 = 220.0;
/// 设置行最小高度，主题菜单使用它在外观分组内部定位。
const SETTINGS_ROW_MIN_HEIGHT: f32 = 44.0;
/// 主题下拉按钮高度，需和通用下拉框保持一致。
const SETTINGS_THEME_DROPDOWN_BUTTON_HEIGHT: f32 = 30.0;
/// 主题下拉菜单与按钮之间的视觉间距。
const SETTINGS_THEME_DROPDOWN_GAP: f32 = 4.0;
/// 主题下拉菜单在外观分组内部的顶部位置。
///
/// 说明：菜单锚定在外观分组局部坐标中，而不是窗口坐标；这样设置页滚动、
/// 上方分组高度变化或窗口尺寸变化时，菜单都会跟随主题设置行。
const SETTINGS_THEME_DROPDOWN_TOP_IN_GROUP: f32 =
    (SETTINGS_ROW_MIN_HEIGHT - SETTINGS_THEME_DROPDOWN_BUTTON_HEIGHT) / 2.0
        + SETTINGS_THEME_DROPDOWN_BUTTON_HEIGHT
        + SETTINGS_THEME_DROPDOWN_GAP;

/// 设置独立窗口根视图；通过订阅主应用实体保持主题和设置值同步。
pub struct SettingsWindow {
    /// 主应用实体，所有设置修改都写回 `ArgusApp`。
    app: Entity<ArgusApp>,
    /// 当前窗口渲染快照。
    snapshot: SettingsWindowSnapshot,
    /// 设置窗口内输入框真实焦点句柄。
    input_focus_handles: SettingsInputFocusHandles,
    /// 主应用状态订阅，确保主题切换和设置变更后窗口刷新。
    _app_observer: Subscription,
}

/// 设置窗口输入框焦点句柄集合。
#[derive(Clone)]
struct SettingsInputFocusHandles {
    /// 设置窗口根区域焦点，用于点击非输入区域时承接键盘焦点。
    root: FocusHandle,
    /// 快搜关键字输入框焦点。
    quick_keywords: FocusHandle,
    /// Jstack 线程名过滤输入框焦点。
    jstack_thread_names: FocusHandle,
    /// Jstack 完整线程段过滤输入框焦点。
    jstack_stack_segments: FocusHandle,
    /// 升级服务器输入框焦点。
    upgrade_server: FocusHandle,
    /// 升级验签公钥输入框焦点。
    upgrade_public_key: FocusHandle,
}

impl SettingsWindow {
    /// 创建设置窗口视图。
    ///
    /// 参数说明：
    /// - `app`：主应用实体。
    /// - `theme`：首次绘制使用的主题快照。
    /// - `snapshot`：首次绘制使用的设置快照。
    /// - `cx`：设置窗口上下文，用于订阅主应用变化。
    ///
    /// 返回值：可渲染的设置窗口视图。
    pub fn new(
        app: Entity<ArgusApp>,
        theme: AppTheme,
        mut snapshot: SettingsWindowSnapshot,
        cx: &mut Context<Self>,
    ) -> Self {
        snapshot.theme = theme;
        let _app_observer = cx.observe(&app, |settings_window, app_entity, cx| {
            settings_window.snapshot =
                app_entity.read_with(cx, |app, _| SettingsWindow::snapshot_from_app(app));
            cx.notify();
        });

        Self {
            app,
            snapshot,
            input_focus_handles: SettingsInputFocusHandles {
                root: cx.focus_handle(),
                quick_keywords: cx.focus_handle(),
                jstack_thread_names: cx.focus_handle(),
                jstack_stack_segments: cx.focus_handle(),
                upgrade_server: cx.focus_handle(),
                upgrade_public_key: cx.focus_handle(),
            },
            _app_observer,
        }
    }

    /// 从主应用状态提取设置窗口只读快照。
    pub fn snapshot_from_app(app: &ArgusApp) -> SettingsWindowSnapshot {
        SettingsWindowSnapshot {
            theme: app.theme.clone(),
            selected_theme_id: app.selected_theme_id.clone(),
            selected_theme_label: app.selected_theme_label(),
            theme_options: app.theme_options(),
            is_theme_dropdown_open: app.is_theme_dropdown_open,
            log_content_font_size: app.log_content_font_size,
            max_archive_depth: app.config.loader.max_archive_depth,
            archive_probe_concurrency: app.config.loader.archive_probe_concurrency,
            follow_symlinks: app.config.loader.follow_symlinks,
            quick_keywords_input: app.settings_quick_keywords_input.clone(),
            jstack_thread_name_filter_input: app.settings_jstack_thread_name_filter_input.clone(),
            jstack_stack_segment_filter_input: app
                .settings_jstack_stack_segment_filter_input
                .clone(),
            upgrade_enabled: app.config.upgrade.enabled,
            upgrade_server_input: app.settings_upgrade_server_input.clone(),
            upgrade_public_key_input: app.settings_upgrade_public_key_input.clone(),
            upgrade_platform_label: app.upgrade_platform_label(),
            is_upgrade_checking: app.is_upgrade_checking,
            upgrade_message: app.upgrade_message.clone(),
            open_with_registration_status: app.open_with_registration_status.clone(),
            is_open_with_registration_busy: app.is_open_with_registration_busy,
            open_with_registration_message: app.open_with_registration_message.clone(),
        }
    }
}

impl Render for SettingsWindow {
    /// 渲染设置窗口主体。
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        render_settings_window(
            &self.snapshot,
            &self.app,
            &self.input_focus_handles,
            window,
            cx,
        )
    }
}

/// 设置窗口快照，避免渲染时跨实体借用主应用状态。
#[derive(Clone, Debug)]
pub struct SettingsWindowSnapshot {
    /// 当前主题令牌。
    pub theme: AppTheme,
    /// 当前选中的主题 ID。
    pub selected_theme_id: String,
    /// 当前选中主题展示文案。
    pub selected_theme_label: String,
    /// 可选主题列表。
    pub theme_options: Vec<ThemeOption>,
    /// 主题下拉框是否展开。
    pub is_theme_dropdown_open: bool,
    /// 日志内容字号。
    pub log_content_font_size: f32,
    /// 最大嵌套压缩包深度。
    pub max_archive_depth: usize,
    /// 当前目录层单文件压缩包探测并发数。
    pub archive_probe_concurrency: usize,
    /// 是否跟随符号链接。
    pub follow_symlinks: bool,
    /// 快搜关键字输入框状态。
    pub quick_keywords_input: SettingsTextInputState,
    /// Jstack 线程名过滤输入框状态。
    pub jstack_thread_name_filter_input: SettingsTextInputState,
    /// Jstack 完整线程段过滤输入框状态。
    pub jstack_stack_segment_filter_input: SettingsTextInputState,
    /// 是否启用启动时自动检查升级。
    pub upgrade_enabled: bool,
    /// 升级服务器输入框状态。
    pub upgrade_server_input: SettingsTextInputState,
    /// 升级验签公钥输入框状态。
    pub upgrade_public_key_input: SettingsTextInputState,
    /// 当前平台 manifest 标识。
    pub upgrade_platform_label: String,
    /// 是否正在检查升级。
    pub is_upgrade_checking: bool,
    /// 最近一次升级消息。
    pub upgrade_message: Option<String>,
    /// 系统右键菜单注册状态。
    pub open_with_registration_status: RegistrationStatus,
    /// 系统右键菜单是否正在注册或卸载。
    pub is_open_with_registration_busy: bool,
    /// 系统右键菜单最近一次操作提示。
    pub open_with_registration_message: Option<String>,
}

/// 渲染设置窗口主体布局。
fn render_settings_window(
    snapshot: &SettingsWindowSnapshot,
    app_handle: &Entity<ArgusApp>,
    input_focus_handles: &SettingsInputFocusHandles,
    _window: &mut Window,
    _cx: &mut Context<SettingsWindow>,
) -> impl IntoElement + use<> {
    let theme = snapshot.theme.clone();
    let close_app = app_handle.clone();
    let root_focus_for_track = input_focus_handles.root.clone();
    let root_focus_for_click = input_focus_handles.root.clone();

    div()
        .id("settings-window-root")
        .size_full()
        .relative()
        .flex()
        .flex_col()
        .bg(rgb(theme.content))
        .font_family(ARGUS_UI_FONT_FAMILY)
        .text_color(rgb(theme.foreground))
        .occlude()
        .focusable()
        .track_focus(&root_focus_for_track)
        .on_click({
            let app_handle = app_handle.clone();
            move |_, window, cx| {
                root_focus_for_click.focus(window);
                update_settings_app(&app_handle, cx, |app, _| {
                    app.close_theme_dropdown();
                    app.clear_all_text_input_focus();
                });
            }
        })
        .child(
            div()
                .h(px(56.0))
                .px_5()
                .flex()
                .items_center()
                .justify_between()
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .text_size(px(14.0))
                        .line_height(px(18.0))
                        .font_weight(FontWeight::SEMIBOLD)
                        .child(render_icon(
                            ArgusIcon::Settings,
                            theme.foreground_muted,
                            SETTINGS_WINDOW_TITLE_ICON_SIZE,
                        ))
                        .child("设置"),
                )
                .child(render_icon_button(
                    "settings-window-close",
                    ArgusIcon::Close,
                    "关闭设置",
                    false,
                    IconButtonSize::Small,
                    &theme,
                    move |_, window, cx| {
                        update_settings_app(&close_app, cx, |app, _| app.close_settings_window());
                        window.remove_window();
                    },
                )),
        )
        .child(
            div()
                .id("settings-window-scroll")
                .flex_1()
                .overflow_y_scroll()
                .scrollbar_width(px(SETTINGS_WINDOW_SCROLLBAR_WIDTH))
                .px_5()
                .pb_5()
                .child(
                    div()
                        .w_full()
                        .flex()
                        .flex_col()
                        .gap_5()
                        .child(settings_section(
                            "关于",
                            ArgusIcon::Info,
                            render_about_section(snapshot, app_handle, &theme),
                            &theme,
                        ))
                        .child(settings_section(
                            "外观",
                            ArgusIcon::Palette,
                            render_appearance_section(snapshot, app_handle, &theme),
                            &theme,
                        ))
                        .child(settings_section(
                            "日志显示",
                            ArgusIcon::Logs,
                            render_log_display_section(
                                snapshot,
                                app_handle,
                                input_focus_handles,
                                &theme,
                            ),
                            &theme,
                        ))
                        .child(settings_section(
                            "日志搜索",
                            ArgusIcon::Search,
                            render_log_search_section(
                                snapshot,
                                app_handle,
                                input_focus_handles,
                                &theme,
                            ),
                            &theme,
                        ))
                        .child(settings_section(
                            "升级",
                            ArgusIcon::Refresh,
                            render_upgrade_section(
                                snapshot,
                                app_handle,
                                input_focus_handles,
                                &theme,
                            ),
                            &theme,
                        ))
                        .child(settings_section(
                            "日志加载",
                            ArgusIcon::FolderPlus,
                            render_log_loading_section(snapshot, app_handle, &theme),
                            &theme,
                        )),
                ),
        )
}

/// 渲染外观分组内的主题下拉菜单浮层。
///
/// 说明：菜单作为外观分组的绝对定位子节点渲染，不参与布局计算，因此不会撑开设置行；
/// 同时它使用分组局部坐标，避免窗口滚动或上方内容变化导致菜单位置漂移。
fn render_theme_dropdown_menu(
    snapshot: &SettingsWindowSnapshot,
    app_handle: &Entity<ArgusApp>,
    theme: &AppTheme,
) -> impl IntoElement + use<> {
    let menu_height = (snapshot.theme_options.len() as f32 * SETTINGS_THEME_DROPDOWN_ROW_HEIGHT)
        .clamp(
            SETTINGS_THEME_DROPDOWN_ROW_HEIGHT,
            SETTINGS_THEME_DROPDOWN_MAX_HEIGHT,
        );
    let select_app_handle = app_handle.clone();
    let options = snapshot.theme_options.clone();
    let selected_theme_id = snapshot.selected_theme_id.clone();
    let panel_theme = theme.clone();

    div()
        .id("settings-theme-dropdown-floating-menu")
        .absolute()
        .top(px(SETTINGS_THEME_DROPDOWN_TOP_IN_GROUP))
        .right(px(SETTINGS_ROW_HORIZONTAL_PADDING))
        .w(px(SETTINGS_THEME_DROPDOWN_WIDTH))
        .h(px(menu_height))
        .rounded_sm()
        .border_1()
        .border_color(rgb(panel_theme.border))
        .bg(rgb(panel_theme.content))
        .shadow_lg()
        .overflow_y_scroll()
        .occlude()
        .on_click(move |_, _, cx| {
            cx.stop_propagation();
        })
        .children(options.into_iter().map(move |option| {
            let is_selected = option.id == selected_theme_id;
            let option_id = option.id.clone();
            let option_label = option.label.clone();
            let select_app = select_app_handle.clone();
            let row_theme = panel_theme.clone();
            let hover_background = row_theme.current_line;
            let foreground = row_theme.foreground;

            div()
                .id(SharedString::from(format!(
                    "settings-theme-dropdown-floating-item-{}",
                    option.id
                )))
                .h(px(SETTINGS_THEME_DROPDOWN_ROW_HEIGHT))
                .w_full()
                .px_2()
                .flex()
                .items_center()
                .cursor_pointer()
                .bg(rgb(if is_selected {
                    row_theme.selection
                } else {
                    row_theme.content
                }))
                .hover(move |this| this.bg(rgb(hover_background)))
                .text_size(px(12.0))
                .text_color(rgb(foreground))
                .child(div().flex_1().truncate().child(option_label))
                .on_click(move |_, _, cx| {
                    cx.stop_propagation();
                    update_settings_app(&select_app, cx, |app, _| {
                        app.select_theme(option_id.clone())
                    });
                })
        }))
}

/// 渲染设置分组标题与内容。
fn settings_section(
    title: &'static str,
    icon: ArgusIcon,
    content: impl IntoElement,
    theme: &AppTheme,
) -> impl IntoElement {
    div()
        .w_full()
        .flex()
        .flex_col()
        .gap_2()
        .child(
            div()
                .h(px(28.0))
                .flex()
                .items_center()
                .gap_2()
                .text_size(px(13.0))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(rgb(theme.foreground))
                .child(render_icon(icon, theme.foreground_muted, 14.0))
                .child(title),
        )
        .child(content)
}

/// 渲染关于设置区；版本、平台和手动检查入口放在一起，便于用户确认当前安装状态。
fn render_about_section(
    snapshot: &SettingsWindowSnapshot,
    app_handle: &Entity<ArgusApp>,
    theme: &AppTheme,
) -> impl IntoElement + use<> {
    let message = snapshot
        .upgrade_message
        .clone()
        .unwrap_or_else(|| "等待检查".to_string());

    setting_group(theme)
        .child(setting_row(
            "程序版本",
            text_value(env!("CARGO_PKG_VERSION"), theme),
            theme,
        ))
        .child(setting_row(
            "当前平台",
            text_value(&snapshot.upgrade_platform_label, theme),
            theme,
        ))
        .child(setting_row(
            "检查更新",
            upgrade_check_control(snapshot, app_handle, &message, theme),
            theme,
        ))
}

/// 渲染外观设置区。
fn render_appearance_section(
    snapshot: &SettingsWindowSnapshot,
    app_handle: &Entity<ArgusApp>,
    theme: &AppTheme,
) -> impl IntoElement + use<> {
    let dropdown_items = snapshot
        .theme_options
        .iter()
        .map(|option| DropdownItem {
            id: option.id.clone(),
            label: option.label.clone(),
        })
        .collect::<Vec<_>>();
    let toggle_app = app_handle.clone();
    let select_app = app_handle.clone();

    let group = setting_group(theme).relative().child(setting_row(
        "主题",
        render_dropdown(
            Dropdown {
                id: "settings-theme-dropdown",
                selected_id: snapshot.selected_theme_id.clone(),
                selected_label: snapshot.selected_theme_label.clone(),
                placeholder: "选择主题",
                is_open: snapshot.is_theme_dropdown_open,
                items: dropdown_items,
                show_inline_menu: false,
            },
            theme,
            move |_, _, cx| {
                cx.stop_propagation();
                update_settings_app(&toggle_app, cx, |app, _| app.toggle_theme_dropdown());
            },
            Arc::new(move |theme_id, _, cx| {
                update_settings_app(&select_app, cx, |app, _| app.select_theme(theme_id));
            }),
        ),
        theme,
    ));

    group.when(snapshot.is_theme_dropdown_open, |this| {
        this.child(render_theme_dropdown_menu(snapshot, app_handle, theme))
    })
}

/// 渲染日志显示设置区。
fn render_log_display_section(
    snapshot: &SettingsWindowSnapshot,
    app_handle: &Entity<ArgusApp>,
    input_focus_handles: &SettingsInputFocusHandles,
    theme: &AppTheme,
) -> impl IntoElement + use<> {
    setting_group(theme)
        .child(setting_row(
            "日志内容字号",
            font_size_control(snapshot.log_content_font_size, app_handle, theme),
            theme,
        ))
        .child(setting_row(
            "线程名过滤",
            jstack_thread_name_filter_input_control(
                snapshot,
                app_handle,
                input_focus_handles,
                theme,
            ),
            theme,
        ))
        .child(setting_row(
            "线程段过滤",
            jstack_stack_segment_filter_input_control(
                snapshot,
                app_handle,
                input_focus_handles,
                theme,
            ),
            theme,
        ))
}

/// 渲染日志搜索设置区。
fn render_log_search_section(
    snapshot: &SettingsWindowSnapshot,
    app_handle: &Entity<ArgusApp>,
    input_focus_handles: &SettingsInputFocusHandles,
    theme: &AppTheme,
) -> impl IntoElement + use<> {
    setting_group(theme).child(setting_row(
        "快搜关键字",
        quick_keywords_input_control(snapshot, app_handle, input_focus_handles, theme),
        theme,
    ))
}

/// 渲染自动升级设置区。
fn render_upgrade_section(
    snapshot: &SettingsWindowSnapshot,
    app_handle: &Entity<ArgusApp>,
    input_focus_handles: &SettingsInputFocusHandles,
    theme: &AppTheme,
) -> impl IntoElement + use<> {
    setting_group(theme)
        .child(setting_row(
            "自动检查",
            upgrade_enabled_control(snapshot.upgrade_enabled, app_handle, theme),
            theme,
        ))
        .child(setting_row(
            "升级服务器",
            upgrade_server_input_control(snapshot, app_handle, input_focus_handles, theme),
            theme,
        ))
        .child(setting_row(
            "验签公钥",
            upgrade_public_key_input_control(snapshot, app_handle, input_focus_handles, theme),
            theme,
        ))
}

/// 渲染日志加载设置区。
fn render_log_loading_section(
    snapshot: &SettingsWindowSnapshot,
    app_handle: &Entity<ArgusApp>,
    theme: &AppTheme,
) -> impl IntoElement + use<> {
    setting_group(theme)
        .child(setting_row(
            "嵌套压缩包深度",
            archive_depth_control(snapshot.max_archive_depth, app_handle, theme),
            theme,
        ))
        .child(setting_row(
            "探测并发数",
            archive_probe_concurrency_control(
                snapshot.archive_probe_concurrency,
                app_handle,
                theme,
            ),
            theme,
        ))
        .child(setting_row(
            "符号链接策略",
            follow_symlink_control(snapshot.follow_symlinks, app_handle, theme),
            theme,
        ))
        .child(setting_row(
            "系统右键菜单",
            open_with_registration_control(snapshot, app_handle, theme),
            theme,
        ))
}

/// 渲染快搜关键字配置输入框。
fn quick_keywords_input_control(
    snapshot: &SettingsWindowSnapshot,
    app_handle: &Entity<ArgusApp>,
    input_focus_handles: &SettingsInputFocusHandles,
    theme: &AppTheme,
) -> impl IntoElement + use<> {
    let input_state = snapshot.quick_keywords_input.clone();
    let key_app = app_handle.clone();
    let click_app = app_handle.clone();
    let pointer_app = app_handle.clone();
    let clear_app = app_handle.clone();
    let native_input = app_native_input(
        app_handle.clone(),
        AppTextInputTarget::SettingsQuickKeywords,
        input_focus_handles.quick_keywords.clone(),
    );

    div().w(px(360.0)).child(render_input(
        Input {
            id: "settings-quick-keywords-input",
            placeholder: "ERROR,WARN,timeout",
            value: input_state.value.clone(),
            is_disabled: false,
            is_focused: input_state.is_focused,
            cursor_index: input_state.cursor,
            selection_range: settings_input_selection_range(&input_state),
            marked_range: input_state.marked_range.clone(),
            is_pointer_selecting: input_state.selection_drag.is_some(),
            size: InputSize::Regular,
            leading_accessory: Some(InputAccessory {
                id: "settings-quick-keywords-leading",
                icon: ArgusIcon::Search,
                tooltip: "英文逗号分隔快搜关键字",
            }),
            trailing_accessory: Some(InputAccessory {
                id: "settings-quick-keywords-clear",
                icon: ArgusIcon::Close,
                tooltip: "清空快搜关键字",
            }),
            native_input: Some(native_input),
        },
        theme,
        move |event: &KeyDownEvent, _, cx| {
            update_settings_app(&key_app, cx, |app, app_cx| {
                app.handle_settings_quick_keywords_key(&event.keystroke, app_cx);
            });
        },
        move |_, _, cx| {
            cx.stop_propagation();
            update_settings_app(&click_app, cx, |app, _| {
                app.focus_settings_quick_keywords_input();
            });
        },
        move |event: &InputPointerEvent, _, cx| {
            cx.stop_propagation();
            update_settings_app(&pointer_app, cx, |app, _| match event.action {
                InputPointerAction::Begin => app.begin_settings_quick_keywords_pointer_selection(
                    event.character_index,
                    event.granularity,
                ),
                InputPointerAction::Extend => {
                    app.update_settings_quick_keywords_pointer_selection(event.character_index)
                }
                InputPointerAction::Finish => {
                    app.finish_settings_quick_keywords_pointer_selection()
                }
            });
        },
        move |_, _, cx| {
            cx.stop_propagation();
            update_settings_app(&clear_app, cx, |app, _| {
                app.clear_settings_quick_keywords_input();
            });
        },
    ))
}

/// 渲染 Jstack 线程名过滤配置输入框。
fn jstack_thread_name_filter_input_control(
    snapshot: &SettingsWindowSnapshot,
    app_handle: &Entity<ArgusApp>,
    input_focus_handles: &SettingsInputFocusHandles,
    theme: &AppTheme,
) -> impl IntoElement + use<> {
    let input_state = snapshot.jstack_thread_name_filter_input.clone();
    let key_app = app_handle.clone();
    let click_app = app_handle.clone();
    let pointer_app = app_handle.clone();
    let clear_app = app_handle.clone();
    let native_input = app_native_input(
        app_handle.clone(),
        AppTextInputTarget::SettingsJstackThreadNameFilter,
        input_focus_handles.jstack_thread_names.clone(),
    );

    div().w(px(360.0)).child(render_input(
        Input {
            id: "settings-jstack-thread-name-filter-input",
            placeholder: "Attach Listener,Signal Dispatcher",
            value: input_state.value.clone(),
            is_disabled: false,
            is_focused: input_state.is_focused,
            cursor_index: input_state.cursor,
            selection_range: settings_input_selection_range(&input_state),
            marked_range: input_state.marked_range.clone(),
            is_pointer_selecting: input_state.selection_drag.is_some(),
            size: InputSize::Regular,
            leading_accessory: Some(InputAccessory {
                id: "settings-jstack-thread-name-filter-leading",
                icon: ArgusIcon::Filter,
                tooltip: "Jstack 线程名过滤",
            }),
            trailing_accessory: Some(InputAccessory {
                id: "settings-jstack-thread-name-filter-clear",
                icon: ArgusIcon::Close,
                tooltip: "清空线程名过滤",
            }),
            native_input: Some(native_input),
        },
        theme,
        move |event: &KeyDownEvent, _, cx| {
            update_settings_app(&key_app, cx, |app, app_cx| {
                app.handle_settings_jstack_thread_name_filter_key(&event.keystroke, app_cx);
            });
        },
        move |_, _, cx| {
            cx.stop_propagation();
            update_settings_app(&click_app, cx, |app, _| {
                app.focus_settings_jstack_thread_name_filter_input();
            });
        },
        move |event: &InputPointerEvent, _, cx| {
            cx.stop_propagation();
            update_settings_app(&pointer_app, cx, |app, _| match event.action {
                InputPointerAction::Begin => app
                    .begin_settings_jstack_thread_name_filter_pointer_selection(
                        event.character_index,
                        event.granularity,
                    ),
                InputPointerAction::Extend => app
                    .update_settings_jstack_thread_name_filter_pointer_selection(
                        event.character_index,
                    ),
                InputPointerAction::Finish => {
                    app.finish_settings_jstack_thread_name_filter_pointer_selection()
                }
            });
        },
        move |_, _, cx| {
            cx.stop_propagation();
            update_settings_app(&clear_app, cx, |app, _| {
                app.clear_settings_jstack_thread_name_filter_input();
            });
        },
    ))
}

/// 渲染 Jstack 完整线程段过滤配置 textarea。
fn jstack_stack_segment_filter_input_control(
    snapshot: &SettingsWindowSnapshot,
    app_handle: &Entity<ArgusApp>,
    input_focus_handles: &SettingsInputFocusHandles,
    theme: &AppTheme,
) -> impl IntoElement + use<> {
    let input_state = snapshot.jstack_stack_segment_filter_input.clone();
    let key_app = app_handle.clone();
    let click_app = app_handle.clone();
    let pointer_app = app_handle.clone();
    let clear_app = app_handle.clone();
    let native_input = app_native_input(
        app_handle.clone(),
        AppTextInputTarget::SettingsJstackStackSegmentFilter,
        input_focus_handles.jstack_stack_segments.clone(),
    );

    div().w(px(360.0)).child(render_textarea(
        Textarea {
            id: "settings-jstack-stack-segment-filter-input",
            placeholder: "SocketInputStream.socketRead\n    at java.net.SocketInputStream.read\n||\nUnsafe.park",
            value: input_state.value.clone(),
            is_disabled: false,
            is_focused: input_state.is_focused,
            cursor_index: input_state.cursor,
            selection_range: settings_input_selection_range(&input_state),
            marked_range: input_state.marked_range.clone(),
            is_pointer_selecting: input_state.selection_drag.is_some(),
            visible_lines: 5,
            trailing_accessory: Some(InputAccessory {
                id: "settings-jstack-stack-segment-filter-clear",
                icon: ArgusIcon::Close,
                tooltip: "清空线程段过滤",
            }),
            native_input: Some(native_input),
        },
        theme,
        move |event: &KeyDownEvent, _, cx| {
            update_settings_app(&key_app, cx, |app, app_cx| {
                app.handle_settings_jstack_stack_segment_filter_key(&event.keystroke, app_cx);
            });
        },
        move |_, _, cx| {
            cx.stop_propagation();
            update_settings_app(&click_app, cx, |app, _| {
                app.focus_settings_jstack_stack_segment_filter_input();
            });
        },
        move |event: &InputPointerEvent, _, cx| {
            cx.stop_propagation();
            update_settings_app(&pointer_app, cx, |app, _| match event.action {
                InputPointerAction::Begin => app
                    .begin_settings_jstack_stack_segment_filter_pointer_selection(
                        event.character_index,
                        event.granularity,
                    ),
                InputPointerAction::Extend => app
                    .update_settings_jstack_stack_segment_filter_pointer_selection(
                        event.character_index,
                    ),
                InputPointerAction::Finish => {
                    app.finish_settings_jstack_stack_segment_filter_pointer_selection()
                }
            });
        },
        move |_, _, cx| {
            cx.stop_propagation();
            update_settings_app(&clear_app, cx, |app, _| {
                app.clear_settings_jstack_stack_segment_filter_input();
            });
        },
    ))
}

/// 渲染升级服务器配置输入框。
fn upgrade_server_input_control(
    snapshot: &SettingsWindowSnapshot,
    app_handle: &Entity<ArgusApp>,
    input_focus_handles: &SettingsInputFocusHandles,
    theme: &AppTheme,
) -> impl IntoElement + use<> {
    let input_state = snapshot.upgrade_server_input.clone();
    let key_app = app_handle.clone();
    let click_app = app_handle.clone();
    let pointer_app = app_handle.clone();
    let clear_app = app_handle.clone();
    let native_input = app_native_input(
        app_handle.clone(),
        AppTextInputTarget::SettingsUpgradeServer,
        input_focus_handles.upgrade_server.clone(),
    );

    div().w(px(360.0)).child(render_input(
        Input {
            id: "settings-upgrade-server-input",
            placeholder: "https://updates.example.com/argus/",
            value: input_state.value.clone(),
            is_disabled: false,
            is_focused: input_state.is_focused,
            cursor_index: input_state.cursor,
            selection_range: settings_input_selection_range(&input_state),
            marked_range: input_state.marked_range.clone(),
            is_pointer_selecting: input_state.selection_drag.is_some(),
            size: InputSize::Regular,
            leading_accessory: Some(InputAccessory {
                id: "settings-upgrade-server-leading",
                icon: ArgusIcon::Connection,
                tooltip: "升级服务器地址",
            }),
            trailing_accessory: Some(InputAccessory {
                id: "settings-upgrade-server-clear",
                icon: ArgusIcon::Close,
                tooltip: "清空升级服务器",
            }),
            native_input: Some(native_input),
        },
        theme,
        move |event: &KeyDownEvent, _, cx| {
            update_settings_app(&key_app, cx, |app, app_cx| {
                app.handle_settings_upgrade_server_key(&event.keystroke, app_cx);
            });
        },
        move |_, _, cx| {
            cx.stop_propagation();
            update_settings_app(&click_app, cx, |app, _| {
                app.focus_settings_upgrade_server_input();
            });
        },
        move |event: &InputPointerEvent, _, cx| {
            cx.stop_propagation();
            update_settings_app(&pointer_app, cx, |app, _| match event.action {
                InputPointerAction::Begin => app.begin_settings_upgrade_server_pointer_selection(
                    event.character_index,
                    event.granularity,
                ),
                InputPointerAction::Extend => {
                    app.update_settings_upgrade_server_pointer_selection(event.character_index)
                }
                InputPointerAction::Finish => {
                    app.finish_settings_upgrade_server_pointer_selection()
                }
            });
        },
        move |_, _, cx| {
            cx.stop_propagation();
            update_settings_app(&clear_app, cx, |app, _| {
                app.clear_settings_upgrade_server_input();
            });
        },
    ))
}

/// 渲染升级 manifest 验签公钥输入框。
fn upgrade_public_key_input_control(
    snapshot: &SettingsWindowSnapshot,
    app_handle: &Entity<ArgusApp>,
    input_focus_handles: &SettingsInputFocusHandles,
    theme: &AppTheme,
) -> impl IntoElement + use<> {
    let input_state = snapshot.upgrade_public_key_input.clone();
    let key_app = app_handle.clone();
    let click_app = app_handle.clone();
    let pointer_app = app_handle.clone();
    let clear_app = app_handle.clone();
    let native_input = app_native_input(
        app_handle.clone(),
        AppTextInputTarget::SettingsUpgradePublicKey,
        input_focus_handles.upgrade_public_key.clone(),
    );

    div().w(px(360.0)).child(render_input(
        Input {
            id: "settings-upgrade-public-key-input",
            placeholder: "ARGUS_UPDATE_PUBLIC_KEY_BASE64",
            value: input_state.value.clone(),
            is_disabled: false,
            is_focused: input_state.is_focused,
            cursor_index: input_state.cursor,
            selection_range: settings_input_selection_range(&input_state),
            marked_range: input_state.marked_range.clone(),
            is_pointer_selecting: input_state.selection_drag.is_some(),
            size: InputSize::Regular,
            leading_accessory: Some(InputAccessory {
                id: "settings-upgrade-public-key-leading",
                icon: ArgusIcon::Key,
                tooltip: "Ed25519 公钥 Base64",
            }),
            trailing_accessory: Some(InputAccessory {
                id: "settings-upgrade-public-key-clear",
                icon: ArgusIcon::Close,
                tooltip: "清空升级验签公钥",
            }),
            native_input: Some(native_input),
        },
        theme,
        move |event: &KeyDownEvent, _, cx| {
            update_settings_app(&key_app, cx, |app, app_cx| {
                app.handle_settings_upgrade_public_key_key(&event.keystroke, app_cx);
            });
        },
        move |_, _, cx| {
            cx.stop_propagation();
            update_settings_app(&click_app, cx, |app, _| {
                app.focus_settings_upgrade_public_key_input();
            });
        },
        move |event: &InputPointerEvent, _, cx| {
            cx.stop_propagation();
            update_settings_app(&pointer_app, cx, |app, _| match event.action {
                InputPointerAction::Begin => app
                    .begin_settings_upgrade_public_key_pointer_selection(
                        event.character_index,
                        event.granularity,
                    ),
                InputPointerAction::Extend => {
                    app.update_settings_upgrade_public_key_pointer_selection(event.character_index)
                }
                InputPointerAction::Finish => {
                    app.finish_settings_upgrade_public_key_pointer_selection()
                }
            });
        },
        move |_, _, cx| {
            cx.stop_propagation();
            update_settings_app(&clear_app, cx, |app, _| {
                app.clear_settings_upgrade_public_key_input();
            });
        },
    ))
}

/// 返回设置输入框的规范化非空选区。
fn settings_input_selection_range(
    input: &SettingsTextInputState,
) -> Option<std::ops::Range<usize>> {
    let anchor = input.selection_anchor?;
    if anchor == input.cursor {
        return None;
    }
    Some(anchor.min(input.cursor)..anchor.max(input.cursor))
}

/// 渲染设置组背景容器。
fn setting_group(theme: &AppTheme) -> gpui::Div {
    div()
        .w_full()
        .flex()
        .flex_col()
        .gap_2()
        .text_size(px(13.0))
        .bg(rgb(theme.content))
}

/// 渲染单个设置行。
fn setting_row(
    label: &'static str,
    control: impl IntoElement,
    theme: &AppTheme,
) -> impl IntoElement {
    div()
        .min_h(px(SETTINGS_ROW_MIN_HEIGHT))
        .px_3()
        .flex()
        .items_center()
        .justify_between()
        .gap_3()
        .rounded_sm()
        .bg(rgb(theme.current_line))
        .child(
            div()
                .text_size(px(13.0))
                .text_color(rgb(theme.foreground))
                .child(label),
        )
        .child(control)
}

/// 渲染只读文本值。
fn text_value(value: &str, theme: &AppTheme) -> impl IntoElement {
    div()
        .text_size(px(12.0))
        .text_color(rgb(theme.foreground_muted))
        .child(value.to_string())
}

/// 渲染日志字号步进控件。
fn font_size_control(
    font_size: f32,
    app_handle: &Entity<ArgusApp>,
    theme: &AppTheme,
) -> impl IntoElement + use<> {
    let minus_app = app_handle.clone();
    let plus_app = app_handle.clone();

    div()
        .flex()
        .items_center()
        .gap_2()
        .child(render_icon_button(
            "settings-log-font-minus",
            ArgusIcon::Minus,
            "减小日志字号",
            false,
            IconButtonSize::Small,
            theme,
            move |_, _, cx| {
                update_settings_app(&minus_app, cx, |app, _| {
                    app.adjust_log_content_font_size(-1.0)
                });
            },
        ))
        .child(value_badge(format!("{font_size:.0}px"), theme))
        .child(render_icon_button(
            "settings-log-font-plus",
            ArgusIcon::Plus,
            "增大日志字号",
            false,
            IconButtonSize::Small,
            theme,
            move |_, _, cx| {
                update_settings_app(&plus_app, cx, |app, _| {
                    app.adjust_log_content_font_size(1.0)
                });
            },
        ))
}

/// 渲染压缩包深度步进控件。
fn archive_depth_control(
    max_archive_depth: usize,
    app_handle: &Entity<ArgusApp>,
    theme: &AppTheme,
) -> impl IntoElement + use<> {
    let minus_app = app_handle.clone();
    let plus_app = app_handle.clone();

    div()
        .flex()
        .items_center()
        .gap_2()
        .child(render_icon_button(
            "settings-archive-depth-minus",
            ArgusIcon::Minus,
            "减少嵌套压缩包深度",
            false,
            IconButtonSize::Small,
            theme,
            move |_, _, cx| {
                update_settings_app(&minus_app, cx, |app, _| app.adjust_max_archive_depth(-1));
            },
        ))
        .child(value_badge(format!("{max_archive_depth} 层"), theme))
        .child(render_icon_button(
            "settings-archive-depth-plus",
            ArgusIcon::Plus,
            "增加嵌套压缩包深度",
            false,
            IconButtonSize::Small,
            theme,
            move |_, _, cx| {
                update_settings_app(&plus_app, cx, |app, _| app.adjust_max_archive_depth(1));
            },
        ))
}

/// 渲染单文件压缩包探测并发数步进控件。
fn archive_probe_concurrency_control(
    concurrency: usize,
    app_handle: &Entity<ArgusApp>,
    theme: &AppTheme,
) -> impl IntoElement + use<> {
    let minus_app = app_handle.clone();
    let plus_app = app_handle.clone();

    div()
        .flex()
        .items_center()
        .gap_2()
        .child(render_icon_button(
            "settings-archive-probe-concurrency-minus",
            ArgusIcon::Minus,
            "减少探测并发数",
            false,
            IconButtonSize::Small,
            theme,
            move |_, _, cx| {
                update_settings_app(&minus_app, cx, |app, _| {
                    app.adjust_archive_probe_concurrency(-1)
                });
            },
        ))
        .child(value_badge(format!("{concurrency} 个"), theme))
        .child(render_icon_button(
            "settings-archive-probe-concurrency-plus",
            ArgusIcon::Plus,
            "增加探测并发数",
            false,
            IconButtonSize::Small,
            theme,
            move |_, _, cx| {
                update_settings_app(&plus_app, cx, |app, _| {
                    app.adjust_archive_probe_concurrency(1)
                });
            },
        ))
}

/// 渲染符号链接策略开关。
fn follow_symlink_control(
    follow_symlinks: bool,
    app_handle: &Entity<ArgusApp>,
    theme: &AppTheme,
) -> impl IntoElement + use<> {
    let toggle_app = app_handle.clone();
    let policy_text = if follow_symlinks {
        "跟随"
    } else {
        "不跟随"
    };

    div()
        .flex()
        .items_center()
        .gap_2()
        .child(value_badge(policy_text.to_string(), theme))
        .child(render_icon_button(
            "settings-follow-symlink-toggle",
            if follow_symlinks {
                ArgusIcon::ToggleRight
            } else {
                ArgusIcon::ToggleLeft
            },
            "切换符号链接策略",
            follow_symlinks,
            IconButtonSize::Small,
            theme,
            move |_, _, cx| {
                update_settings_app(&toggle_app, cx, |app, _| app.toggle_follow_symlinks());
            },
        ))
}

/// 渲染自动升级开关。
fn upgrade_enabled_control(
    upgrade_enabled: bool,
    app_handle: &Entity<ArgusApp>,
    theme: &AppTheme,
) -> impl IntoElement + use<> {
    let toggle_app = app_handle.clone();
    let policy_text = if upgrade_enabled { "启用" } else { "关闭" };

    div()
        .flex()
        .items_center()
        .gap_2()
        .child(value_badge(policy_text.to_string(), theme))
        .child(render_icon_button(
            "settings-upgrade-enabled-toggle",
            if upgrade_enabled {
                ArgusIcon::ToggleRight
            } else {
                ArgusIcon::ToggleLeft
            },
            "切换自动升级检查",
            upgrade_enabled,
            IconButtonSize::Small,
            theme,
            move |_, _, cx| {
                update_settings_app(&toggle_app, cx, |app, _| app.toggle_upgrade_enabled());
            },
        ))
}

/// 渲染升级状态和手动检查按钮。
fn upgrade_check_control(
    snapshot: &SettingsWindowSnapshot,
    app_handle: &Entity<ArgusApp>,
    message: &str,
    theme: &AppTheme,
) -> impl IntoElement + use<> {
    let check_app = app_handle.clone();
    let is_busy = snapshot.is_upgrade_checking;

    div()
        .flex()
        .items_center()
        .gap_2()
        .child(
            div()
                .max_w(px(260.0))
                .h(px(28.0))
                .px_2()
                .flex()
                .items_center()
                .rounded_sm()
                .bg(rgb(theme.current_line))
                .text_size(px(12.0))
                .line_height(px(28.0))
                .text_color(rgb(theme.foreground_muted))
                .child(div().truncate().child(if is_busy {
                    "检查中...".to_string()
                } else {
                    message.to_string()
                })),
        )
        .child(registration_action_button(
            "settings-upgrade-check",
            "检查",
            ArgusIcon::Refresh,
            is_busy,
            theme,
            move |cx| {
                update_settings_app(&check_app, cx, |app, cx| app.start_upgrade_check(true, cx));
            },
        ))
}

/// 渲染系统右键菜单注册状态与操作按钮。
fn open_with_registration_control(
    snapshot: &SettingsWindowSnapshot,
    app_handle: &Entity<ArgusApp>,
    theme: &AppTheme,
) -> impl IntoElement + use<> {
    let register_app = app_handle.clone();
    let unregister_app = app_handle.clone();
    let status = snapshot.open_with_registration_status.clone();
    let status_label = status.label();
    let message = snapshot.open_with_registration_message.clone();
    let is_busy = snapshot.is_open_with_registration_busy;
    let can_register = !is_busy && status.can_register();
    let can_unregister = !is_busy && status.can_unregister();

    div()
        .flex()
        .items_center()
        .gap_2()
        .child(
            div()
                .max_w(px(220.0))
                .h(px(28.0))
                .px_2()
                .flex()
                .items_center()
                .rounded_sm()
                .bg(rgb(match status {
                    RegistrationStatus::Registered => theme.selection,
                    RegistrationStatus::Unsupported(_) => theme.content,
                    RegistrationStatus::NotRegistered | RegistrationStatus::Unknown(_) => {
                        theme.current_line
                    }
                }))
                .text_size(px(12.0))
                .line_height(px(28.0))
                .text_color(rgb(
                    if matches!(
                        snapshot.open_with_registration_status,
                        RegistrationStatus::Registered
                    ) {
                        theme.foreground
                    } else {
                        theme.foreground_muted
                    },
                ))
                .child(div().truncate().child(if is_busy {
                    "执行中...".to_string()
                } else {
                    status_label
                })),
        )
        .when_some(message, |this, message| {
            this.child(
                div()
                    .max_w(px(180.0))
                    .text_size(px(12.0))
                    .text_color(rgb(theme.foreground_muted))
                    .truncate()
                    .child(message),
            )
        })
        .child(registration_action_button(
            "settings-open-with-register",
            "注册",
            ArgusIcon::FolderPlus,
            !can_register,
            theme,
            move |cx| {
                update_settings_app(&register_app, cx, |app, cx| app.register_open_with_menu(cx));
            },
        ))
        .child(registration_action_button(
            "settings-open-with-unregister",
            "卸载",
            ArgusIcon::Close,
            !can_unregister,
            theme,
            move |cx| {
                update_settings_app(&unregister_app, cx, |app, cx| {
                    app.unregister_open_with_menu(cx)
                });
            },
        ))
}

/// 渲染设置窗口里带图标的紧凑文字按钮。
fn registration_action_button(
    id: &'static str,
    label: &'static str,
    icon: ArgusIcon,
    is_disabled: bool,
    theme: &AppTheme,
    action: impl Fn(&mut App) + 'static,
) -> impl IntoElement {
    let button_theme = theme.clone();

    div()
        .id(id)
        .h(px(28.0))
        .px_3()
        .flex()
        .items_center()
        .justify_center()
        .gap_1()
        .rounded_sm()
        .bg(rgb(if is_disabled {
            button_theme.content
        } else {
            button_theme.current_line
        }))
        .text_size(px(12.0))
        .line_height(px(28.0))
        .text_color(rgb(if is_disabled {
            button_theme.foreground_muted
        } else {
            button_theme.foreground
        }))
        .when(!is_disabled, |this| {
            this.cursor_pointer()
                .hover(move |this| this.bg(rgb(button_theme.selection)))
        })
        .child(render_icon(
            icon,
            if is_disabled {
                theme.foreground_muted
            } else {
                theme.foreground
            },
            13.0,
        ))
        .child(label)
        .on_click(move |_, _, cx| {
            cx.stop_propagation();
            if !is_disabled {
                action(cx);
            }
        })
}

/// 渲染紧凑数值徽标。
fn value_badge(value: String, theme: &AppTheme) -> impl IntoElement {
    div()
        .w(px(78.0))
        .h(px(28.0))
        .flex()
        .items_center()
        .justify_center()
        .rounded_sm()
        .bg(rgb(theme.content))
        .text_size(px(12.0))
        .line_height(px(28.0))
        .text_color(rgb(theme.foreground))
        .child(value)
}

/// 统一更新主应用状态；设置窗口只负责表现，不直接持有业务配置。
fn update_settings_app(
    app_handle: &Entity<ArgusApp>,
    cx: &mut App,
    update: impl FnOnce(&mut ArgusApp, &mut Context<ArgusApp>),
) {
    let _ = app_handle.update(cx, |app, app_cx| {
        update(app, app_cx);
        app_cx.notify();
    });
}
