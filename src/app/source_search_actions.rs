//! 文件职责：维护来源树过滤输入框的本地状态和过滤索引。
//! 创建日期：2026-06-11
//! 修改日期：2026-06-16
//! 作者：Argus 开发团队
//! 主要功能：处理来源树过滤输入、鼠标选区、剪贴板操作和可见节点重建。

use std::borrow::Borrow;
use std::collections::HashSet;

use gpui::{ClipboardItem, Context, Keystroke};

use super::{ArgusApp, InputTextSelectionDrag};
use crate::infra::text_selection::{
    TextSelectionGranularity, character_count, insert_text_at_character_index,
    remove_character_range, slice_character_range, word_range_at,
};

impl ArgusApp {
    /// 打开来源树搜索输入模式，并准备接收目录树过滤关键字。
    pub(crate) fn open_source_tree_search(&mut self) {
        self.is_source_tree_search_open = true;
        self.source_tree_search_query.clear();
        self.source_tree_search_cursor = 0;
        self.source_tree_search_selection_anchor = None;
        self.source_tree_search_marked_range = None;
        self.source_tree_search_selection_drag = None;
        self.is_source_tree_search_focused = true;
        self.source_tree_search_animation_generation =
            self.source_tree_search_animation_generation.wrapping_add(1);
        self.filtered_source_ids.clear();
        self.placeholder_notice = "已打开来源树搜索，仅过滤已加载日志节点".to_string();
    }

    /// 关闭来源树搜索输入模式，清空过滤条件并恢复完整来源树。
    pub(crate) fn close_source_tree_search(&mut self) {
        self.is_source_tree_search_open = false;
        self.source_tree_search_query.clear();
        self.source_tree_search_cursor = 0;
        self.source_tree_search_selection_anchor = None;
        self.source_tree_search_marked_range = None;
        self.source_tree_search_selection_drag = None;
        self.is_source_tree_search_focused = false;
        self.source_tree_search_animation_generation =
            self.source_tree_search_animation_generation.wrapping_add(1);
        self.filtered_source_ids.clear();
        self.placeholder_notice = "已关闭来源树搜索".to_string();
    }

    /// 更新来源树搜索关键字，并同步重建过滤后的可见节点索引。
    pub(crate) fn update_source_tree_search_query(&mut self, query: String) {
        self.source_tree_search_query = query;
        self.source_tree_search_cursor = character_count(&self.source_tree_search_query);
        self.source_tree_search_selection_anchor = None;
        self.source_tree_search_marked_range = None;
        self.source_tree_search_selection_drag = None;
        self.rebuild_filtered_source_ids();

        if self.source_tree_search_query.is_empty() {
            self.placeholder_notice = "来源树搜索框为空，显示完整目录树".to_string();
        } else {
            self.placeholder_notice = format!(
                "来源树搜索「{}」命中 {} 个可见节点",
                self.source_tree_search_query,
                self.filtered_source_ids.len()
            );
        }
    }

    /// 设置来源树搜索框焦点状态；聚焦时将光标放到当前文本末尾。
    pub(crate) fn set_source_tree_search_focused(&mut self, is_focused: bool) {
        self.is_source_tree_search_focused = is_focused;
        if is_focused {
            self.source_tree_search_cursor = character_count(&self.source_tree_search_query);
            self.source_tree_search_selection_anchor = None;
            self.source_tree_search_marked_range = None;
            self.source_tree_search_selection_drag = None;
        } else {
            self.source_tree_search_marked_range = None;
            self.source_tree_search_selection_drag = None;
        }
    }

