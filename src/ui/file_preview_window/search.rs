//! 文件职责：管理只读预览的窗口内查找、原生输入、后台搜索和命中定位。

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use gpui::{App, ScrollStrategy};

use super::*;
use crate::infra::text_input::TextInputState;
use crate::infra::text_selection::NativeTextEdit;
use crate::search::preview_search::{
    PreviewSearchMatch, PreviewSearchResults, search_preview_lines,
};
use crate::ui::components::icon_button::{IconButtonSize, render_icon_button};
use crate::ui::components::input::{
    Input, InputPointerAction, InputSize, NativeInput, render_input,
};

/// 连续输入只搜索最后一次快照，避免每个按键启动全文扫描。
const PREVIEW_SEARCH_DEBOUNCE: Duration = Duration::from_millis(180);
/// 浮动查找条宽度，窄窗口由外层最大宽度约束。
const PREVIEW_SEARCH_BAR_WIDTH: f32 = 470.0;
/// 命中定位与可见区域边界保留的空间。
const PREVIEW_SEARCH_REVEAL_MARGIN: f32 = 24.0;

/// 搜索状态只保留结果索引，正文始终由预览与工作线程共享。
#[derive(Debug)]
pub(super) struct PreviewSearchState {
    pub(super) is_open: bool,
    pub(super) input: TextInputState,
    pub(super) is_case_sensitive: bool,
    pub(super) results: Arc<PreviewSearchResults>,
    pub(super) active_index: Option<usize>,
    pub(super) is_searching: bool,
    pub(super) error: Option<String>,
    pub(super) generation: usize,
    pub(super) cancel_token: Option<Arc<AtomicBool>>,
    should_reveal_active: bool,
}

impl Default for PreviewSearchState {
    fn default() -> Self {
        Self {
            is_open: false,
            input: TextInputState::default(),
            is_case_sensitive: false,
            results: Arc::new(PreviewSearchResults {
                matches: Vec::new(),
                is_truncated: false,
            }),
            active_index: None,
            is_searching: false,
            error: None,
            generation: 0,
            cancel_token: None,
            should_reveal_active: false,
        }
    }
}

impl PreviewSearchState {
    /// 令在途任务失效，防抖任务和扫描任务使用同一令牌。
    pub(super) fn cancel(&mut self) {
        if let Some(token) = self.cancel_token.take() {
            token.store(true, Ordering::Relaxed);
        }
        self.generation = self.generation.wrapping_add(1);
        self.is_searching = false;
        self.should_reveal_active = false;
    }

    /// 返回当前命中；输入改变后旧结果会立即清理，不会跳转至过期位置。
    pub(super) fn active_match(&self) -> Option<&PreviewSearchMatch> {
        self.active_index
            .and_then(|index| self.results.matches.get(index))
    }
}

impl Drop for PreviewSearchState {
    fn drop(&mut self) {
        self.cancel();
    }
}

