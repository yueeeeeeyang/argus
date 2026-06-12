//! 文件职责：维护日志阅读区的文本选择、复制和分页滚动行为。
//! 创建日期：2026-06-11
//! 修改日期：2026-06-11
//! 作者：Argus 开发团队
//! 主要功能：将日志正文鼠标选择、键盘复制、只读粘贴提示和大日志滚动从主应用状态中拆分出来。

use std::borrow::Borrow;

use gpui::{
    ClipboardItem, Context, Keystroke, Pixels, ScrollWheelEvent, SharedString, TextRun, Window, px,
};

use super::{
    ArgusApp, LOG_VIEWER_ROW_HEIGHT, LOG_VIEWER_TEXT_LEFT_PADDING, LOG_VIEWER_TEXT_RIGHT_PADDING,
    LogTextPosition, LogTextSelection, LogTextSelectionDrag, TabKind, log_text_position_le,
    log_viewer_display_text, log_viewer_line_number_width,
};
use crate::fonts::ARGUS_LOG_FONT_FAMILY;
use crate::loader::SourceId;
use crate::reader::log_file_reader::{LogDocument, LogOpenState};
use crate::text_selection::{
    TextSelectionGranularity, byte_index_for_character, char_column_for_byte_index,
    character_count, slice_character_range, word_range_at,
};

impl ArgusApp {
    /// 返回当前是否存在可搜索的日志标签页。
    ///
    /// 返回值：只要标签列表中包含日志来源标签即返回 `true`；设置页和空标签不计入。
    pub fn has_open_log_tab(&self) -> bool {
        self.tabs
            .iter()
            .any(|tab| matches!(tab.kind, TabKind::LogSource { .. }))
    }

    /// 确保搜索功能拥有一个活动日志标签页。
    ///
    /// 返回值：当前已经是日志标签，或成功切换到第一个已打开日志标签时返回 `true`。
    pub fn ensure_active_log_tab_for_search(&mut self) -> bool {
        if matches!(self.active_tab_kind(), TabKind::LogSource { .. }) {
            return true;
        }

        let Some(tab_id) = self.tabs.iter().find_map(|tab| {
            if matches!(tab.kind, TabKind::LogSource { .. }) {
                Some(tab.id)
            } else {
                None
            }
        }) else {
            return false;
        };

        self.activate_tab(tab_id);
        true
    }

    /// 返回当前活动日志内容区是否拥有业务焦点，用于限制日志搜索快捷键的触发范围。
    pub fn is_active_log_view_focused(&self) -> bool {
        let Some(active_tab) = self.active_tab() else {
            return false;
        };
        if !matches!(active_tab.kind, TabKind::LogSource { .. }) {
            return false;
        }

        self.log_tab_view_state(active_tab.id)
            .is_some_and(|state| state.is_focused)
    }

    /// 标记指定日志 tab 获得内容焦点；不改变现有选区。
    pub fn focus_log_text_view(&mut self, tab_id: usize) {
        for (state_tab_id, state) in self.log_tab_view_states.iter_mut() {
            state.is_focused = *state_tab_id == tab_id;
        }
        self.log_tab_view_states
            .entry(tab_id)
            .or_default()
            .is_focused = true;
    }

    /// 清理所有日志内容焦点；保留选区本身，避免切换焦点时丢失已选文本。
    pub fn clear_log_text_focus(&mut self) {
        for state in self.log_tab_view_states.values_mut() {
            state.is_focused = false;
        }
    }

    /// 按标签页和行号读取一行日志文本，用于鼠标事件按需命中测试。
    ///
    /// 参数说明：
    /// - `tab_id`：日志标签页 ID。
    /// - `line_number`：需要读取的 0 基行号。
    ///
    /// 返回值：该行原始日志文本；标签不是日志、日志未读取完成或读取失败时返回 `None`。
    pub fn log_line_text_for_tab(&self, tab_id: usize, line_number: usize) -> Option<String> {
        let source_id = self.tabs.iter().find_map(|tab| {
            if tab.id != tab_id {
                return None;
            }
            match tab.kind {
                TabKind::LogSource { source_id, .. } => Some(source_id),
                TabKind::Empty | TabKind::Settings => None,
            }
        })?;
        let LogOpenState::Ready(handle) = self.log_read_state(source_id)? else {
            return None;
        };

        handle
            .lines(line_number, 1)
            .ok()
            .and_then(|mut lines| lines.pop())
            .map(|line| line.text)
    }