    /// 处理来源树搜索框按键输入，只改变本地过滤状态，不读取日志正文。
    pub(crate) fn handle_source_tree_search_key(
        &mut self,
        keystroke: &Keystroke,
        cx: &mut Context<Self>,
    ) {
        let key = keystroke.key.to_lowercase();

        if keystroke.modifiers.secondary() {
            match key.as_str() {
                "a" => {
                    self.select_all_source_tree_search();
                    self.placeholder_notice = "已全选来源树搜索关键字".to_string();
                }
                "c" => {
                    self.copy_source_tree_search_selection(cx);
                }
                "x" => {
                    self.cut_source_tree_search_selection(cx);
                }
                "v" => {
                    self.paste_source_tree_search_clipboard(cx);
                }
                "left" | "arrowleft" => {
                    self.move_source_tree_search_cursor(0, keystroke.modifiers.shift);
                }
                "right" | "arrowright" => {
                    let end = character_count(&self.source_tree_search_query);
                    self.move_source_tree_search_cursor(end, keystroke.modifiers.shift);
                }
                _ => {}
            }
            self.rebuild_filtered_source_ids();
            return;
        }

        match key.as_str() {
            "backspace" => {
                self.delete_source_tree_search_backward();
            }
            "delete" => self.delete_source_tree_search_forward(),
            "escape" => {
                self.close_source_tree_search();
                return;
            }
            "enter" => {
                self.placeholder_notice = if self.source_tree_search_query.is_empty() {
                    "来源树搜索框为空，未执行过滤".to_string()
                } else {
                    format!(
                        "来源树已按「{}」过滤当前已加载日志",
                        self.source_tree_search_query
                    )
                };
                return;
            }
            "left" | "arrowleft" => self.move_source_tree_search_left(keystroke.modifiers.shift),
            "right" | "arrowright" => self.move_source_tree_search_right(keystroke.modifiers.shift),
            "home" => self.move_source_tree_search_cursor(0, keystroke.modifiers.shift),
            "end" => {
                let end = character_count(&self.source_tree_search_query);
                self.move_source_tree_search_cursor(end, keystroke.modifiers.shift);
            }
            _ => {
                if let Some(key_char) = keystroke.key_char.as_ref()
                    && !keystroke.modifiers.control
                    && !keystroke.modifiers.alt
                    && !keystroke.modifiers.platform
                    && !keystroke.modifiers.function
                    && !key_char.chars().any(char::is_control)
                {
                    self.insert_source_tree_search_text(key_char);
                }
            }
        }

        self.rebuild_filtered_source_ids();
        if self.source_tree_search_query.is_empty() {
            self.placeholder_notice = "来源树搜索框为空，显示完整目录树".to_string();
        } else {
            self.placeholder_notice = format!(
                "来源树搜索「{}」命中 {} 个可见节点",
                self.source_tree_search_query,
                self.filtered_source_ids.len()
            );
        }
    }

    /// 返回来源树搜索框当前选区范围；没有有效选区时返回 `None`。
    pub(crate) fn source_tree_search_selection_range(&self) -> Option<std::ops::Range<usize>> {
        let anchor = self.source_tree_search_selection_anchor?;
        if anchor == self.source_tree_search_cursor {
            return None;
        }

        Some(anchor.min(self.source_tree_search_cursor)..anchor.max(self.source_tree_search_cursor))
    }

    /// 将来源树搜索框光标移动到指定字符位置，并根据 Shift 状态维护选区。
    pub(crate) fn move_source_tree_search_cursor(
        &mut self,
        next_cursor: usize,
        should_select: bool,
    ) {
        let previous_cursor = self.source_tree_search_cursor;
        let text_length = character_count(&self.source_tree_search_query);
        self.source_tree_search_cursor = next_cursor.min(text_length);
        self.source_tree_search_marked_range = None;
        self.source_tree_search_selection_drag = None;

        if should_select {
            if self.source_tree_search_selection_anchor.is_none() {
                self.source_tree_search_selection_anchor = Some(previous_cursor);
            }
        } else {
            self.source_tree_search_selection_anchor = None;
        }
    }

    /// 将来源树搜索框向左移动一个字符；存在选区时退到选区起点。
    pub(crate) fn move_source_tree_search_left(&mut self, should_select: bool) {
        if !should_select && let Some(selection_range) = self.source_tree_search_selection_range() {
            self.move_source_tree_search_cursor(selection_range.start, false);
            return;
        }

        self.move_source_tree_search_cursor(
            self.source_tree_search_cursor.saturating_sub(1),
            should_select,
        );
    }

    /// 将来源树搜索框向右移动一个字符；存在选区时跳到选区终点。
    fn move_source_tree_search_right(&mut self, should_select: bool) {
        if !should_select && let Some(selection_range) = self.source_tree_search_selection_range() {
            self.move_source_tree_search_cursor(selection_range.end, false);
            return;
        }

        self.move_source_tree_search_cursor(self.source_tree_search_cursor + 1, should_select);
    }

