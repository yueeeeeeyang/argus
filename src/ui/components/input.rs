//! 文件职责：提供 Argus 界面可复用的紧凑输入框组件。
//! 创建日期：2026-06-10
//! 修改日期：2026-07-15
//! 作者：Argus 开发团队
//! 主要功能：统一输入框和多行文本域尺寸、图标、占位文本、禁用态、系统输入法和键盘输入回调。

use crate::infra::text_selection::{
    NativeTextEdit, TextSelectionGranularity, byte_index_for_character, character_count,
    character_range_for_utf16_range, replace_character_range, slice_character_range,
    utf16_range_for_character_range,
};
use crate::theme::AppTheme;
use crate::ui::components::icon::{ArgusIcon, render_icon};
use crate::ui::components::icon_button::{IconButtonSize, render_icon_button};
use gpui::{
    Animation, AnimationExt, AnyElement, App, Bounds, ClickEvent, FocusHandle, Hsla, InputHandler,
    IntoElement, KeyDownEvent, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, Pixels,
    ScrollHandle, ShapedLine, SharedString, TextRun, UTF16Selection, UnderlineStyle, Window,
    canvas, div, fill, point, prelude::*, px, rgb, size,
};
use std::cell::RefCell;
use std::ops::Range;
use std::rc::Rc;
use std::time::Duration;

/// 单行输入框自动横向滚动时给光标保留的可视边距。
const INPUT_HORIZONTAL_SCROLL_MARGIN: f32 = 8.0;
/// 多行文本域自动滚动时给光标上下保留的可视边距。
const TEXTAREA_VERTICAL_SCROLL_MARGIN: f32 = 4.0;
/// 多行文本域显式滚动条占用宽度。
const TEXTAREA_SCROLLBAR_WIDTH: f32 = 6.0;
/// 多行文本域滚动条和边缘之间的留白。
const TEXTAREA_SCROLLBAR_PADDING: f32 = 1.0;
/// 多行文本域滚动条滑块最小长度，避免长文本时滑块小到不可见。
const TEXTAREA_SCROLLBAR_MIN_THUMB: f32 = 18.0;
/// 多行文本域滚动条滑块厚度。
const TEXTAREA_SCROLLBAR_THUMB_SIZE: f32 = 4.0;

/// 原生文本编辑写回回调；单行输入框和文本域共享同一签名。
type NativeEditCallback = Rc<dyn Fn(NativeTextEdit, &mut Window, &mut App)>;

/// 输入框尺寸规格，便于不同工具栏复用同一组件。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum InputSize {
    /// 紧凑输入框，用于来源侧栏等窄区域。
    Compact,
    /// 常规输入框，用于内容区搜索面板。
    Regular,
}

/// 输入框前置或后置附件配置，当前主要承载 Lucide 图标。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct InputAccessory {
    /// 附件元素稳定 ID，用于后置按钮测试定位；前置静态图标可忽略该值。
    pub id: &'static str,
    /// 附件展示图标。
    pub icon: ArgusIcon,
    /// 附件悬停提示。
    pub tooltip: &'static str,
}

/// 系统文本输入桥接配置，用于接收中文输入法等 IME 提交文本。
#[derive(Clone)]
pub(crate) struct NativeInput {
    /// 当前输入框对应的真实 GPUI 焦点句柄。
    pub focus_handle: FocusHandle,
    /// 系统输入提交后的业务写回回调。
    pub on_edit: NativeEditCallback,
}

/// 文本域内部滚动状态，保存滚动条拖拽态等纯 UI 交互数据。
#[derive(Clone, Default)]
pub(crate) struct TextareaScrollState {
    /// 当前滚动条拖拽状态；使用内部可变性避免把临时拖拽态写入业务配置。
    scrollbar_drag: Rc<RefCell<Option<TextareaScrollbarDrag>>>,
    /// 最近一次自动跟随光标的内容签名，避免用户手动滚动后被每帧强制拉回光标。
    last_caret_sync: Rc<RefCell<Option<TextareaCaretSyncSignature>>>,
}

impl TextareaScrollState {
    /// 创建空文本域滚动状态。
    pub(crate) fn new() -> Self {
        Self::default()
    }
}

/// 文本域自动滚动到光标位置时使用的轻量签名。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct TextareaCaretSyncSignature {
    /// 当前文本字符数；内容变化时通常需要重新确认光标可见。
    text_length: usize,
    /// 当前光标字符位置。
    cursor_index: usize,
}

impl NativeInput {
    /// 创建系统文本输入桥接配置。
    ///
    /// 参数说明：
    /// - `focus_handle`：输入框真实焦点句柄。
    /// - `on_edit`：输入法提交或 marked text 变化时的业务写回回调。
    ///
    /// 返回值：可传给 `Input` 的原生输入桥接配置。
    pub(crate) fn new(
        focus_handle: FocusHandle,
        on_edit: impl Fn(NativeTextEdit, &mut Window, &mut App) + 'static,
    ) -> Self {
        Self {
            focus_handle,
            on_edit: Rc::new(on_edit),
        }
    }
}

/// 输入框渲染配置；业务输入状态由调用方维护。
#[derive(Clone)]
pub(crate) struct Input {
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
    /// 输入法 marked text 字符范围，用于候选态替换和候选窗定位。
    pub marked_range: Option<Range<usize>>,
    /// 当前输入框是否正在进行鼠标拖拽选择。
    pub is_pointer_selecting: bool,
    /// 是否以密码掩码显示真实值；业务状态仍保存真实文本。
    pub is_secret: bool,
    /// 输入框尺寸规格。
    pub size: InputSize,
    /// 前置图标附件。
    pub leading_accessory: Option<InputAccessory>,
    /// 后置可点击图标附件。
    pub trailing_accessory: Option<InputAccessory>,
    /// 系统文本输入桥接配置；为空时退回按键事件输入。
    pub native_input: Option<NativeInput>,
}

/// 多行文本域渲染配置；业务输入状态由调用方维护。
#[derive(Clone)]
pub(crate) struct Textarea {
    /// 文本域稳定元素 ID。
    pub id: &'static str,
    /// 文本域占位提示。
    pub placeholder: &'static str,
    /// 文本域当前展示值。
    pub value: String,
    /// 文本域是否禁用。
    pub is_disabled: bool,
    /// 文本域是否聚焦；聚焦时展示光标和选区。
    pub is_focused: bool,
    /// 当前光标字符位置。
    pub cursor_index: usize,
    /// 当前选区字符范围。
    pub selection_range: Option<Range<usize>>,
    /// 输入法 marked text 字符范围，用于候选态替换和候选窗定位。
    pub marked_range: Option<Range<usize>>,
    /// 当前文本域是否正在进行鼠标拖拽选择。
    pub is_pointer_selecting: bool,
    /// 文本域可见行数，内容超出后通过滚动条查看。
    pub visible_lines: usize,
    /// 是否填满父级剩余高度；大编辑器使用该模式避免底部空白。
    pub fill_height: bool,
    /// 文本域滚动句柄，负责在光标移动到可视区外时同步横向和纵向滚动。
    pub scroll_handle: ScrollHandle,
    /// 文本域滚动交互状态，负责支持自绘滚动条拖拽。
    pub scroll_state: TextareaScrollState,
    /// 文本域视觉变体；对话输入使用更醒目的悬浮编辑器样式。
    pub style: TextareaStyle,
    /// 后置可点击图标附件。
    pub trailing_accessory: Option<InputAccessory>,
    /// 后置图标在文本域中的悬浮位置。
    pub trailing_accessory_position: TextareaAccessoryPosition,
    /// 是否在文本域失焦时仍显示后置图标。
    pub trailing_accessory_always_visible: bool,
    /// 后置图标是否使用选中态背景，用于主要发送操作。
    pub trailing_accessory_selected: bool,
    /// 系统文本输入桥接配置；为空时退回按键事件输入。
    pub native_input: Option<NativeInput>,
}

/// 多行文本域视觉变体。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum TextareaStyle {
    /// 普通表单和设置编辑器使用的紧凑样式。
    Default,
    /// Agent 对话输入使用的圆角悬浮编辑器样式。
    Composer,
}

/// 文本域后置图标的悬浮位置。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum TextareaAccessoryPosition {
    /// 位于右上角，适合清空等辅助操作。
    TopRight,
    /// 位于右下角，适合对话发送等主要操作。
    BottomRight,
}

/// 输入框鼠标选择阶段。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum InputPointerAction {
    /// 鼠标按下，开始一次选择。
    Begin,
    /// 鼠标拖拽，扩展当前选择。
    Extend,
    /// 鼠标释放，结束当前选择。
    Finish,
}

