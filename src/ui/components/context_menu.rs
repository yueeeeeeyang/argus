//! 文件职责：提供通用上下文菜单与下拉菜单组件。
//! 创建日期：2026-06-09
//! 修改日期：2026-06-16
//! 作者：Argus 开发团队
//! 主要功能：按窗口坐标渲染可滚动菜单，并将菜单项动作分发给应用状态。

use std::ops::Range;

use crate::app::ArgusApp;
use crate::loader::SourceId;
use crate::theme::AppTheme;
use gpui::{
    Context, Corner, IntoElement, Pixels, Point, SharedString, UniformListScrollHandle, anchored,
    div, prelude::*, px, rgb, rgba, uniform_list,
};

/// 标签页右键菜单宽度，紧贴三项动作文本但保留舒适内边距。
const TAB_CONTEXT_MENU_WIDTH: f32 = 132.0;
/// 标签页溢出菜单宽度，保留长日志文件名的展示空间。
const TAB_OVERFLOW_MENU_WIDTH: f32 = 220.0;
/// 搜索结果面板菜单宽度，仅包含展开/收起两项批量动作。
const SEARCH_RESULTS_MENU_WIDTH: f32 = 132.0;
/// 来源树右键菜单宽度，容纳 Jstack 分析中文动作。
const SOURCE_TREE_CONTEXT_MENU_WIDTH: f32 = 178.0;
/// 菜单项固定行高，供 `uniform_list` 稳定计算滚动范围。
const MENU_ROW_HEIGHT: f32 = 30.0;
/// 菜单最大高度，超出后只在菜单内部滚动。
const MENU_MAX_HEIGHT: f32 = 280.0;
/// 菜单贴边保护距离，避免浮层紧贴窗口边缘。
const MENU_WINDOW_MARGIN: f32 = 8.0;

/// 当前打开的菜单类型。
#[derive(Clone, Debug)]
pub enum ActiveMenuKind {
    /// 标签页右键菜单，动作作用于指定标签。
    TabContext {
        /// 被右键点击的标签 ID。
        tab_id: usize,
    },
    /// 标签溢出下拉菜单，展示全部标签并支持切换。
    TabOverflow,
    /// 搜索结果面板右键菜单，动作作用于全部结果分组。
    SearchResultsPanel,
    /// 来源树右键菜单，动作作用于被点击的日志候选节点或当前多选集合。
    SourceTree {
        /// 被右键点击的来源节点 ID。
        source_id: SourceId,
    },
}

/// 当前打开的菜单状态。
#[derive(Clone, Debug)]
pub struct ActiveMenu {
    /// 菜单类型，用于决定菜单项集合。
    pub kind: ActiveMenuKind,
    /// 菜单锚点，使用窗口坐标以便跨布局区域定位。
    pub anchor: Point<Pixels>,
}

/// 菜单项点击后要执行的应用动作。
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum MenuAction {
    /// 激活指定标签页。
    ActivateTab {
        /// 目标标签 ID。
        tab_id: usize,
    },
    /// 关闭指定标签页。
    CloseTab {
        /// 目标标签 ID。
        tab_id: usize,
    },
    /// 关闭指定标签之外的所有标签。
    CloseOtherTabs {
        /// 需要保留的标签 ID。
        tab_id: usize,
    },
    /// 关闭全部标签，并保留一个空标签。
    CloseAllTabs,
    /// 展开全部搜索结果文件分组。
    ExpandAllSearchResults,
    /// 收起全部搜索结果文件分组。
    CollapseAllSearchResults,
    /// 打开 Jstack 线程日志分析页签。
    OpenJstackAnalysis {
        /// 右键触发分析的来源节点 ID。
        source_id: SourceId,
    },
}

impl MenuAction {
    /// 返回菜单项元素 ID 使用的稳定后缀。
    fn id_suffix(&self) -> String {
        match self {
            Self::ActivateTab { tab_id } => format!("activate-tab-{tab_id}"),
            Self::CloseTab { tab_id } => format!("close-tab-{tab_id}"),
            Self::CloseOtherTabs { tab_id } => format!("close-other-tabs-{tab_id}"),
            Self::CloseAllTabs => "close-all-tabs".to_string(),
            Self::ExpandAllSearchResults => "expand-all-search-results".to_string(),
            Self::CollapseAllSearchResults => "collapse-all-search-results".to_string(),
            Self::OpenJstackAnalysis { source_id } => {
                format!("open-jstack-analysis-{source_id}")
            }
        }
    }
}

/// 菜单渲染条目。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MenuEntry {
    /// 展示文案。
    pub label: String,
    /// 点击动作。
    pub action: MenuAction,
    /// 是否为当前选中项。
    pub is_selected: bool,
    /// 是否为破坏性操作，用于后续可选的颜色强调。
    pub is_danger: bool,
}

impl MenuEntry {
    /// 创建普通菜单项。
    pub fn new(label: impl Into<String>, action: MenuAction) -> Self {
        Self {
            label: label.into(),
            action,
            is_selected: false,
            is_danger: false,
        }
    }

