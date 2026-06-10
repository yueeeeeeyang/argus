//! 文件职责：提供 Argus 界面可复用的紧凑输入框组件。
//! 创建日期：2026-06-10
//! 修改日期：2026-06-10
//! 作者：Argus 开发团队
//! 主要功能：统一输入框尺寸、图标、占位文本、禁用态和键盘输入回调。

use crate::theme::AppTheme;
use crate::ui::components::icon::{ArgusIcon, render_icon};
use crate::ui::components::icon_button::{IconButtonSize, render_icon_button};
use gpui::{
    Animation, AnimationExt, AnyElement, App, ClickEvent, IntoElement, KeyDownEvent, Window, div,
    prelude::*, px, rgb,
};
use std::ops::Range;
use std::time::Duration;

/// 输入框尺寸规格，便于不同工具栏复用同一组件。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InputSize {
    /// 紧凑输入框，用于来源侧栏等窄区域。
    Compact,
    /// 常规输入框，用于内容区搜索面板。
    Regular,
}

/// 输入框前置或后置附件配置，当前主要承载 Lucide 图标。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct InputAccessory {
    /// 附件元素稳定 ID，用于后置按钮测试定位；前置静态图标可忽略该值。
    pub id: &'static str,
    /// 附件展示图标。
    pub icon: ArgusIcon,
    /// 附件悬停提示。
    pub tooltip: &'static str,
}

/// 输入框渲染配置；业务输入状态由调用方维护。
#[derive(Clone, Debug)]
pub struct Input {
    /// 输入框稳定元素 ID。
    pub id: &'static str,
    /// 输入框占位提示。
    pub placeholder: &'static str,
    /// 输入框当前展示值。
    pub value: String,
    /// 输入框是否禁用。
    pub is_disabled: bool,
    /// 输入框是否聚焦；聚焦时展示光标和选区。
    pub is_focused: bool,
    /// 当前光标字符位置。
    pub cursor_index: usize,
    /// 当前选区字符范围。
    pub selection_range: Option<Range<usize>>,
    /// 输入框尺寸规格。
    pub size: InputSize,
    /// 前置图标附件。
    pub leading_accessory: Option<InputAccessory>,
    /// 后置可点击图标附件。
    pub trailing_accessory: Option<InputAccessory>,
}

/// 渲染通用输入框，并将键盘输入与后置按钮事件交给调用方处理。
///
/// 参数说明：
/// - `input`：输入框视觉和展示值配置。
/// - `theme`：当前主题令牌。
/// - `on_key_down`：键盘输入回调，通常更新调用方的本地状态。
/// - `on_trailing_click`：后置附件点击回调，未配置后置附件时不会触发。
///
/// 返回值：GPUI 元素树；组件自身不保存业务状态。
pub fn render_input(
    input: Input,
    theme: &AppTheme,
    on_key_down: impl Fn(&KeyDownEvent, &mut Window, &mut App) + 'static,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    on_trailing_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    let (height, font_size, icon_size, horizontal_padding) = match input.size {
        InputSize::Compact => (28.0, 12.0, 14.0, 8.0),
        InputSize::Regular => (30.0, 13.0, 16.0, 8.0),
    };
    let trailing_button_size = 24.0_f32;
    let right_padding = if input.trailing_accessory.is_some() {
        ((height - trailing_button_size) / 2.0_f32).max(2.0_f32)
    } else {
        horizontal_padding
    };
    let display_text = if input.value.is_empty() {
        input.placeholder.to_string()
    } else {
        input.value.clone()
    };
    let text_color = if input.value.is_empty() {
        theme.foreground_muted
    } else {
        theme.foreground
    };
    let border_color = theme.border;
    let hover_border_color = theme.foreground_muted;
    let background_color = theme.content;
    let selection_background = theme.selection;
    let cursor_color = theme.foreground;
    let cursor_index = input.cursor_index.min(character_count(&input.value));
    let selection_range = input
        .selection_range
        .clone()
        .filter(|range| range.start < range.end);

    div()
        .id(input.id)
        .h(px(height))
        .w_full()
        .pl(px(horizontal_padding))
        .pr(px(right_padding))
        .flex()
        .items_center()
        .gap_2()
        .rounded_sm()
        .border_1()
        .border_color(rgb(border_color))
        .bg(rgb(background_color))
        .text_size(px(font_size))
        .text_color(rgb(text_color))
        .when(!input.is_disabled, |this| {
            this.focusable()
                .hover(move |this| this.border_color(rgb(hover_border_color)))
                .on_key_down(on_key_down)
                .on_click(on_click)
        })
        .when(input.is_disabled, |this| this.opacity(0.55))
        .when_some(input.leading_accessory, |this, accessory| {
            this.child(render_icon(
                accessory.icon,
                theme.foreground_muted,
                icon_size,
            ))
        })
        .child(
            div()
                .flex_1()
                .min_w(px(0.0))
                .overflow_hidden()
                .flex()
                .items_center()
                .child(render_editable_text(
                    input.id,
                    &input.value,
                    &display_text,
                    cursor_index,
                    selection_range,
                    input.is_focused,
                    text_color,
                    selection_background,
                    cursor_color,
                )),
        )
        .when_some(input.trailing_accessory, |this, accessory| {
            this.child(render_icon_button(
                accessory.id,
                accessory.icon,
                accessory.tooltip,
                false,
                IconButtonSize::Tiny,
                theme,
                on_trailing_click,
            ))
        })
}

