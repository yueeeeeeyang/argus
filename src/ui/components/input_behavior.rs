//! 文件职责：为拥有本地 `TextInputState` 的子视图提供统一键盘、剪贴板和光标编辑行为。
//! 创建日期：2026-07-15
//! 修改日期：2026-07-15
//! 作者：Argus 开发团队
//! 主要功能：处理 Unicode 输入、删除、移动、选择、复制、剪切、粘贴以及单行或多行提交语义。

use gpui::{App, ClipboardItem, Keystroke};

use crate::infra::text_input::TextInputState;
use crate::infra::text_selection::{
    character_count, remove_character_range, replace_character_range, slice_character_range,
};

/// 输入按键处理结果，由具体子视图决定提交、关闭或只刷新界面。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum LocalInputAction {
    /// 按键不需要业务动作。
    None,
    /// 文本或光标状态已经变化。
    Changed,
    /// 单行 Enter 或多行 Cmd/Ctrl+Enter 请求提交。
    Submit,
    /// Escape 请求关闭当前弹窗。
    Close,
}

/// 处理本地输入状态的键盘和剪贴板事件。
///
/// 参数说明：
/// - `input`：当前输入状态。
/// - `keystroke`：GPUI 归一化按键。
/// - `multiline`：是否允许普通 Enter 插入换行；多行提交使用 Cmd/Ctrl+Enter。
/// - `cx`：用于读写系统剪贴板。
pub(crate) fn handle_local_input_key(
    input: &mut TextInputState,
    keystroke: &Keystroke,
    multiline: bool,
    cx: &mut App,
) -> LocalInputAction {
    if let Some(action) = handle_clipboard(input, keystroke, multiline, cx) {
        return action;
    }
    match keystroke.key.as_str() {
        "escape" => LocalInputAction::Close,
        "enter" if multiline && keystroke.modifiers.secondary() => LocalInputAction::Submit,
        "enter" if multiline => {
            insert_text(input, "\n");
            LocalInputAction::Changed
        }
        "enter" => LocalInputAction::Submit,
        "backspace" => {
            delete_backward(input);
            LocalInputAction::Changed
        }
        "delete" => {
            delete_forward(input);
            LocalInputAction::Changed
        }
        "left" | "arrowleft" => {
            move_cursor(
                input,
                input.cursor.saturating_sub(1),
                keystroke.modifiers.shift,
            );
            LocalInputAction::Changed
        }
        "right" | "arrowright" => {
            move_cursor(
                input,
                input
                    .cursor
                    .saturating_add(1)
                    .min(character_count(&input.value)),
                keystroke.modifiers.shift,
            );
            LocalInputAction::Changed
        }
        "home" => {
            move_cursor(input, 0, keystroke.modifiers.shift);
            LocalInputAction::Changed
        }
        "end" => {
            move_cursor(
                input,
                character_count(&input.value),
                keystroke.modifiers.shift,
            );
            LocalInputAction::Changed
        }
        _ => {
            if let Some(key_char) = keystroke.key_char.as_ref()
                && !keystroke.modifiers.control
                && !keystroke.modifiers.platform
                && !key_char.chars().any(char::is_control)
            {
                insert_text(input, key_char);
                LocalInputAction::Changed
            } else {
                LocalInputAction::None
            }
        }
    }
}

/// 处理复制、剪切、粘贴和全选快捷键。
fn handle_clipboard(
    input: &mut TextInputState,
    keystroke: &Keystroke,
    multiline: bool,
    cx: &mut App,
) -> Option<LocalInputAction> {
    if !keystroke.modifiers.secondary() {
        return None;
    }
    match keystroke.key.to_ascii_lowercase().as_str() {
        "v" => {
            if let Some(mut text) = cx.read_from_clipboard().and_then(|item| item.text()) {
                if !multiline {
                    text = text.replace(['\r', '\n'], " ");
                }
                insert_text(input, &text);
            }
            Some(LocalInputAction::Changed)
        }
        "c" => {
            if let Some(range) = input.selection_range() {
                cx.write_to_clipboard(ClipboardItem::new_string(slice_character_range(
                    &input.value,
                    range,
                )));
            }
            Some(LocalInputAction::None)
        }
        "x" => {
            if let Some(range) = input.selection_range() {
                cx.write_to_clipboard(ClipboardItem::new_string(slice_character_range(
                    &input.value,
                    range.clone(),
                )));
                replace_selection(input, range, "");
            }
            Some(LocalInputAction::Changed)
        }
        "a" => {
            input.cursor = character_count(&input.value);
            input.selection_anchor = Some(0);
            input.marked_range = None;
            input.selection_drag = None;
            Some(LocalInputAction::Changed)
        }
        _ => None,
    }
}

/// 在当前选区或光标处插入文本。
fn insert_text(input: &mut TextInputState, text: &str) {
    let range = input.selection_range().unwrap_or(
        input.cursor.min(character_count(&input.value))
            ..input.cursor.min(character_count(&input.value)),
    );
    let start = range.start;
    input.value = replace_character_range(&input.value, range, text);
    input.cursor = start + character_count(text);
    clear_selection(input);
}

/// 删除当前选区或光标前一个字符。
fn delete_backward(input: &mut TextInputState) {
    if let Some(range) = input.selection_range() {
        replace_selection(input, range, "");
        return;
    }
    if input.cursor == 0 {
        return;
    }
    let range = input.cursor - 1..input.cursor;
    input.value = remove_character_range(&input.value, range.clone());
    input.cursor = range.start;
    clear_selection(input);
}

/// 删除当前选区或光标后一个字符。
fn delete_forward(input: &mut TextInputState) {
    if let Some(range) = input.selection_range() {
        replace_selection(input, range, "");
        return;
    }
    let length = character_count(&input.value);
    if input.cursor >= length {
        return;
    }
    input.value = remove_character_range(&input.value, input.cursor..input.cursor + 1);
    clear_selection(input);
}

/// 替换指定字符范围并把光标移动到替换文本末尾。
fn replace_selection(input: &mut TextInputState, range: std::ops::Range<usize>, text: &str) {
    let start = range.start;
    input.value = replace_character_range(&input.value, range, text);
    input.cursor = start + character_count(text);
    clear_selection(input);
}

/// 移动光标并按 Shift 状态保留或清除选区锚点。
fn move_cursor(input: &mut TextInputState, next: usize, extend_selection: bool) {
    if extend_selection && input.selection_anchor.is_none() {
        input.selection_anchor = Some(input.cursor);
    } else if !extend_selection {
        input.selection_anchor = None;
    }
    input.cursor = next.min(character_count(&input.value));
    input.marked_range = None;
    input.selection_drag = None;
}

/// 清理编辑后的选区和输入法临时态。
fn clear_selection(input: &mut TextInputState) {
    input.selection_anchor = None;
    input.marked_range = None;
    input.selection_drag = None;
}
