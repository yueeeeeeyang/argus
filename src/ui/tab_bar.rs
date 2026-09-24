//! 文件职责：渲染自定义标题栏中的日志标签区域。
//! 创建日期：2026-06-09
//! 修改日期：2026-09-24
//! 作者：Argus 开发团队
//! 主要功能：展示可切换标签、右键菜单和多标签溢出下拉入口。

use std::ops::Range;

use crate::app::{ArgusApp, JstackAnalysisTaskState, RuntimeAnalysisTaskState, TabKind};
use crate::platform::custom_titlebar;
use crate::reader::log_file_reader::LogOpenState;
use crate::theme::AppTheme;
use crate::ui::components::context_menu::ActiveMenuKind;
use crate::ui::components::icon::{ArgusIcon, render_icon};
use crate::ui::components::icon_button::{IconButtonSize, render_icon_button};
use crate::ui::components::loading_spinner::render_loading_spinner;
#[cfg(not(target_os = "windows"))]
use gpui::WindowControlArea;
use gpui::{
    ClickEvent, Context, IntoElement, MouseButton, MouseDownEvent, MouseUpEvent, SharedString,
    Window, div, prelude::*, px, rgb,
};

/// 标签标题字号，保持标题栏紧凑密度。
const TAB_TITLE_FONT_SIZE: f32 = 12.0;
/// 标签块高度；对齐 opencode v2 客户端标签（h-7 = 28px）。
const TAB_BLOCK_HEIGHT: f32 = 28.0;
/// 标签块距标题栏底边的留白；与标签上方留白一致（标题栏 44px、标签 28px 时为 8px）。
const TAB_BOTTOM_INSET: f32 = 8.0;
/// 相邻标签块之间的水平间距（像素）。
const TAB_GAP: f32 = 6.0;
/// 标签块圆角半径（像素）。
const TAB_BLOCK_RADIUS: f32 = 6.0;
/// 普通标签最小宽度；短标题标签在空间充足时可保持紧凑。
const TAB_MIN_WIDTH: f32 = 72.0;
/// 极窄窗口下的兜底宽度，优先保证不突破可视区域。
const TAB_EMERGENCY_MIN_WIDTH: f32 = 48.0;
/// 普通标签最大宽度。
const TAB_MAX_WIDTH: f32 = 230.0;
/// 下拉按钮占位宽度；按钮始终展示，便于从固定入口查看全部标签。
const TAB_OVERFLOW_BUTTON_WIDTH: f32 = 32.0;
/// Agent 助手面板开关占位宽度；固定放在全部标签按钮右侧。
const TAB_ASSISTANT_BUTTON_WIDTH: f32 = 32.0;
/// 标签区与右侧下拉按钮之间的固定间距；同时也是标题栏拖拽空白的最小宽度。
const TAB_OVERFLOW_BUTTON_GAP: f32 = 8.0;
/// 标题栏中标签栏左侧外部留白，对应 `custom_title_bar` 中的间距（与玻璃板内边距一致）。
const TAB_EXTERNAL_LEFT_GAP: f32 = 8.0;
/// 标题栏右侧固定按钮与窗口右边缘的间距，对应 `custom_title_bar` 中的间距。
const TAB_EXTERNAL_RIGHT_GAP: f32 = 12.0;
/// 来源侧栏折叠时，紧凑标题栏在标签栏左侧占用的宽度：
/// 栏内左内边距 12 + 窗口控制占位 76 + 间距 8 + 展开按钮 28 + 组件间距 8。
const COMPACT_LEFT_CONTROLS_WIDTH: f32 = 12.0 + 76.0 + 8.0 + 28.0 + 8.0;
/// 关闭按钮固定占位宽度，避免 hover 时插入按钮撑宽标签。
const TAB_CLOSE_SLOT_WIDTH: f32 = 18.0;
/// 标签关闭按钮命中区尺寸，比通用标题栏按钮更紧凑。
const TAB_CLOSE_BUTTON_SIZE: f32 = 18.0;
/// 标签关闭图标尺寸，匹配 12px 标题文本。
const TAB_CLOSE_ICON_SIZE: f32 = 13.0;
/// 标签标题前加载动画尺寸，需低于标签文字行高以避免撑高标题栏。
const TAB_LOADING_SPINNER_SIZE: f32 = 12.0;
/// 标题文本宽度估算后额外保留的内边距和关闭按钮槽宽度。
const TAB_TITLE_CHROME_WIDTH: f32 = 44.0;
/// ASCII 字符在 12px 标题字号下的平均宽度估算。
const ASCII_TITLE_CHAR_WIDTH: f32 = 7.0;
/// CJK 等非 ASCII 字符在 12px 标题字号下的平均宽度估算。
const WIDE_TITLE_CHAR_WIDTH: f32 = 12.0;