impl FilePreviewWindow {
    /// 打开查找并全选查询；仅首次打开时采用正文中的单行选区。
    pub(super) fn open_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.text_lines().is_none() {
            return;
        }
        if !self.search.is_open {
            let selected = self.preview_selection.as_ref().and_then(|selection| {
                let (start, end) = selection.normalized();
                (start.line == end.line)
                    .then(|| self.selected_preview_text())
                    .flatten()
            });
            if let Some(selected) = selected.filter(|selected| !selected.is_empty()) {
                self.search.input = TextInputState::from_value(selected);
            }
            self.search.is_open = true;
            self.queue_search(cx);
        }
        self.search.input.is_focused = true;
        self.search.input.cursor = character_count(&self.search.input.value);
        self.search.input.selection_anchor = Some(0);
        self.search.input.marked_range = None;
        self.search_focus.focus(window);
        cx.notify();
    }

    /// 关闭查找只取消查询和高亮，保留查询文本供下次使用，焦点返回正文。
    pub(super) fn close_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.search.cancel();
        self.search.is_open = false;
        self.search.input.clear_focus();
        self.search.results = Arc::new(PreviewSearchResults {
            matches: Vec::new(),
            is_truncated: false,
        });
        self.search.active_index = None;
        self.search.error = None;
        self.root_focus.focus(window);
        cx.notify();
    }

    /// 接收系统输入法编辑，候选态只更新输入框，提交后再搜索。
    pub(super) fn apply_search_native_edit(
        &mut self,
        edit: NativeTextEdit,
        cx: &mut Context<Self>,
    ) {
        if !self.search.is_open {
            return;
        }
        self.search.input.apply_native_edit(&edit);
        self.search.input.is_focused = true;
        self.queue_search(cx);
        cx.notify();
    }

    /// 以共享正文和不可变查询快照启动可取消的防抖搜索。
    pub(super) fn queue_search(&mut self, cx: &mut Context<Self>) {
        self.search.cancel();
        self.search.results = Arc::new(PreviewSearchResults {
            matches: Vec::new(),
            is_truncated: false,
        });
        self.search.active_index = None;
        self.search.error = None;
        if !self.search.is_open
            || self.search.input.value.is_empty()
            || self.search.input.marked_range.is_some()
        {
            return;
        }
        let FilePreviewBody::Text { lines, .. } = &self.body else {
            return;
        };
        let lines = lines.clone();
        let keyword = self.search.input.value.clone();
        let is_case_sensitive = self.search.is_case_sensitive;
        let generation = self.search.generation;
        let cancel_token = Arc::new(AtomicBool::new(false));
        self.search.cancel_token = Some(cancel_token.clone());
        self.search.is_searching = true;
        cx.spawn(async move |view, cx| {
            cx.background_executor()
                .timer(PREVIEW_SEARCH_DEBOUNCE)
                .await;
            if cancel_token.load(Ordering::Relaxed) {
                return;
            }
            let result = cx
                .background_executor()
                .spawn(async move {
                    search_preview_lines(&lines, &keyword, is_case_sensitive, &cancel_token)
                })
                .await;
            // 窗口关闭后弱引用失效属于预期取消，无需再向已经销毁的窗口报告结果。
            view.update(cx, |view, cx| {
                if !view.search.is_open || view.search.generation != generation {
                    return;
                }
                view.search.cancel_token = None;
                view.search.is_searching = false;
                match result {
                    Ok(Some(results)) => {
                        view.search.active_index = (!results.matches.is_empty()).then_some(0);
                        view.search.results = Arc::new(results);
                        view.search.should_reveal_active = view.search.active_index.is_some();
                    }
                    Ok(None) => return,
                    Err(message) => view.search.error = Some(message),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// 文档顺序循环跳转；没有完成的命中时保持当前位置。
    pub(super) fn navigate_search(
        &mut self,
        backwards: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let count = self.search.results.matches.len();
        if !self.search.is_open || count == 0 {
            return;
        }
        let current = self
            .search
            .active_index
            .unwrap_or(if backwards { 0 } else { count - 1 });
        self.search.active_index = Some(if backwards {
            (current + count - 1) % count
        } else {
            (current + 1) % count
        });
        self.search.should_reveal_active = true;
        self.reveal_search_match(window);
        cx.notify();
    }

    /// 用正文实际字体定位字节范围，同时更新横向内容宽度，保证长行命中可见。
    pub(super) fn reveal_search_match(&mut self, window: &mut Window) {
        if !self.search.should_reveal_active {
            return;
        }
        let Some(hit) = self.search.active_match().cloned() else {
            return;
        };
        let handle = self.scroll.0.as_ref().borrow().base_handle.clone();
        let width = handle.bounds().size.width;
        if width <= px(0.0) {
            return;
        }
        let Some(line) = self.text_lines().and_then(|lines| lines.get(hit.line)) else {
            return;
        };
        let line_count = self.text_lines().map(<[String]>::len).unwrap_or(0);
        let mut text_style = window.text_style();
        text_style.font_family = ARGUS_LOG_FONT_FAMILY.into();
        text_style.font_size = px(FILE_PREVIEW_FONT_SIZE).into();
        let shaped = window.text_system().shape_line(
            SharedString::from(line.clone()),
            px(FILE_PREVIEW_FONT_SIZE),
            &[TextRun {
                len: line.len(),
                font: text_style.font(),
                color: text_style.color,
                background_color: None,
                underline: None,
                strikethrough: None,
            }],
            None,
        );
        let gutter = px(log_viewer_line_number_width(line_count) + FILE_PREVIEW_TEXT_LEFT_PADDING);
        let start = gutter + shaped.x_for_index(hit.range.start);
        let end = gutter + shaped.x_for_index(hit.range.end);
        let line_width = gutter + shaped.width + px(FILE_PREVIEW_TEXT_LEFT_PADDING);
        self.content_min_width = self.content_min_width.max(line_width);
        let offset = handle.offset();
        let visible_left = -offset.x;
        let margin = px(PREVIEW_SEARCH_REVEAL_MARGIN);
        let next_left = if line_width <= width {
            px(0.0)
        } else if end - start > width - margin * 2.0 || start < visible_left + margin {
            (start - margin).max(px(0.0))
        } else if end > visible_left + width - margin {
            (end - width + margin).max(px(0.0))
        } else {
            visible_left
        };
        handle.set_offset(point(-next_left, offset.y));
        self.scroll.scroll_to_item(hit.line, ScrollStrategy::Center);
        self.search.should_reveal_active = false;
    }

    /// 只处理查找快捷键和正文选择快捷键，普通字符必须继续交给原生输入系统。
    pub(super) fn handle_preview_key(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let key = event.keystroke.key.to_lowercase();
        let shortcut = event.keystroke.modifiers.control || event.keystroke.modifiers.secondary();
        if shortcut && key == "f" {
            cx.stop_propagation();
            self.open_search(window, cx);
            return;
        }
        if self.search.is_open && key == "escape" {
            if self.search.input.marked_range.is_some() && self.search_focus.is_focused(window) {
                return;
            }
            cx.stop_propagation();
            self.close_search(window, cx);
            return;
        }
        if self.search.is_open && matches!(key.as_str(), "f3" | "enter") {
            if self.search.input.marked_range.is_some() && self.search_focus.is_focused(window) {
                return;
            }
            cx.stop_propagation();
            self.navigate_search(event.keystroke.modifiers.shift, window, cx);
            return;
        }
        if self.search_focus.is_focused(window) {
            return;
        }
        if shortcut {
            match key.as_str() {
                "c" => {
                    cx.stop_propagation();
                    self.copy_preview_selection(cx);
                }
                "a" => {
                    cx.stop_propagation();
                    self.select_all_preview_text();
                    cx.notify();
                }
                _ => {}
            }
        } else if key == "escape"
            && (self.preview_selection.is_some() || self.preview_selection_drag.is_some())
        {
            cx.stop_propagation();
            self.clear_preview_selection();
            cx.notify();
        }
    }

    /// 输入框快捷键与正文选择隔离；返回是否消费事件。
    fn handle_search_input_key(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let key = event.keystroke.key.to_lowercase();
        let shortcut = event.keystroke.modifiers.control || event.keystroke.modifiers.secondary();
        if self.search.input.marked_range.is_some() && matches!(key.as_str(), "enter" | "escape") {
            return false;
        }
        if shortcut {
            match key.as_str() {
                "f" => {
                    self.open_search(window, cx);
                }
                "a" => {
                    self.search.input.cursor = character_count(&self.search.input.value);
                    self.search.input.selection_anchor = Some(0);
                    self.search.input.marked_range = None;
                }
                "c" | "x" => {
                    if let Some(range) = self.search.input.selection_range() {
                        cx.write_to_clipboard(ClipboardItem::new_string(slice_character_range(
                            &self.search.input.value,
                            range.clone(),
                        )));
                        if key == "x" {
                            self.replace_search_input(range, "", cx);
                        }
                    }
                }
                "v" => {
                    if let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) {
                        let range = self
                            .search
                            .input
                            .selection_range()
                            .unwrap_or(self.search.input.cursor..self.search.input.cursor);
                        self.replace_search_input(range, &text.replace(['\r', '\n'], " "), cx);
                    }
                }
                _ => return false,
            }
        } else {
            match key.as_str() {
                "escape" => self.close_search(window, cx),
                "enter" | "f3" => self.navigate_search(event.keystroke.modifiers.shift, window, cx),
                "backspace" | "delete" => {
                    let cursor = self.search.input.cursor;
                    let length = character_count(&self.search.input.value);
                    let range = self.search.input.selection_range().unwrap_or_else(|| {
                        if key == "backspace" {
                            cursor.saturating_sub(1)..cursor
                        } else {
                            cursor..(cursor + 1).min(length)
                        }
                    });
                    self.replace_search_input(range, "", cx);
                }
                "left" | "arrowleft" | "right" | "arrowright" | "home" | "end" => {
                    let length = character_count(&self.search.input.value);
                    let current = self.search.input.cursor;
                    let next = match key.as_str() {
                        "home" => 0,
                        "end" => length,
                        "left" | "arrowleft" => self
                            .search
                            .input
                            .selection_range()
                            .filter(|_| !event.keystroke.modifiers.shift)
                            .map(|range| range.start)
                            .unwrap_or(current.saturating_sub(1)),
                        _ => self
                            .search
                            .input
                            .selection_range()
                            .filter(|_| !event.keystroke.modifiers.shift)
                            .map(|range| range.end)
                            .unwrap_or((current + 1).min(length)),
                    };
                    if event.keystroke.modifiers.shift {
                        self.search.input.selection_anchor.get_or_insert(current);
                    } else {
                        self.search.input.selection_anchor = None;
                    }
                    self.search.input.cursor = next;
                    self.search.input.marked_range = None;
                }
                _ => return false,
            }
        }
        cx.notify();
        true
    }

    /// 统一键盘删除、剪贴板与原生输入的字符范围语义。
    fn replace_search_input(&mut self, range: Range<usize>, text: &str, cx: &mut Context<Self>) {
        let end = range.start + character_count(text);
        self.apply_search_native_edit(
            NativeTextEdit {
                replacement_range: range,
                text: text.to_string(),
                selected_range: end..end,
                marked_range: None,
            },
            cx,
        );
    }

    /// 渲染右上角浮动查找条；输入内容和按钮不会把点击冒泡给正文。
    pub(super) fn render_search_bar(&self, cx: &mut Context<Self>) -> AnyElement {
        let entity = cx.entity();
        let native_entity = entity.clone();
        let key_entity = entity.clone();
        let click_entity = entity.clone();
        let pointer_entity = entity.clone();
        let input = &self.search.input;
        let focus = self.search_focus.clone();
        let native = NativeInput::new(focus.clone(), move |edit, _, cx: &mut App| {
            native_entity.update(cx, |view, cx| view.apply_search_native_edit(edit, cx));
        });
        let input_element = render_input(
            Input {
                id: "file-preview-search-input",
                placeholder: "在文件中查找",
                value: input.value.clone(),
                is_disabled: false,
                is_focused: input.is_focused,
                cursor_index: input.cursor,
                selection_range: input.selection_range(),
                marked_range: input.marked_range.clone(),
                is_pointer_selecting: input.selection_drag.is_some(),
                is_secret: false,
                size: InputSize::Compact,
                leading_accessory: None,
                trailing_accessory: None,
                native_input: Some(native),
                scroll_handle: input.scroll_handle.clone(),
            },
            &self.theme,
            move |event, window, cx| {
                key_entity.update(cx, |view, cx| {
                    if view.handle_search_input_key(event, window, cx) {
                        cx.stop_propagation();
                    }
                });
            },
            move |_, window, cx| {
                click_entity.update(cx, |view, cx| {
                    view.search.input.is_focused = true;
                    focus.focus(window);
                    cx.notify();
                });
            },
            move |event, _, cx| {
                pointer_entity.update(cx, |view, cx| {
                    view.search.input.is_focused = true;
                    match event.action {
                        InputPointerAction::Begin => view
                            .search
                            .input
                            .begin_pointer_selection(event.character_index, event.granularity),
                        InputPointerAction::Extend => view
                            .search
                            .input
                            .update_pointer_selection(event.character_index),
                        InputPointerAction::Finish => view.search.input.finish_pointer_selection(),
                    }
                    cx.notify();
                });
            },
            move |_, _, _| {},
        );
        let count = if self.search.is_searching {
            "查找中...".to_string()
        } else if self.search.input.value.is_empty() {
            "0 / 0".to_string()
        } else {
            format!(
                "{} / {}{}",
                self.search.active_index.map(|index| index + 1).unwrap_or(0),
                self.search.results.matches.len(),
                if self.search.results.is_truncated {
                    "+"
                } else {
                    ""
                }
            )
        };
        let mut bar = div()
            .id("file-preview-search-bar")
            .debug_selector(|| "file-preview-search-bar".to_string())
            .w(px(PREVIEW_SEARCH_BAR_WIDTH))
            .max_w_full()
            .p_2()
            .flex()
            .flex_col()
            .gap_1()
            .rounded_sm()
            .shadow_lg()
            .border_1()
            .border_color(rgb(self.theme.border))
            .bg(rgb(self.theme.title_bar))
            .font_family(ARGUS_UI_FONT_FAMILY)
            .text_size(px(11.0))
            .occlude()
            .on_click(|_, _, cx| cx.stop_propagation())
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_1()
                    .child(div().flex_1().min_w(px(0.0)).child(input_element))
                    .child(self.search_icon_button(
                        "file-preview-search-case",
                        ArgusIcon::CaseSensitive,
                        "区分大小写",
                        self.search.is_case_sensitive,
                        |view, _, cx| {
                            view.search.is_case_sensitive = !view.search.is_case_sensitive;
                            view.queue_search(cx);
                            cx.notify();
                        },
                        cx,
                    ))
                    .child(
                        div()
                            .w(px(90.0))
                            .flex_none()
                            .whitespace_nowrap()
                            .text_color(rgb(self.theme.foreground_muted))
                            .child(count),
                    )
                    .child(self.search_icon_button(
                        "file-preview-search-previous",
                        ArgusIcon::ArrowUp,
                        "上一处（Shift+Enter / Shift+F3）",
                        false,
                        |view, window, cx| view.navigate_search(true, window, cx),
                        cx,
                    ))
                    .child(self.search_icon_button(
                        "file-preview-search-next",
                        ArgusIcon::ArrowDown,
                        "下一处（Enter / F3）",
                        false,
                        |view, window, cx| view.navigate_search(false, window, cx),
                        cx,
                    ))
                    .child(self.search_icon_button(
                        "file-preview-search-close",
                        ArgusIcon::Close,
                        "关闭查找（Esc）",
                        false,
                        |view, window, cx| view.close_search(window, cx),
                        cx,
                    )),
            );
        if let Some(error) = &self.search.error {
            bar = bar.child(div().text_color(rgb(self.theme.error)).child(error.clone()));
        }
        if matches!(
            &self.body,
            FilePreviewBody::Text {
                truncated: true,
                ..
            }
        ) {
            bar = bar.child(
                div()
                    .text_color(rgb(self.theme.foreground_muted))
                    .child("内容已截断，仅查找已加载部分"),
            );
        }
        if self.search.results.is_truncated {
            bar = bar.child(
                div()
                    .text_color(rgb(self.theme.foreground_muted))
                    .child("匹配过多，仅显示前 20,000 处"),
            );
        }
        div()
            .absolute()
            .top(px(TITLE_BAR_HEIGHT + 14.0))
            .left(px(22.0))
            .right(px(22.0))
            .flex()
            .justify_end()
            .child(bar)
            .into_any_element()
    }

    /// 查找开关和导航复用统一图标按钮与中文提示，并保持原输入焦点。
    fn search_icon_button(
        &self,
        id: &'static str,
        icon: ArgusIcon,
        tooltip: &'static str,
        selected: bool,
        action: impl Fn(&mut Self, &mut Window, &mut Context<Self>) + 'static,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let entity = cx.entity();
        div()
            .debug_selector(move || id.to_string())
            .flex_none()
            .child(render_icon_button(
                id,
                icon,
                tooltip,
                selected,
                IconButtonSize::Small,
                &self.theme,
                move |_, window, cx| {
                    cx.stop_propagation();
                    entity.update(cx, |view, cx| action(view, window, cx));
                },
            ))
            .into_any_element()
    }
}
