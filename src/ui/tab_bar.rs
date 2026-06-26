//! 文件职责：渲染自定义标题栏中的日志标签区域。
//! 创建日期：2026-06-09
//! 修改日期：2026-06-10
//! 作者：Argus 开发团队
//! 主要功能：展示可切换标签、右键菜单和多标签溢出下拉入口。

use std::ops::Range;

use crate::app::ArgusApp;
use crate::theme::AppTheme;
use crate::ui::components::context_menu::ActiveMenuKind;
use crate::ui::components::icon::{ArgusIcon, render_icon};
use crate::ui::components::icon_button::{IconButtonSize, render_icon_button};
use gpui::{
    ClickEvent, Context, IntoElement, MouseButton, MouseUpEvent, SharedString, Window,
    WindowControlArea, div, prelude::*, px, rgb, svg,
};

/// 激活标签底部凹弧连接遮罩尺寸。
const ACTIVE_TAB_CONNECTOR_SIZE: f32 = 6.0;
/// 标签标题字号，保持标题栏紧凑密度。
const TAB_TITLE_FONT_SIZE: f32 = 12.0;
/// 激活和悬停标签高度，确保 hover 时与当前标签保持一致。
const TAB_ACTIVE_HEIGHT: f32 = 32.0;
/// 静止未激活标签高度，保留标题栏层次。
const TAB_INACTIVE_HEIGHT: f32 = 30.0;
/// 普通标签最小宽度；短标题标签在空间充足时可保持紧凑。
const TAB_MIN_WIDTH: f32 = 72.0;
/// 极窄窗口下的兜底宽度，优先保证不突破可视区域。
const TAB_EMERGENCY_MIN_WIDTH: f32 = 48.0;
/// 普通标签最大宽度。
const TAB_MAX_WIDTH: f32 = 230.0;
/// 下拉按钮占位宽度；按钮始终展示，便于从固定入口查看全部标签。
const TAB_OVERFLOW_BUTTON_WIDTH: f32 = 32.0;
/// 标签页和右侧下拉按钮之间的最小间距；更多空间优先让标签页使用。
const TITLE_RIGHT_DRAG_MIN_WIDTH: f32 = 8.0;
/// 标题栏中标签栏左侧外部留白，对应 `custom_title_bar` 中的间距。
const TAB_EXTERNAL_LEFT_GAP: f32 = 16.0;
/// 标题栏右侧固定按钮与窗口右边缘的间距，对应 `custom_title_bar` 中的间距。
const TAB_EXTERNAL_RIGHT_GAP: f32 = 12.0;
/// 来源侧栏折叠时标题栏左侧控制区的估算宽度。
const COMPACT_LEFT_CONTROLS_WIDTH: f32 = 232.0;
/// 关闭按钮固定占位宽度，避免 hover 时插入按钮撑宽标签。
const TAB_CLOSE_SLOT_WIDTH: f32 = 18.0;
/// 标签关闭按钮命中区尺寸，比通用标题栏按钮更紧凑。
const TAB_CLOSE_BUTTON_SIZE: f32 = 18.0;
/// 标签关闭图标尺寸，匹配 12px 标题文本。
const TAB_CLOSE_ICON_SIZE: f32 = 13.0;
/// 标题文本宽度估算后额外保留的内边距、关闭按钮槽和激活凹弧连接件宽度。
const TAB_TITLE_CHROME_WIDTH: f32 = 56.0;
/// ASCII 字符在 12px 标题字号下的平均宽度估算。
const ASCII_TITLE_CHAR_WIDTH: f32 = 7.0;
/// CJK 等非 ASCII 字符在 12px 标题字号下的平均宽度估算。
const WIDE_TITLE_CHAR_WIDTH: f32 = 12.0;