/// 标签栏布局计算结果。
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct TabBarLayout {
    /// 需要直接渲染在标题栏上的标签范围。
    pub visible_range: Range<usize>,
    /// 每个可见标签的计算宽度，与 `visible_range` 顺序一致。
    pub visible_widths: Vec<f32>,
    /// 是否存在未直接渲染的隐藏标签。
    pub has_overflow: bool,
    /// 可见标签整体占用宽度，不包含右侧拖拽区和下拉按钮。
    pub tabs_width: f32,
}

/// 渲染标题栏中的当前标签区域。
///
/// 参数说明：
/// - `app`：应用状态，用于读取主题、标签和菜单状态。
/// - `window`：当前窗口，用于估算标题栏可用宽度。
/// - `cx`：应用上下文，用于绑定切换、关闭、右键菜单和溢出菜单。
///
/// 返回值：GPUI 元素树；不包含新增标签页和拖拽排序入口。
pub(crate) fn render(
    app: &ArgusApp,
    window: &mut Window,
    cx: &mut Context<ArgusApp>,
) -> impl IntoElement {
    let theme = app.theme.clone();
    let tabs = app.tabs.clone();
    let active_tab_id = app.active_tab_id;
    let hovered_tab_id = app.hovered_tab_id;
    let active_index = tabs
        .iter()
        .position(|tab| tab.id == active_tab_id)
        .unwrap_or(0);
    let layout = calculate_tab_layout(&tabs, active_index, available_tab_bar_width(app, window));
    let overflow_selected = matches!(
        app.active_menu.as_ref().map(|menu| &menu.kind),
        Some(ActiveMenuKind::TabOverflow)
    );
    let visible_tabs = tabs[layout.visible_range.clone()]
        .iter()
        .cloned()
        .zip(layout.visible_widths.iter().copied())
        .collect::<Vec<_>>();

    div()
        .h_full()
        .w_full()
        .flex()
        .items_center()
        .overflow_hidden()
        .child(
            div()
                .h_full()
                .w(px(layout.tabs_width))
                .flex_none()
                .flex()
                .items_end()
                // 标签块下方留白：与标签上方留白一致。
                .pb(px(TAB_BOTTOM_INSET))
                .gap(px(TAB_GAP))
                .overflow_hidden()
                .children(
                    visible_tabs
                        .into_iter()
                        .map(|(tab, tab_width)| {
                            let is_loading = is_tab_loading(app, &tab.kind);
                            render_tab(
                                tab,
                                tab_width,
                                active_tab_id,
                                hovered_tab_id,
                                is_loading,
                                &theme,
                                cx,
                            )
                            .into_any_element()
                        })
                        .collect::<Vec<_>>(),
                ),
        )
        // 拖拽空白兼作标签区与右侧下拉按钮之间的最小间距：标签未铺满时它吸收剩余宽度，
        // 下拉按钮与助手面板按钮因此始终固定在标题栏右侧。
        .child(tab_drag_area(cx))
        .child(render_overflow_button(overflow_selected, &theme, cx))
        .child(render_assistant_panel_button(app, &theme, cx))
}

/// 根据窗口宽度估算标签栏可用空间。
fn available_tab_bar_width(app: &ArgusApp, window: &Window) -> f32 {
    let viewport_width = window.viewport_size().width / px(1.0);
    let left_reserved_width = if app.is_source_panel_collapsed {
        COMPACT_LEFT_CONTROLS_WIDTH
    } else {
        app.current_source_panel_width()
    };

    (viewport_width - left_reserved_width - TAB_EXTERNAL_LEFT_GAP - TAB_EXTERNAL_RIGHT_GAP)
        .max(TAB_EMERGENCY_MIN_WIDTH)
}