/// 输入框鼠标选择事件；字符索引由组件根据文本布局计算。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct InputPointerEvent {
    /// 当前选择阶段。
    pub action: InputPointerAction,
    /// 鼠标命中的字符索引。
    pub character_index: usize,
    /// 本次选择粒度，由点击次数决定。
    pub granularity: TextSelectionGranularity,
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
pub(crate) fn render_input(
    input: Input,
    theme: &AppTheme,
    on_key_down: impl Fn(&KeyDownEvent, &mut Window, &mut App) + 'static,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    on_pointer_select: impl Fn(&InputPointerEvent, &mut Window, &mut App) + 'static,
    on_trailing_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    let (height, font_size, icon_size, horizontal_padding) = match input.size {
        InputSize::Compact => (28.0, 12.0, 14.0, 8.0),
        InputSize::Regular => (30.0, 13.0, 16.0, 8.0),
    };
    // 失焦后隐藏右侧清除按钮，并同步收回右侧预留空间，避免输入框出现空洞。
    let visible_trailing_accessory = input
        .trailing_accessory
        .filter(|_| input.is_focused && !input.is_disabled);
    let trailing_button_size = 24.0_f32;
    let right_padding = if visible_trailing_accessory.is_some() {
        ((height - trailing_button_size) / 2.0_f32).max(2.0_f32)
    } else {
        horizontal_padding
    };
    let masked_value = mask_secret_value(&input.value);
    let display_text = if input.value.is_empty() {
        input.placeholder.to_string()
    } else if input.is_secret {
        masked_value.clone()
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
    let marked_range = input
        .marked_range
        .clone()
        .filter(|range| range.start < range.end);
    let on_pointer_select = Rc::new(on_pointer_select);
    let pointer_value = if input.is_secret && !input.value.is_empty() {
        masked_value
    } else {
        input.value.clone()
    };
    let native_input = input.native_input.clone();
    let native_input_for_focus = native_input.clone();
    let native_input_for_click = native_input.clone();
    let native_input_for_pointer = native_input.clone();
    let native_input_for_key = native_input.clone();
    let runtime_focus_handle_for_text = native_input
        .as_ref()
        .map(|native_input| native_input.focus_handle.clone());

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
        // 输入框需要阻止点击穿透，但必须保留后层滚动容器的命中，避免鼠标经过输入框时页面滚动中断。
        .block_mouse_except_scroll()
        .text_size(px(font_size))
        .text_color(rgb(text_color))
        .when(!input.is_disabled, |this| {
            let element = this
                .focusable()
                .hover(move |this| this.border_color(rgb(hover_border_color)))
                .on_key_down(move |event, window, cx| {
                    if native_input_for_key.is_some() && is_plain_text_key(event) {
                        return;
                    }
                    on_key_down(event, window, cx);
                })
                .on_click(move |event, window, cx| {
                    if let Some(native_input) = native_input_for_click.as_ref() {
                        native_input.focus_handle.focus(window);
                    }
                    on_click(event, window, cx);
                });
            if let Some(native_input) = native_input_for_focus.as_ref() {
                element.track_focus(&native_input.focus_handle)
            } else {
                element
            }
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
                .relative()
                .flex_1()
                .min_w(px(0.0))
                .overflow_hidden()
                .flex()
                .items_center()
                .child(render_editable_text(
                    input.id,
                    &input.value,
                    &display_text,
                    font_size,
                    cursor_index,
                    selection_range,
                    marked_range,
                    input.is_focused,
                    runtime_focus_handle_for_text,
                    text_color,
                    selection_background,
                    cursor_color,
                ))
                .child(render_pointer_layer(
                    input.id,
                    pointer_value,
                    font_size,
                    cursor_index,
                    input.selection_range.clone(),
                    input.marked_range.clone(),
                    input.is_pointer_selecting,
                    on_pointer_select,
                    native_input_for_pointer,
                )),
        )
        .when_some(visible_trailing_accessory, |this, accessory| {
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

/// 渲染通用多行文本域，并将键盘输入、鼠标选择和清空按钮事件交给调用方处理。
///
/// 参数说明：
/// - `textarea`：文本域视觉和展示值配置。
/// - `theme`：当前主题令牌。
/// - `on_key_down`：键盘输入回调，通常更新调用方的本地状态。
/// - `on_click`：点击文本域回调，通常负责聚焦业务输入状态。
/// - `on_pointer_select`：鼠标选择回调，组件负责把位置转换为字符索引。
/// - `on_trailing_click`：后置附件点击回调，未配置后置附件时不会触发。
///
/// 返回值：GPUI 元素树；组件自身不保存业务状态。
pub(crate) fn render_textarea(
    textarea: Textarea,
    theme: &AppTheme,
    on_key_down: impl Fn(&KeyDownEvent, &mut Window, &mut App) + 'static,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    on_pointer_select: impl Fn(&InputPointerEvent, &mut Window, &mut App) + 'static,
    on_trailing_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    let font_size = 13.0_f32;
    let line_height = 18.0_f32;
    let vertical_padding = 8.0_f32;
    let horizontal_padding = 8.0_f32;
    let trailing_button_size = 24.0_f32;
    let visible_lines = textarea.visible_lines.max(3);
    let height = line_height * visible_lines as f32 + vertical_padding * 2.0;
    // 普通清除按钮失焦后隐藏；对话发送按钮可由调用方声明始终可见。
    let visible_trailing_accessory = textarea.trailing_accessory.filter(|_| {
        !textarea.is_disabled && (textarea.trailing_accessory_always_visible || textarea.is_focused)
    });
    let right_padding = if visible_trailing_accessory.is_some() {
        if textarea.style == TextareaStyle::Composer {
            // 对话编辑器在发送按钮左侧预留一个同尺寸操作位，供调用方放置停止等会话动作。
            trailing_button_size * 2.0 + horizontal_padding + 4.0
        } else {
            trailing_button_size + horizontal_padding
        }
    } else {
        horizontal_padding
    };
    let has_user_content = !textarea.value.is_empty();
    let display_text = if !has_user_content {
        textarea.placeholder.to_string()
    } else {
        textarea.value.clone()
    };
    let text_color = if !has_user_content {
        theme.foreground_muted
    } else {
        theme.foreground
    };
    let cursor_index = textarea.cursor_index.min(character_count(&textarea.value));
    let selection_range = textarea
        .selection_range
        .clone()
        .filter(|range| range.start < range.end);
    let marked_range = textarea
        .marked_range
        .clone()
        .filter(|range| range.start < range.end);
    let on_pointer_select = Rc::new(on_pointer_select);
    let pointer_value = textarea.value.clone();
    let native_input = textarea.native_input.clone();
    let native_input_for_focus = native_input.clone();
    let native_input_for_click = native_input.clone();
    let native_input_for_pointer = native_input.clone();
    let native_input_for_key = native_input.clone();
    let runtime_focus_handle_for_text = native_input
        .as_ref()
        .map(|native_input| native_input.focus_handle.clone());
    let runtime_focus_handle_for_scroll_sync = runtime_focus_handle_for_text.clone();
    let content_lines = textarea_text_lines(if !has_user_content {
        &display_text
    } else {
        &textarea.value
    });
    let content_line_count = content_lines.len().max(visible_lines);
    let content_height = content_line_count as f32 * line_height;
    let content_width = if !has_user_content {
        // 占位文案只用于提示，不属于用户内容，不能让空文本框出现横向滚动条。
        0.0
    } else {
        content_lines
            .iter()
            // 这里使用偏保守的字符宽度估算，确保中文、英文标点和长线程段都有足够横向滚动空间。
            .map(|line| character_count(&line.text) as f32 * font_size + 8.0)
            .fold(0.0_f32, f32::max)
    };
    let scroll_handle = textarea.scroll_handle.clone();
    let scroll_handle_for_viewport = scroll_handle.clone();
    let scroll_handle_for_wheel = scroll_handle.clone();
    let scroll_handle_for_pointer = scroll_handle.clone();
    let scroll_handle_for_bars = scroll_handle.clone();
    let scroll_state = textarea.scroll_state.clone();
    if !has_user_content {
        // 清空内容后同步清除旧偏移和拖拽态，避免上一段长文本的滚动信息残留到占位状态。
        scroll_handle.set_offset(point(px(0.0), px(0.0)));
        scroll_state.scrollbar_drag.replace(None);
        scroll_state.last_caret_sync.replace(None);
    }
    let scroll_state_for_sync = scroll_state.clone();
    let scroll_state_for_bars = scroll_state.clone();

    div()
        .id(textarea.id)
        .w_full()
        .relative()
        .bg(rgb(theme.content))
        .when(textarea.style == TextareaStyle::Default, |this| {
            this.rounded_sm().border_1().border_color(rgb(theme.border))
        })
        .when(textarea.style == TextareaStyle::Composer, |this| {
            this.rounded_lg()
                .border_1()
                .border_color(rgb(if textarea.is_focused {
                    theme.info
                } else {
                    theme.border
                }))
                .shadow_lg()
        })
        // 文本域自身可滚动；允许外层滚动命中后，滚到边界时页面仍能继续响应同方向滚轮。
        .block_mouse_except_scroll()
        .text_size(px(font_size))
        .text_color(rgb(text_color))
        .when(textarea.fill_height, |this| this.flex_1().min_h(px(0.0)))
        .when(!textarea.fill_height, |this| this.h(px(height)))
        .when(!textarea.is_disabled, |this| {
            let element = this
                .focusable()
                .hover({
                    let hover_border_color = if textarea.style == TextareaStyle::Composer {
                        theme.info
                    } else {
                        theme.foreground_muted
                    };
                    move |this| this.border_color(rgb(hover_border_color))
                })
                .on_key_down(move |event, window, cx| {
                    if native_input_for_key.is_some() && is_plain_text_key(event) {
                        return;
                    }
                    on_key_down(event, window, cx);
                })
                .on_click(move |event, window, cx| {
                    if let Some(native_input) = native_input_for_click.as_ref() {
                        native_input.focus_handle.focus(window);
                    }
                    on_click(event, window, cx);
                });
            if let Some(native_input) = native_input_for_focus.as_ref() {
                element.track_focus(&native_input.focus_handle)
            } else {
                element
            }
        })
        .when(textarea.is_disabled, |this| this.opacity(0.55))
        .child(
            div()
                .absolute()
                .left(px(horizontal_padding))
                .right(px(right_padding))
                .top(px(vertical_padding))
                .bottom(px(vertical_padding))
                .child(
                    div()
                        .relative()
                        .size_full()
                        .child(
                            div()
                                .id((textarea.id, 14usize))
                                .size_full()
                                .overflow_scroll()
                                // GPUI 的滚动条宽度会永久预留轨道；置零后只使用下方按真实溢出量绘制的浮层滑块。
                                .scrollbar_width(px(0.0))
                                .track_scroll(&scroll_handle_for_viewport)
                                .on_scroll_wheel(move |event, window, cx| {
                                    if !has_user_content {
                                        // 空文本域不消费滚轮，外层可滚动对话框应继续响应。
                                        return;
                                    }
                                    let current_offset = scroll_handle_for_wheel.offset();
                                    let next_offset = textarea_scroll_offset_for_wheel(
                                        current_offset,
                                        scroll_handle_for_wheel.max_offset(),
                                        event.delta.pixel_delta(window.line_height()),
                                    );
                                    if next_offset != current_offset {
                                        // 文本域尚可继续滚动时由自身消费；到达边界后不阻断事件，让外层页面接管。
                                        scroll_handle_for_wheel.set_offset(next_offset);
                                        window.refresh();
                                        cx.stop_propagation();
                                    }
                                })
                                .child(
                                    div()
                                        .relative()
                                        .h(px(content_height))
                                        .w_full()
                                        .min_w(px(content_width))
                                        .child(render_textarea_scroll_sync(
                                            textarea.id,
                                            textarea.value.clone(),
                                            font_size,
                                            line_height,
                                            cursor_index,
                                            textarea.is_focused,
                                            runtime_focus_handle_for_scroll_sync,
                                            scroll_handle,
                                            scroll_state_for_sync,
                                        ))
                                        .child(render_textarea_text(
                                            textarea.id,
                                            &textarea.value,
                                            &display_text,
                                            font_size,
                                            line_height,
                                            cursor_index,
                                            selection_range,
                                            marked_range,
                                            textarea.is_focused,
                                            runtime_focus_handle_for_text,
                                            text_color,
                                            theme.selection,
                                            theme.foreground,
                                        )),
                                ),
                        )
                        .child(render_textarea_pointer_layer(
                            textarea.id,
                            pointer_value,
                            font_size,
                            line_height,
                            cursor_index,
                            textarea.selection_range.clone(),
                            textarea.marked_range.clone(),
                            textarea.is_pointer_selecting,
                            on_pointer_select,
                            native_input_for_pointer,
                            scroll_handle_for_pointer,
                        ))
                        .children(render_textarea_scrollbars(
                            textarea.id,
                            scroll_handle_for_bars,
                            scroll_state_for_bars,
                            theme.foreground_muted,
                            has_user_content,
                        )),
                ),
        )
        .when_some(visible_trailing_accessory, |this, accessory| {
            this.child(
                div()
                    .absolute()
                    .right(px(4.0))
                    .when(
                        textarea.trailing_accessory_position == TextareaAccessoryPosition::TopRight,
                        |this| this.top(px(4.0)),
                    )
                    .when(
                        textarea.trailing_accessory_position
                            == TextareaAccessoryPosition::BottomRight,
                        |this| this.bottom(px(4.0)),
                    )
                    .child(render_icon_button(
                        accessory.id,
                        accessory.icon,
                        accessory.tooltip,
                        textarea.trailing_accessory_selected,
                        IconButtonSize::Tiny,
                        theme,
                        on_trailing_click,
                    )),
            )
        })
}

/// 判断是否应交给系统输入法处理的普通文本键。
fn is_plain_text_key(event: &KeyDownEvent) -> bool {
    event.keystroke.key_char.as_ref().is_some_and(|key_char| {
        !event.keystroke.modifiers.control
            && !event.keystroke.modifiers.platform
            && !event.keystroke.modifiers.function
            && !key_char.chars().any(char::is_control)
    })
}

/// 根据真实密码长度生成等长掩码文本，避免 UI 展示敏感字段。
fn mask_secret_value(value: &str) -> String {
    "•".repeat(character_count(value))
}

/// 判断业务焦点和 GPUI 真实焦点是否同时有效，避免输入法焦点已丢失时仍绘制闪烁光标。
fn effective_input_focus(
    is_focused: bool,
    runtime_focus_handle: Option<&FocusHandle>,
    window: &Window,
) -> bool {
    is_focused && runtime_focus_handle.is_none_or(|focus_handle| focus_handle.is_focused(window))
}

/// 渲染输入框文本、选区和光标；光标通过循环动画实现静止闪烁。
fn render_editable_text(
    input_id: &'static str,
    value: &str,
    display_text: &str,
    font_size: f32,
    cursor_index: usize,
    selection_range: Option<Range<usize>>,
    marked_range: Option<Range<usize>>,
    is_focused: bool,
    runtime_focus_handle: Option<FocusHandle>,
    text_color: u32,
    selection_background: u32,
    cursor_color: u32,
) -> impl IntoElement {
    let value = value.to_string();
    let display_text = display_text.to_string();
    let visual_value_for_canvas = if value.is_empty() {
        value.clone()
    } else {
        display_text.clone()
    };
    let display_text_for_canvas = display_text.clone();
    let selection_range_for_canvas = selection_range.clone();
    let marked_range_for_canvas = marked_range.clone();
    let visual_value_for_caret = if value.is_empty() {
        value.clone()
    } else {
        display_text.clone()
    };
    let runtime_focus_handle_for_canvas = runtime_focus_handle.clone();

    div()
        .relative()
        .h(px(18.0))
        .w_full()
        .min_w(px(0.0))
        .overflow_hidden()
        .child(
            canvas(
                |_, _, _| (),
                move |bounds, _, window: &mut Window, cx| {
                    let is_effectively_focused = effective_input_focus(
                        is_focused,
                        runtime_focus_handle_for_canvas.as_ref(),
                        window,
                    );
                    let is_placeholder = visual_value_for_canvas.is_empty();
                    let painted_text = display_text_for_canvas.as_str();
                    if painted_text.is_empty() {
                        return;
                    }

                    let shaped_line = shape_input_line(
                        painted_text,
                        font_size,
                        rgb(text_color).into(),
                        (!is_placeholder)
                            .then(|| marked_range_for_canvas.clone())
                            .flatten(),
                        window,
                    );
                    let scroll_x = if is_placeholder {
                        px(0.0)
                    } else {
                        input_scroll_x_for_shaped_line(
                            &visual_value_for_canvas,
                            cursor_index,
                            &shaped_line,
                            bounds.size.width,
                        )
                    };

                    if !is_placeholder
                        && is_effectively_focused
                        && let Some(range) = selection_range_for_canvas.as_ref()
                    {
                        paint_input_selection(
                            &visual_value_for_canvas,
                            range.clone(),
                            &shaped_line,
                            bounds,
                            scroll_x,
                            selection_background,
                            window,
                        );
                    }

                    let text_origin = point(bounds.left() - scroll_x, bounds.top());
                    let _ = shaped_line.paint(text_origin, bounds.size.height, window, cx);
                },
            )
            .size_full(),
        )
        .when(is_focused, |this| {
            this.child(render_caret(
                input_id,
                visual_value_for_caret.clone(),
                cursor_index.min(character_count(&visual_value_for_caret)),
                font_size,
                runtime_focus_handle,
                cursor_color,
            ))
        })
}

/// 渲染绝对定位闪烁光标，并用真实字形排版计算位置。
fn render_caret(
    input_id: &'static str,
    value: String,
    cursor_index: usize,
    font_size: f32,
    runtime_focus_handle: Option<FocusHandle>,
    cursor_color: u32,
) -> impl IntoElement {
    div()
        .id((input_id, 1usize))
        .absolute()
        .left_0()
        .top_0()
        .right_0()
        .bottom_0()
        .child(
            canvas(
                |_, _, _| (),
                move |bounds, _, window: &mut Window, _| {
                    if !effective_input_focus(true, runtime_focus_handle.as_ref(), window) {
                        return;
                    }
                    let (caret_x, scroll_x) = caret_x_and_scroll_for_character_index(
                        &value,
                        cursor_index,
                        font_size,
                        bounds.size.width,
                        window,
                    );
                    window.paint_quad(fill(
                        Bounds::new(
                            point(bounds.left() + caret_x - scroll_x, bounds.top() + px(1.0)),
                            size(px(1.0), px(16.0)),
                        ),
                        rgb(cursor_color),
                    ));
                },
            )
            .size_full(),
        )
        .with_animation(
            (input_id, 2usize),
            Animation::new(Duration::from_millis(900))
                .repeat()
                .with_easing(gpui::pulsating_between(0.08, 1.0)),
            |this, opacity| this.opacity(opacity),
        )
}

/// 按输入框字体生成单行排版，文本绘制、光标和鼠标命中都复用该结果。
fn shape_input_line(
    value: &str,
    font_size: f32,
    color: Hsla,
    marked_range: Option<Range<usize>>,
    window: &mut Window,
) -> ShapedLine {
    let mut text_style = window.text_style();
    text_style.font_size = px(font_size).into();
    let base_run = TextRun {
        len: value.len(),
        font: text_style.font(),
        color,
        background_color: None,
        underline: None,
        strikethrough: None,
    };
    let runs = input_text_runs(value, base_run, marked_range);

    window.text_system().shape_line(
        SharedString::from(value.to_string()),
        text_style.font_size.to_pixels(window.rem_size()),
        &runs,
        None,
    )
}

/// 生成输入框文本 run；marked text 使用下划线提示输入法候选态。
fn input_text_runs(
    value: &str,
    base_run: TextRun,
    marked_range: Option<Range<usize>>,
) -> Vec<TextRun> {
    let Some(marked_range) = marked_range.filter(|range| range.start < range.end) else {
        return vec![base_run];
    };
    let start = byte_index_for_character(value, marked_range.start);
    let end = byte_index_for_character(value, marked_range.end);

    [
        TextRun {
            len: start,
            ..base_run.clone()
        },
        TextRun {
            len: end.saturating_sub(start),
            underline: Some(UnderlineStyle {
                color: Some(base_run.color),
                thickness: px(1.0),
                wavy: false,
            }),
            ..base_run.clone()
        },
        TextRun {
            len: value.len().saturating_sub(end),
            ..base_run
        },
    ]
    .into_iter()
    .filter(|run| run.len > 0)
    .collect()
}

/// 绘制输入框选区背景，边界使用同一个 shaped line 的真实字符位置。
fn paint_input_selection(
    value: &str,
    range: Range<usize>,
    shaped_line: &ShapedLine,
    bounds: Bounds<Pixels>,
    scroll_x: Pixels,
    selection_background: u32,
    window: &mut Window,
) {
    let start_x = caret_x_for_shaped_character_index(value, range.start, shaped_line);
    let end_x = caret_x_for_shaped_character_index(value, range.end, shaped_line);
    if end_x <= start_x {
        return;
    }

    window.paint_quad(fill(
        Bounds::from_corners(
            point(bounds.left() + start_x - scroll_x, bounds.top() + px(1.0)),
            point(bounds.left() + end_x - scroll_x, bounds.bottom() - px(1.0)),
        ),
        rgb(selection_background),
    ));
}

/// 使用 GPUI 实际 shaped line 计算光标位置和横向滚动量，保证显示和鼠标命中完全对齐。
fn caret_x_and_scroll_for_character_index(
    value: &str,
    cursor_index: usize,
    font_size: f32,
    viewport_width: Pixels,
    window: &mut Window,
) -> (Pixels, Pixels) {
    if value.is_empty() {
        return (px(0.0), px(0.0));
    }

    let color = window.text_style().color;
    let shaped_line = shape_input_line(value, font_size, color, None, window);
    let cursor_index = cursor_index.min(character_count(value));
    let caret_x = caret_x_for_shaped_character_index(value, cursor_index, &shaped_line);
    let scroll_x =
        input_scroll_x_for_shaped_line(value, cursor_index, &shaped_line, viewport_width);

    (caret_x, scroll_x)
}

/// 返回指定字符索引对应的 shaped line 横坐标。
fn caret_x_for_shaped_character_index(
    value: &str,
    cursor_index: usize,
    shaped_line: &ShapedLine,
) -> Pixels {
    shaped_line.x_for_index(byte_index_for_character(value, cursor_index))
}

/// 根据当前光标和文本宽度计算单行输入框的横向滚动量。
fn input_scroll_x_for_shaped_line(
    value: &str,
    cursor_index: usize,
    shaped_line: &ShapedLine,
    viewport_width: Pixels,
) -> Pixels {
    if value.is_empty() {
        return px(0.0);
    }

    let text_length = character_count(value);
    let caret_x =
        caret_x_for_shaped_character_index(value, cursor_index.min(text_length), shaped_line);
    let content_width = caret_x_for_shaped_character_index(value, text_length, shaped_line);

    input_scroll_x_for_caret(caret_x, content_width, viewport_width)
}

/// 根据光标位置、内容宽度和视口宽度得到横向滚动量；该纯计算方便测试边界。
fn input_scroll_x_for_caret(
    caret_x: Pixels,
    content_width: Pixels,
    viewport_width: Pixels,
) -> Pixels {
    if viewport_width <= px(0.0) || content_width <= viewport_width {
        return px(0.0);
    }

    let margin = px(INPUT_HORIZONTAL_SCROLL_MARGIN).min(viewport_width / 2.0);
    // 末尾光标需要额外尾部空间，否则文本宽度刚好贴齐视口右侧时，1px 光标会被裁剪。
    let max_scroll = (content_width + margin - viewport_width).max(px(0.0));
    let visible_right = viewport_width - margin;
    if caret_x <= visible_right {
        px(0.0)
    } else {
        (caret_x + margin - viewport_width)
            .max(px(0.0))
            .min(max_scroll)
    }
}

/// 根据当前输入框快照计算横向滚动量，鼠标命中和输入法候选框定位都复用该偏移。
fn input_scroll_x_for_value(
    value: &str,
    cursor_index: usize,
    font_size: f32,
    viewport_width: Pixels,
    window: &mut Window,
) -> Pixels {
    if value.is_empty() {
        return px(0.0);
    }

    let color = window.text_style().color;
    let shaped_line = shape_input_line(value, font_size, color, None, window);
    input_scroll_x_for_shaped_line(value, cursor_index, &shaped_line, viewport_width)
}

/// 渲染输入框透明命中层，用于把鼠标选择转换成字符索引。
fn render_pointer_layer(
    input_id: &'static str,
    value: String,
    font_size: f32,
    cursor_index: usize,
    selection_range: Option<Range<usize>>,
    marked_range: Option<Range<usize>>,
    is_pointer_selecting: bool,
    on_pointer_select: Rc<impl Fn(&InputPointerEvent, &mut Window, &mut App) + 'static>,
    native_input: Option<NativeInput>,
) -> impl IntoElement {
    div()
        .id((input_id, 3usize))
        .absolute()
        .left(px(0.0))
        .top(px(0.0))
        .right(px(0.0))
        .bottom(px(0.0))
        .child(
            canvas(
                |_, _, _| (),
                move |bounds, _, window: &mut Window, cx| {
                    let visible_bounds = bounds.intersect(&window.content_mask().bounds);
                    if let Some(native_input) = native_input.as_ref() {
                        install_native_input_handler(
                            native_input,
                            value.clone(),
                            font_size,
                            cursor_index,
                            selection_range.clone(),
                            marked_range.clone(),
                            bounds,
                            window,
                            cx,
                        );
                    }

                    window.on_mouse_event({
                        let value = value.clone();
                        let on_pointer_select = on_pointer_select.clone();
                        let native_input = native_input.clone();
                        move |event: &MouseDownEvent, phase, window, cx| {
                            if !phase.bubble()
                                || event.button != MouseButton::Left
                                || !visible_bounds.contains(&event.position)
                            {
                                return;
                            }

                            if let Some(native_input) = native_input.as_ref() {
                                native_input.focus_handle.focus(window);
                            }
                            let character_index = input_character_index_from_pointer(
                                &value,
                                font_size,
                                cursor_index,
                                event.position.x,
                                bounds,
                                window,
                            );
                            on_pointer_select(
                                &InputPointerEvent {
                                    action: InputPointerAction::Begin,
                                    character_index,
                                    granularity: input_granularity_for_click_count(
                                        event.click_count,
                                    ),
                                },
                                window,
                                cx,
                            );
                            cx.stop_propagation();
                        }
                    });

                    window.on_mouse_event({
                        let value = value.clone();
                        let on_pointer_select = on_pointer_select.clone();
                        move |event: &MouseMoveEvent, phase, window, cx| {
                            if !phase.bubble() || !event.dragging() || !is_pointer_selecting {
                                return;
                            }

                            let character_index = input_character_index_from_pointer(
                                &value,
                                font_size,
                                cursor_index,
                                event.position.x,
                                bounds,
                                window,
                            );
                            on_pointer_select(
                                &InputPointerEvent {
                                    action: InputPointerAction::Extend,
                                    character_index,
                                    granularity: TextSelectionGranularity::Character,
                                },
                                window,
                                cx,
                            );
                            cx.stop_propagation();
                        }
                    });

                    window.on_mouse_event({
                        let on_pointer_select = on_pointer_select.clone();
                        move |event: &MouseUpEvent, phase, window, cx| {
                            if !phase.bubble()
                                || event.button != MouseButton::Left
                                || !is_pointer_selecting
                            {
                                return;
                            }

                            on_pointer_select(
                                &InputPointerEvent {
                                    action: InputPointerAction::Finish,
                                    character_index: 0,
                                    granularity: TextSelectionGranularity::Character,
                                },
                                window,
                                cx,
                            );
                            cx.stop_propagation();
                        }
                    });
                },
            )
            .size_full(),
        )
}

/// 渲染文本域滚动同步层，使键盘输入或光标移动后光标始终保持在可视范围内。
fn render_textarea_scroll_sync(
    textarea_id: &'static str,
    value: String,
    font_size: f32,
    line_height: f32,
    cursor_index: usize,
    is_focused: bool,
    runtime_focus_handle: Option<FocusHandle>,
    scroll_handle: ScrollHandle,
    scroll_state: TextareaScrollState,
) -> impl IntoElement {
    div()
        .id((textarea_id, 15usize))
        .absolute()
        .left_0()
        .top_0()
        .right_0()
        .bottom_0()
        .child(
            canvas(
                |_, _, _| (),
                move |_, _, window: &mut Window, _| {
                    if !effective_input_focus(is_focused, runtime_focus_handle.as_ref(), window)
                        || value.is_empty()
                        || scroll_state.scrollbar_drag.borrow().is_some()
                    {
                        return;
                    }

                    let sync_signature = TextareaCaretSyncSignature {
                        text_length: character_count(&value),
                        cursor_index,
                    };
                    if !textarea_should_sync_caret(&scroll_state, sync_signature) {
                        return;
                    }

                    let viewport_bounds = scroll_handle.bounds();
                    if viewport_bounds.size.width <= px(0.0)
                        || viewport_bounds.size.height <= px(0.0)
                    {
                        return;
                    }

                    let (caret_x, caret_y) = textarea_caret_local_position(
                        &value,
                        font_size,
                        line_height,
                        cursor_index,
                        window,
                    );
                    let current_offset = scroll_handle.offset();
                    let next_offset = textarea_scroll_offset_for_caret(
                        current_offset,
                        caret_x,
                        caret_y,
                        px(line_height),
                        viewport_bounds.size,
                        scroll_handle.max_offset(),
                    );
                    if next_offset != current_offset {
                        scroll_handle.set_offset(next_offset);
                    }
                    scroll_state.last_caret_sync.replace(Some(sync_signature));
                },
            )
            .size_full(),
        )
}

/// 判断本次绘制是否需要自动把 textarea 滚动到光标位置。
fn textarea_should_sync_caret(
    scroll_state: &TextareaScrollState,
    signature: TextareaCaretSyncSignature,
) -> bool {
    scroll_state.last_caret_sync.borrow().as_ref() != Some(&signature)
}

/// 文本域滚动条布局度量，供横纵两个方向复用。
#[derive(Clone, Copy, Debug, PartialEq)]
struct TextareaScrollbarMetrics {
    /// 滑块起点，使用滚动视口内部局部坐标。
    thumb_start: Pixels,
    /// 滑块长度。
    thumb_length: Pixels,
    /// 轨道起点，使用滚动视口内部局部坐标。
    track_start: Pixels,
    /// 轨道长度。
    track_length: Pixels,
    /// 最大滚动距离。
    max_scroll: Pixels,
}

/// 文本域自绘滚动条方向。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum TextareaScrollbarAxis {
    /// 横向滚动条。
    Horizontal,
    /// 纵向滚动条。
    Vertical,
}

/// 文本域自绘滚动条拖拽状态。
#[derive(Clone, Copy, Debug)]
struct TextareaScrollbarDrag {
    /// 当前拖拽的滚动条方向。
    axis: TextareaScrollbarAxis,
    /// 鼠标按下位置到滑块起点的距离。
    cursor_offset: Pixels,
}

/// 渲染文本域显式滚动条；只在对应方向内容溢出时显示。
fn render_textarea_scrollbars(
    textarea_id: &'static str,
    scroll_handle: ScrollHandle,
    scroll_state: TextareaScrollState,
    color: u32,
    has_user_content: bool,
) -> Vec<AnyElement> {
    let bounds = scroll_handle.bounds();
    let max_offset = scroll_handle.max_offset();
    let offset = scroll_handle.offset();
    let (has_horizontal, has_vertical) =
        textarea_scrollbar_visibility(has_user_content, max_offset.width, max_offset.height);
    let mut scrollbars = Vec::new();

    if (!has_horizontal && !has_vertical)
        || bounds.size.width <= px(0.0)
        || bounds.size.height <= px(0.0)
    {
        return scrollbars;
    }

    if has_vertical {
        let reserved_corner = if has_horizontal {
            px(TEXTAREA_SCROLLBAR_WIDTH)
        } else {
            px(0.0)
        };
        if let Some(metrics) = textarea_scrollbar_metrics(
            bounds.size.height - reserved_corner,
            max_offset.height,
            -offset.y,
        ) {
            scrollbars.push(render_textarea_scrollbar_thumb(
                textarea_id,
                TextareaScrollbarAxis::Vertical,
                metrics,
                bounds,
                scroll_handle.clone(),
                scroll_state.clone(),
                color,
            ));
        }
    }

    if has_horizontal {
        let reserved_corner = if has_vertical {
            px(TEXTAREA_SCROLLBAR_WIDTH)
        } else {
            px(0.0)
        };
        if let Some(metrics) = textarea_scrollbar_metrics(
            bounds.size.width - reserved_corner,
            max_offset.width,
            -offset.x,
        ) {
            scrollbars.push(render_textarea_scrollbar_thumb(
                textarea_id,
                TextareaScrollbarAxis::Horizontal,
                metrics,
                bounds,
                scroll_handle.clone(),
                scroll_state.clone(),
                color,
            ));
        }
    }

    scrollbars
}

/// 根据用户真实内容和布局溢出量判断应显示的滚动条方向。
fn textarea_scrollbar_visibility(
    has_user_content: bool,
    max_horizontal_offset: Pixels,
    max_vertical_offset: Pixels,
) -> (bool, bool) {
    if !has_user_content {
        return (false, false);
    }
    (
        max_horizontal_offset > px(0.5),
        max_vertical_offset > px(0.5),
    )
}

/// 渲染可拖拽的文本域滚动条滑块。
fn render_textarea_scrollbar_thumb(
    textarea_id: &'static str,
    axis: TextareaScrollbarAxis,
    metrics: TextareaScrollbarMetrics,
    viewport_bounds: Bounds<Pixels>,
    scroll_handle: ScrollHandle,
    scroll_state: TextareaScrollState,
    color: u32,
) -> AnyElement {
    let is_horizontal = axis == TextareaScrollbarAxis::Horizontal;
    let mut thumb = div()
        .id(SharedString::from(format!(
            "{textarea_id}-scrollbar-{axis:?}"
        )))
        .absolute()
        .rounded_full()
        .bg(rgb(color))
        .opacity(0.55)
        .hover(|this| this.opacity(0.8))
        .cursor_pointer()
        // 滚动条滑块只阻断点击拖拽，滚轮仍交给文本域或外层页面处理。
        .block_mouse_except_scroll();

    thumb = if is_horizontal {
        thumb
            .left(metrics.thumb_start)
            .bottom(px(TEXTAREA_SCROLLBAR_PADDING))
            .w(metrics.thumb_length)
            .h(px(TEXTAREA_SCROLLBAR_THUMB_SIZE))
    } else {
        thumb
            .right(px(TEXTAREA_SCROLLBAR_PADDING))
            .top(metrics.thumb_start)
            .w(px(TEXTAREA_SCROLLBAR_THUMB_SIZE))
            .h(metrics.thumb_length)
    };

    thumb
        .child(
            canvas(
                |_, _, _| (),
                move |thumb_bounds, _, window: &mut Window, _| {
                    window.on_mouse_event({
                        let scroll_state = scroll_state.clone();
                        move |event: &MouseDownEvent, phase, window, cx| {
                            if !phase.bubble()
                                || event.button != MouseButton::Left
                                || !thumb_bounds.contains(&event.position)
                            {
                                return;
                            }

                            let pointer = if is_horizontal {
                                event.position.x
                            } else {
                                event.position.y
                            };
                            let thumb_start = if is_horizontal {
                                thumb_bounds.origin.x
                            } else {
                                thumb_bounds.origin.y
                            };
                            scroll_state
                                .scrollbar_drag
                                .replace(Some(TextareaScrollbarDrag {
                                    axis,
                                    cursor_offset: pointer - thumb_start,
                                }));
                            cx.stop_propagation();
                            window.refresh();
                        }
                    });

                    window.on_mouse_event({
                        let scroll_state = scroll_state.clone();
                        move |event: &MouseUpEvent, phase, window, cx| {
                            if !phase.bubble() || event.button != MouseButton::Left {
                                return;
                            }

                            let handled = scroll_state.scrollbar_drag.borrow().is_some();
                            if handled {
                                scroll_state.scrollbar_drag.replace(None);
                                cx.stop_propagation();
                                window.refresh();
                            }
                        }
                    });

                    window.on_mouse_event({
                        let scroll_state = scroll_state.clone();
                        let scroll_handle = scroll_handle.clone();
                        move |event: &MouseMoveEvent, phase, window, cx| {
                            if !phase.bubble() || !event.dragging() {
                                return;
                            }

                            let Some(drag) = *scroll_state.scrollbar_drag.borrow() else {
                                return;
                            };
                            if drag.axis != axis {
                                return;
                            }

                            let pointer = if is_horizontal {
                                event.position.x - viewport_bounds.left()
                            } else {
                                event.position.y - viewport_bounds.top()
                            };
                            let scroll = textarea_scroll_for_scrollbar_drag(
                                pointer,
                                drag.cursor_offset,
                                metrics,
                            );
                            let current = scroll_handle.offset();
                            if is_horizontal {
                                scroll_handle.set_offset(point(-scroll, current.y));
                            } else {
                                scroll_handle.set_offset(point(current.x, -scroll));
                            }
                            cx.stop_propagation();
                            window.refresh();
                        }
                    });
                },
            )
            .size_full(),
        )
        .into_any_element()
}

/// 根据视口长度和最大滚动距离计算滚动条滑块位置。
fn textarea_scrollbar_metrics(
    viewport_length: Pixels,
    max_scroll: Pixels,
    current_scroll: Pixels,
) -> Option<TextareaScrollbarMetrics> {
    if viewport_length <= px(0.0) || max_scroll <= px(0.0) {
        return None;
    }

    let track_padding = px(TEXTAREA_SCROLLBAR_PADDING);
    let track_length = (viewport_length - track_padding * 2.0).max(px(1.0));
    let content_length = viewport_length + max_scroll;
    let thumb_length = ((viewport_length / content_length) * track_length)
        .clamp(px(TEXTAREA_SCROLLBAR_MIN_THUMB), track_length);
    let movable_length = (track_length - thumb_length).max(px(0.0));
    let ratio = (current_scroll / max_scroll).clamp(0.0, 1.0);

    Some(TextareaScrollbarMetrics {
        thumb_start: track_padding + movable_length * ratio,
        thumb_length,
        track_start: track_padding,
        track_length,
        max_scroll,
    })
}

/// 根据拖拽中的鼠标位置换算目标滚动距离。
fn textarea_scroll_for_scrollbar_drag(
    pointer: Pixels,
    cursor_offset: Pixels,
    metrics: TextareaScrollbarMetrics,
) -> Pixels {
    let movable_length = (metrics.track_length - metrics.thumb_length).max(px(1.0));
    let thumb_start =
        (pointer - cursor_offset).clamp(metrics.track_start, metrics.track_start + movable_length);
    let ratio = (thumb_start - metrics.track_start) / movable_length;
    metrics.max_scroll * ratio
}

/// 文本域单行布局信息，保存该行在完整文本中的字符范围。
#[derive(Clone, Debug, Eq, PartialEq)]
struct TextareaLine {
    /// 行内展示文本，不包含换行符。
    text: String,
    /// 行首在完整文本中的字符索引。
    start: usize,
    /// 行尾在完整文本中的字符索引，不包含换行符。
    end: usize,
}

/// 渲染文本域多行文本、跨行选区、marked text 和光标。
fn render_textarea_text(
    textarea_id: &'static str,
    value: &str,
    display_text: &str,
    font_size: f32,
    line_height: f32,
    cursor_index: usize,
    selection_range: Option<Range<usize>>,
    marked_range: Option<Range<usize>>,
    is_focused: bool,
    runtime_focus_handle: Option<FocusHandle>,
    text_color: u32,
    selection_background: u32,
    cursor_color: u32,
) -> impl IntoElement {
    let value = value.to_string();
    let display_text = display_text.to_string();
    let is_placeholder = value.is_empty();
    let painted_text = if is_placeholder {
        display_text.clone()
    } else {
        value.clone()
    };
    let lines = textarea_text_lines(&painted_text);
    let lines_for_canvas = lines.clone();
    let lines_for_caret = lines.clone();
    let value_for_canvas = value.clone();
    let painted_text_for_canvas = painted_text.clone();
    let selection_range_for_canvas = selection_range.clone();
    let marked_range_for_canvas = marked_range.clone();
    let runtime_focus_handle_for_canvas = runtime_focus_handle.clone();

    div()
        .relative()
        .size_full()
        .child(
            canvas(
                |_, _, _| (),
                move |bounds, _, window: &mut Window, cx| {
                    let is_effectively_focused = effective_input_focus(
                        is_focused,
                        runtime_focus_handle_for_canvas.as_ref(),
                        window,
                    );
                    if painted_text_for_canvas.is_empty() {
                        return;
                    }

                    for (line_index, line) in lines_for_canvas.iter().enumerate() {
                        let line_origin = point(
                            bounds.left(),
                            bounds.top() + px(line_height * line_index as f32),
                        );
                        let shaped_line = shape_input_line(
                            &line.text,
                            font_size,
                            rgb(text_color).into(),
                            (!is_placeholder)
                                .then(|| {
                                    textarea_local_range(
                                        line,
                                        marked_range_for_canvas.clone(),
                                        &value_for_canvas,
                                    )
                                })
                                .flatten(),
                            window,
                        );

                        if !is_placeholder
                            && is_effectively_focused
                            && let Some(local_range) = textarea_local_range(
                                line,
                                selection_range_for_canvas.clone(),
                                &value_for_canvas,
                            )
                        {
                            paint_input_selection(
                                &line.text,
                                local_range,
                                &shaped_line,
                                Bounds::new(line_origin, size(bounds.size.width, px(line_height))),
                                px(0.0),
                                selection_background,
                                window,
                            );
                        }

                        if !line.text.is_empty() {
                            let _ = shaped_line.paint(line_origin, px(line_height), window, cx);
                        }
                    }
                },
            )
            .size_full(),
        )
        .when(is_focused, |this| {
            this.child(render_textarea_caret(
                textarea_id,
                lines_for_caret,
                cursor_index,
                font_size,
                line_height,
                runtime_focus_handle,
                cursor_color,
            ))
        })
}

/// 渲染文本域多行光标。
fn render_textarea_caret(
    textarea_id: &'static str,
    lines: Vec<TextareaLine>,
    cursor_index: usize,
    font_size: f32,
    line_height: f32,
    runtime_focus_handle: Option<FocusHandle>,
    cursor_color: u32,
) -> impl IntoElement {
    div()
        .id((textarea_id, 11usize))
        .absolute()
        .left_0()
        .top_0()
        .right_0()
        .bottom_0()
        .child(
            canvas(
                |_, _, _| (),
                move |bounds, _, window: &mut Window, _| {
                    if !effective_input_focus(true, runtime_focus_handle.as_ref(), window) {
                        return;
                    }
                    let (line_index, column) =
                        textarea_cursor_line_and_column(&lines, cursor_index);
                    let line_text = lines
                        .get(line_index)
                        .map(|line| line.text.as_str())
                        .unwrap_or("");
                    let color = window.text_style().color;
                    let shaped_line = shape_input_line(line_text, font_size, color, None, window);
                    let caret_x = caret_x_for_shaped_character_index(
                        line_text,
                        column.min(character_count(line_text)),
                        &shaped_line,
                    );
                    let caret_y = px(line_height * line_index as f32);
                    window.paint_quad(fill(
                        Bounds::new(
                            point(bounds.left() + caret_x, bounds.top() + caret_y + px(1.0)),
                            size(px(1.0), px(line_height - 2.0)),
                        ),
                        rgb(cursor_color),
                    ));
                },
            )
            .size_full(),
        )
        .with_animation(
            (textarea_id, 12usize),
            Animation::new(Duration::from_millis(900))
                .repeat()
                .with_easing(gpui::pulsating_between(0.08, 1.0)),
            |this, opacity| this.opacity(opacity),
        )
}

/// 渲染文本域透明命中层，用于把鼠标选择转换成多行字符索引。
fn render_textarea_pointer_layer(
    textarea_id: &'static str,
    value: String,
    font_size: f32,
    line_height: f32,
    cursor_index: usize,
    selection_range: Option<Range<usize>>,
    marked_range: Option<Range<usize>>,
    is_pointer_selecting: bool,
    on_pointer_select: Rc<impl Fn(&InputPointerEvent, &mut Window, &mut App) + 'static>,
    native_input: Option<NativeInput>,
    scroll_handle: ScrollHandle,
) -> impl IntoElement {
    div()
        .id((textarea_id, 13usize))
        .absolute()
        .left(px(0.0))
        .top(px(0.0))
        .right(px(0.0))
        .bottom(px(0.0))
        .child(
            canvas(
                |_, _, _| (),
                move |bounds, _, window: &mut Window, cx| {
                    let visible_bounds = bounds.intersect(&window.content_mask().bounds);
                    if let Some(native_input) = native_input.as_ref() {
                        install_native_textarea_handler(
                            native_input,
                            value.clone(),
                            font_size,
                            line_height,
                            cursor_index,
                            selection_range.clone(),
                            marked_range.clone(),
                            bounds,
                            scroll_handle.offset(),
                            window,
                            cx,
                        );
                    }

                    window.on_mouse_event({
                        let value = value.clone();
                        let on_pointer_select = on_pointer_select.clone();
                        let native_input = native_input.clone();
                        let scroll_handle = scroll_handle.clone();
                        move |event: &MouseDownEvent, phase, window, cx| {
                            if !phase.bubble()
                                || event.button != MouseButton::Left
                                || !visible_bounds.contains(&event.position)
                            {
                                return;
                            }

                            if let Some(native_input) = native_input.as_ref() {
                                native_input.focus_handle.focus(window);
                            }
                            let character_index = textarea_character_index_from_pointer(
                                &value,
                                font_size,
                                line_height,
                                event.position,
                                bounds,
                                scroll_handle.offset(),
                                window,
                            );
                            on_pointer_select(
                                &InputPointerEvent {
                                    action: InputPointerAction::Begin,
                                    character_index,
                                    granularity: input_granularity_for_click_count(
                                        event.click_count,
                                    ),
                                },
                                window,
                                cx,
                            );
                            cx.stop_propagation();
                        }
                    });

                    window.on_mouse_event({
                        let value = value.clone();
                        let on_pointer_select = on_pointer_select.clone();
                        let scroll_handle = scroll_handle.clone();
                        move |event: &MouseMoveEvent, phase, window, cx| {
                            if !phase.bubble() || !event.dragging() || !is_pointer_selecting {
                                return;
                            }

                            let character_index = textarea_character_index_from_pointer(
                                &value,
                                font_size,
                                line_height,
                                event.position,
                                bounds,
                                scroll_handle.offset(),
                                window,
                            );
                            on_pointer_select(
                                &InputPointerEvent {
                                    action: InputPointerAction::Extend,
                                    character_index,
                                    granularity: TextSelectionGranularity::Character,
                                },
                                window,
                                cx,
                            );
                            cx.stop_propagation();
                        }
                    });

                    window.on_mouse_event({
                        let on_pointer_select = on_pointer_select.clone();
                        move |event: &MouseUpEvent, phase, window, cx| {
                            if !phase.bubble()
                                || event.button != MouseButton::Left
                                || !is_pointer_selecting
                            {
                                return;
                            }

                            on_pointer_select(
                                &InputPointerEvent {
                                    action: InputPointerAction::Finish,
                                    character_index: 0,
                                    granularity: TextSelectionGranularity::Character,
                                },
                                window,
                                cx,
                            );
                            cx.stop_propagation();
                        }
                    });
                },
            )
            .size_full(),
        )
}

/// 在输入框绘制阶段安装系统输入处理器。
fn install_native_input_handler(
    native_input: &NativeInput,
    value: String,
    font_size: f32,
    cursor_index: usize,
    selection_range: Option<Range<usize>>,
    marked_range: Option<Range<usize>>,
    bounds: Bounds<Pixels>,
    window: &mut Window,
    cx: &mut App,
) {
    window.handle_input(
        &native_input.focus_handle,
        NativeInputHandler {
            value,
            font_size,
            cursor_index,
            selection_range,
            marked_range,
            bounds,
            on_edit: native_input.on_edit.clone(),
        },
        cx,
    );
}

/// 单行输入框系统输入处理器，把 UTF-16 输入法范围转换为项目内部字符范围。
struct NativeInputHandler {
    /// 当前文本快照。
    value: String,
    /// 当前输入框字号，用于候选窗定位。
    font_size: f32,
    /// 当前光标字符位置。
    cursor_index: usize,
    /// 当前选区字符范围。
    selection_range: Option<Range<usize>>,
    /// 当前输入法 marked text 字符范围。
    marked_range: Option<Range<usize>>,
    /// 输入框文本区域绘制边界。
    bounds: Bounds<Pixels>,
    /// 编辑写回回调。
    on_edit: NativeEditCallback,
}

impl NativeInputHandler {
    /// 返回当前选区；无选区时使用光标空范围。
    fn selected_range(&self) -> Range<usize> {
        self.selection_range
            .clone()
            .unwrap_or(self.cursor_index..self.cursor_index)
    }

    /// 根据输入法给出的 UTF-16 范围选择替换范围。
    fn replacement_range(&self, range_utf16: Option<Range<usize>>) -> Range<usize> {
        range_utf16
            .map(|range| character_range_for_utf16_range(&self.value, range))
            .or_else(|| self.marked_range.clone())
            .unwrap_or_else(|| self.selected_range())
    }

    /// 生成编辑后的选区范围。
    fn selected_range_after_edit(
        &self,
        replacement_range: &Range<usize>,
        new_text: &str,
        new_selected_range_utf16: Option<Range<usize>>,
    ) -> Range<usize> {
        if let Some(new_selected_range_utf16) = new_selected_range_utf16 {
            let relative_range =
                character_range_for_utf16_range(new_text, new_selected_range_utf16);
            replacement_range.start + relative_range.start
                ..replacement_range.start + relative_range.end
        } else {
            let cursor = replacement_range.start + character_count(new_text);
            cursor..cursor
        }
    }

    /// 创建写回业务状态的编辑对象。
    fn edit_for(
        &self,
        replacement_range: Range<usize>,
        new_text: &str,
        selected_range: Range<usize>,
        marked_range: Option<Range<usize>>,
    ) -> NativeTextEdit {
        NativeTextEdit {
            replacement_range,
            text: new_text.to_string(),
            selected_range,
            marked_range,
        }
    }

    /// 同步当前处理器快照，保证输入法同一事件内再次查询时能读到最新状态。
    fn apply_edit_snapshot(&mut self, edit: &NativeTextEdit) {
        self.value =
            replace_character_range(&self.value, edit.replacement_range.clone(), &edit.text);
        let text_length = character_count(&self.value);
        let selection_start = edit.selected_range.start.min(text_length);
        let selection_end = edit.selected_range.end.min(text_length);
        self.cursor_index = selection_end;
        self.selection_range =
            (selection_start != selection_end).then_some(selection_start..selection_end);
        self.marked_range = edit
            .marked_range
            .clone()
            .map(|range| range.start.min(text_length)..range.end.min(text_length))
            .filter(|range| range.start < range.end);
    }

    /// 计算指定字符范围在输入框中的屏幕边界，用于定位输入法候选窗。
    fn bounds_for_character_range(
        &self,
        range: Range<usize>,
        window: &mut Window,
    ) -> Option<Bounds<Pixels>> {
        let color = window.text_style().color;
        let shaped_line = shape_input_line(&self.value, self.font_size, color, None, window);
        let scroll_x = input_scroll_x_for_shaped_line(
            &self.value,
            self.cursor_index,
            &shaped_line,
            self.bounds.size.width,
        );
        let start = caret_x_for_shaped_character_index(&self.value, range.start, &shaped_line);
        let end = caret_x_for_shaped_character_index(&self.value, range.end, &shaped_line);
        Some(Bounds::from_corners(
            point(self.bounds.left() + start - scroll_x, self.bounds.top()),
            point(self.bounds.left() + end - scroll_x, self.bounds.bottom()),
        ))
    }
}

impl InputHandler for NativeInputHandler {
    /// 返回当前选区的 UTF-16 范围。
    fn selected_text_range(
        &mut self,
        _ignore_disabled_input: bool,
        _window: &mut Window,
        _cx: &mut App,
    ) -> Option<UTF16Selection> {
        Some(UTF16Selection {
            range: utf16_range_for_character_range(&self.value, self.selected_range()),
            reversed: false,
        })
    }

    /// 返回 marked text 的 UTF-16 范围。
    fn marked_text_range(&mut self, _window: &mut Window, _cx: &mut App) -> Option<Range<usize>> {
        self.marked_range
            .clone()
            .map(|range| utf16_range_for_character_range(&self.value, range))
    }

    /// 返回指定 UTF-16 范围内的文本。
    fn text_for_range(
        &mut self,
        range_utf16: Range<usize>,
        adjusted_range: &mut Option<Range<usize>>,
        _window: &mut Window,
        _cx: &mut App,
    ) -> Option<String> {
        let range = character_range_for_utf16_range(&self.value, range_utf16);
        adjusted_range.replace(utf16_range_for_character_range(&self.value, range.clone()));
        Some(slice_character_range(&self.value, range))
    }

    /// 替换文本并清除 marked text。
    fn replace_text_in_range(
        &mut self,
        replacement_range_utf16: Option<Range<usize>>,
        text: &str,
        window: &mut Window,
        cx: &mut App,
    ) {
        let replacement_range = self.replacement_range(replacement_range_utf16);
        let cursor = replacement_range.start + character_count(text);
        let edit = self.edit_for(replacement_range, text, cursor..cursor, None);
        (self.on_edit)(edit.clone(), window, cx);
        self.apply_edit_snapshot(&edit);
    }

    /// 替换文本并设置 marked text，用于输入法候选态。
    fn replace_and_mark_text_in_range(
        &mut self,
        replacement_range_utf16: Option<Range<usize>>,
        new_text: &str,
        new_selected_range_utf16: Option<Range<usize>>,
        window: &mut Window,
        cx: &mut App,
    ) {
        let replacement_range = self.replacement_range(replacement_range_utf16);
        let selected_range =
            self.selected_range_after_edit(&replacement_range, new_text, new_selected_range_utf16);
        let marked_range = (!new_text.is_empty())
            .then(|| replacement_range.start..replacement_range.start + character_count(new_text));
        let edit = self.edit_for(replacement_range, new_text, selected_range, marked_range);
        (self.on_edit)(edit.clone(), window, cx);
        self.apply_edit_snapshot(&edit);
    }

    /// 清除 marked text。
    fn unmark_text(&mut self, window: &mut Window, cx: &mut App) {
        let edit = NativeTextEdit {
            replacement_range: self.cursor_index..self.cursor_index,
            text: String::new(),
            selected_range: self.cursor_index..self.cursor_index,
            marked_range: None,
        };
        (self.on_edit)(edit, window, cx);
        self.marked_range = None;
    }

    /// 返回指定 UTF-16 范围的屏幕边界。
    fn bounds_for_range(
        &mut self,
        range_utf16: Range<usize>,
        window: &mut Window,
        _cx: &mut App,
    ) -> Option<Bounds<Pixels>> {
        let range = character_range_for_utf16_range(&self.value, range_utf16);
        self.bounds_for_character_range(range, window)
    }

    /// 返回鼠标点对应的 UTF-16 字符偏移。
    fn character_index_for_point(
        &mut self,
        point: gpui::Point<Pixels>,
        window: &mut Window,
        _cx: &mut App,
    ) -> Option<usize> {
        let character_index = input_character_index_from_pointer(
            &self.value,
            self.font_size,
            self.cursor_index,
            point.x,
            self.bounds,
            window,
        );
        Some(utf16_range_for_character_range(&self.value, character_index..character_index).start)
    }
}

/// 在文本域绘制阶段安装系统输入处理器。
fn install_native_textarea_handler(
    native_input: &NativeInput,
    value: String,
    font_size: f32,
    line_height: f32,
    cursor_index: usize,
    selection_range: Option<Range<usize>>,
    marked_range: Option<Range<usize>>,
    bounds: Bounds<Pixels>,
    scroll_offset: gpui::Point<Pixels>,
    window: &mut Window,
    cx: &mut App,
) {
    window.handle_input(
        &native_input.focus_handle,
        NativeTextareaInputHandler {
            value,
            font_size,
            line_height,
            cursor_index,
            selection_range,
            marked_range,
            bounds,
            scroll_offset,
            on_edit: native_input.on_edit.clone(),
        },
        cx,
    );
}

/// 多行文本域系统输入处理器，把 UTF-16 输入法范围转换为项目内部字符范围。
struct NativeTextareaInputHandler {
    /// 当前文本快照。
    value: String,
    /// 当前文本域字号，用于候选窗定位。
    font_size: f32,
    /// 当前文本域行高，用于候选窗定位和鼠标命中。
    line_height: f32,
    /// 当前光标字符位置。
    cursor_index: usize,
    /// 当前选区字符范围。
    selection_range: Option<Range<usize>>,
    /// 当前输入法 marked text 字符范围。
    marked_range: Option<Range<usize>>,
    /// 文本域可见视口边界。
    bounds: Bounds<Pixels>,
    /// 当前滚动内容相对可见视口的偏移，GPUI 向右/下滚动时为负值。
    scroll_offset: gpui::Point<Pixels>,
    /// 编辑写回回调。
    on_edit: NativeEditCallback,
}

impl NativeTextareaInputHandler {
    /// 返回当前选区；无选区时使用光标空范围。
    fn selected_range(&self) -> Range<usize> {
        self.selection_range
            .clone()
            .unwrap_or(self.cursor_index..self.cursor_index)
    }

    /// 根据输入法给出的 UTF-16 范围选择替换范围。
    fn replacement_range(&self, range_utf16: Option<Range<usize>>) -> Range<usize> {
        range_utf16
            .map(|range| character_range_for_utf16_range(&self.value, range))
            .or_else(|| self.marked_range.clone())
            .unwrap_or_else(|| self.selected_range())
    }

    /// 生成编辑后的选区范围。
    fn selected_range_after_edit(
        &self,
        replacement_range: &Range<usize>,
        new_text: &str,
        new_selected_range_utf16: Option<Range<usize>>,
    ) -> Range<usize> {
        if let Some(new_selected_range_utf16) = new_selected_range_utf16 {
            let relative_range =
                character_range_for_utf16_range(new_text, new_selected_range_utf16);
            replacement_range.start + relative_range.start
                ..replacement_range.start + relative_range.end
        } else {
            let cursor = replacement_range.start + character_count(new_text);
            cursor..cursor
        }
    }

    /// 创建写回业务状态的编辑对象。
    fn edit_for(
        &self,
        replacement_range: Range<usize>,
        new_text: &str,
        selected_range: Range<usize>,
        marked_range: Option<Range<usize>>,
    ) -> NativeTextEdit {
        NativeTextEdit {
            replacement_range,
            text: new_text.to_string(),
            selected_range,
            marked_range,
        }
    }

    /// 同步当前处理器快照，保证输入法同一事件内再次查询时能读到最新状态。
    fn apply_edit_snapshot(&mut self, edit: &NativeTextEdit) {
        self.value =
            replace_character_range(&self.value, edit.replacement_range.clone(), &edit.text);
        let text_length = character_count(&self.value);
        let selection_start = edit.selected_range.start.min(text_length);
        let selection_end = edit.selected_range.end.min(text_length);
        self.cursor_index = selection_end;
        self.selection_range =
            (selection_start != selection_end).then_some(selection_start..selection_end);
        self.marked_range = edit
            .marked_range
            .clone()
            .map(|range| range.start.min(text_length)..range.end.min(text_length))
            .filter(|range| range.start < range.end);
    }

    /// 计算指定字符范围在文本域中的屏幕边界，用于定位输入法候选窗。
    fn bounds_for_character_range(
        &self,
        range: Range<usize>,
        window: &mut Window,
    ) -> Option<Bounds<Pixels>> {
        let lines = textarea_text_lines(&self.value);
        let (line_index, column) = textarea_cursor_line_and_column(&lines, range.start);
        let line = lines.get(line_index)?;
        let color = window.text_style().color;
        let shaped_line = shape_input_line(&line.text, self.font_size, color, None, window);
        let start = caret_x_for_shaped_character_index(&line.text, column, &shaped_line);
        let end_column = (range.end.saturating_sub(line.start)).min(character_count(&line.text));
        let end = caret_x_for_shaped_character_index(&line.text, end_column, &shaped_line);
        let top =
            self.bounds.top() + self.scroll_offset.y + px(self.line_height * line_index as f32);
        Some(Bounds::from_corners(
            point(self.bounds.left() + self.scroll_offset.x + start, top),
            point(
                self.bounds.left() + self.scroll_offset.x + end,
                top + px(self.line_height),
            ),
        ))
    }
}

impl InputHandler for NativeTextareaInputHandler {
    /// 返回当前选区的 UTF-16 范围。
    fn selected_text_range(
        &mut self,
        _ignore_disabled_input: bool,
        _window: &mut Window,
        _cx: &mut App,
    ) -> Option<UTF16Selection> {
        Some(UTF16Selection {
            range: utf16_range_for_character_range(&self.value, self.selected_range()),
            reversed: false,
        })
    }

    /// 返回 marked text 的 UTF-16 范围。
    fn marked_text_range(&mut self, _window: &mut Window, _cx: &mut App) -> Option<Range<usize>> {
        self.marked_range
            .clone()
            .map(|range| utf16_range_for_character_range(&self.value, range))
    }

    /// 返回指定 UTF-16 范围内的文本。
    fn text_for_range(
        &mut self,
        range_utf16: Range<usize>,
        adjusted_range: &mut Option<Range<usize>>,
        _window: &mut Window,
        _cx: &mut App,
    ) -> Option<String> {
        let range = character_range_for_utf16_range(&self.value, range_utf16);
        adjusted_range.replace(utf16_range_for_character_range(&self.value, range.clone()));
        Some(slice_character_range(&self.value, range))
    }

    /// 替换文本并清除 marked text。
    fn replace_text_in_range(
        &mut self,
        replacement_range_utf16: Option<Range<usize>>,
        text: &str,
        window: &mut Window,
        cx: &mut App,
    ) {
        let replacement_range = self.replacement_range(replacement_range_utf16);
        let cursor = replacement_range.start + character_count(text);
        let edit = self.edit_for(replacement_range, text, cursor..cursor, None);
        (self.on_edit)(edit.clone(), window, cx);
        self.apply_edit_snapshot(&edit);
    }

    /// 替换文本并设置 marked text，用于输入法候选态。
    fn replace_and_mark_text_in_range(
        &mut self,
        replacement_range_utf16: Option<Range<usize>>,
        new_text: &str,
        new_selected_range_utf16: Option<Range<usize>>,
        window: &mut Window,
        cx: &mut App,
    ) {
        let replacement_range = self.replacement_range(replacement_range_utf16);
        let selected_range =
            self.selected_range_after_edit(&replacement_range, new_text, new_selected_range_utf16);
        let marked_range = (!new_text.is_empty())
            .then(|| replacement_range.start..replacement_range.start + character_count(new_text));
        let edit = self.edit_for(replacement_range, new_text, selected_range, marked_range);
        (self.on_edit)(edit.clone(), window, cx);
        self.apply_edit_snapshot(&edit);
    }

    /// 清除 marked text。
    fn unmark_text(&mut self, window: &mut Window, cx: &mut App) {
        let edit = NativeTextEdit {
            replacement_range: self.cursor_index..self.cursor_index,
            text: String::new(),
            selected_range: self.cursor_index..self.cursor_index,
            marked_range: None,
        };
        (self.on_edit)(edit, window, cx);
        self.marked_range = None;
    }

    /// 返回指定 UTF-16 范围的屏幕边界。
    fn bounds_for_range(
        &mut self,
        range_utf16: Range<usize>,
        window: &mut Window,
        _cx: &mut App,
    ) -> Option<Bounds<Pixels>> {
        let range = character_range_for_utf16_range(&self.value, range_utf16);
        self.bounds_for_character_range(range, window)
    }

    /// 返回鼠标点对应的 UTF-16 字符偏移。
    fn character_index_for_point(
        &mut self,
        point: gpui::Point<Pixels>,
        window: &mut Window,
        _cx: &mut App,
    ) -> Option<usize> {
        let character_index = textarea_character_index_from_pointer(
            &self.value,
            self.font_size,
            self.line_height,
            point,
            self.bounds,
            self.scroll_offset,
            window,
        );
        Some(utf16_range_for_character_range(&self.value, character_index..character_index).start)
    }
}

/// 根据点击次数返回输入框选择粒度。
fn input_granularity_for_click_count(click_count: usize) -> TextSelectionGranularity {
    match click_count {
        0 | 1 => TextSelectionGranularity::Character,
        2 => TextSelectionGranularity::Word,
        _ => TextSelectionGranularity::Line,
    }
}

/// 按换行符拆分文本域内容，并保留每行在完整文本中的字符范围。
fn textarea_text_lines(value: &str) -> Vec<TextareaLine> {
    let mut lines = Vec::new();
    let mut current = String::new();
    let mut line_start = 0_usize;

    for (character_index, character) in value.chars().enumerate() {
        if character == '\n' {
            lines.push(TextareaLine {
                text: std::mem::take(&mut current),
                start: line_start,
                end: character_index,
            });
            line_start = character_index + 1;
        } else {
            current.push(character);
        }
    }

    lines.push(TextareaLine {
        text: current,
        start: line_start,
        end: character_count(value),
    });
    lines
}

/// 把完整文本范围转换为当前行内范围；空交集返回 `None`。
fn textarea_local_range(
    line: &TextareaLine,
    range: Option<Range<usize>>,
    value: &str,
) -> Option<Range<usize>> {
    let range = range?;
    let value_length = character_count(value);
    // 设置窗口关闭或输入法状态切换时，渲染层可能短暂拿到旧行范围；
    // 这里先把完整文本范围和行范围裁剪到当前文本长度内，避免无符号下溢导致 UI 线程崩溃。
    let line_start = line.start.min(value_length);
    let line_end = line.end.min(value_length).max(line_start);
    let start = range.start.min(value_length).max(line_start);
    let mut end = range.end.min(value_length).min(line_end);
    // 当选区覆盖行尾换行符时，将选区绘制到行尾，符合多行文本域的视觉预期。
    if range.end > line_end && line_end < value_length {
        end = line_end;
    }
    if start >= end {
        return None;
    }

    let local_start = start.saturating_sub(line_start);
    let local_end = end.saturating_sub(line_start);
    (local_start < local_end).then_some(local_start..local_end)
}

/// 返回光标所在行和行内字符列。
fn textarea_cursor_line_and_column(lines: &[TextareaLine], cursor_index: usize) -> (usize, usize) {
    let Some(last_line) = lines.last() else {
        return (0, 0);
    };
    let cursor_index = cursor_index.min(last_line.end);

    for (line_index, line) in lines.iter().enumerate() {
        let is_last = line_index + 1 == lines.len();
        if cursor_index <= line.end || is_last {
            return (line_index, cursor_index.saturating_sub(line.start));
        }
    }

    let last_index = lines.len().saturating_sub(1);
    (last_index, last_line.end.saturating_sub(last_line.start))
}

/// 根据当前光标字符索引计算其在文本域内容坐标系中的位置。
fn textarea_caret_local_position(
    value: &str,
    font_size: f32,
    line_height: f32,
    cursor_index: usize,
    window: &mut Window,
) -> (Pixels, Pixels) {
    let lines = textarea_text_lines(value);
    let (line_index, column) = textarea_cursor_line_and_column(&lines, cursor_index);
    let line_text = lines
        .get(line_index)
        .map(|line| line.text.as_str())
        .unwrap_or("");
    let color = window.text_style().color;
    let shaped_line = shape_input_line(line_text, font_size, color, None, window);
    let caret_x = caret_x_for_shaped_character_index(
        line_text,
        column.min(character_count(line_text)),
        &shaped_line,
    );
    let caret_y = px(line_height * line_index as f32);

    (caret_x, caret_y)
}

/// 根据光标位置计算文本域滚动偏移，返回值使用 GPUI 负向滚动坐标。
fn textarea_scroll_offset_for_caret(
    current_offset: gpui::Point<Pixels>,
    caret_x: Pixels,
    caret_y: Pixels,
    line_height: Pixels,
    viewport_size: gpui::Size<Pixels>,
    max_offset: gpui::Size<Pixels>,
) -> gpui::Point<Pixels> {
    let horizontal_margin = px(INPUT_HORIZONTAL_SCROLL_MARGIN);
    let vertical_margin = px(TEXTAREA_VERTICAL_SCROLL_MARGIN);
    let max_scroll_x = max_offset.width.max(px(0.0));
    let max_scroll_y = max_offset.height.max(px(0.0));
    let mut scroll_x = (-current_offset.x).clamp(px(0.0), max_scroll_x);
    let mut scroll_y = (-current_offset.y).clamp(px(0.0), max_scroll_y);

    let caret_right = caret_x + px(1.0);
    let viewport_right = scroll_x + viewport_size.width;
    if caret_right + horizontal_margin > viewport_right {
        scroll_x = caret_right + horizontal_margin - viewport_size.width;
    } else if caret_x < scroll_x + horizontal_margin {
        scroll_x = (caret_x - horizontal_margin).max(px(0.0));
    }

    let caret_bottom = caret_y + line_height;
    let viewport_bottom = scroll_y + viewport_size.height;
    if caret_bottom + vertical_margin > viewport_bottom {
        scroll_y = caret_bottom + vertical_margin - viewport_size.height;
    } else if caret_y < scroll_y + vertical_margin {
        scroll_y = (caret_y - vertical_margin).max(px(0.0));
    }

    point(
        -scroll_x.clamp(px(0.0), max_scroll_x),
        -scroll_y.clamp(px(0.0), max_scroll_y),
    )
}

/// 根据滚轮增量计算文本域的新偏移；到达边界时保持不变，以便调用方把事件交还外层滚动区。
fn textarea_scroll_offset_for_wheel(
    current_offset: gpui::Point<Pixels>,
    max_offset: gpui::Size<Pixels>,
    mut delta: gpui::Point<Pixels>,
) -> gpui::Point<Pixels> {
    // 触控板可能同时产生两个轴的微小增量；沿主方向滚动可避免文本域斜向漂移。
    if delta.x != px(0.0) && delta.y != px(0.0) {
        if delta.x.abs() > delta.y.abs() {
            delta.y = px(0.0);
        } else {
            delta.x = px(0.0);
        }
    }
    point(
        (current_offset.x + delta.x).clamp(-max_offset.width.max(px(0.0)), px(0.0)),
        (current_offset.y + delta.y).clamp(-max_offset.height.max(px(0.0)), px(0.0)),
    )
}

/// 根据鼠标位置和 GPUI 字形布局计算文本域内的字符位置。
fn textarea_character_index_from_pointer(
    value: &str,
    font_size: f32,
    line_height: f32,
    pointer: gpui::Point<Pixels>,
    viewport_bounds: Bounds<Pixels>,
    scroll_offset: gpui::Point<Pixels>,
    window: &mut Window,
) -> usize {
    let lines = textarea_text_lines(value);
    if lines.is_empty() {
        return 0;
    }

    let relative_y = (pointer.y - viewport_bounds.top() - scroll_offset.y).max(px(0.0));
    let line_index = (((relative_y / px(1.0)) / line_height).floor() as usize)
        .min(lines.len().saturating_sub(1));
    let line = &lines[line_index];
    let text_relative_x = pointer.x - viewport_bounds.left() - scroll_offset.x;
    if text_relative_x <= px(0.0) {
        return line.start;
    }

    let color = window.text_style().color;
    let shaped_line = shape_input_line(&line.text, font_size, color, None, window);
    let column = character_index_for_shaped_x(&line.text, &shaped_line, text_relative_x);
    line.start + column
}

/// 根据鼠标横坐标和 GPUI 字形布局计算输入框内的字符位置。
fn input_character_index_from_pointer(
    value: &str,
    font_size: f32,
    cursor_index: usize,
    pointer_x: Pixels,
    bounds: Bounds<Pixels>,
    window: &mut Window,
) -> usize {
    if value.is_empty() {
        return 0;
    }

    let scroll_x =
        input_scroll_x_for_value(value, cursor_index, font_size, bounds.size.width, window);
    let text_relative_x = pointer_x - bounds.left() + scroll_x;
    if text_relative_x <= px(0.0) {
        return 0;
    }

    let color = window.text_style().color;
    let shaped_line = shape_input_line(value, font_size, color, None, window);

    character_index_for_shaped_x(value, &shaped_line, text_relative_x)
}

/// 根据每个真实字符边界选择最近光标位，避免英文标点等窄字形点击偏移。
fn character_index_for_shaped_x(value: &str, shaped_line: &ShapedLine, x: Pixels) -> usize {
    let text_length = character_count(value);
    if text_length == 0 || x <= px(0.0) {
        return 0;
    }

    let mut previous_index = 0_usize;
    let mut previous_x = px(0.0);
    for character_index in 1..=text_length {
        let boundary_x = caret_x_for_shaped_character_index(value, character_index, shaped_line);
        if x <= boundary_x {
            let midpoint = previous_x + (boundary_x - previous_x) / 2.0;
            return if x < midpoint {
                previous_index
            } else {
                character_index
            };
        }
        previous_index = character_index;
        previous_x = boundary_x;
    }

    text_length
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 将 Pixels 转为 f32，便于断言横向滚动量。
    fn pixels_to_f32(value: Pixels) -> f32 {
        value / px(1.0)
    }

    /// 内容宽度未超过视口时，不应产生横向滚动。
    #[test]
    fn input_scroll_keeps_short_content_at_origin() {
        let scroll = input_scroll_x_for_caret(px(80.0), px(180.0), px(200.0));

        assert_eq!(scroll, px(0.0));
    }

    /// 光标超过可视右侧边距时，输入框应把内容横向滚到光标附近。
    #[test]
    fn input_scroll_follows_caret_past_right_edge() {
        let scroll = input_scroll_x_for_caret(px(300.0), px(500.0), px(200.0));

        assert!((pixels_to_f32(scroll) - 108.0).abs() < 0.01);
    }

    /// 光标位于文本末尾时，滚动量应包含尾部边距，避免光标被右边界裁剪。
    #[test]
    fn input_scroll_keeps_trailing_caret_visible() {
        let scroll = input_scroll_x_for_caret(px(500.0), px(500.0), px(200.0));

        assert_eq!(scroll, px(308.0));
        assert!(px(500.0) - scroll <= px(192.0));
    }

    /// 文本域关闭或重新渲染时即使遇到旧行范围，也不能因范围相减导致崩溃。
    #[test]
    fn textarea_local_range_clamps_stale_line_bounds() {
        let stale_line = TextareaLine {
            text: String::new(),
            start: 8,
            end: 4,
        };

        assert_eq!(textarea_local_range(&stale_line, Some(0..12), "abc"), None);
    }

    /// 文本域光标越过右侧可视区时，应自动横向滚动并保留右侧边距。
    #[test]
    fn textarea_scroll_follows_caret_past_right_edge() {
        let offset = textarea_scroll_offset_for_caret(
            point(px(0.0), px(0.0)),
            px(260.0),
            px(0.0),
            px(18.0),
            size(px(120.0), px(80.0)),
            size(px(240.0), px(100.0)),
        );

        assert!((pixels_to_f32(offset.x) + 149.0).abs() < 0.01);
        assert_eq!(offset.y, px(0.0));
    }

    /// 文本域光标越过底部可视区时，应自动纵向滚动到当前行。
    #[test]
    fn textarea_scroll_follows_caret_past_bottom_edge() {
        let offset = textarea_scroll_offset_for_caret(
            point(px(0.0), px(0.0)),
            px(0.0),
            px(126.0),
            px(18.0),
            size(px(120.0), px(90.0)),
            size(px(240.0), px(120.0)),
        );

        assert_eq!(offset.x, px(0.0));
        assert!((pixels_to_f32(offset.y) + 58.0).abs() < 0.01);
    }

    /// 光标仍在当前可视区内时，文本域不应打断用户已有滚动位置。
    #[test]
    fn textarea_scroll_keeps_offset_when_caret_visible() {
        let offset = textarea_scroll_offset_for_caret(
            point(px(-80.0), px(-40.0)),
            px(100.0),
            px(54.0),
            px(18.0),
            size(px(120.0), px(90.0)),
            size(px(240.0), px(120.0)),
        );

        assert_eq!(offset, point(px(-80.0), px(-40.0)));
    }

    /// 文本域仍有剩余内容时，滚轮应推进自身偏移并由文本域消费。
    #[test]
    fn textarea_wheel_scroll_moves_within_content_bounds() {
        let offset = textarea_scroll_offset_for_wheel(
            point(px(0.0), px(-20.0)),
            size(px(0.0), px(100.0)),
            point(px(0.0), px(-24.0)),
        );

        assert_eq!(offset, point(px(0.0), px(-44.0)));
    }

    /// 文本域到达滚动边界后偏移不得继续变化，外层页面据此接管同方向滚轮。
    #[test]
    fn textarea_wheel_scroll_releases_event_at_boundary() {
        let offset = textarea_scroll_offset_for_wheel(
            point(px(0.0), px(-100.0)),
            size(px(0.0), px(100.0)),
            point(px(0.0), px(-24.0)),
        );

        assert_eq!(offset, point(px(0.0), px(-100.0)));
    }

    /// 同一光标和内容签名已经同步过时，不应再次强制滚动，避免覆盖用户手动滚动。
    #[test]
    fn textarea_caret_sync_signature_prevents_repeated_auto_scroll() {
        let scroll_state = TextareaScrollState::new();
        let signature = TextareaCaretSyncSignature {
            text_length: 20,
            cursor_index: 8,
        };

        assert!(textarea_should_sync_caret(&scroll_state, signature));
        scroll_state.last_caret_sync.replace(Some(signature));
        assert!(!textarea_should_sync_caret(&scroll_state, signature));
        assert!(textarea_should_sync_caret(
            &scroll_state,
            TextareaCaretSyncSignature {
                text_length: 21,
                cursor_index: 9,
            }
        ));
    }

    /// 文本域内容没有溢出时，不应绘制无意义的滚动条滑块。
    #[test]
    fn textarea_scrollbar_metrics_hidden_without_overflow() {
        assert_eq!(
            textarea_scrollbar_metrics(px(120.0), px(0.0), px(0.0)),
            None
        );
    }

    /// 空文本域即使滚动句柄残留旧溢出量，也不得继续显示任一方向的滚动条。
    #[test]
    fn textarea_scrollbars_hidden_without_user_content() {
        assert_eq!(
            textarea_scrollbar_visibility(false, px(80.0), px(160.0)),
            (false, false)
        );
        assert_eq!(
            textarea_scrollbar_visibility(true, px(80.0), px(160.0)),
            (true, true)
        );
    }

    /// 文本域内容溢出后，应根据当前滚动位置计算可见滑块。
    #[test]
    fn textarea_scrollbar_metrics_tracks_scroll_position() {
        let metrics = textarea_scrollbar_metrics(px(100.0), px(300.0), px(150.0))
            .expect("内容溢出时应显示滚动条");

        assert_eq!(metrics.thumb_length, px(24.5));
        assert!((pixels_to_f32(metrics.thumb_start) - 37.75).abs() < 0.01);
    }

    /// 拖拽文本域滚动条滑块时，应按轨道位置换算为目标滚动距离。
    #[test]
    fn textarea_scrollbar_drag_converts_pointer_to_scroll() {
        let metrics = TextareaScrollbarMetrics {
            thumb_start: px(10.0),
            thumb_length: px(20.0),
            track_start: px(2.0),
            track_length: px(102.0),
            max_scroll: px(400.0),
        };

        let scroll = textarea_scroll_for_scrollbar_drag(px(53.0), px(10.0), metrics);

        assert_eq!(scroll, px(200.0));
    }
}