    /// 删除来源树搜索框当前选区，并返回是否发生了删除。
    fn delete_source_tree_search_selection(&mut self) -> bool {
        let Some(selection_range) = self.source_tree_search_selection_range() else {
            return false;
        };

        self.source_tree_search_query =
            remove_character_range(&self.source_tree_search_query, selection_range.clone());
        self.source_tree_search_cursor = selection_range.start;
        self.source_tree_search_selection_anchor = None;
        self.source_tree_search_marked_range = None;
        self.source_tree_search_selection_drag = None;
        true
    }

    /// 在光标位置插入文本；插入前会替换现有选区。
    pub(crate) fn insert_source_tree_search_text(&mut self, text: &str) {
        self.delete_source_tree_search_selection();
        self.source_tree_search_query = insert_text_at_character_index(
            &self.source_tree_search_query,
            self.source_tree_search_cursor,
            text,
        );
        self.source_tree_search_cursor += character_count(text);
        self.source_tree_search_selection_anchor = None;
        self.source_tree_search_marked_range = None;
        self.source_tree_search_selection_drag = None;
    }

    /// 向后删除一个字符；若存在选区则删除整个选区。
    pub(crate) fn delete_source_tree_search_backward(&mut self) {
        if self.delete_source_tree_search_selection() || self.source_tree_search_cursor == 0 {
            return;
        }

        let delete_range = self.source_tree_search_cursor - 1..self.source_tree_search_cursor;
        self.source_tree_search_query =
            remove_character_range(&self.source_tree_search_query, delete_range);
        self.source_tree_search_cursor -= 1;
        self.source_tree_search_marked_range = None;
        self.source_tree_search_selection_drag = None;
    }

    /// 向前删除一个字符；若存在选区则删除整个选区。
    fn delete_source_tree_search_forward(&mut self) {
        if self.delete_source_tree_search_selection() {
            return;
        }

        let text_length = character_count(&self.source_tree_search_query);
        if self.source_tree_search_cursor >= text_length {
            return;
        }

        let delete_range = self.source_tree_search_cursor..self.source_tree_search_cursor + 1;
        self.source_tree_search_query =
            remove_character_range(&self.source_tree_search_query, delete_range);
        self.source_tree_search_marked_range = None;
        self.source_tree_search_selection_drag = None;
    }

    /// 全选来源树搜索框文本。
    pub(crate) fn select_all_source_tree_search(&mut self) {
        self.source_tree_search_selection_anchor = Some(0);
        self.source_tree_search_cursor = character_count(&self.source_tree_search_query);
        self.source_tree_search_marked_range = None;
        self.source_tree_search_selection_drag = None;
    }

    /// 根据鼠标按下位置开始来源树搜索框选择。
    pub(crate) fn begin_source_tree_search_pointer_selection(
        &mut self,
        character_index: usize,
        granularity: TextSelectionGranularity,
    ) {
        let anchor_range =
            self.source_tree_search_range_for_granularity(character_index, granularity);
        self.apply_source_tree_search_pointer_range(anchor_range.clone(), anchor_range.clone());
        self.source_tree_search_selection_drag = Some(InputTextSelectionDrag {
            anchor_range,
            granularity,
        });
        self.source_tree_search_marked_range = None;
        self.is_source_tree_search_focused = true;
    }

    /// 鼠标拖拽过程中按初始粒度扩展来源树搜索框选区。
    pub(crate) fn update_source_tree_search_pointer_selection(&mut self, character_index: usize) {
        let Some(drag) = self.source_tree_search_selection_drag.clone() else {
            return;
        };
        let focus_range =
            self.source_tree_search_range_for_granularity(character_index, drag.granularity);
        self.apply_source_tree_search_pointer_range(drag.anchor_range, focus_range);
        self.source_tree_search_marked_range = None;
        self.is_source_tree_search_focused = true;
    }

    /// 结束来源树搜索框鼠标选择；空选区会退化为普通光标。
    pub(crate) fn finish_source_tree_search_pointer_selection(&mut self) {
        self.source_tree_search_selection_drag = None;
        if self.source_tree_search_selection_range().is_none() {
            self.source_tree_search_selection_anchor = None;
        }
    }

    /// 根据选择粒度返回来源树搜索框内的目标字符范围。
    fn source_tree_search_range_for_granularity(
        &self,
        character_index: usize,
        granularity: TextSelectionGranularity,
    ) -> std::ops::Range<usize> {
        let text_length = character_count(&self.source_tree_search_query);
        let cursor = character_index.min(text_length);
        match granularity {
            TextSelectionGranularity::Character => cursor..cursor,
            TextSelectionGranularity::Word => {
                word_range_at(&self.source_tree_search_query, cursor).unwrap_or(cursor..cursor)
            }
            TextSelectionGranularity::Line => 0..text_length,
        }
    }

