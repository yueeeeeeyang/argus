//! 文件职责：提供日志查看器与输入框共享的文本选择工具。
//! 创建日期：2026-06-11
//! 修改日期：2026-06-11
//! 作者：Argus 开发团队
//! 主要功能：统一字符索引、UTF-8 字节索引、词边界和字符串编辑逻辑。

use std::ops::Range;

/// 文本选择粒度，用于统一日志正文和输入框的单击、双击、三击行为。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum TextSelectionGranularity {
    /// 字符级选择，通常由单击拖拽触发。
    Character,
    /// 词级选择，通常由双击触发。
    Word,
    /// 行级选择，通常由三击触发；单行输入框中等同全选。
    Line,
}

/// 返回字符串的字符数量，避免中文等多字节字符破坏光标位置。
pub(crate) fn character_count(text: &str) -> usize {
    text.chars().count()
}

/// 将字符索引转换为 UTF-16 偏移；系统输入法回调使用 UTF-16 范围。
pub(crate) fn utf16_offset_for_character_index(text: &str, character_index: usize) -> usize {
    text.chars()
        .take(character_index)
        .map(char::len_utf16)
        .sum()
}

/// 将 UTF-16 偏移转换为字符索引；越界时返回字符串末尾字符位置。
pub(crate) fn character_index_for_utf16_offset(text: &str, utf16_offset: usize) -> usize {
    let mut consumed_units = 0_usize;
    for (character_index, character) in text.chars().enumerate() {
        if consumed_units >= utf16_offset {
            return character_index;
        }
        consumed_units += character.len_utf16();
    }
    character_count(text)
}

/// 将字符范围转换为 UTF-16 范围。
pub(crate) fn utf16_range_for_character_range(text: &str, range: Range<usize>) -> Range<usize> {
    utf16_offset_for_character_index(text, range.start)
        ..utf16_offset_for_character_index(text, range.end)
}

/// 将 UTF-16 范围转换为字符范围，并自动夹在当前文本长度内。
pub(crate) fn character_range_for_utf16_range(text: &str, range: Range<usize>) -> Range<usize> {
    let start = character_index_for_utf16_offset(text, range.start);
    let end = character_index_for_utf16_offset(text, range.end);
    start.min(end)..start.max(end)
}

/// 将字符索引转换为 UTF-8 字节索引；越界时返回字符串末尾。
pub(crate) fn byte_index_for_character(text: &str, character_index: usize) -> usize {
    text.char_indices()
        .map(|(byte_index, _)| byte_index)
        .nth(character_index)
        .unwrap_or(text.len())
}

/// 将 UTF-8 字节下标转换为字符列，避免 GPUI 命中结果落在多字节字符中间。
pub(crate) fn char_column_for_byte_index(text: &str, byte_index: usize) -> usize {
    let byte_index = byte_index.min(text.len());
    let safe_byte_index = if text.is_char_boundary(byte_index) {
        byte_index
    } else {
        text.char_indices()
            .map(|(index, _)| index)
            .take_while(|index| *index < byte_index)
            .last()
            .unwrap_or(0)
    };

    text[..safe_byte_index].chars().count()
}

/// 截取指定字符范围内的文本，供复制和选区渲染复用。
pub(crate) fn slice_character_range(text: &str, range: Range<usize>) -> String {
    let start = byte_index_for_character(text, range.start);
    let end = byte_index_for_character(text, range.end);
    text[start..end].to_string()
}

/// 删除指定字符范围内的文本，并返回新字符串。
pub(crate) fn remove_character_range(text: &str, range: Range<usize>) -> String {
    let start = byte_index_for_character(text, range.start);
    let end = byte_index_for_character(text, range.end);
    let mut next_text = String::with_capacity(text.len().saturating_sub(end - start));
    next_text.push_str(&text[..start]);
    next_text.push_str(&text[end..]);
    next_text
}

/// 在指定字符位置插入文本，并返回新字符串。
pub(crate) fn insert_text_at_character_index(
    text: &str,
    character_index: usize,
    inserted_text: &str,
) -> String {
    let byte_index = byte_index_for_character(text, character_index);
    let mut next_text = String::with_capacity(text.len() + inserted_text.len());
    next_text.push_str(&text[..byte_index]);
    next_text.push_str(inserted_text);
    next_text.push_str(&text[byte_index..]);
    next_text
}

