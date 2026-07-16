//! 文件职责：渲染 Argus 主窗口设置模态框和独立设置编辑器。
//! 创建日期：2026-06-12
//! 修改日期：2026-07-16
//! 作者：Argus 开发团队
//! 主要功能：以主窗口模态框展示分类设置，并提供智能分析、系统提示词和 Jstack 线程段过滤编辑入口。

use std::sync::Arc;

use gpui::{
    AnyElement, App, Context, Entity, FocusHandle, FontWeight, IntoElement, KeyDownEvent, Render,
    ScrollHandle, SharedString, Subscription, Window, div, prelude::*, px, rgb,
};

use crate::analysis::jstack::split_stack_segment_filter_blocks;
use crate::app::{
    AppInputFocusHandles, AppTextInputTarget, ArgusApp, SettingsSection, TextInputState,
};
use crate::config::{AiModelProfile, LogTypeProfile};
use crate::fonts::ARGUS_UI_FONT_FAMILY;
use crate::platform::open_with_registration::RegistrationStatus;
use crate::theme::{AppTheme, ThemeOption};
use crate::ui::components::dropdown::{Dropdown, DropdownItem, render_dropdown};
use crate::ui::components::icon::{ArgusIcon, render_icon};
use crate::ui::components::icon_button::{IconButtonSize, render_icon_button};
use crate::ui::components::input::{
    Input, InputAccessory, InputPointerAction, InputPointerEvent, InputSize, Textarea,
    TextareaAccessoryPosition, TextareaScrollState, TextareaStyle, render_input, render_textarea,
};
use crate::ui::components::modal_dialog::{ModalDialog, render_modal_dialog};
use crate::ui::input_native::app_native_input;

/// 设置行右侧内边距，用于让浮层菜单和选择框左边缘对齐。
const SETTINGS_ROW_HORIZONTAL_PADDING: f32 = 12.0;
/// 设置模态框左侧分类导航宽度。
const SETTINGS_MODAL_SIDEBAR_WIDTH: f32 = 168.0;
/// 设置模态框右侧内容宽度，沿用改造前独立设置窗口的默认宽度。
const SETTINGS_MODAL_CONTENT_WIDTH: f32 = 760.0;
/// 设置模态框总宽度；新增导航不压缩原设置内容和控件。
const SETTINGS_MODAL_WIDTH: f32 = SETTINGS_MODAL_SIDEBAR_WIDTH + SETTINGS_MODAL_CONTENT_WIDTH;
/// 设置模态框高度，沿用改造前独立设置窗口的默认高度。
const SETTINGS_MODAL_HEIGHT: f32 = 560.0;
/// 设置模态框标题图标尺寸，和 14px 标题文字保持协调比例。
const SETTINGS_MODAL_TITLE_ICON_SIZE: f32 = 16.0;
/// Jstack 线程段过滤编辑器标题图标尺寸，复用设置模态框标题栏视觉比例。
const JSTACK_STACK_SEGMENT_EDITOR_TITLE_ICON_SIZE: f32 = 16.0;
/// Jstack 线程段过滤编辑器 textarea 默认可见行数。
const JSTACK_STACK_SEGMENT_EDITOR_VISIBLE_LINES: usize = 22;
/// 设置模态框主内容滚动条宽度；GPUI 需要显式宽度才会绘制滚动条。
const SETTINGS_MODAL_SCROLLBAR_WIDTH: f32 = 8.0;
/// 主题下拉框固定宽度，需和通用下拉框按钮宽度保持一致。
const SETTINGS_THEME_DROPDOWN_WIDTH: f32 = 260.0;
/// 主题下拉菜单单行高度。
const SETTINGS_THEME_DROPDOWN_ROW_HEIGHT: f32 = 30.0;
/// 主题下拉菜单最大高度，用户主题较多时在菜单内部滚动。
const SETTINGS_THEME_DROPDOWN_MAX_HEIGHT: f32 = 220.0;
/// 设置行最小高度，主题菜单使用它在外观分组内部定位。
const SETTINGS_ROW_MIN_HEIGHT: f32 = 44.0;
/// 是否在设置模态框展示升级相关入口；当前按产品要求隐藏，底层升级能力保留。
const SHOW_UPGRADE_SETTINGS_ENTRIES: bool = false;
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

/// 设置模态框输入框焦点句柄集合。
#[derive(Clone)]
struct SettingsInputFocusHandles {
    /// 设置模态框根区域焦点，用于点击非输入区域时承接键盘焦点。
    root: FocusHandle,
    /// 快搜关键字输入框焦点。
    quick_keywords: FocusHandle,
    /// Jstack 线程名过滤输入框焦点。
    jstack_thread_names: FocusHandle,
    /// 升级服务器输入框焦点。
    upgrade_server: FocusHandle,
    /// 升级验签公钥输入框焦点。
    upgrade_public_key: FocusHandle,
}

