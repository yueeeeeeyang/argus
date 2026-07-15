//! 文件职责：维护应用内通用的一维文本输入状态。
//! 创建日期：2026-07-15
//! 修改日期：2026-07-15
//! 作者：Argus 开发团队
//! 主要功能：统一文本值、Unicode 字符光标、选区、输入法标记区、鼠标拖拽和焦点清理语义。

use std::ops::Range;

use crate::infra::text_selection::{
    NativeTextEdit, TextSelectionGranularity, character_count, replace_character_range,
    word_range_at,
};

/// 单行或多行输入框的拖拽选择状态，记录按下位置和当前选取粒度。
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct InputTextSelectionDrag {
    /// 鼠标按下时形成的基础字符范围。
    pub anchor_range: Range<usize>,
    /// 当前拖拽粒度，决定移动时按字符、词或整行扩展。
    pub granularity: TextSelectionGranularity,
}

/// 应用通用文本输入状态；字符位置均按 Unicode 标量值计数，不直接使用 UTF-8 字节下标。
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct TextInputState {
    /// 输入框当前文本。
    pub value: String,
    /// 光标字符位置。
    pub cursor: usize,
    /// 选区锚点；与光标不一致时表示存在选区。
    pub selection_anchor: Option<usize>,
    /// 输入法 marked text 字符范围，候选态替换时使用。
    pub marked_range: Option<Range<usize>>,
    /// 鼠标拖拽选区状态。
    pub selection_drag: Option<InputTextSelectionDrag>,
    /// 是否处于焦点状态。
    pub is_focused: bool,
}

impl TextInputState {
    /// 根据已有文本构造输入状态，光标默认位于文本末尾。
    pub(crate) fn from_value(value: String) -> Self {
        let cursor = character_count(&value);
        Self {
            value,
            cursor,
            selection_anchor: None,
            marked_range: None,
            selection_drag: None,
            is_focused: false,
        }
    }

    /// 返回归一化后的非空选区；正向与反向选择均以升序字符范围表示。
    pub(crate) fn selection_range(&self) -> Option<Range<usize>> {
        let anchor = self.selection_anchor?;
        let start = anchor.min(self.cursor);
        let end = anchor.max(self.cursor);
        (start < end).then_some(start..end)
    }

    /// 应用系统原生文本编辑快照，统一处理 Unicode 范围、IME 标记区和编辑后选区。
    pub(crate) fn apply_native_edit(&mut self, edit: &NativeTextEdit) {
        let text_length = character_count(&self.value);
        let replacement_range = clamp_range(edit.replacement_range.clone(), text_length);
        let next_value = replace_character_range(&self.value, replacement_range, &edit.text);
        let next_length = character_count(&next_value);
        let selected_range = clamp_range(edit.selected_range.clone(), next_length);

        self.value = next_value;
        self.cursor = selected_range.end;
        self.selection_anchor =
            (selected_range.start != selected_range.end).then_some(selected_range.start);
        self.marked_range = edit
            .marked_range
            .clone()
            .map(|range| clamp_range(range, next_length))
            .filter(|range| range.start < range.end);
        self.selection_drag = None;
    }

    /// 清理瞬时焦点、选区和输入法状态，同时保留文本与光标供再次编辑。
    pub(crate) fn clear_focus(&mut self) {
        self.is_focused = false;
        self.selection_anchor = None;
        self.marked_range = None;
        self.selection_drag = None;
    }

    /// 按单双三击粒度返回目标字符范围；多行文本的三击只覆盖当前行。
    pub(crate) fn range_for_granularity(
        &self,
        character_index: usize,
        granularity: TextSelectionGranularity,
    ) -> Range<usize> {
        let text_length = character_count(&self.value);
        let cursor = character_index.min(text_length);
        match granularity {
            TextSelectionGranularity::Character => cursor..cursor,
            TextSelectionGranularity::Word => {
                word_range_at(&self.value, cursor).unwrap_or(cursor..cursor)
            }
            TextSelectionGranularity::Line => current_line_range(&self.value, cursor),
        }
    }