    /// 将两个选择范围合并为最终输入框选区。
    fn apply_source_tree_search_pointer_range(
        &mut self,
        anchor_range: std::ops::Range<usize>,
        focus_range: std::ops::Range<usize>,
    ) {
        if focus_range.end <= anchor_range.start {
            self.source_tree_search_selection_anchor = Some(anchor_range.end);
            self.source_tree_search_cursor = focus_range.start;
        } else {
            self.source_tree_search_selection_anchor = Some(anchor_range.start);
            self.source_tree_search_cursor = anchor_range.end.max(focus_range.end);
        }
        self.source_tree_search_marked_range = None;
    }

    /// 返回来源树搜索框当前选中的文本。
    pub(crate) fn selected_source_tree_search_text(&self) -> Option<String> {
        let selection_range = self.source_tree_search_selection_range()?;
        Some(slice_character_range(
            &self.source_tree_search_query,
            selection_range,
        ))
    }

    /// 将来源树搜索框选区写入系统剪贴板。
    fn copy_source_tree_search_selection(&mut self, cx: &mut Context<Self>) {
        let Some(selected_text) = self.selected_source_tree_search_text() else {
            self.placeholder_notice = "来源树搜索框没有可复制的选中文本".to_string();
            return;
        };

        let app_context: &gpui::App = (&*cx).borrow();
        app_context.write_to_clipboard(ClipboardItem::new_string(selected_text));
        self.placeholder_notice = "已复制来源树搜索关键字选区".to_string();
    }

    /// 剪切来源树搜索框选区，并同步刷新过滤结果。
    fn cut_source_tree_search_selection(&mut self, cx: &mut Context<Self>) {
        let Some(selected_text) = self.selected_source_tree_search_text() else {
            self.placeholder_notice = "来源树搜索框没有可剪切的选中文本".to_string();
            return;
        };

        let app_context: &gpui::App = (&*cx).borrow();
        app_context.write_to_clipboard(ClipboardItem::new_string(selected_text));
        self.delete_source_tree_search_selection();
        self.rebuild_filtered_source_ids();
        self.placeholder_notice = "已剪切来源树搜索关键字选区".to_string();
    }

    /// 从系统剪贴板粘贴纯文本到来源树搜索框光标位置。
    fn paste_source_tree_search_clipboard(&mut self, cx: &mut Context<Self>) {
        let app_context: &gpui::App = (&*cx).borrow();
        let Some(clipboard_text) = app_context
            .read_from_clipboard()
            .and_then(|clipboard_item| clipboard_item.text())
        else {
            self.placeholder_notice = "系统剪贴板没有可粘贴文本".to_string();
            return;
        };

        self.insert_source_tree_search_text(&clipboard_text.replace(['\r', '\n'], " "));
        self.rebuild_filtered_source_ids();
        self.placeholder_notice = "已粘贴来源树搜索关键字".to_string();
    }

    /// 返回来源树当前是否正在使用非空关键字过滤。
    pub(crate) fn is_source_tree_filtering(&self) -> bool {
        self.is_source_tree_search_open && !self.source_tree_search_query.trim().is_empty()
    }

    /// 重建来源树过滤结果；匹配已加载日志候选和未完成单文件探测的压缩包，并保留祖先目录上下文。
    pub(crate) fn rebuild_filtered_source_ids(&mut self) {
        self.filtered_source_ids.clear();

        let query = self.source_tree_search_query.trim().to_lowercase();
        if query.is_empty() {
            return;
        }

        let ordered_source_ids = self.source_registry.tree_order_source_ids();
        let mut included_ids = HashSet::new();

        for source_id in ordered_source_ids.iter().copied() {
            if !self.is_source_selectable_for_search_selection(source_id)
                || !self
                    .source_registry
                    .search_key(source_id)
                    .map(|search_key| search_key.contains(&query))
                    .unwrap_or(false)
            {
                continue;
            }

            included_ids.extend(self.source_registry.ancestor_ids(source_id));
            included_ids.insert(source_id);
        }

        self.filtered_source_ids = ordered_source_ids
            .iter()
            .copied()
            .filter(|source_id| included_ids.contains(source_id))
            .collect();
    }
}