/// 渲染输入框文本、选区和光标；光标通过循环动画实现静止闪烁。
fn render_editable_text(
    input_id: &'static str,
    value: &str,
    display_text: &str,
    cursor_index: usize,
    selection_range: Option<Range<usize>>,
    is_focused: bool,
    text_color: u32,
    selection_background: u32,
    cursor_color: u32,
) -> impl IntoElement {
    let mut text_children: Vec<AnyElement> = Vec::new();

    if value.is_empty() {
        if is_focused {
            text_children.push(render_caret(input_id, cursor_color).into_any_element());
        }
        text_children.push(
            div()
                .flex_none()
                .truncate()
                .text_color(rgb(text_color))
                .child(display_text.to_string())
                .into_any_element(),
        );
    } else if let Some(range) = selection_range {
        let before = slice_character_range(value, 0..range.start);
        let selected = slice_character_range(value, range.clone());
        let after = slice_character_range(value, range.end..character_count(value));

        if cursor_index <= range.start && is_focused {
            text_children.push(render_caret(input_id, cursor_color).into_any_element());
        }
        text_children.push(text_segment(before, text_color).into_any_element());
        text_children.push(
            div()
                .flex_none()
                .rounded_xs()
                .bg(rgb(selection_background))
                .text_color(rgb(text_color))
                .child(selected)
                .into_any_element(),
        );
        if cursor_index >= range.end && is_focused {
            text_children.push(render_caret(input_id, cursor_color).into_any_element());
        }
        text_children.push(text_segment(after, text_color).into_any_element());
    } else {
        let before = slice_character_range(value, 0..cursor_index);
        let after = slice_character_range(value, cursor_index..character_count(value));
        text_children.push(text_segment(before, text_color).into_any_element());
        if is_focused {
            text_children.push(render_caret(input_id, cursor_color).into_any_element());
        }
        text_children.push(text_segment(after, text_color).into_any_element());
    }

    div()
        .min_w(px(0.0))
        .flex()
        .items_center()
        .overflow_hidden()
        .children(text_children)
}

/// 渲染普通文本片段，空片段不会占据额外空间。
fn text_segment(text: String, text_color: u32) -> impl IntoElement {
    div()
        .flex_none()
        .truncate()
        .text_color(rgb(text_color))
        .child(text)
}

/// 渲染闪烁光标，使用循环透明度动画模拟原生输入框呼吸节奏。
fn render_caret(input_id: &'static str, cursor_color: u32) -> impl IntoElement {
    div()
        .id((input_id, 1usize))
        .flex_none()
        .w(px(1.0))
        .h(px(16.0))
        .bg(rgb(cursor_color))
        .with_animation(
            (input_id, 2usize),
            Animation::new(Duration::from_millis(900))
                .repeat()
                .with_easing(gpui::pulsating_between(0.08, 1.0)),
            |this, opacity| this.opacity(opacity),
        )
}

/// 返回字符串的字符数量，保证输入框光标不会落在 UTF-8 字节中间。
fn character_count(text: &str) -> usize {
    text.chars().count()
}

/// 将字符索引转换为字节索引，越界时返回字符串末尾。
fn byte_index_for_character(text: &str, character_index: usize) -> usize {
    text.char_indices()
        .map(|(byte_index, _)| byte_index)
        .nth(character_index)
        .unwrap_or(text.len())
}

/// 截取指定字符范围的字符串。
fn slice_character_range(text: &str, range: Range<usize>) -> String {
    let start = byte_index_for_character(text, range.start);
    let end = byte_index_for_character(text, range.end);
    text[start..end].to_string()
}