/// 计算标签直接显示范围与压缩后的标签宽度。
///
/// 说明：标签之间固定留出 `TAB_GAP` 间距，因此总宽度、可见数量和压缩后的宽度
/// 都必须把间距计入，否则标签会溢出预留区域或错误触发溢出菜单。
pub(crate) fn calculate_tab_layout(
    tabs: &[crate::app::ArgusTab],
    active_index: usize,
    available_width: f32,
) -> TabBarLayout {
    let tab_count = tabs.len();
    if tab_count == 0 {
        return TabBarLayout {
            visible_range: 0..0,
            visible_widths: Vec::new(),
            has_overflow: false,
            tabs_width: 0.0,
        };
    }

    let tab_area_width = (available_width
        - TAB_OVERFLOW_BUTTON_WIDTH
        - TAB_ASSISTANT_BUTTON_WIDTH
        - TAB_OVERFLOW_BUTTON_GAP)
        .max(TAB_EMERGENCY_MIN_WIDTH);
    let ideal_widths = tabs
        .iter()
        .map(|tab| ideal_tab_width(&tab.title))
        .collect::<Vec<_>>();
    let ideal_total_width: f32 = ideal_widths.iter().sum::<f32>() + tab_gaps_width(tab_count);

    if ideal_total_width <= tab_area_width {
        // 标签可全部展示时按内容宽度渲染，不再均分剩余宽度铺满标签区：
        // 剩余空间由渲染层的拖拽空白（flex_1）吸收，右侧下拉按钮仍固定在标题栏右侧。
        return TabBarLayout {
            visible_range: 0..tab_count,
            visible_widths: ideal_widths,
            has_overflow: false,
            tabs_width: ideal_total_width,
        };
    }

    // 每个标签除最小宽度外还要占用一个间距，因此按“最小宽度 + 间距”估算可见数量。
    let visible_count = (((tab_area_width + TAB_GAP) / (TAB_MIN_WIDTH + TAB_GAP)).floor() as usize)
        .max(1)
        .min(tab_count);
    let safe_active_index = active_index.min(tab_count - 1);
    let mut start = safe_active_index.saturating_sub(visible_count / 2);
    if start + visible_count > tab_count {
        start = tab_count - visible_count;
    }
    let end = start + visible_count;
    let visible_ideal_widths = ideal_widths[start..end].to_vec();
    let visible_widths = fit_tab_widths(
        &visible_ideal_widths,
        tab_area_width - tab_gaps_width(end - start),
    );
    let tabs_width = visible_widths.iter().sum::<f32>() + tab_gaps_width(end - start);

    TabBarLayout {
        visible_range: start..end,
        visible_widths,
        has_overflow: end - start < tab_count,
        tabs_width,
    }
}

/// 返回指定数量标签之间占用的间距总宽度。
fn tab_gaps_width(tab_count: usize) -> f32 {
    TAB_GAP * tab_count.saturating_sub(1) as f32
}

/// 根据标题估算空间充足时的标签宽度。
fn ideal_tab_width(title: &str) -> f32 {
    let title_width = title
        .chars()
        .map(|character| {
            if character.is_ascii() {
                ASCII_TITLE_CHAR_WIDTH
            } else {
                WIDE_TITLE_CHAR_WIDTH
            }
        })
        .sum::<f32>();

    (title_width + TAB_TITLE_CHROME_WIDTH).clamp(TAB_MIN_WIDTH, TAB_MAX_WIDTH)
}