/// 替换指定字符范围内的文本，并返回新字符串。
pub(crate) fn replace_character_range(
    text: &str,
    range: Range<usize>,
    replacement: &str,
) -> String {
    let start = byte_index_for_character(text, range.start);
    let end = byte_index_for_character(text, range.end);
    let mut next_text = String::with_capacity(text.len() + replacement.len());
    next_text.push_str(&text[..start]);
    next_text.push_str(replacement);
    next_text.push_str(&text[end..]);
    next_text
}

/// 系统文本输入提交的一次编辑，范围均已转换为项目内部字符索引。
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct NativeTextEdit {
    /// 被替换的旧文本字符范围。
    pub replacement_range: Range<usize>,
    /// 写入的新文本。
    pub text: String,
    /// 编辑后的选区或光标范围。
    pub selected_range: Range<usize>,
    /// 输入法候选态 marked text 范围；为 `None` 时表示提交完成。
    pub marked_range: Option<Range<usize>>,
}

/// 返回指定字符列所在的词范围；点到空白或标点时返回 `None`。
pub(crate) fn word_range_at(text: &str, character_index: usize) -> Option<Range<usize>> {
    let chars = text.chars().collect::<Vec<_>>();
    if chars.is_empty() {
        return None;
    }

    let index = word_hit_index(&chars, character_index)?;
    let mut start = index;
    while start > 0 && is_selectable_word_char(chars[start - 1]) {
        start -= 1;
    }

    let mut end = index + 1;
    while end < chars.len() && is_selectable_word_char(chars[end]) {
        end += 1;
    }

    Some(start..end)
}

/// 根据光标列找到真正被命中的词字符，允许用户点在词尾右侧仍选中该词。
fn word_hit_index(chars: &[char], character_index: usize) -> Option<usize> {
    if character_index < chars.len() && is_selectable_word_char(chars[character_index]) {
        return Some(character_index);
    }

    if character_index == chars.len()
        && character_index > 0
        && is_selectable_word_char(chars[character_index - 1])
    {
        return Some(character_index - 1);
    }

    None
}

/// 判断字符是否属于双击选词范围；`.` 作为层级/扩展名分隔符，不并入同一个词。
fn is_selectable_word_char(ch: char) -> bool {
    ch.is_alphanumeric() || matches!(ch, '_' | '-')
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 验证中文、英文和常见日志令牌都按同一套词边界选中，点号分隔的片段会拆开。
    #[test]
    fn word_range_keeps_unicode_and_log_tokens_together() {
        let text = "中文 thread_001.zip java.lang.Class";

        assert_eq!(
            slice_character_range(text, word_range_at(text, 0).unwrap()),
            "中文"
        );
        assert_eq!(
            slice_character_range(text, word_range_at(text, 3).unwrap()),
            "thread_001"
        );
        assert_eq!(
            slice_character_range(text, word_range_at(text, 18).unwrap()),
            "java"
        );
        assert_eq!(
            slice_character_range(text, word_range_at(text, 23).unwrap()),
            "lang"
        );
        assert_eq!(
            slice_character_range(text, word_range_at(text, 28).unwrap()),
            "Class"
        );
    }

    /// 验证点到空白时不会误选相邻词。
    #[test]
    fn word_range_ignores_whitespace() {
        assert_eq!(word_range_at("abc def", 3), None);
    }

    /// 验证字符编辑工具不会按字节截断中文。
    #[test]
    fn character_editing_preserves_utf8_boundaries() {
        let text = "日a志";
        assert_eq!(slice_character_range(text, 0..1), "日");
        assert_eq!(remove_character_range(text, 1..2), "日志");
        assert_eq!(insert_text_at_character_index(text, 2, "b"), "日ab志");
    }

    /// 验证字符索引和 UTF-16 偏移在中文和代理对字符中保持稳定映射。
    #[test]
    fn utf16_offsets_round_trip_for_multibyte_text() {
        let text = "中a🧪文";

        assert_eq!(utf16_offset_for_character_index(text, 0), 0);
        assert_eq!(utf16_offset_for_character_index(text, 1), 1);
        assert_eq!(utf16_offset_for_character_index(text, 2), 2);
        assert_eq!(utf16_offset_for_character_index(text, 3), 4);
        assert_eq!(character_index_for_utf16_offset(text, 4), 3);
        assert_eq!(utf16_range_for_character_range(text, 1..3), 1..4);
        assert_eq!(character_range_for_utf16_range(text, 1..4), 1..3);
    }
}