    /// 返回指定日志 tab 当前是否处于文本拖拽选择过程中。
    pub fn is_log_text_selection_drag_active(&self, tab_id: usize) -> bool {
        self.log_tab_view_states
            .get(&tab_id)
            .and_then(|state| state.selection_drag.as_ref())
            .is_some()
    }

    /// 清理指定 tab 的日志文本选区和焦点状态。
    pub(crate) fn reset_log_text_selection_for_tab(&mut self, tab_id: usize) {
        if let Some(state) = self.log_tab_view_states.get_mut(&tab_id) {
            state.selection = None;
            state.selection_drag = None;
            state.is_focused = false;
        }
    }

    /// 清理所有日志 tab 的选区和焦点状态，用于替换来源或关闭全部标签。
    pub(crate) fn reset_log_text_selection(&mut self) {
        for state in self.log_tab_view_states.values_mut() {
            state.selection = None;
            state.selection_drag = None;
            state.is_focused = false;
        }
    }

    /// 根据鼠标横坐标和 GPUI 字形布局计算行内字符列。
    pub fn log_text_position_from_pointer(
        &self,
        tab_id: usize,
        line_index: usize,
        line: &str,
        pointer_x: Pixels,
        window: &mut Window,
    ) -> LogTextPosition {
        let Some(state) = self.log_tab_view_state(tab_id) else {
            return LogTextPosition {
                line_index,
                column: 0,
            };
        };
        let active_handle = self.active_log_handle();
        let line_number_width = log_viewer_line_number_width(
            active_handle.map(|handle| handle.line_count()).unwrap_or(0),
        );
        let horizontal_offset = match active_handle.map(|handle| handle.document()) {
            Some(LogDocument::Paged(_)) => px(-(state.paged_scroll.left_px as f32)),
            Some(LogDocument::InMemory(_)) | None => {
                state
                    .scroll_handle
                    .0
                    .as_ref()
                    .borrow()
                    .base_handle
                    .offset()
                    .x
            }
        };
        let bounds = match active_handle.map(|handle| handle.document()) {
            Some(LogDocument::Paged(_)) => state.paged_viewport_handle.bounds(),
            Some(LogDocument::InMemory(_)) | None => {
                state.scroll_handle.0.as_ref().borrow().base_handle.bounds()
            }
        };
        let text_relative_x = pointer_x
            - bounds.left()
            - horizontal_offset
            - px(line_number_width + LOG_VIEWER_TEXT_LEFT_PADDING);
        let display_line = log_viewer_display_text(line);
        if display_line.is_empty() || text_relative_x <= px(0.0) {
            return LogTextPosition {
                line_index,
                column: 0,
            };
        }

        let mut text_style = window.text_style();
        text_style.font_family = ARGUS_LOG_FONT_FAMILY.into();
        text_style.font_size = px(self.log_content_font_size).into();
        let run = TextRun {
            len: display_line.len(),
            font: text_style.font(),
            color: text_style.color,
            background_color: None,
            underline: None,
            strikethrough: None,
        };
        let font_size = text_style.font_size.to_pixels(window.rem_size());
        let shaped_line = window.text_system().shape_line(
            SharedString::from(display_line.to_string()),
            font_size,
            &[run],
            None,
        );
        let byte_index = shaped_line.closest_index_for_x(text_relative_x);
        let column = char_column_for_byte_index(&display_line, byte_index);

        LogTextPosition { line_index, column }
    }

    /// 从指定行和鼠标位置开始选择日志文本。
    pub fn begin_log_text_selection(
        &mut self,
        tab_id: usize,
        line_index: usize,
        line: &str,
        pointer_x: Pixels,
        window: &mut Window,
    ) {
        self.begin_log_text_selection_with_click_count(
            tab_id, line_index, line, pointer_x, 1, window,
        );
    }