/// 判断指定标签是否处于后台加载或分析中，用于在标签标题前展示转动状态。
fn is_tab_loading(app: &ArgusApp, kind: &TabKind) -> bool {
    match kind {
        TabKind::LogSource { source_id, .. } => {
            matches!(
                app.log_read_state(*source_id),
                Some(LogOpenState::Loading { .. })
            )
        }
        TabKind::JstackAnalysis { analysis_id } => app
            .jstack_analysis_state(*analysis_id)
            .is_some_and(|state| {
                matches!(state.task_state, JstackAnalysisTaskState::Loading { .. })
            }),
        TabKind::RuntimeAnalysis { analysis_id } => app
            .runtime_analysis_state(*analysis_id)
            .is_some_and(|state| {
                matches!(state.task_state, RuntimeAnalysisTaskState::Loading { .. })
            }),
        TabKind::Empty | TabKind::SshTerminal { .. } | TabKind::RemoteFileManager { .. } => false,
    }
}

/// 将一组理想标签宽度压缩到可用范围内，避免标题栏溢出。
fn fit_tab_widths(ideal_widths: &[f32], available_width: f32) -> Vec<f32> {
    if ideal_widths.is_empty() {
        return Vec::new();
    }

    let ideal_total_width: f32 = ideal_widths.iter().sum();
    if ideal_total_width <= available_width {
        return ideal_widths.to_vec();
    }

    let average_width =
        (available_width / ideal_widths.len() as f32).clamp(TAB_EMERGENCY_MIN_WIDTH, TAB_MAX_WIDTH);
    vec![average_width; ideal_widths.len()]
}

/// 渲染单个可切换、可关闭、可打开右键菜单的标签。
fn render_tab(
    tab: crate::app::ArgusTab,
    tab_width: f32,
    active_tab_id: usize,
    hovered_tab_id: Option<usize>,
    is_loading: bool,
    theme: &AppTheme,
    cx: &mut Context<ArgusApp>,
) -> impl IntoElement {
    let tab_id = tab.id;
    let is_active = tab_id == active_tab_id;
    let is_hovered = hovered_tab_id == Some(tab_id);
    let should_show_close = is_active || is_hovered;
    // 标签统一为标题栏内的灰色矩形块，与下方内容玻璃板之间留出窗口背景色间距。
    let background = if is_active {
        theme.current_line
    } else if is_hovered {
        theme.status_bar
    } else {
        theme.title_bar
    };
    let height = TAB_BLOCK_HEIGHT;
    let foreground = if is_active || is_hovered {
        theme.foreground
    } else {
        theme.foreground_muted
    };

    div()
        .id(SharedString::from(format!("tab-{tab_id}")))
        .debug_selector(move || format!("tab-{tab_id}"))
        .w(px(tab_width))
        .h(px(height))
        .flex_none()
        .flex()
        .items_end()
        .cursor_pointer()
        .occlude()
        .child(
            div()
                .h(px(height))
                .min_w(px(0.0))
                .flex_1()
                .relative()
                .pl_3()
                .pr_1()
                .flex()
                .items_center()
                .gap_1()
                .rounded(px(TAB_BLOCK_RADIUS))
                .bg(rgb(background))
                .text_color(rgb(foreground))
                .when(is_loading, |this| {
                    this.child(render_loading_spinner(
                        ("tab-loading-spinner", tab_id),
                        foreground,
                        TAB_LOADING_SPINNER_SIZE,
                    ))
                })
                .child(
                    div()
                        .min_w(px(0.0))
                        .flex_1()
                        .truncate()
                        .text_size(px(TAB_TITLE_FONT_SIZE))
                        .child(tab.title),
                )
                .child(render_tab_close_slot(tab_id, should_show_close, theme, cx)),
        )
        .on_hover(cx.listener(move |app, is_hovered: &bool, _, cx| {
            if app.set_hovered_tab(tab_id, *is_hovered) {
                cx.notify();
            }
        }))
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(|_, _: &MouseDownEvent, _, cx| {
                // 标签页属于交互区域，按下阶段即阻止事件落到标题栏拖拽层。
                cx.stop_propagation();
            }),
        )
        .on_mouse_up(
            MouseButton::Right,
            cx.listener(move |app, event: &MouseUpEvent, _, cx| {
                app.open_tab_context_menu(tab_id, event.position);
                cx.stop_propagation();
                cx.notify();
            }),
        )
        .on_click(cx.listener(move |app, event: &ClickEvent, _, cx| {
            cx.stop_propagation();
            if event.standard_click() && app.active_tab_id != tab_id {
                app.activate_tab_with_context(tab_id, cx);
                cx.notify();
            }
        }))
}