impl SettingsInputFocusHandles {
    /// 从主窗口稳定焦点句柄中提取设置模态框需要的部分。
    fn from_app_handles(handles: &AppInputFocusHandles) -> Self {
        Self {
            root: handles.root.clone(),
            quick_keywords: handles.settings_quick_keywords.clone(),
            jstack_thread_names: handles.settings_jstack_thread_names.clone(),
            upgrade_server: handles.settings_upgrade_server.clone(),
            upgrade_public_key: handles.settings_upgrade_public_key.clone(),
        }
    }
}

/// 设置模态框快照，避免构建元素树时重复借用主应用状态。
#[derive(Clone, Debug, PartialEq)]
struct SettingsModalSnapshot {
    /// 当前主题令牌。
    theme: AppTheme,
    /// 当前选中的设置分类。
    selected_section: SettingsSection,
    /// 当前选中的主题 ID。
    selected_theme_id: String,
    /// 可供智能分析会话选择的模型配置列表。
    ai_model_profiles: Vec<AiModelProfile>,
    /// 自定义日志类型说明列表。
    ai_log_profiles: Vec<LogTypeProfile>,
    /// 用户可编辑的默认专业系统提示词。
    ai_system_prompt: String,
    /// 当前选中主题展示文案。
    selected_theme_label: String,
    /// 可选主题列表。
    theme_options: Vec<ThemeOption>,
    /// 主题下拉框是否展开。
    is_theme_dropdown_open: bool,
    /// 日志内容字号。
    log_content_font_size: f32,
    /// 最大嵌套压缩包深度。
    max_archive_depth: usize,
    /// 当前目录层单文件压缩包探测并发数。
    archive_probe_concurrency: usize,
    /// 是否跟随符号链接。
    follow_symlinks: bool,
    /// 快搜关键字输入框状态。
    quick_keywords_input: TextInputState,
    /// Jstack 线程名过滤输入框状态。
    jstack_thread_name_filter_input: TextInputState,
    /// Jstack 完整线程段过滤输入框状态。
    jstack_stack_segment_filter_input: TextInputState,
    /// 是否启用启动时自动检查升级。
    upgrade_enabled: bool,
    /// 升级服务器输入框状态。
    upgrade_server_input: TextInputState,
    /// 升级验签公钥输入框状态。
    upgrade_public_key_input: TextInputState,
    /// 当前平台 manifest 标识。
    upgrade_platform_label: String,
    /// 是否正在检查升级。
    is_upgrade_checking: bool,
    /// 最近一次升级消息。
    upgrade_message: Option<String>,
    /// 系统右键菜单注册状态。
    open_with_registration_status: RegistrationStatus,
    /// 系统右键菜单是否正在注册或卸载。
    is_open_with_registration_busy: bool,
    /// 系统右键菜单最近一次操作提示。
    open_with_registration_message: Option<String>,
}

