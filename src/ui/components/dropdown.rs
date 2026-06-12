//! 文件职责：提供 Argus 通用下拉选择组件。
//! 创建日期：2026-06-12
//! 修改日期：2026-06-12
//! 作者：Argus 开发团队
//! 主要功能：统一渲染设置页等场景中的单选下拉框，支持主题化、展开列表和选项点击回调。

use std::sync::Arc;

use gpui::{App, ClickEvent, IntoElement, SharedString, Window, div, prelude::*, px, rgb};

use crate::theme::AppTheme;
use crate::ui::components::icon::{ArgusIcon, render_icon};

/// 下拉框选项数据。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DropdownItem {
    /// 选项稳定 ID，业务方可用它持久化选择。
    pub id: String,
    /// 选项展示文案。
    pub label: String,
}

/// 下拉框渲染数据。
#[derive(Clone, Debug)]
pub struct Dropdown {
    /// 组件根节点 ID，便于测试与多下拉框共存。
    pub id: &'static str,
    /// 当前选中项 ID。
    pub selected_id: String,
    /// 当前选中项文案。
    pub selected_label: String,
    /// 没有选中项时的占位文案。
    pub placeholder: &'static str,
    /// 是否展开选项列表。
    pub is_open: bool,
    /// 下拉框选项。
    pub items: Vec<DropdownItem>,
    /// 是否在组件内部渲染菜单；复杂窗口可关闭该项并在根层渲染浮层菜单。
    pub show_inline_menu: bool,
}

/// 下拉框选项点击回调类型。
pub type DropdownSelectCallback = Arc<dyn Fn(String, &mut Window, &mut App) + 'static>;

/// 渲染通用下拉框。
///
/// 参数说明：
/// - `dropdown`：当前下拉框状态快照。
/// - `theme`：当前主题令牌。
/// - `on_toggle`：点击输入区域时触发展开/收起。
/// - `on_select`：点击选项时回传选项 ID。
///
/// 返回值：可直接嵌入设置页面的 GPUI 元素树。
pub fn render_dropdown(
    dropdown: Dropdown,
    theme: &AppTheme,
    on_toggle: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    on_select: DropdownSelectCallback,
) -> impl IntoElement {
    let selected_text = if dropdown.selected_label.is_empty() {
        dropdown.placeholder.to_string()
    } else {
        dropdown.selected_label.clone()
    };
    let list_height = (dropdown.items.len() as f32 * 30.0).clamp(30.0, 220.0);
    let items = dropdown.items.clone();
    let selected_id = dropdown.selected_id.clone();
    let dropdown_id = dropdown.id;
    let menu_theme = theme.clone();

    div()
        .id(dropdown.id)
        .relative()
        .w(px(260.0))
        .h(px(30.0))
        .rounded_sm()
        .text_size(px(12.0))
        .text_color(rgb(theme.foreground))
        .occlude()
        .child(
            div()
                .id(SharedString::from(format!("{dropdown_id}-button")))
                .w_full()
                .h(px(30.0))
                .px_2()
                .flex()
                .items_center()
                .gap_2()
                .rounded_sm()
                .border_1()
                .border_color(rgb(theme.border))
                .bg(rgb(theme.content))
                .cursor_pointer()
                .hover(|this| this.bg(rgb(theme.current_line)))
                .child(div().flex_1().truncate().child(selected_text))
                .child(render_icon(
                    if dropdown.is_open {
                        ArgusIcon::Collapse
                    } else {
                        ArgusIcon::Expand
                    },
                    theme.foreground_muted,
                    14.0,
                ))
                .on_click(on_toggle),
        )
        .when(dropdown.is_open && dropdown.show_inline_menu, |this| {
            this.child(
                div()
                    .id(SharedString::from(format!("{dropdown_id}-menu")))
                    .absolute()
                    .top(px(34.0))
                    .left_0()
                    .right_0()
                    .h(px(list_height))
                    .rounded_sm()
                    .border_1()
                    .border_color(rgb(theme.border))
                    .bg(rgb(theme.content))
                    .shadow_lg()
                    .overflow_y_scroll()
                    .occlude()
                    .children(items.into_iter().map(move |item| {
                        let is_selected = item.id == selected_id;
                        let item_id = item.id.clone();
                        let on_select = on_select.clone();
                        let theme = menu_theme.clone();
                        let hover_background = theme.current_line;
                        let foreground = theme.foreground;

                        div()
                            .id(SharedString::from(format!(
                                "{dropdown_id}-item-{}",
                                item.id
                            )))
                            .h(px(30.0))
                            .w_full()
                            .px_2()
                            .flex()
                            .items_center()
                            .cursor_pointer()
                            .bg(rgb(if is_selected {
                                theme.selection
                            } else {
                                theme.content
                            }))
                            .hover(move |this| this.bg(rgb(hover_background)))
                            .text_color(rgb(foreground))
                            .child(div().flex_1().truncate().child(item.label))
                            .on_click(move |_, window, cx| {
                                cx.stop_propagation();
                                on_select(item_id.clone(), window, cx);
                            })
                    })),
            )
        })
}