/// 渲染固定宽度的关闭按钮槽；按钮显隐不改变标签整体宽度。
fn render_tab_close_slot(
    tab_id: usize,
    should_show_close: bool,
    theme: &AppTheme,
    cx: &mut Context<ArgusApp>,
) -> impl IntoElement {
    let close_hover_background = theme.border;
    let close_foreground = theme.foreground_muted;

    div()
        .w(px(TAB_CLOSE_SLOT_WIDTH))
        .h(px(TAB_CLOSE_BUTTON_SIZE))
        .flex_none()
        .flex()
        .items_center()
        .justify_end()
        .when(should_show_close, |this| {
            this.child(
                div()
                    .id(SharedString::from(format!("tab-close-{tab_id}")))
                    .w(px(TAB_CLOSE_BUTTON_SIZE))
                    .h(px(TAB_CLOSE_BUTTON_SIZE))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded_sm()
                    .hover(move |this| this.bg(rgb(close_hover_background)))
                    .child(render_icon(
                        ArgusIcon::Close,
                        close_foreground,
                        TAB_CLOSE_ICON_SIZE,
                    ))
                    .on_mouse_up(
                        MouseButton::Right,
                        cx.listener(move |app, event: &MouseUpEvent, _, cx| {
                            app.open_tab_context_menu(tab_id, event.position);
                            cx.stop_propagation();
                            cx.notify();
                        }),
                    )
                    .on_click(cx.listener(move |app, event: &ClickEvent, _, cx| {
                        if event.standard_click() {
                            cx.stop_propagation();
                            app.close_tab_with_context(tab_id, cx);
                            cx.notify();
                        }
                    })),
            )
        })
}

/// 渲染标签区域右侧的标题栏拖拽空白，并支持双击最大化或还原。
fn tab_drag_area(cx: &mut Context<ArgusApp>) -> impl IntoElement {
    let drag_area = div()
        .id("tab-bar-drag-area")
        .debug_selector(|| "tab-bar-drag-area".to_string())
        .h_full()
        .min_w(px(TAB_OVERFLOW_BUTTON_GAP))
        .flex_1();
    // Windows 需要普通客户区事件来调用显式 HWND 拖动；其余平台保留 GPUI 控制区语义。
    #[cfg(not(target_os = "windows"))]
    let drag_area = drag_area.window_control_area(WindowControlArea::Drag);

    drag_area.on_mouse_down(
        MouseButton::Left,
        cx.listener(|app, event: &MouseDownEvent, window, cx| {
            match event.click_count {
                1 => custom_titlebar::start_window_drag(window),
                2 => {
                    window.zoom_window();
                    app.placeholder_notice = "已切换窗口最大化状态".to_string();
                    cx.notify();
                }
                _ => {}
            }
            // 该 hitbox 只覆盖标签右侧空白，拖动或缩放后不再向其他标题栏元素传播。
            cx.stop_propagation();
        }),
    )
}

/// 渲染标签溢出下拉按钮。
fn render_overflow_button(
    is_selected: bool,
    theme: &AppTheme,
    cx: &mut Context<ArgusApp>,
) -> impl IntoElement {
    div()
        .debug_selector(|| "tab-overflow-slot".to_string())
        .w(px(TAB_OVERFLOW_BUTTON_WIDTH))
        .h_full()
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .child(render_icon_button(
            "tab-overflow-button",
            ArgusIcon::Collapse,
            "全部标签页",
            is_selected,
            IconButtonSize::Small,
            theme,
            cx.listener(move |app, event: &ClickEvent, _, cx| {
                cx.stop_propagation();
                if is_selected {
                    app.close_active_menu();
                } else {
                    app.open_tab_overflow_menu(event.position());
                }
                cx.notify();
            }),
        ))
}