    /// 从指定行和鼠标位置开始选择日志文本，并根据点击次数选择粒度。
    pub fn begin_log_text_selection_with_click_count(
        &mut self,
        tab_id: usize,
        line_index: usize,
        line: &str,
        pointer_x: Pixels,
        click_count: usize,
        window: &mut Window,
    ) {
        let position =
            self.log_text_position_from_pointer(tab_id, line_index, line, pointer_x, window);
        let granularity = text_selection_granularity_for_click_count(click_count);
        let anchor_range =
            log_text_range_for_granularity(line_index, line, position.column, granularity);
        let state = self.log_tab_view_states.entry(tab_id).or_default();
        state.selection = Some(anchor_range.clone());
        state.selection_drag = Some(LogTextSelectionDrag {
            anchor_range,
            granularity,
        });
        state.is_focused = true;
    }

    /// 鼠标拖拽过程中更新日志文本选区。
    pub fn update_log_text_selection(
        &mut self,
        tab_id: usize,
        line_index: usize,
        line: &str,
        pointer_x: Pixels,
        window: &mut Window,
    ) {
        self.update_log_text_selection_by_drag_mode(tab_id, line_index, line, pointer_x, window);
    }

    /// 鼠标拖拽过程中按开始时的粒度扩展日志选区。
    pub fn update_log_text_selection_by_drag_mode(
        &mut self,
        tab_id: usize,
        line_index: usize,
        line: &str,
        pointer_x: Pixels,
        window: &mut Window,
    ) {
        let Some(drag) = self
            .log_tab_view_states
            .get(&tab_id)
            .and_then(|state| state.selection_drag.clone())
        else {
            return;
        };
        let position =
            self.log_text_position_from_pointer(tab_id, line_index, line, pointer_x, window);
        let focus_range =
            log_text_range_for_granularity(line_index, line, position.column, drag.granularity);
        let selection = merge_log_text_ranges(&drag.anchor_range, &focus_range);
        if let Some(state) = self.log_tab_view_states.get_mut(&tab_id) {
            state.selection = Some(selection);
            state.is_focused = true;
        }
    }

    /// 结束日志文本鼠标选择；若没有选中内容则清理锚点。
    pub fn finish_log_text_selection(&mut self, tab_id: usize) {
        if let Some(state) = self.log_tab_view_states.get_mut(&tab_id) {
            state.selection_drag = None;
            if state
                .selection
                .as_ref()
                .is_some_and(LogTextSelection::is_empty)
            {
                state.selection = None;
            }
        }
    }

    /// 在指定日志行内按字符列选中一个词；点到空白时清空选区。
    pub fn select_log_word_at(
        &mut self,
        tab_id: usize,
        line_index: usize,
        line: &str,
        column: usize,
    ) {
        let selection = log_text_range_for_granularity(
            line_index,
            line,
            column,
            TextSelectionGranularity::Word,
        );
        let state = self.log_tab_view_states.entry(tab_id).or_default();
        state.selection = (!selection.is_empty()).then_some(selection);
        state.selection_drag = None;
        state.is_focused = true;
    }

    /// 在指定日志行选中整行展示文本。
    pub fn select_log_text_line(&mut self, tab_id: usize, line_index: usize, line: &str) {
        let selection =
            log_text_range_for_granularity(line_index, line, 0, TextSelectionGranularity::Line);
        let state = self.log_tab_view_states.entry(tab_id).or_default();
        state.selection = Some(selection);
        state.selection_drag = None;
        state.is_focused = true;
    }

    /// 返回指定 tab 中某行的选区字节范围。
    pub fn log_text_selection_byte_range_for_line(
        &self,
        tab_id: usize,
        line_index: usize,
        line: &str,
    ) -> Option<std::ops::Range<usize>> {
        let selection = self.log_tab_view_state(tab_id)?.selection.as_ref()?;
        let (start, end) = selection.normalized();
        if line_index < start.line_index || line_index > end.line_index {
            return None;
        }

        let display_line = log_viewer_display_text(line);
        let line_char_count = character_count(&display_line);
        let start_column = if line_index == start.line_index {
            start.column.min(line_char_count)
        } else {
            0
        };
        let end_column = if line_index == end.line_index {
            end.column.min(line_char_count)
        } else {
            line_char_count
        };
        if start_column == end_column {
            return None;
        }

        Some(
            byte_index_for_character(&display_line, start_column)
                ..byte_index_for_character(&display_line, end_column),
        )
    }

