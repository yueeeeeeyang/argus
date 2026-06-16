//! 文件职责：提供 Argus 界面可复用的紧凑输入框组件。
//! 创建日期：2026-06-10
//! 修改日期：2026-06-16
//! 作者：Argus 开发团队
//! 主要功能：统一输入框尺寸、图标、占位文本、禁用态、系统输入法和键盘输入回调。

use crate::text_selection::{
    NativeTextEdit, TextSelectionGranularity, byte_index_for_character, character_count,
    character_range_for_utf16_range, replace_character_range, slice_character_range,
    utf16_range_for_character_range,
};
use crate::theme::AppTheme;
use crate::ui::components::icon::{ArgusIcon, render_icon};
use crate::ui::components::icon_button::{IconButtonSize, render_icon_button};
use gpui::{
    Animation, AnimationExt, App, Bounds, ClickEvent, FocusHandle, Hsla, InputHandler, IntoElement,
    KeyDownEvent, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, Pixels, ShapedLine,
    SharedString, TextRun, UTF16Selection, UnderlineStyle, Window, canvas, div, fill, point,
    prelude::*, px, rgb, size,
};
use std::ops::Range;
use std::rc::Rc;
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

/// 系统文本输入桥接配置，用于接收中文输入法等 IME 提交文本。
#[derive(Clone)]
pub struct NativeInput {
    /// 当前输入框对应的真实 GPUI 焦点句柄。
    pub focus_handle: FocusHandle,
    /// 系统输入提交后的业务写回回调。
    pub on_edit: Rc<dyn Fn(NativeTextEdit, &mut Window, &mut App)>,
}

impl NativeInput {
    /// 创建系统文本输入桥接配置。
    ///
    /// 参数说明：
    /// - `focus_handle`：输入框真实焦点句柄。
    /// - `on_edit`：输入法提交或 marked text 变化时的业务写回回调。
    ///
    /// 返回值：可传给 `Input` 的原生输入桥接配置。
    pub fn new(
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
    /// 输入法 marked text 字符范围，用于候选态替换和候选窗定位。
    pub marked_range: Option<Range<usize>>,
    /// 当前输入框是否正在进行鼠标拖拽选择。
    pub is_pointer_selecting: bool,
    /// 输入框尺寸规格。
    pub size: InputSize,
    /// 前置图标附件。
    pub leading_accessory: Option<InputAccessory>,
    /// 后置可点击图标附件。
    pub trailing_accessory: Option<InputAccessory>,
    /// 系统文本输入桥接配置；为空时退回按键事件输入。
    pub native_input: Option<NativeInput>,
}

/// 输入框鼠标选择阶段。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InputPointerAction {
    /// 鼠标按下，开始一次选择。
    Begin,
    /// 鼠标拖拽，扩展当前选择。
    Extend,
    /// 鼠标释放，结束当前选择。
    Finish,
}

/// 输入框鼠标选择事件；字符索引由组件根据文本布局计算。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct InputPointerEvent {
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
pub fn render_input(
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
    let marked_range = input
        .marked_range
        .clone()
        .filter(|range| range.start < range.end);
    let on_pointer_select = Rc::new(on_pointer_select);
    let pointer_value = input.value.clone();
    let native_input = input.native_input.clone();
    let native_input_for_focus = native_input.clone();
    let native_input_for_click = native_input.clone();
    let native_input_for_pointer = native_input.clone();
    let native_input_for_key = native_input.clone();

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
        .occlude()
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

/// 判断是否应交给系统输入法处理的普通文本键。
fn is_plain_text_key(event: &KeyDownEvent) -> bool {
    event.keystroke.key_char.as_ref().is_some_and(|key_char| {
        !event.keystroke.modifiers.control
            && !event.keystroke.modifiers.platform
            && !event.keystroke.modifiers.function
            && !key_char.chars().any(char::is_control)
    })
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
    text_color: u32,
    selection_background: u32,
    cursor_color: u32,
) -> impl IntoElement {
    let value = value.to_string();
    let display_text = display_text.to_string();
    let value_for_canvas = value.clone();
    let display_text_for_canvas = display_text.clone();
    let selection_range_for_canvas = selection_range.clone();
    let marked_range_for_canvas = marked_range.clone();

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
                    let is_placeholder = value_for_canvas.is_empty();
                    let painted_text = if is_placeholder {
                        display_text_for_canvas.as_str()
                    } else {
                        value_for_canvas.as_str()
                    };
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

                    if !is_placeholder && let Some(range) = selection_range_for_canvas.as_ref() {
                        paint_input_selection(
                            &value_for_canvas,
                            range.clone(),
                            &shaped_line,
                            bounds,
                            selection_background,
                            window,
                        );
                    }

                    let _ = shaped_line.paint(bounds.origin, bounds.size.height, window, cx);
                },
            )
            .size_full(),
        )
        .when(is_focused, |this| {
            this.child(render_caret(
                input_id,
                value.clone(),
                cursor_index.min(character_count(&value)),
                font_size,
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
                    let caret_x =
                        caret_x_for_character_index(&value, cursor_index, font_size, window);
                    window.paint_quad(fill(
                        Bounds::new(
                            point(bounds.left() + caret_x, bounds.top() + px(1.0)),
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
            point(bounds.left() + start_x, bounds.top() + px(1.0)),
            point(bounds.left() + end_x, bounds.bottom() - px(1.0)),
        ),
        rgb(selection_background),
    ));
}

/// 使用 GPUI 实际 shaped line 计算光标位置，保证显示和鼠标命中完全对齐。
fn caret_x_for_character_index(
    value: &str,
    cursor_index: usize,
    font_size: f32,
    window: &mut Window,
) -> Pixels {
    if value.is_empty() {
        return px(0.0);
    }

    let color = window.text_style().color;
    let shaped_line = shape_input_line(value, font_size, color, None, window);

    caret_x_for_shaped_character_index(value, cursor_index, &shaped_line)
}

/// 返回指定字符索引对应的 shaped line 横坐标。
fn caret_x_for_shaped_character_index(
    value: &str,
    cursor_index: usize,
    shaped_line: &ShapedLine,
) -> Pixels {
    shaped_line.x_for_index(byte_index_for_character(value, cursor_index))
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
                                || !bounds.contains(&event.position)
                            {
                                return;
                            }

                            if let Some(native_input) = native_input.as_ref() {
                                native_input.focus_handle.focus(window);
                            }
                            let character_index = input_character_index_from_pointer(
                                &value,
                                font_size,
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
    on_edit: Rc<dyn Fn(NativeTextEdit, &mut Window, &mut App)>,
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
        let start = caret_x_for_shaped_character_index(&self.value, range.start, &shaped_line);
        let end = caret_x_for_shaped_character_index(&self.value, range.end, &shaped_line);
        Some(Bounds::from_corners(
            point(self.bounds.left() + start, self.bounds.top()),
            point(self.bounds.left() + end, self.bounds.bottom()),
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
            point.x,
            self.bounds,
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

/// 根据鼠标横坐标和 GPUI 字形布局计算输入框内的字符位置。
fn input_character_index_from_pointer(
    value: &str,
    font_size: f32,
    pointer_x: Pixels,
    bounds: Bounds<Pixels>,
    window: &mut Window,
) -> usize {
    if value.is_empty() {
        return 0;
    }

    let text_relative_x = pointer_x - bounds.left();
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