/// 渲染固定在全部标签按钮右侧的 Agent 助手面板开关。
fn render_assistant_panel_button(
    app: &ArgusApp,
    theme: &AppTheme,
    cx: &mut Context<ArgusApp>,
) -> impl IntoElement {
    let is_collapsed = app.is_assistant_panel_collapsed;
    div()
        .w(px(TAB_ASSISTANT_BUTTON_WIDTH))
        .h_full()
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .child(render_icon_button(
            "assistant-panel-toggle",
            // 展开与收起使用成对的面板图标，与左侧来源树开关保持一致的语义区分。
            ArgusIcon::assistant_panel_toggle(is_collapsed),
            if is_collapsed {
                "展开日志助手"
            } else {
                "收起日志助手"
            },
            false,
            IconButtonSize::Small,
            theme,
            cx.listener(|app, _, window, cx| {
                cx.stop_propagation();
                app.toggle_assistant_panel(Some(window), cx);
                cx.notify();
            }),
        ))
}

#[cfg(test)]
mod tests {
    use crate::app::{ArgusTab, TabKind};
    use crate::config::ConfigManager;
    use crate::config::paths::isolated_test_dir;
    use crate::loader::SourceId;
    use gpui::{Modifiers, MouseDownEvent, MouseUpEvent, TestAppContext, point, px};

    use super::*;

    /// 构造使用 `.argus_test` 独立目录的应用状态，避免视觉测试污染真实设置文件。
    fn visual_test_app() -> ArgusApp {
        let config_dir = isolated_test_dir("tab-bar-visual");
        ArgusApp::new_with_config_manager(ConfigManager::new(config_dir.join("settings.toml")))
    }

    /// 在指定位置模拟连续点击，确保测试覆盖 GPUI 的按下、释放和点击合成链路。
    fn simulate_repeated_clicks(
        position: gpui::Point<gpui::Pixels>,
        repeat_count: usize,
        cx: &mut gpui::VisualTestContext,
    ) {
        for click_count in 1..=repeat_count {
            cx.simulate_event(MouseDownEvent {
                button: MouseButton::Left,
                position,
                modifiers: Modifiers::default(),
                click_count,
                first_mouse: false,
            });
            cx.simulate_event(MouseUpEvent {
                button: MouseButton::Left,
                position,
                modifiers: Modifiers::default(),
                click_count,
            });
        }
    }

    /// 验证标签与空白拖拽区没有重叠，且连续点击标签不会进入 GPUI 窗口缩放处理器。
    #[gpui::test]
    fn repeated_clicking_tab_does_not_zoom_window(cx: &mut TestAppContext) {
        let (app, cx) = cx.add_window_view(|_, _| visual_test_app());
        let tab_bounds = cx.debug_bounds("tab-1").expect("应渲染默认标签");
        let drag_bounds = cx
            .debug_bounds("tab-bar-drag-area")
            .expect("应渲染标签栏空白拖拽区");
        let window_bounds_before = cx.update(|window, _| window.bounds());
        let notice_before = cx.update(|_, cx| app.read(cx).placeholder_notice.clone());

        assert!(
            tab_bounds.right() <= drag_bounds.left(),
            "标签边界不应覆盖空白拖拽区：tab={tab_bounds:?}, drag={drag_bounds:?}"
        );
        assert!(
            drag_bounds.size.width >= px(TAB_OVERFLOW_BUTTON_GAP),
            "标题栏应保留稳定可命中的拖拽宽度：drag={drag_bounds:?}"
        );

        simulate_repeated_clicks(
            point(
                tab_bounds.left() + tab_bounds.size.width / 2.0,
                tab_bounds.top() + px(8.0),
            ),
            3,
            cx,
        );

        assert_eq!(cx.update(|window, _| window.bounds()), window_bounds_before);
        assert_eq!(
            cx.update(|_, cx| app.read(cx).placeholder_notice.clone()),
            notice_before
        );
    }

    /// 构造仅用于布局测试的空标签集合。
    fn tabs_from_titles(titles: &[&str]) -> Vec<ArgusTab> {
        titles
            .iter()
            .enumerate()
            .map(|(index, title)| ArgusTab {
                id: index + 1,
                title: (*title).to_string(),
                kind: TabKind::Empty,
            })
            .collect()
    }