impl SettingsModalSnapshot {
    /// 从主应用状态提取设置模态框只读快照。
    fn from_app(app: &ArgusApp) -> Self {
        Self {
            theme: app.theme.clone(),
            selected_section: app.selected_settings_section,
            selected_theme_id: app.selected_theme_id.clone(),
            ai_model_profiles: app.config.ai.model_profiles.clone(),
            ai_log_profiles: app.config.ai.log_profiles.clone(),
            ai_system_prompt: app.config.ai.system_prompt.clone(),
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

/// Jstack 线程段过滤大编辑器窗口；使用独立窗口承载长 textarea，避免设置页行内编辑困难。
pub(crate) struct JstackStackSegmentFilterEditorWindow {
    /// 主应用实体，编辑内容直接写回 `ArgusApp` 的设置输入状态。
    app: Entity<ArgusApp>,
    /// 当前编辑器渲染快照。
    snapshot: JstackStackSegmentFilterEditorSnapshot,
    /// 编辑器内焦点和滚动句柄。
    focus_handles: JstackStackSegmentFilterEditorFocusHandles,
    /// 主应用状态订阅，确保设置页清空或主题切换后编辑器同步刷新。
    _app_observer: Subscription,
}

/// Jstack 线程段过滤编辑器焦点与滚动句柄集合。
#[derive(Clone)]
struct JstackStackSegmentFilterEditorFocusHandles {
    /// 编辑器根焦点，用于点击空白区域时承接键盘焦点。
    root: FocusHandle,
    /// 大 textarea 的真实输入焦点。
    textarea: FocusHandle,
    /// 大 textarea 的滚动句柄，支持横纵滚动条和光标跟随。
    textarea_scroll: ScrollHandle,
    /// 大 textarea 的滚动条拖拽状态。
    textarea_scroll_state: TextareaScrollState,
}

impl JstackStackSegmentFilterEditorWindow {
    /// 创建 Jstack 线程段过滤大编辑器。
    ///
    /// 参数说明：
    /// - `app`：主应用实体。
    /// - `theme`：首次绘制使用的主题。
    /// - `snapshot`：首次绘制使用的输入快照。
    /// - `cx`：编辑器窗口上下文，用于订阅主应用状态。
    ///
    /// 返回值：可渲染的编辑器窗口视图。
    pub(crate) fn new(
        app: Entity<ArgusApp>,
        theme: AppTheme,
        mut snapshot: JstackStackSegmentFilterEditorSnapshot,
        cx: &mut Context<Self>,
    ) -> Self {
        snapshot.theme = theme;
        let _app_observer = cx.observe(&app, |editor, app_entity, cx| {
            let next_snapshot = app_entity.read_with(cx, |app, _| {
                JstackStackSegmentFilterEditorWindow::snapshot_from_app(app)
            });
            if editor.snapshot == next_snapshot {
                return;
            }
            editor.snapshot = next_snapshot;
            cx.notify();
        });

        Self {
            app,
            snapshot,
            focus_handles: JstackStackSegmentFilterEditorFocusHandles {
                root: cx.focus_handle(),
                textarea: cx.focus_handle(),
                textarea_scroll: ScrollHandle::new(),
                textarea_scroll_state: TextareaScrollState::new(),
            },
            _app_observer,
        }
    }

    /// 从主应用状态提取编辑器渲染快照。
    pub(crate) fn snapshot_from_app(app: &ArgusApp) -> JstackStackSegmentFilterEditorSnapshot {
        JstackStackSegmentFilterEditorSnapshot {
            theme: app.theme.clone(),
            input: app.settings_jstack_stack_segment_filter_input.clone(),
        }
    }
}

impl Render for JstackStackSegmentFilterEditorWindow {
    /// 渲染 Jstack 线程段过滤编辑器主体。
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        render_jstack_stack_segment_filter_editor_window(
            &self.snapshot,
            &self.app,
            &self.focus_handles,
            window,
            cx,
        )
    }
}

/// Jstack 线程段过滤编辑器快照。
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct JstackStackSegmentFilterEditorSnapshot {
    /// 当前主题令牌。
    pub theme: AppTheme,
    /// 当前线程段过滤输入状态。
    pub input: TextInputState,
}

/// 渲染覆盖主窗口的设置模态框。
///
/// 参数说明：
/// - `app`：主应用状态，用于构造当前设置快照。
/// - `app_focus_handles`：主窗口稳定焦点句柄，供设置输入框复用。
/// - `cx`：主应用上下文，用于更新设置状态并阻断遮罩层事件。
///
/// 返回值：包含遮罩、居中容器、分类导航和设置内容的 GPUI 元素树。
pub(crate) fn render_settings_modal(
    app: &ArgusApp,
    app_focus_handles: &AppInputFocusHandles,
    cx: &mut Context<ArgusApp>,
) -> AnyElement {
    let snapshot = SettingsModalSnapshot::from_app(app);
    let input_focus_handles = SettingsInputFocusHandles::from_app_handles(app_focus_handles);
    let app_handle = cx.entity();
    let theme = snapshot.theme.clone();
    let content = render_settings_modal_content(&snapshot, &app_handle, &input_focus_handles);

    render_modal_dialog(
        ModalDialog {
            overlay_id: "settings-modal-overlay",
            container_id: "settings-modal-container",
            width: SETTINGS_MODAL_WIDTH,
            height: SETTINGS_MODAL_HEIGHT,
            content: content.into_any_element(),
        },
        theme,
        cx,
    )
    .into_any_element()
}

/// 渲染设置模态框内部的左右分栏布局。
fn render_settings_modal_content(
    snapshot: &SettingsModalSnapshot,
    app_handle: &Entity<ArgusApp>,
    input_focus_handles: &SettingsInputFocusHandles,
) -> impl IntoElement + use<> {
    let theme = snapshot.theme.clone();
    let close_app = app_handle.clone();
    let escape_app = app_handle.clone();
    let root_focus_for_track = input_focus_handles.root.clone();
    let root_focus_for_click = input_focus_handles.root.clone();

    div()
        .id("settings-modal-root")
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
        .on_key_down(move |event: &KeyDownEvent, _, cx| {
            if event.keystroke.key != "escape" {
                return;
            }
            cx.stop_propagation();
            update_settings_app(&escape_app, cx, |app, _| app.close_settings_modal());
        })
        .child(render_settings_sidebar(snapshot, app_handle, &theme))
        .child(
            div()
                .flex_1()
                .min_w(px(0.0))
                .flex()
                .flex_col()
                .bg(rgb(theme.content))
                .child(
                    div()
                        .h(px(56.0))
                        .px_5()
                        .flex()
                        .items_center()
                        .justify_between()
                        .occlude()
                        .child(
                            div()
                                .text_size(px(14.0))
                                .line_height(px(18.0))
                                .font_weight(FontWeight::SEMIBOLD)
                                .child(snapshot.selected_section.label()),
                        )
                        .child(render_icon_button(
                            "settings-modal-close",
                            ArgusIcon::Close,
                            "关闭设置",
                            false,
                            IconButtonSize::Small,
                            &theme,
                            move |_, _, cx| {
                                cx.stop_propagation();
                                update_settings_app(&close_app, cx, |app, _| {
                                    app.close_settings_modal()
                                });
                            },
                        )),
                )
                .child(
                    div()
                        .id("settings-modal-content-scroll")
                        .flex_1()
                        .overflow_y_scroll()
                        .scrollbar_width(px(SETTINGS_MODAL_SCROLLBAR_WIDTH))
                        .p_5()
                        .child(render_selected_settings_section(
                            snapshot,
                            app_handle,
                            input_focus_handles,
                            &theme,
                        )),
                ),
        )
}

/// 渲染设置模态框左侧的分类导航与版本信息。
fn render_settings_sidebar(
    snapshot: &SettingsModalSnapshot,
    app_handle: &Entity<ArgusApp>,
    theme: &AppTheme,
) -> impl IntoElement + use<> {
    div()
        .w(px(SETTINGS_MODAL_SIDEBAR_WIDTH))
        .h_full()
        .flex_none()
        .flex()
        .flex_col()
        .bg(rgb(theme.side_bar))
        .child(
            div()
                .h(px(56.0))
                .px_4()
                .flex()
                .items_center()
                .gap_2()
                .text_size(px(14.0))
                .line_height(px(18.0))
                .font_weight(FontWeight::SEMIBOLD)
                .child(render_icon(
                    ArgusIcon::Settings,
                    theme.foreground_muted,
                    SETTINGS_MODAL_TITLE_ICON_SIZE,
                ))
                .child("设置"),
        )
        .child(
            div()
                .flex_1()
                .px_2()
                .flex()
                .flex_col()
                .child(settings_navigation_group_label("应用", theme))
                .child(settings_navigation_item(
                    SettingsSection::About,
                    ArgusIcon::Info,
                    snapshot.selected_section,
                    app_handle,
                    theme,
                ))
                .child(settings_navigation_item(
                    SettingsSection::Appearance,
                    ArgusIcon::Palette,
                    snapshot.selected_section,
                    app_handle,
                    theme,
                ))
                .child(
                    div()
                        .mt_3()
                        .child(settings_navigation_group_label("智能分析", theme)),
                )
                .child(settings_navigation_item(
                    SettingsSection::AiModel,
                    ArgusIcon::Settings,
                    snapshot.selected_section,
                    app_handle,
                    theme,
                ))
                .child(settings_navigation_item(
                    SettingsSection::AiLogProfiles,
                    ArgusIcon::FileText,
                    snapshot.selected_section,
                    app_handle,
                    theme,
                ))
                .child(settings_navigation_item(
                    SettingsSection::AiSystemPrompt,
                    ArgusIcon::FileText,
                    snapshot.selected_section,
                    app_handle,
                    theme,
                ))
                .child(
                    div()
                        .mt_3()
                        .child(settings_navigation_group_label("日志", theme)),
                )
                .child(settings_navigation_item(
                    SettingsSection::LogDisplay,
                    ArgusIcon::Logs,
                    snapshot.selected_section,
                    app_handle,
                    theme,
                ))
                .child(settings_navigation_item(
                    SettingsSection::LogSearch,
                    ArgusIcon::Search,
                    snapshot.selected_section,
                    app_handle,
                    theme,
                ))
                .child(settings_navigation_item(
                    SettingsSection::LogLoading,
                    ArgusIcon::FolderPlus,
                    snapshot.selected_section,
                    app_handle,
                    theme,
                )),
        )
        .child(
            div()
                .px_4()
                .pb_4()
                .flex()
                .flex_col()
                .gap_1()
                .text_size(px(11.0))
                .text_color(rgb(theme.foreground_muted))
                .child("Argus")
                .child(format!("v{}", env!("CARGO_PKG_VERSION"))),
        )
}

/// 渲染设置导航分组标题。
fn settings_navigation_group_label(label: &'static str, theme: &AppTheme) -> impl IntoElement {
    div()
        .h(px(26.0))
        .px_2()
        .flex()
        .items_center()
        .text_size(px(11.0))
        .text_color(rgb(theme.foreground_muted))
        .child(label)
}

/// 渲染设置左侧导航中的单个分类入口。
fn settings_navigation_item(
    section: SettingsSection,
    icon: ArgusIcon,
    selected_section: SettingsSection,
    app_handle: &Entity<ArgusApp>,
    theme: &AppTheme,
) -> impl IntoElement + use<> {
    let is_selected = section == selected_section;
    let select_app = app_handle.clone();
    let hover_background = theme.current_line;

    div()
        .id(SharedString::from(format!(
            "settings-navigation-{}",
            settings_section_id(section)
        )))
        .h(px(34.0))
        .px_2()
        .flex()
        .items_center()
        .gap_2()
        .rounded_sm()
        .cursor_pointer()
        .bg(rgb(if is_selected {
            theme.selection
        } else {
            theme.side_bar
        }))
        .hover(move |this| this.bg(rgb(hover_background)))
        .text_size(px(13.0))
        .text_color(rgb(theme.foreground))
        .child(render_icon(
            icon,
            if is_selected {
                theme.foreground
            } else {
                theme.foreground_muted
            },
            15.0,
        ))
        .child(section.label())
        .on_click(move |_, _, cx| {
            cx.stop_propagation();
            update_settings_app(&select_app, cx, |app, _| {
                app.select_settings_section(section)
            });
        })
}

/// 返回设置分类对应的稳定元素 ID 片段。
fn settings_section_id(section: SettingsSection) -> &'static str {
    match section {
        SettingsSection::About => "about",
        SettingsSection::Appearance => "appearance",
        SettingsSection::AiModel => "ai-model",
        SettingsSection::AiLogProfiles => "ai-log-profiles",
        SettingsSection::AiSystemPrompt => "ai-system-prompt",
        SettingsSection::LogDisplay => "log-display",
        SettingsSection::LogSearch => "log-search",
        SettingsSection::LogLoading => "log-loading",
    }
}

/// 根据左侧选中分类渲染右侧设置内容，已有设置行和控件尺寸保持不变。
fn render_selected_settings_section(
    snapshot: &SettingsModalSnapshot,
    app_handle: &Entity<ArgusApp>,
    input_focus_handles: &SettingsInputFocusHandles,
    theme: &AppTheme,
) -> AnyElement {
    match snapshot.selected_section {
        SettingsSection::About => div()
            .w_full()
            .flex()
            .flex_col()
            .gap_5()
            .child(render_about_section(snapshot, app_handle, theme))
            .when(SHOW_UPGRADE_SETTINGS_ENTRIES, |this| {
                this.child(settings_section(
                    "升级",
                    ArgusIcon::Refresh,
                    render_upgrade_section(snapshot, app_handle, input_focus_handles, theme),
                    theme,
                ))
            })
            .into_any_element(),
        SettingsSection::Appearance => {
            render_appearance_section(snapshot, app_handle, theme).into_any_element()
        }
        SettingsSection::AiModel => {
            render_ai_model_section(snapshot, app_handle, theme).into_any_element()
        }
        SettingsSection::AiLogProfiles => {
            render_ai_log_profiles_section(snapshot, app_handle, theme).into_any_element()
        }
        SettingsSection::AiSystemPrompt => {
            render_ai_system_prompt_section(snapshot, app_handle, theme).into_any_element()
        }
        SettingsSection::LogDisplay => {
            render_log_display_section(snapshot, app_handle, input_focus_handles, theme)
                .into_any_element()
        }
        SettingsSection::LogSearch => {
            render_log_search_section(snapshot, app_handle, input_focus_handles, theme)
                .into_any_element()
        }
        SettingsSection::LogLoading => {
            render_log_loading_section(snapshot, app_handle, theme).into_any_element()
        }
    }
}

/// 渲染智能分析分组下的模型配置页面。
fn render_ai_model_section(
    snapshot: &SettingsModalSnapshot,
    app_handle: &Entity<ArgusApp>,
    theme: &AppTheme,
) -> AnyElement {
    let add_model_app = app_handle.clone();
    let list_theme = theme.clone();
    settings_section(
        "模型配置",
        ArgusIcon::Settings,
        setting_group(theme)
            .child(setting_row(
                "当前状态",
                ai_settings_entry_control(
                    format!(
                        "已配置 {} 个模型 · 可选择 {} 个",
                        snapshot.ai_model_profiles.len(),
                        snapshot
                            .ai_model_profiles
                            .iter()
                            .filter(|model| model.enabled)
                            .count()
                    ),
                    "新增模型",
                    "settings-add-ai-model",
                    ArgusIcon::Settings,
                    theme,
                    move |cx| {
                        update_settings_app(&add_model_app, cx, |app, app_cx| {
                            app.open_ai_model_editor(None, app_cx)
                        });
                    },
                ),
                theme,
            ))
            .when(snapshot.ai_model_profiles.is_empty(), |this| {
                this.child(ai_settings_empty_row(
                    "尚未配置模型，新增后才能使用智能分析。",
                    &list_theme,
                ))
            })
            .children(
                snapshot
                    .ai_model_profiles
                    .iter()
                    .enumerate()
                    .map(|(index, model)| {
                        let edit_app = app_handle.clone();
                        let row_theme = theme.clone();
                        let summary = format!(
                            "{} · {} · 上下文 {} · {}",
                            if model.enabled { "启用" } else { "停用" },
                            model.model,
                            model.context_window_label(),
                            model.base_url
                        );
                        div()
                            .id(("settings-ai-model-row", index))
                            .min_h(px(52.0))
                            .px_3()
                            .py_2()
                            .flex()
                            .items_center()
                            .justify_between()
                            .gap_3()
                            .rounded_sm()
                            .bg(rgb(theme.current_line))
                            .cursor_pointer()
                            .hover(move |this| this.bg(rgb(row_theme.selection)))
                            .child(
                                div()
                                    .min_w(px(0.0))
                                    .flex_1()
                                    .flex()
                                    .flex_col()
                                    .gap_1()
                                    .child(
                                        div()
                                            .text_size(px(13.0))
                                            .font_weight(FontWeight::MEDIUM)
                                            .child(model.name.clone()),
                                    )
                                    .child(
                                        div()
                                            .truncate()
                                            .text_size(px(11.0))
                                            .text_color(rgb(theme.foreground_muted))
                                            .child(summary),
                                    ),
                            )
                            .child(render_icon(ArgusIcon::Expand, theme.foreground_muted, 14.0))
                            .on_click(move |_, _, cx| {
                                cx.stop_propagation();
                                update_settings_app(&edit_app, cx, |app, app_cx| {
                                    app.open_ai_model_editor(Some(index), app_cx);
                                });
                            })
                    }),
            ),
        theme,
    )
    .into_any_element()
}

/// 渲染智能分析分组下的日志类型说明页面。
fn render_ai_log_profiles_section(
    snapshot: &SettingsModalSnapshot,
    app_handle: &Entity<ArgusApp>,
    theme: &AppTheme,
) -> AnyElement {
    let add_profile_app = app_handle.clone();
    let list_theme = theme.clone();
    settings_section(
        "日志类型说明",
        ArgusIcon::FileText,
        setting_group(theme)
            .child(setting_row(
                "当前配置",
                ai_settings_entry_control(
                    format!("{} 个日志类型", snapshot.ai_log_profiles.len()),
                    "新增",
                    "settings-add-ai-log-profile",
                    ArgusIcon::FileText,
                    theme,
                    move |cx| {
                        update_settings_app(&add_profile_app, cx, |app, app_cx| {
                            app.open_ai_log_profile_editor(None, app_cx)
                        });
                    },
                ),
                theme,
            ))
            .when(snapshot.ai_log_profiles.is_empty(), |this| {
                this.child(ai_settings_empty_row(
                    "尚未配置日志类型说明，Agent 将仅依赖自动识别结果。",
                    &list_theme,
                ))
            })
            .children(
                snapshot
                    .ai_log_profiles
                    .iter()
                    .enumerate()
                    .map(|(index, profile)| {
                        let edit_app = app_handle.clone();
                        let row_theme = theme.clone();
                        div()
                            .id(("settings-ai-log-profile-row", index))
                            .min_h(px(52.0))
                            .px_3()
                            .py_2()
                            .flex()
                            .items_center()
                            .justify_between()
                            .gap_3()
                            .rounded_sm()
                            .bg(rgb(theme.current_line))
                            .cursor_pointer()
                            .hover(move |this| this.bg(rgb(row_theme.selection)))
                            .child(
                                div()
                                    .min_w(px(0.0))
                                    .flex_1()
                                    .flex()
                                    .flex_col()
                                    .gap_1()
                                    .child(
                                        div()
                                            .text_size(px(13.0))
                                            .font_weight(FontWeight::MEDIUM)
                                            .child(profile.name.clone()),
                                    )
                                    .child(
                                        div()
                                            .text_size(px(11.0))
                                            .text_color(rgb(theme.foreground_muted))
                                            .child(format!(
                                                "{} · {} 条名称规则 · 优先级 {}",
                                                if profile.enabled { "启用" } else { "停用" },
                                                profile.matchers.len(),
                                                profile.priority
                                            )),
                                    ),
                            )
                            .child(render_icon(ArgusIcon::Expand, theme.foreground_muted, 14.0))
                            .on_click(move |_, _, cx| {
                                cx.stop_propagation();
                                update_settings_app(&edit_app, cx, |app, app_cx| {
                                    app.open_ai_log_profile_editor(Some(index), app_cx);
                                });
                            })
                    }),
            ),
        theme,
    )
    .into_any_element()
}

/// 渲染智能分析分组下的默认系统提示词入口。
fn render_ai_system_prompt_section(
    snapshot: &SettingsModalSnapshot,
    app_handle: &Entity<ArgusApp>,
    theme: &AppTheme,
) -> AnyElement {
    let edit_prompt_app = app_handle.clone();
    let character_count = snapshot.ai_system_prompt.chars().count();
    settings_section(
        "系统提示词",
        ArgusIcon::FileText,
        setting_group(theme).child(setting_row(
            "默认专业提示",
            ai_settings_entry_control(
                format!("已配置 {character_count} 个字符"),
                "编辑",
                "settings-edit-ai-system-prompt",
                ArgusIcon::FileText,
                theme,
                move |cx| {
                    update_settings_app(&edit_prompt_app, cx, |app, app_cx| {
                        app.open_ai_system_prompt_editor(app_cx)
                    });
                },
            ),
            theme,
        )),
        theme,
    )
    .into_any_element()
}

/// 渲染模型或日志类型列表的空状态，和其它设置行保持相同高度、背景及文字层级。
fn ai_settings_empty_row(message: &'static str, theme: &AppTheme) -> impl IntoElement {
    div()
        .min_h(px(SETTINGS_ROW_MIN_HEIGHT))
        .px_3()
        .flex()
        .items_center()
        .rounded_sm()
        .bg(rgb(theme.current_line))
        .text_size(px(12.0))
        .text_color(rgb(theme.foreground_muted))
        .child(message)
}

/// 渲染智能分析设置行右侧的摘要和配置按钮。
fn ai_settings_entry_control(
    summary: String,
    button_label: &'static str,
    button_id: &'static str,
    icon: ArgusIcon,
    theme: &AppTheme,
    action: impl Fn(&mut App) + 'static,
) -> impl IntoElement {
    let button_theme = theme.clone();
    div()
        .min_w(px(0.0))
        .flex()
        .items_center()
        .justify_end()
        .gap_3()
        .child(
            div()
                .max_w(px(280.0))
                .truncate()
                .text_size(px(12.0))
                .text_color(rgb(theme.foreground_muted))
                .child(summary),
        )
        .child(
            div()
                .id(button_id)
                .h(px(30.0))
                .px_3()
                .flex()
                .items_center()
                .justify_center()
                .gap_1()
                .rounded_sm()
                .bg(rgb(theme.current_line))
                .text_size(px(12.0))
                .text_color(rgb(theme.foreground))
                .cursor_pointer()
                .hover(move |this| this.bg(rgb(button_theme.selection)))
                .child(render_icon(icon, theme.foreground_muted, 13.0))
                .child(button_label)
                .on_click(move |_, _, cx| {
                    cx.stop_propagation();
                    action(cx);
                }),
        )
}

/// 渲染 Jstack 线程段过滤大编辑器窗口。
fn render_jstack_stack_segment_filter_editor_window(
    snapshot: &JstackStackSegmentFilterEditorSnapshot,
    app_handle: &Entity<ArgusApp>,
    focus_handles: &JstackStackSegmentFilterEditorFocusHandles,
    _window: &mut Window,
    _cx: &mut Context<JstackStackSegmentFilterEditorWindow>,
) -> impl IntoElement + use<> {
    let theme = snapshot.theme.clone();
    let close_app = app_handle.clone();
    let root_focus_for_track = focus_handles.root.clone();
    let root_focus_for_click = focus_handles.root.clone();

    div()
        .id("jstack-stack-segment-editor-root")
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
                .occlude()
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .text_size(px(14.0))
                        .line_height(px(18.0))
                        .font_weight(FontWeight::SEMIBOLD)
                        .child(render_icon(
                            ArgusIcon::FileText,
                            theme.foreground_muted,
                            JSTACK_STACK_SEGMENT_EDITOR_TITLE_ICON_SIZE,
                        ))
                        .child("线程段过滤编辑"),
                )
                .child(render_icon_button(
                    "jstack-stack-segment-editor-close",
                    ArgusIcon::Close,
                    "关闭编辑器",
                    false,
                    IconButtonSize::Small,
                    &theme,
                    move |_, window, cx| {
                        cx.stop_propagation();
                        update_settings_app(&close_app, cx, |app, _| {
                            app.close_jstack_stack_segment_filter_editor();
                        });
                        window.remove_window();
                    },
                )),
        )
        .child(
            div()
                .flex_1()
                .min_h(px(0.0))
                .px_5()
                .pb_5()
                .flex()
                .flex_col()
                .gap_3()
                .child(
                    div()
                        .text_size(px(12.0))
                        .line_height(px(18.0))
                        .text_color(rgb(theme.foreground_muted))
                        .child("每个完整线程段用空行分隔；内容会自动保存并立即作用于线程日志分析过滤。"),
                )
                .child(render_jstack_stack_segment_editor_textarea(
                    snapshot,
                    app_handle,
                    focus_handles,
                    &theme,
                )),
        )
}