/// 激活标签凹弧连接件方向。
#[derive(Clone, Copy)]
enum TabConnectorSide {
    /// 标签左侧连接件。
    Left,
    /// 标签右侧连接件。
    Right,
}

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
pub fn render(app: &ArgusApp, window: &mut Window, cx: &mut Context<ArgusApp>) -> impl IntoElement {
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
                .overflow_hidden()
                .children(
                    visible_tabs
                        .into_iter()
                        .map(|(tab, tab_width)| {
                            render_tab(tab, tab_width, active_tab_id, hovered_tab_id, &theme, cx)
                                .into_any_element()
                        })
                        .collect::<Vec<_>>(),
                ),
        )
        .child(tab_drag_area(cx))
        .child(render_overflow_button(overflow_selected, &theme, cx))
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

    let tab_area_width = (available_width - TAB_OVERFLOW_BUTTON_WIDTH - TITLE_RIGHT_DRAG_MIN_WIDTH)
        .max(TAB_EMERGENCY_MIN_WIDTH);
    let ideal_widths = tabs
        .iter()
        .map(|tab| ideal_tab_width(&tab.title))
        .collect::<Vec<_>>();
    let ideal_total_width: f32 = ideal_widths.iter().sum();

    if ideal_total_width <= tab_area_width {
        return TabBarLayout {
            visible_range: 0..tab_count,
            visible_widths: ideal_widths,
            has_overflow: false,
            tabs_width: ideal_total_width,
        };
    }

    let visible_count = ((tab_area_width / TAB_MIN_WIDTH).floor() as usize)
        .max(1)
        .min(tab_count);
    let safe_active_index = active_index.min(tab_count - 1);
    let mut start = safe_active_index.saturating_sub(visible_count / 2);
    if start + visible_count > tab_count {
        start = tab_count - visible_count;
    }
    let end = start + visible_count;
    let visible_ideal_widths = ideal_widths[start..end].to_vec();
    let visible_widths = fit_tab_widths(&visible_ideal_widths, tab_area_width);
    let tabs_width = visible_widths.iter().sum();

    TabBarLayout {
        visible_range: start..end,
        visible_widths,
        has_overflow: end - start < tab_count,
        tabs_width,
    }
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
    theme: &AppTheme,
    cx: &mut Context<ArgusApp>,
) -> impl IntoElement {
    let tab_id = tab.id;
    let is_active = tab_id == active_tab_id;
    let is_hovered = hovered_tab_id == Some(tab_id);
    let should_show_close = is_active || is_hovered;
    let background = if is_active {
        theme.content
    } else if is_hovered {
        theme.current_line
    } else {
        theme.title_bar
    };
    let height = if is_active || is_hovered {
        TAB_ACTIVE_HEIGHT
    } else {
        TAB_INACTIVE_HEIGHT
    };
    let foreground = if is_active || is_hovered {
        theme.foreground
    } else {
        theme.foreground_muted
    };

    div()
        .id(SharedString::from(format!("tab-{tab_id}")))
        .w(px(tab_width))
        .h(px(height))
        .flex_none()
        .flex()
        .items_end()
        .cursor_pointer()
        .when(is_active, |this| {
            this.child(active_tab_connector(TabConnectorSide::Left, theme))
        })
        .child(
            div()
                .h(px(height))
                .min_w(px(0.0))
                .flex_1()
                .relative()
                .pl_3()
                .pr_1()
                .pb(px(1.0))
                .flex()
                .items_center()
                .gap_1()
                .rounded_t(px(8.0))
                .bg(rgb(background))
                .text_color(rgb(foreground))
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
        .when(is_active, |this| {
            this.child(active_tab_connector(TabConnectorSide::Right, theme))
        })
        .on_hover(cx.listener(move |app, is_hovered: &bool, _, cx| {
            app.set_hovered_tab(tab_id, *is_hovered);
            cx.notify();
        }))
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
    div()
        .id("tab-bar-drag-area")
        .h_full()
        .min_w(px(TITLE_RIGHT_DRAG_MIN_WIDTH))
        .flex_1()
        .window_control_area(WindowControlArea::Drag)
        .on_click(cx.listener(|app, event: &ClickEvent, window, cx| {
            if let ClickEvent::Mouse(mouse_event) = event
                && mouse_event.up.click_count >= 2
            {
                window.zoom_window();
                app.placeholder_notice = "已切换窗口最大化状态".to_string();
                cx.stop_propagation();
                cx.notify();
            }
        }))
}

/// 渲染标签溢出下拉按钮。
fn render_overflow_button(
    is_selected: bool,
    theme: &AppTheme,
    cx: &mut Context<ArgusApp>,
) -> impl IntoElement {
    div()
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

/// 渲染激活标签与内容区衔接处的单侧凹弧连接件。
fn active_tab_connector(side: TabConnectorSide, theme: &AppTheme) -> impl IntoElement {
    let path = match side {
        TabConnectorSide::Left => "chrome/tab-connector-left.svg",
        TabConnectorSide::Right => "chrome/tab-connector-right.svg",
    };

    div()
        .w(px(ACTIVE_TAB_CONNECTOR_SIZE))
        .h(px(ACTIVE_TAB_CONNECTOR_SIZE))
        .flex_none()
        .bg(rgb(theme.content))
        .child(
            svg()
                .path(path)
                .size(px(ACTIVE_TAB_CONNECTOR_SIZE))
                .text_color(rgb(theme.title_bar)),
        )
}

#[cfg(test)]
mod tests {
    use crate::app::{ArgusTab, TabKind};

    use super::*;

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

    /// 验证少量标签可以全部直接展示。
    #[test]
    fn tab_layout_shows_all_tabs_when_space_is_enough() {
        let tabs = tabs_from_titles(&["app.log", "设置", "memory.log"]);
        let layout = calculate_tab_layout(&tabs, 1, 600.0);

        assert_eq!(layout.visible_range, 0..3);
        assert!(!layout.has_overflow);
        assert_eq!(layout.visible_widths.len(), 3);
        assert!(layout.visible_widths[0] < 120.0);
        assert!(layout.tabs_width < 360.0);
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
            layout.tabs_width + TAB_OVERFLOW_BUTTON_WIDTH + TITLE_RIGHT_DRAG_MIN_WIDTH <= 360.0
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
            layout.tabs_width + TAB_OVERFLOW_BUTTON_WIDTH + TITLE_RIGHT_DRAG_MIN_WIDTH <= 320.0
        );
    }
}