    /// 全选当前日志文档，供 `Cmd/Ctrl+A` 使用。
    pub fn select_all_log_text(&mut self) {
        let Some(tab_id) = self.active_tab().map(|tab| tab.id) else {
            return;
        };
        let Some(handle) = self.active_log_handle() else {
            return;
        };
        let line_count = handle.line_count();
        if line_count == 0 {
            return;
        }
        let last_line_text = handle
            .lines(line_count.saturating_sub(1), 1)
            .ok()
            .and_then(|mut lines| lines.pop())
            .map(|line| log_viewer_display_text(&line.text).into_owned())
            .unwrap_or_default();
        let state = self.log_tab_view_states.entry(tab_id).or_default();
        state.selection = Some(LogTextSelection {
            anchor: LogTextPosition {
                line_index: 0,
                column: 0,
            },
            focus: LogTextPosition {
                line_index: line_count - 1,
                column: character_count(&last_line_text),
            },
        });
        state.selection_drag = None;
        state.is_focused = true;
        self.placeholder_notice = format!("已全选日志文本，共 {line_count} 行");
    }

    /// 返回日志文本当前选中的内容，供复制和搜索关键字预填复用。
    pub(crate) fn selected_log_text(&self) -> Option<String> {
        let active_tab = self.active_tab()?;
        let selection = self.log_tab_view_state(active_tab.id)?.selection.as_ref()?;
        if selection.is_empty() {
            return None;
        }
        let handle = self.active_log_handle()?;
        let (start, end) = selection.normalized();
        if start.line_index >= handle.line_count() {
            return None;
        }

        let end_line = end.line_index.min(handle.line_count().saturating_sub(1));
        let lines = handle
            .lines(
                start.line_index,
                end_line.saturating_sub(start.line_index) + 1,
            )
            .ok()?;
        let mut selected_text = String::new();
        for displayed_line in lines {
            if !selected_text.is_empty() {
                selected_text.push('\n');
            }
            let line = log_viewer_display_text(&displayed_line.text).into_owned();
            let line_char_count = character_count(&line);
            let start_column = if displayed_line.line_number == start.line_index {
                start.column.min(line_char_count)
            } else {
                0
            };
            let end_column = if displayed_line.line_number == end_line {
                end.column.min(line_char_count)
            } else {
                line_char_count
            };
            if start_column < end_column {
                selected_text.push_str(&slice_character_range(&line, start_column..end_column));
            }
        }

        Some(selected_text)
    }

    /// 复制当前活动日志选区；主窗口快捷键拦截层也会调用该入口，避免 GPUI 子元素焦点丢失时复制失效。
    pub fn copy_active_log_text_selection(&mut self, cx: &mut Context<Self>) {
        self.copy_log_text_selection(cx);
    }

    /// 将日志文本选区写入系统剪贴板。
    fn copy_log_text_selection(&mut self, cx: &mut Context<Self>) {
        let Some(selected_text) = self.selected_log_text() else {
            self.placeholder_notice = "日志文本没有可复制的选区，请先选择文本".to_string();
            return;
        };

        let selected_length = character_count(&selected_text);
        let app_context: &gpui::App = (&*cx).borrow();
        app_context.write_to_clipboard(ClipboardItem::new_string(selected_text));
        self.placeholder_notice = format!("已复制日志文本选区，共 {selected_length} 个字符");
    }

    /// 处理日志文本区域的粘贴快捷键；日志查看器只读，因此不会修改真实内容。
    fn paste_log_text_clipboard(&mut self, cx: &mut Context<Self>) {
        let app_context: &gpui::App = (&*cx).borrow();
        let Some(clipboard_text) = app_context
            .read_from_clipboard()
            .and_then(|clipboard_item| clipboard_item.text())
        else {
            self.placeholder_notice = "系统剪贴板没有可粘贴文本".to_string();
            return;
        };

        self.placeholder_notice = format!(
            "日志内容为只读，已读取剪贴板中的 {} 个字符但未写入日志",
            character_count(&clipboard_text)
        );
    }