    /// 开始鼠标选区，记录初始范围以便后续拖拽保持单双三击粒度。
    pub(crate) fn begin_pointer_selection(
        &mut self,
        character_index: usize,
        granularity: TextSelectionGranularity,
    ) {
        let range = self.range_for_granularity(character_index, granularity);
        self.cursor = range.end;
        self.selection_anchor = Some(range.start);
        self.marked_range = None;
        self.selection_drag = Some(InputTextSelectionDrag {
            anchor_range: range,
            granularity,
        });
    }

    /// 更新鼠标拖拽选区，并保留从右向左拖拽时的真实锚点方向。
    pub(crate) fn update_pointer_selection(&mut self, character_index: usize) {
        let Some(drag) = self.selection_drag.clone() else {
            return;
        };
        let focus_range = self.range_for_granularity(character_index, drag.granularity);
        if focus_range.start < drag.anchor_range.start {
            self.selection_anchor = Some(drag.anchor_range.end);
            self.cursor = focus_range.start;
        } else {
            self.selection_anchor = Some(drag.anchor_range.start);
            self.cursor = focus_range.end;
        }
        self.marked_range = None;
    }

    /// 结束鼠标拖拽；空选区会清除锚点，避免后续键盘编辑误判为选中状态。
    pub(crate) fn finish_pointer_selection(&mut self) {
        self.selection_drag = None;
        if self.selection_range().is_none() {
            self.selection_anchor = None;
        }
    }
}

impl Default for TextInputState {
    /// 创建空文本输入状态。
    fn default() -> Self {
        Self::from_value(String::new())
    }
}

/// 将外部输入范围限制到当前文本长度内，并纠正可能反向的起止位置。
fn clamp_range(range: Range<usize>, text_length: usize) -> Range<usize> {
    let start = range.start.min(text_length);
    let end = range.end.min(text_length);
    start.min(end)..start.max(end)
}

/// 返回光标所在行的字符范围，不包含换行符。
fn current_line_range(value: &str, cursor: usize) -> Range<usize> {
    let chars = value.chars().collect::<Vec<_>>();
    let cursor = cursor.min(chars.len());
    let mut start = cursor;
    while start > 0 && chars[start - 1] != '\n' {
        start -= 1;
    }
    let mut end = cursor;
    while end < chars.len() && chars[end] != '\n' {
        end += 1;
    }
    start..end
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 验证反向选区会被统一归一化，业务模块无需重复判断方向。
    #[test]
    fn selection_range_normalizes_reverse_selection() {
        let mut input = TextInputState::from_value("甲乙丙".to_string());
        input.cursor = 1;
        input.selection_anchor = Some(3);

        assert_eq!(input.selection_range(), Some(1..3));
    }

    /// 验证原生输入编辑按字符范围替换中文，并完整保留输入法标记区。
    #[test]
    fn native_edit_preserves_unicode_and_ime_ranges() {
        let mut input = TextInputState::from_value("a中文b".to_string());
        input.apply_native_edit(&NativeTextEdit {
            replacement_range: 1..3,
            text: "输入".to_string(),
            selected_range: 3..3,
            marked_range: Some(1..3),
        });

        assert_eq!(input.value, "a输入b");
        assert_eq!(input.cursor, 3);
        assert_eq!(input.marked_range, Some(1..3));
    }

    /// 验证反向词拖拽仍保存真实方向，最终规范化选区覆盖两个词。
    #[test]
    fn reverse_word_drag_preserves_direction_and_range() {
        let mut input = TextInputState::from_value("first second third".to_string());
        input.begin_pointer_selection(14, TextSelectionGranularity::Word);
        input.update_pointer_selection(1);

        assert!(input.cursor < input.selection_anchor.expect("反向拖拽应保留锚点"));
        assert_eq!(input.selection_range(), Some(0..18));
    }
}