    /// 验证标签未铺满时拖拽空白吸收剩余宽度，标签与下拉按钮之间保持至少 8px 间距，
    /// 且下拉按钮仍固定在标题栏右侧。
    #[gpui::test]
    fn overflow_button_keeps_minimum_gap_after_tabs(cx: &mut TestAppContext) {
        let (_app, cx) = cx.add_window_view(|_, _| visual_test_app());
        let tab_bounds = cx.debug_bounds("tab-1").expect("应渲染默认标签");
        let overflow_bounds = cx
            .debug_bounds("tab-overflow-slot")
            .expect("应渲染标签下拉按钮");
        let drag_bounds = cx
            .debug_bounds("tab-bar-drag-area")
            .expect("应渲染标签栏空白拖拽区");
        let window_bounds = cx.update(|window, _| window.bounds());

        let gap = overflow_bounds.left() - tab_bounds.right();
        assert!(
            gap >= px(TAB_OVERFLOW_BUTTON_GAP - 0.6),
            "标签与下拉按钮之间应保持至少 {TAB_OVERFLOW_BUTTON_GAP}px 间距：tab={tab_bounds:?}, overflow={overflow_bounds:?}"
        );
        assert!(
            drag_bounds.left() >= tab_bounds.right()
                && drag_bounds.right() <= overflow_bounds.left(),
            "拖拽空白应位于标签与下拉按钮之间：drag={drag_bounds:?}"
        );
        assert!(
            (overflow_bounds.right()
                - (window_bounds.right()
                    - px(TAB_EXTERNAL_RIGHT_GAP)
                    - px(TAB_ASSISTANT_BUTTON_WIDTH)))
            .abs()
                <= px(0.6),
            "下拉按钮应固定在标题栏右侧：overflow={overflow_bounds:?}, window={window_bounds:?}"
        );
    }

    /// 验证标签块左边缘与内容玻璃板左边缘对齐，且玻璃板保持固定内边距。
    #[gpui::test]
    fn tab_left_edge_aligns_with_content_panel(cx: &mut TestAppContext) {
        let (app, cx) = cx.add_window_view(|_, _| visual_test_app());
        let tab_bounds = cx.debug_bounds("tab-1").expect("应渲染默认标签");
        let panel_bounds = cx
            .debug_bounds("window-content-panel")
            .expect("应渲染内容玻璃板");
        let source_panel_width = app.read_with(cx, |app, _| app.current_source_panel_width());
        let window_bounds = cx.update(|window, _| window.bounds());

        assert_eq!(
            panel_bounds.left(),
            px(source_panel_width + 8.0),
            "内容玻璃板左侧应保留 8px 内边距：panel={panel_bounds:?}"
        );
        assert_eq!(
            panel_bounds.top(),
            px(crate::ui::custom_title_bar::TITLE_BAR_HEIGHT
                + crate::ui::main_window::WINDOW_CONTENT_RING_ALLOWANCE),
            "内容玻璃板顶边应紧贴标题栏下沿并留出外描边余量：panel={panel_bounds:?}"
        );
        assert_eq!(
            panel_bounds.right(),
            window_bounds.right() - px(8.0),
            "内容玻璃板右侧应保留 8px 内边距：panel={panel_bounds:?}"
        );
        assert_eq!(
            panel_bounds.bottom(),
            window_bounds.bottom() - px(8.0),
            "内容玻璃板底部应保留 8px 内边距：panel={panel_bounds:?}"
        );
        assert_eq!(
            tab_bounds.left(),
            panel_bounds.left(),
            "标签左边缘应与内容玻璃板左边缘对齐：tab={tab_bounds:?}, panel={panel_bounds:?}"
        );
    }