    /// 处理日志文本阅读区按键，仅维护只读查看器的选择和剪贴板行为。
    pub fn handle_log_text_key(&mut self, keystroke: &Keystroke, cx: &mut Context<Self>) {
        let Some(active_tab_id) = self.active_tab().map(|tab| tab.id) else {
            return;
        };
        if !self
            .log_tab_view_state(active_tab_id)
            .is_some_and(|state| state.is_focused)
        {
            return;
        }

        let key = keystroke.key.to_lowercase();
        if keystroke.modifiers.secondary() && key == "f" {
            self.open_log_search_window(cx);
            return;
        }

        if keystroke.modifiers.secondary() {
            match key.as_str() {
                "a" => self.select_all_log_text(),
                "c" => self.copy_log_text_selection(cx),
                "v" => self.paste_log_text_clipboard(cx),
                _ => {}
            }
            return;
        }

        if key.as_str() == "escape" {
            self.reset_log_text_selection_for_tab(active_tab_id);
            self.placeholder_notice = "已取消日志文本选区".to_string();
        }
    }

    /// 处理分页日志滚轮事件；大日志不交给 GPUI 完整滚动容器，避免巨大内容高度造成精度问题。
    pub fn scroll_paged_log(
        &mut self,
        tab_id: usize,
        source_id: SourceId,
        event: &ScrollWheelEvent,
    ) {
        let Some(LogOpenState::Ready(handle)) = self.log_read_state(source_id) else {
            return;
        };
        let line_count = handle.line_count();
        let longest_display_columns = handle.estimated_longest_display_columns();
        let Some(state) = self.log_tab_view_states.get_mut(&tab_id) else {
            return;
        };

        let pixel_delta = event.delta.pixel_delta(px(LOG_VIEWER_ROW_HEIGHT));
        let viewport = state.paged_viewport_handle.bounds().size;
        let max_vertical = (line_count as f64 * LOG_VIEWER_ROW_HEIGHT as f64
            - f64::from(viewport.height))
        .max(0.0);
        let estimated_char_width = (self.log_content_font_size * 0.62).max(6.0);
        let content_width = longest_display_columns as f64 * estimated_char_width as f64
            + f64::from(px(log_viewer_line_number_width(line_count)
                + LOG_VIEWER_TEXT_LEFT_PADDING
                + LOG_VIEWER_TEXT_RIGHT_PADDING));
        let max_horizontal = (content_width - f64::from(viewport.width)).max(0.0);

        state.paged_scroll.top_px =
            (state.paged_scroll.top_px - f64::from(pixel_delta.y)).clamp(0.0, max_vertical);
        state.paged_scroll.left_px =
            (state.paged_scroll.left_px - f64::from(pixel_delta.x)).clamp(0.0, max_horizontal);
        state.is_focused = true;
    }
}

/// 根据鼠标点击次数返回文本选择粒度。
pub(crate) fn text_selection_granularity_for_click_count(
    click_count: usize,
) -> TextSelectionGranularity {
    match click_count {
        0 | 1 => TextSelectionGranularity::Character,
        2 => TextSelectionGranularity::Word,
        _ => TextSelectionGranularity::Line,
    }
}

/// 根据日志行、字符列和粒度生成目标选区范围。
pub(crate) fn log_text_range_for_granularity(
    line_index: usize,
    line: &str,
    column: usize,
    granularity: TextSelectionGranularity,
) -> LogTextSelection {
    let display_line = log_viewer_display_text(line);
    let line_char_count = character_count(&display_line);
    let cursor = column.min(line_char_count);
    let range = match granularity {
        TextSelectionGranularity::Character => cursor..cursor,
        TextSelectionGranularity::Word => {
            word_range_at(&display_line, cursor).unwrap_or(cursor..cursor)
        }
        TextSelectionGranularity::Line => 0..line_char_count,
    };

    LogTextSelection {
        anchor: LogTextPosition {
            line_index,
            column: range.start,
        },
        focus: LogTextPosition {
            line_index,
            column: range.end,
        },
    }
}

/// 合并两个日志选区范围，确保拖拽时完整覆盖起始词/行和当前词/行。
pub(crate) fn merge_log_text_ranges(
    anchor_range: &LogTextSelection,
    focus_range: &LogTextSelection,
) -> LogTextSelection {
    let (anchor_start, anchor_end) = anchor_range.normalized();
    let (focus_start, focus_end) = focus_range.normalized();
    let start = if log_text_position_le(anchor_start, focus_start) {
        anchor_start
    } else {
        focus_start
    };
    let end = if log_text_position_le(anchor_end, focus_end) {
        focus_end
    } else {
        anchor_end
    };

    LogTextSelection {
        anchor: start,
        focus: end,
    }
}