    /// 标记菜单项为当前选中项。
    pub fn selected(mut self, is_selected: bool) -> Self {
        self.is_selected = is_selected;
        self
    }

    /// 标记菜单项为破坏性操作。
    pub fn danger(mut self) -> Self {
        self.is_danger = true;
        self
    }
}

/// 渲染当前活动菜单；由主窗口根节点叠加调用。
///
/// 参数说明：
/// - `app`：应用状态，用于读取菜单类型、主题与滚动句柄。
/// - `cx`：应用上下文，用于绑定菜单关闭与动作回调。
///
/// 返回值：覆盖全窗口的透明命中层与定位菜单；没有菜单时返回空容器。
pub fn render_active_menu(app: &ArgusApp, cx: &mut Context<ArgusApp>) -> impl IntoElement {
    let Some(active_menu) = app.active_menu.clone() else {
        return div().into_any_element();
    };

    let entry_count = app.active_menu_entries().len();
    let menu_width = match active_menu.kind {
        ActiveMenuKind::TabContext { .. } => TAB_CONTEXT_MENU_WIDTH,
        ActiveMenuKind::TabOverflow => TAB_OVERFLOW_MENU_WIDTH,
        ActiveMenuKind::SearchResultsPanel => SEARCH_RESULTS_MENU_WIDTH,
        ActiveMenuKind::SourceTree { .. } => SOURCE_TREE_CONTEXT_MENU_WIDTH,
    };
    let menu_height = (entry_count as f32 * MENU_ROW_HEIGHT)
        .min(MENU_MAX_HEIGHT)
        .max(MENU_ROW_HEIGHT);
    let scroll_handle = app.tab_menu_scroll.clone();
    let theme = app.theme.clone();

    div()
        .id("active-context-menu-layer")
        .absolute()
        .size_full()
        .occlude()
        .bg(rgba(0x00000000))
        .on_click(cx.listener(|app, _, _, cx| {
            cx.stop_propagation();
            app.close_active_menu();
            cx.notify();
        }))
        .child(
            anchored()
                .position(active_menu.anchor)
                .anchor(Corner::TopLeft)
                .snap_to_window_with_margin(px(MENU_WINDOW_MARGIN))
                .child(render_menu_panel(
                    entry_count,
                    menu_width,
                    menu_height,
                    scroll_handle,
                    &theme,
                    cx,
                )),
        )
        .into_any_element()
}

/// 渲染菜单容器，内部统一使用固定行高虚拟列表承载条目。
fn render_menu_panel(
    entry_count: usize,
    menu_width: f32,
    menu_height: f32,
    scroll_handle: UniformListScrollHandle,
    theme: &AppTheme,
    cx: &mut Context<ArgusApp>,
) -> impl IntoElement {
    let panel_background = theme.content;
    let panel_border = theme.border;

    div()
        .id("context-menu-panel")
        .w(px(menu_width))
        .h(px(menu_height))
        .rounded(px(6.0))
        .border_1()
        .border_color(rgb(panel_border))
        .bg(rgb(panel_background))
        .shadow_lg()
        .overflow_hidden()
        .occlude()
        .on_click(cx.listener(|_, _, _, cx| {
            cx.stop_propagation();
        }))
        .child(
            uniform_list(
                "context-menu-list",
                entry_count,
                cx.processor(move |app, range: Range<usize>, _window, cx| {
                    let entries = app.active_menu_entries();
                    let theme = app.theme.clone();

                    entries[range]
                        .iter()
                        .cloned()
                        .map(|entry| {
                            render_menu_entry(entry, menu_width, &theme, cx).into_any_element()
                        })
                        .collect::<Vec<_>>()
                }),
            )
            .size_full()
            .block_mouse_except_scroll()
            .track_scroll(scroll_handle),
        )
}

/// 渲染单个菜单项；点击后交给应用状态执行对应动作。
fn render_menu_entry(
    entry: MenuEntry,
    menu_width: f32,
    theme: &AppTheme,
    cx: &mut Context<ArgusApp>,
) -> impl IntoElement {
    let action = entry.action.clone();
    let background = if entry.is_selected {
        theme.selection
    } else {
        theme.content
    };
    let hover_background = if entry.is_selected {
        theme.selection
    } else {
        theme.current_line
    };
    let foreground = if entry.is_danger {
        theme.error
    } else {
        theme.foreground
    };

    div()
        .id(SharedString::from(format!(
            "context-menu-entry-{}",
            entry.action.id_suffix()
        )))
        .w(px(menu_width))
        .h(px(MENU_ROW_HEIGHT))
        .px_3()
        .flex()
        .items_center()
        .cursor_pointer()
        .occlude()
        .bg(rgb(background))
        .hover(move |this| this.bg(rgb(hover_background)))
        .text_size(px(12.0))
        .text_color(rgb(foreground))
        .child(div().flex_1().truncate().child(entry.label))
        .on_click(cx.listener(move |app, _, _, cx| {
            cx.stop_propagation();
            app.handle_menu_action_with_context(action.clone(), cx);
            cx.notify();
        }))
}