    /// 验证标签块上下留白一致，并沿用 opencode v2 的 44px 标题栏 / 28px 标签 / 8px 边距。
    #[gpui::test]
    fn tab_block_vertical_margins_match_title_bar(cx: &mut TestAppContext) {
        let (_app, cx) = cx.add_window_view(|_, _| visual_test_app());
        let tab_bounds = cx.debug_bounds("tab-1").expect("应渲染默认标签");
        let window_bounds = cx.update(|window, _| window.bounds());
        let title_bar_bottom =
            window_bounds.top() + px(crate::ui::custom_title_bar::TITLE_BAR_HEIGHT);

        assert_eq!(
            tab_bounds.size.height,
            px(28.0),
            "标签块高度应与 opencode v2 的 h-7 一致：tab={tab_bounds:?}"
        );
        assert_eq!(
            tab_bounds.top() - window_bounds.top(),
            px(8.0),
            "标签上方应留 8px：tab={tab_bounds:?}"
        );
        assert_eq!(
            title_bar_bottom - tab_bounds.bottom(),
            px(8.0),
            "标签下方应留 8px：tab={tab_bounds:?}"
        );
    }

    /// 验证少量标签可以全部直接展示，且按内容宽度渲染而非铺满标签区。
    #[test]
    fn tab_layout_shows_all_tabs_when_space_is_enough() {
        let tabs = tabs_from_titles(&["app.log", "设置", "memory.log"]);
        let layout = calculate_tab_layout(&tabs, 1, 600.0);

        assert_eq!(layout.visible_range, 0..3);
        assert!(!layout.has_overflow);
        // 空间充足时标签保持按标题估算的内容宽度，剩余空间交给拖拽空白。
        let expected_widths = tabs
            .iter()
            .map(|tab| ideal_tab_width(&tab.title))
            .collect::<Vec<_>>();
        assert_eq!(layout.visible_widths, expected_widths);
        let expected_total = expected_widths.iter().sum::<f32>() + tab_gaps_width(3);
        assert!(
            (layout.tabs_width - expected_total).abs() <= 0.5,
            "标签总宽应等于内容宽度之和：{}",
            layout.tabs_width
        );
    }

    /// 验证大量标签只渲染包含激活项的可见窗口。
    #[test]
    fn tab_layout_keeps_active_tab_visible_when_overflowing() {
        let titles = (0..20)
            .map(|index| format!("thread_{index:04}.log"))
            .collect::<Vec<_>>();
        let tabs = titles.iter().map(String::as_str).collect::<Vec<_>>();
        let tabs = tabs_from_titles(&tabs);
        let layout = calculate_tab_layout(&tabs, 12, 360.0);

        assert!(layout.has_overflow);
        assert!(layout.visible_range.contains(&12));
        assert!(layout.visible_range.len() <= 4);
        assert!(
            layout.tabs_width
                + TAB_OVERFLOW_BUTTON_GAP
                + TAB_OVERFLOW_BUTTON_WIDTH
                + TAB_OVERFLOW_BUTTON_GAP
                <= 360.0
        );
    }

    /// 验证激活标签靠近末尾时可见窗口不会越界。
    #[test]
    fn tab_layout_clamps_visible_window_at_end() {
        let titles = (0..10)
            .map(|index| format!("thread_{index:04}.log"))
            .collect::<Vec<_>>();
        let tabs = titles.iter().map(String::as_str).collect::<Vec<_>>();
        let tabs = tabs_from_titles(&tabs);
        let layout = calculate_tab_layout(&tabs, 9, 320.0);

        assert!(layout.visible_range.contains(&9));
        assert_eq!(layout.visible_range.end, 10);
        assert!(
            layout.tabs_width
                + TAB_OVERFLOW_BUTTON_GAP
                + TAB_OVERFLOW_BUTTON_WIDTH
                + TAB_OVERFLOW_BUTTON_GAP
                <= 320.0
        );
    }

    /// 验证日志读取中的标签会被识别为加载状态，便于标题前显示旋转动画。
    #[test]
    fn tab_loading_detects_log_reader_state() {
        let mut app = ArgusApp::new();
        let source_id = SourceId(7);
        let log_tab = TabKind::LogSource {
            source_id,
            path: "/tmp/app.log".to_string(),
        };

        assert!(!is_tab_loading(&app, &log_tab));

        app.log_read_states.insert(
            source_id,
            LogOpenState::Loading {
                message: "正在读取".to_string(),
            },
        );

        assert!(is_tab_loading(&app, &log_tab));
    }
}