/// 渲染 Jstack 线程段过滤编辑器中的大 textarea。
fn render_jstack_stack_segment_editor_textarea(
    snapshot: &JstackStackSegmentFilterEditorSnapshot,
    app_handle: &Entity<ArgusApp>,
    focus_handles: &JstackStackSegmentFilterEditorFocusHandles,
    theme: &AppTheme,
) -> impl IntoElement + use<> {
    let input_state = snapshot.input.clone();
    let key_app = app_handle.clone();
    let click_app = app_handle.clone();
    let pointer_app = app_handle.clone();
    let clear_app = app_handle.clone();
    let native_input = app_native_input(
        app_handle.clone(),
        AppTextInputTarget::SettingsJstackStackSegmentFilter,
        focus_handles.textarea.clone(),
    );

    div()
        .w_full()
        .flex_1()
        .min_h(px(0.0))
        .flex()
        .flex_col()
        .child(render_textarea(
        Textarea {
            id: "jstack-stack-segment-editor-textarea",
            placeholder: "SocketInputStream.socketRead\n    at java.net.SocketInputStream.read\n\nUnsafe.park",
            value: input_state.value.clone(),
            is_disabled: false,
            is_focused: input_state.is_focused,
            cursor_index: input_state.cursor,
            selection_range: settings_input_selection_range(&input_state),
            marked_range: input_state.marked_range.clone(),
            is_pointer_selecting: input_state.selection_drag.is_some(),
            visible_lines: JSTACK_STACK_SEGMENT_EDITOR_VISIBLE_LINES,
            fill_height: true,
            scroll_handle: focus_handles.textarea_scroll.clone(),
            scroll_state: focus_handles.textarea_scroll_state.clone(),
            style: TextareaStyle::Default,
            trailing_accessory: Some(InputAccessory {
                id: "jstack-stack-segment-editor-clear",
                icon: ArgusIcon::Close,
                tooltip: "清空线程段过滤",
            }),
            trailing_accessory_position: TextareaAccessoryPosition::TopRight,
            trailing_accessory_always_visible: false,
            trailing_accessory_selected: false,
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

/// 渲染外观分组内的主题下拉菜单浮层。
///
/// 说明：菜单作为外观分组的绝对定位子节点渲染，不参与布局计算，因此不会撑开设置行；
/// 同时它使用分组局部坐标，避免窗口滚动或上方内容变化导致菜单位置漂移。
fn render_theme_dropdown_menu(
    snapshot: &SettingsModalSnapshot,
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

/// 渲染关于设置区；升级入口当前隐藏，仅保留版本和平台信息。
fn render_about_section(
    snapshot: &SettingsModalSnapshot,
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
        .when(SHOW_UPGRADE_SETTINGS_ENTRIES, |this| {
            this.child(setting_row(
                "检查更新",
                upgrade_check_control(snapshot, app_handle, &message, theme),
                theme,
            ))
        })
}

/// 渲染外观设置区。
fn render_appearance_section(
    snapshot: &SettingsModalSnapshot,
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
    snapshot: &SettingsModalSnapshot,
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
            jstack_stack_segment_filter_input_control(snapshot, app_handle, theme),
            theme,
        ))
}

/// 渲染日志搜索设置区。
fn render_log_search_section(
    snapshot: &SettingsModalSnapshot,
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
    snapshot: &SettingsModalSnapshot,
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
    snapshot: &SettingsModalSnapshot,
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
    snapshot: &SettingsModalSnapshot,
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
            is_secret: false,
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
    snapshot: &SettingsModalSnapshot,
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
            is_secret: false,
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

/// 渲染 Jstack 完整线程段过滤配置摘要和编辑入口。
fn jstack_stack_segment_filter_input_control(
    snapshot: &SettingsModalSnapshot,
    app_handle: &Entity<ArgusApp>,
    theme: &AppTheme,
) -> impl IntoElement + use<> {
    let input_state = snapshot.jstack_stack_segment_filter_input.clone();
    let clear_app = app_handle.clone();
    let edit_app = app_handle.clone();
    let is_empty = input_state.value.trim().is_empty();
    let summary = jstack_stack_segment_filter_summary(&input_state.value);

    div()
        .w(px(360.0))
        .flex()
        .items_center()
        .justify_end()
        .gap_2()
        .child(
            div()
                .max_w(px(180.0))
                .h(px(28.0))
                .px_2()
                .flex()
                .items_center()
                .rounded_sm()
                .bg(rgb(theme.content))
                .text_size(px(12.0))
                .line_height(px(28.0))
                .text_color(rgb(if is_empty {
                    theme.foreground_muted
                } else {
                    theme.foreground
                }))
                .child(div().truncate().child(summary)),
        )
        .child(registration_action_button(
            "settings-jstack-stack-segment-filter-clear",
            "清空",
            ArgusIcon::Close,
            is_empty,
            theme,
            move |cx| {
                update_settings_app(&clear_app, cx, |app, _| {
                    app.clear_settings_jstack_stack_segment_filter_input();
                });
            },
        ))
        .child(registration_action_button(
            "settings-jstack-stack-segment-filter-edit",
            "编辑",
            ArgusIcon::FileText,
            false,
            theme,
            move |cx| {
                update_settings_app(&edit_app, cx, |app, app_cx| {
                    app.open_jstack_stack_segment_filter_editor(app_cx);
                });
            },
        ))
}

/// 渲染升级服务器配置输入框。
fn upgrade_server_input_control(
    snapshot: &SettingsModalSnapshot,
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
            is_secret: false,
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
    snapshot: &SettingsModalSnapshot,
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
            is_secret: false,
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
fn settings_input_selection_range(input: &TextInputState) -> Option<std::ops::Range<usize>> {
    input.selection_range()
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

/// 汇总 Jstack 完整线程段过滤配置，供设置页行内展示。
fn jstack_stack_segment_filter_summary(value: &str) -> String {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return "未配置".to_string();
    }

    // 线程段过滤以空行分隔，兼容旧版 `||` 分隔，摘要帮助用户判断配置规模。
    let segment_count = stack_segment_filter_blocks_for_summary(trimmed).len();
    let line_count = trimmed
        .lines()
        .filter(|line| !line.trim().is_empty())
        .count();
    format!("{segment_count} 段，{line_count} 行")
}

/// 按当前线程段过滤规则统计配置块数量；旧版 `||` 仅用于兼容历史配置展示。
fn stack_segment_filter_blocks_for_summary(value: &str) -> Vec<String> {
    let value = value.replace("||", "\n\n");
    split_stack_segment_filter_blocks(&value)
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
    snapshot: &SettingsModalSnapshot,
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
    snapshot: &SettingsModalSnapshot,
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

/// 渲染设置模态框里带图标的紧凑文字按钮。
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

/// 统一更新主应用状态；设置模态框只负责表现，不直接持有业务配置。
fn update_settings_app(
    app_handle: &Entity<ArgusApp>,
    cx: &mut App,
    update: impl FnOnce(&mut ArgusApp, &mut Context<ArgusApp>),
) {
    app_handle.update(cx, |app, app_cx| {
        update(app, app_cx);
        app_cx.notify();
    });
}
